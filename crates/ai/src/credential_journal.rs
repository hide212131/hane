//! Persistent credential operation journal.
//!
//! Per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section
//! 7.3, updating or deleting the Custom Provider credential is transacted
//! through this journal so a crash at any point (before the secret is
//! written, between writing it and swapping `AiSettings` to reference it,
//! or between that swap and cleaning up the now-unreferenced old
//! credential) leaves enough non-secret state on disk for the next startup
//! to finish the operation instead of leaking or losing track of a
//! credential store entry. The journal never stores a secret value itself.
//!
//! Contract (`update`, mirrors section 7.3 steps 1-7):
//! 1. Caller assigns a new [`CredentialRef`] (`CredentialRef::generate`).
//! 2. [`CredentialJournal::begin`] durably records the operation as
//!    `PendingNew` *before* the secret is written anywhere.
//! 3. Caller writes the new secret to the [`crate::secrets::CredentialStore`].
//! 4. Caller atomically replaces `AiSettings` to reference the new
//!    `CredentialRef`.
//! 5. [`CredentialJournal::mark_settings_swapped`] durably records that step
//!    4 landed; only cleanup of the old credential remains.
//! 6. Caller deletes the now-unreferenced old credential.
//! 7. [`CredentialJournal::complete`] removes the journal entry.
//!
//! `delete` follows the same shape with no `new_credential_ref`: settings is
//! rewritten to drop `credential_ref` (step 4), then the old credential is
//! deleted (step 6).
//!
//! [`recover`] implements step 6 of section 7.3's recovery table: given the
//! *current*, already-durable `AiSettings`, it infers which side of an
//! in-flight operation actually won and retries only the safe, idempotent
//! remainder (a credential delete, or a settings replace this process
//! itself would otherwise redo) rather than guessing.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

use crate::atomic_file::{atomic_write_bytes, read_to_string_if_exists, AtomicWriteError};
use crate::secrets::{CredentialRef, CredentialStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperationKind {
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperationState {
    /// The new `CredentialRef` (if any) has been durably recorded in the
    /// journal; the secret and the `AiSettings` swap referencing it may or
    /// may not have landed yet.
    PendingNew,
    /// The non-secret `AiSettings` replace has durably landed; only cleanup
    /// of the now-unreferenced old credential remains.
    SettingsSwapped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialOperation {
    pub kind: JournalOperationKind,
    pub state: JournalOperationState,
    /// Absent for a `Delete` with no replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_credential_ref: Option<CredentialRef>,
    /// Absent for an `Update`/`Delete` that had no prior credential (e.g.
    /// the very first Custom Provider key ever saved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_credential_ref: Option<CredentialRef>,
    /// `AiSettings.settings_generation` observed when this operation began,
    /// recorded for diagnostics only.
    pub settings_generation_before: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JournalFile {
    #[serde(default)]
    operations: Vec<CredentialOperation>,
}

pub struct CredentialJournal {
    path: PathBuf,
}

impl CredentialJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        CredentialJournal { path: path.into() }
    }

    fn read(&self) -> io::Result<JournalFile> {
        match read_to_string_if_exists(&self.path)? {
            Some(contents) => {
                serde_json::from_str(&contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            None => Ok(JournalFile::default()),
        }
    }

    fn write(&self, file: &JournalFile) -> io::Result<()> {
        let bytes =
            serde_json::to_vec_pretty(file).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Unlike `AiSettingsStore::write_while_locked`, no caller here reacts
        // to a write failure by deleting a resource the new content might
        // already reference (`recover`'s decisions are always re-derived
        // from a fresh read of the *settings* file, not from whether this
        // journal write's own durability was confirmed), so both
        // `AtomicWriteError` variants collapse to a plain I/O error here.
        atomic_write_bytes(&self.path, &bytes).map_err(|e| match e {
            AtomicWriteError::NotPersisted(e) | AtomicWriteError::RenameSucceededSyncFailed(e) => e,
        })
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.read()?.operations.is_empty())
    }

    pub fn pending_operations(&self) -> io::Result<Vec<CredentialOperation>> {
        Ok(self.read()?.operations)
    }

    /// Step 2 (`update`) / the `delete` equivalent: durably record the
    /// pending operation before any secret is written or `AiSettings` is
    /// touched.
    pub fn begin(&self, operation: CredentialOperation) -> io::Result<()> {
        let mut file = self.read()?;
        file.operations.push(operation);
        self.write(&file)
    }

    /// Step 5: mark the in-flight operation matching `new_credential_ref`
    /// (or, for a `delete`, the operation whose `old_credential_ref` matches
    /// and has no `new_credential_ref`) as `SettingsSwapped` once the
    /// non-secret `AiSettings` replace durably landed.
    fn mark_settings_swapped_matching(&self, matches: impl Fn(&CredentialOperation) -> bool) -> io::Result<()> {
        let mut file = self.read()?;
        for op in &mut file.operations {
            if op.state == JournalOperationState::PendingNew && matches(op) {
                op.state = JournalOperationState::SettingsSwapped;
            }
        }
        self.write(&file)
    }

    pub fn mark_update_settings_swapped(&self, new_credential_ref: &CredentialRef) -> io::Result<()> {
        self.mark_settings_swapped_matching(|op| {
            op.kind == JournalOperationKind::Update && op.new_credential_ref.as_ref() == Some(new_credential_ref)
        })
    }

    pub fn mark_delete_settings_swapped(&self, old_credential_ref: &CredentialRef) -> io::Result<()> {
        self.mark_settings_swapped_matching(|op| {
            op.kind == JournalOperationKind::Delete && op.old_credential_ref.as_ref() == Some(old_credential_ref)
        })
    }

    /// Step 7: remove a fully completed operation record (cleanup of the old
    /// credential, if any, must already have been attempted).
    pub fn complete(
        &self,
        new_credential_ref: Option<&CredentialRef>,
        old_credential_ref: Option<&CredentialRef>,
    ) -> io::Result<()> {
        let mut file = self.read()?;
        file.operations
            .retain(|op| !(op.new_credential_ref.as_ref() == new_credential_ref && op.old_credential_ref.as_ref() == old_credential_ref));
        self.write(&file)
    }
}

#[derive(Debug)]
pub struct RecoveryOutcome {
    /// Operations that were fully resolved and removed from the journal.
    pub completed: usize,
    /// Operations left in the journal because neither settings side could be
    /// safely confirmed against `current_credential_ref` (an inconsistency
    /// requiring diagnosis rather than an automatic choice).
    pub left_for_diagnosis: usize,
    /// Operations whose settings side was confirmed, but cleaning up the
    /// now-unreferenced credential via [`CredentialStore::delete`] failed
    /// (e.g. the OS credential store was transiently unavailable). The
    /// journal entry is deliberately left in place (fail-closed: never
    /// marked complete) instead of being dropped as if cleanup had
    /// succeeded, so a later `recover` retries the same idempotent delete.
    pub deletion_failed: usize,
}

/// Recovers every pending journal entry against the already-durable
/// `current_credential_ref` read from `AiSettings.custom.credential_ref`.
/// Never chooses which side "wins" by guessing: it only retries the
/// idempotent credential-store cleanup implied by whichever side
/// `current_credential_ref` already shows was durably applied.
///
/// `on_pending_settings_swap` is invoked when a `delete` operation's
/// `AiSettings` replace (dropping `credential_ref`) did not durably land
/// before the crash: recovery cannot itself decide new field values for an
/// unrelated settings write, so this hands control back to the caller (which
/// owns the actual settings mutation) to retry that specific, already-known
/// replace. Returning `Ok(true)` tells recovery the replace has now landed
/// so cleanup can proceed in the same pass; `Ok(false)` or an error leaves
/// the entry for the next recovery attempt.
pub fn recover(
    journal: &CredentialJournal,
    current_credential_ref: Option<&CredentialRef>,
    store: &dyn CredentialStore,
    mut on_pending_settings_swap: impl FnMut(&CredentialOperation) -> io::Result<bool>,
) -> io::Result<RecoveryOutcome> {
    let mut completed = 0usize;
    let mut left_for_diagnosis = 0usize;
    let mut deletion_failed = 0usize;

    // Deletes the now-unreferenced credential (if any) and completes the
    // journal entry only when that delete actually succeeded (idempotently
    // succeeding when the entry is already gone counts as success). A
    // failed delete must never be treated as if cleanup had happened: the
    // entry is left in the journal (fail-closed) so a later `recover` retries
    // it, rather than the journal silently forgetting a credential store
    // entry that may still exist.
    let delete_and_complete = |to_delete: Option<&CredentialRef>,
                                    new_ref: Option<&CredentialRef>,
                                    old_ref: Option<&CredentialRef>|
     -> io::Result<bool> {
        if let Some(target) = to_delete
            && store.delete(target).is_err()
        {
            return Ok(false);
        }
        journal.complete(new_ref, old_ref)?;
        Ok(true)
    };

    for op in journal.pending_operations()? {
        match op.kind {
            JournalOperationKind::Update => {
                if current_credential_ref == op.new_credential_ref.as_ref() {
                    // The new side won: retry deleting the now-unreferenced
                    // old credential (idempotent if already gone).
                    if delete_and_complete(op.old_credential_ref.as_ref(), op.new_credential_ref.as_ref(), op.old_credential_ref.as_ref())? {
                        completed += 1;
                    } else {
                        deletion_failed += 1;
                    }
                } else if current_credential_ref == op.old_credential_ref.as_ref() {
                    // The old side is still active: the settings swap never
                    // landed. Clean up the unreferenced new credential
                    // instead of leaking it.
                    if delete_and_complete(op.new_credential_ref.as_ref(), op.new_credential_ref.as_ref(), op.old_credential_ref.as_ref())? {
                        completed += 1;
                    } else {
                        deletion_failed += 1;
                    }
                } else {
                    // Neither side matches the durable settings: leave for
                    // diagnosis rather than deleting a credential that might
                    // still be the one actually in use.
                    left_for_diagnosis += 1;
                }
            }
            JournalOperationKind::Delete => {
                if current_credential_ref == op.old_credential_ref.as_ref() {
                    // The settings replace dropping `credential_ref` never
                    // durably landed. Ask the caller to retry that specific
                    // write; only proceed to cleanup once it confirms.
                    if on_pending_settings_swap(&op)? {
                        if delete_and_complete(op.old_credential_ref.as_ref(), None, op.old_credential_ref.as_ref())? {
                            completed += 1;
                        } else {
                            deletion_failed += 1;
                        }
                    }
                } else if current_credential_ref.is_none() {
                    // credential_ref already cleared: retry deleting the
                    // now-unreferenced secret.
                    if delete_and_complete(op.old_credential_ref.as_ref(), None, op.old_credential_ref.as_ref())? {
                        completed += 1;
                    } else {
                        deletion_failed += 1;
                    }
                } else {
                    // Settings now reference something else entirely: leave
                    // for diagnosis instead of deleting a credential that
                    // might still be relevant.
                    left_for_diagnosis += 1;
                }
            }
        }
    }

    Ok(RecoveryOutcome { completed, left_for_diagnosis, deletion_failed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::FakeCredentialStore;

    fn unique_journal(name: &str) -> CredentialJournal {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("hane-ai-journal-test-{}-{name}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CredentialJournal::new(dir.join("credential-journal.json"))
    }

    #[test]
    fn new_journal_is_empty() {
        let journal = unique_journal("empty");
        assert!(journal.is_empty().unwrap());
    }

    #[test]
    fn the_persisted_journal_file_never_contains_a_secret_value() {
        let journal = unique_journal("no_secret_leak");
        let new_ref = CredentialRef::generate();
        let old_ref = CredentialRef::generate();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Update,
                state: JournalOperationState::PendingNew,
                new_credential_ref: Some(new_ref),
                old_credential_ref: Some(old_ref),
                settings_generation_before: 1,
            })
            .unwrap();

        // `CredentialOperation` has no field capable of holding a raw secret
        // (only `CredentialRef` identifiers), but assert directly against
        // the bytes actually written to disk so a future field addition
        // that accidentally serializes a secret is caught here.
        let on_disk = std::fs::read_to_string(&journal.path).unwrap();
        assert!(!on_disk.to_ascii_lowercase().contains("sk-"), "journal file must never contain an API key: {on_disk}");
        assert!(!on_disk.to_ascii_lowercase().contains("authorization"), "journal file must never contain an Authorization value: {on_disk}");
    }

    #[test]
    fn begin_then_complete_round_trips_through_settings_swapped() {
        let journal = unique_journal("round_trip");
        let new_ref = CredentialRef::generate();
        let old_ref = CredentialRef::generate();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Update,
                state: JournalOperationState::PendingNew,
                new_credential_ref: Some(new_ref.clone()),
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 3,
            })
            .unwrap();
        assert!(!journal.is_empty().unwrap());

        journal.mark_update_settings_swapped(&new_ref).unwrap();
        let pending = journal.pending_operations().unwrap();
        assert_eq!(pending[0].state, JournalOperationState::SettingsSwapped);

        journal.complete(Some(&new_ref), Some(&old_ref)).unwrap();
        assert!(journal.is_empty().unwrap());
    }

    #[test]
    fn recover_update_where_new_side_won_deletes_old_and_completes() {
        let journal = unique_journal("recover_new_won");
        let store = FakeCredentialStore::new();
        let new_ref = CredentialRef::generate();
        let old_ref = CredentialRef::generate();
        store.set(&old_ref, "old-secret").unwrap();
        store.set(&new_ref, "new-secret").unwrap();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Update,
                state: JournalOperationState::SettingsSwapped,
                new_credential_ref: Some(new_ref.clone()),
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        let outcome = recover(&journal, Some(&new_ref), &store, |_| Ok(false)).unwrap();

        assert_eq!(outcome.completed, 1);
        assert_eq!(outcome.left_for_diagnosis, 0);
        assert!(journal.is_empty().unwrap());
        assert!(store.get(&old_ref).unwrap().is_none(), "old credential must be cleaned up");
        assert!(store.get(&new_ref).unwrap().is_some(), "new credential must be kept");
    }

    #[test]
    fn recover_update_where_old_side_still_active_deletes_unreferenced_new() {
        let journal = unique_journal("recover_old_won");
        let store = FakeCredentialStore::new();
        let new_ref = CredentialRef::generate();
        let old_ref = CredentialRef::generate();
        store.set(&old_ref, "old-secret").unwrap();
        store.set(&new_ref, "new-secret").unwrap();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Update,
                state: JournalOperationState::PendingNew,
                new_credential_ref: Some(new_ref.clone()),
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        // The AiSettings replace never landed: current settings still
        // reference the old credential.
        let outcome = recover(&journal, Some(&old_ref), &store, |_| Ok(false)).unwrap();

        assert_eq!(outcome.completed, 1);
        assert!(journal.is_empty().unwrap());
        assert!(store.get(&new_ref).unwrap().is_none(), "unreferenced new credential must be cleaned up");
        assert!(store.get(&old_ref).unwrap().is_some(), "old credential must be kept");
    }

    #[test]
    fn recover_update_where_neither_side_matches_is_left_for_diagnosis() {
        let journal = unique_journal("recover_neither");
        let store = FakeCredentialStore::new();
        let new_ref = CredentialRef::generate();
        let old_ref = CredentialRef::generate();
        let unrelated_ref = CredentialRef::generate();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Update,
                state: JournalOperationState::PendingNew,
                new_credential_ref: Some(new_ref.clone()),
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        let outcome = recover(&journal, Some(&unrelated_ref), &store, |_| Ok(false)).unwrap();

        assert_eq!(outcome.completed, 0);
        assert_eq!(outcome.left_for_diagnosis, 1);
        assert!(!journal.is_empty().unwrap(), "an unresolvable entry must not be silently dropped");
    }

    #[test]
    fn recover_delete_where_credential_ref_already_cleared_retries_cleanup() {
        let journal = unique_journal("recover_delete_cleared");
        let store = FakeCredentialStore::new();
        let old_ref = CredentialRef::generate();
        store.set(&old_ref, "old-secret").unwrap();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Delete,
                state: JournalOperationState::SettingsSwapped,
                new_credential_ref: None,
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        let outcome = recover(&journal, None, &store, |_| Ok(false)).unwrap();

        assert_eq!(outcome.completed, 1);
        assert!(journal.is_empty().unwrap());
        assert!(store.get(&old_ref).unwrap().is_none());
    }

    #[test]
    fn recover_delete_where_settings_replace_never_landed_asks_caller_to_retry() {
        let journal = unique_journal("recover_delete_pending");
        let store = FakeCredentialStore::new();
        let old_ref = CredentialRef::generate();
        store.set(&old_ref, "old-secret").unwrap();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Delete,
                state: JournalOperationState::PendingNew,
                new_credential_ref: None,
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        // Current settings still reference the old credential: the replace
        // never landed. The caller's retry callback confirms it now has.
        let outcome = recover(&journal, Some(&old_ref), &store, |_| Ok(true)).unwrap();

        assert_eq!(outcome.completed, 1);
        assert!(journal.is_empty().unwrap());
        assert!(store.get(&old_ref).unwrap().is_none());
    }

    #[test]
    fn recover_delete_leaves_entry_when_caller_retry_does_not_confirm() {
        let journal = unique_journal("recover_delete_retry_fails");
        let store = FakeCredentialStore::new();
        let old_ref = CredentialRef::generate();
        store.set(&old_ref, "old-secret").unwrap();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Delete,
                state: JournalOperationState::PendingNew,
                new_credential_ref: None,
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        let outcome = recover(&journal, Some(&old_ref), &store, |_| Ok(false)).unwrap();

        assert_eq!(outcome.completed, 0);
        assert!(!journal.is_empty().unwrap());
        assert!(store.get(&old_ref).unwrap().is_some(), "credential must not be deleted before the replace is confirmed");
    }

    /// A [`CredentialStore`] that always fails `delete` (backed by a real
    /// [`FakeCredentialStore`] for `set`/`get`), used to exercise recovery's
    /// fail-closed behavior when cleanup itself cannot be confirmed.
    struct DeleteAlwaysFailsStore(FakeCredentialStore);

    impl CredentialStore for DeleteAlwaysFailsStore {
        fn set(&self, credential_ref: &CredentialRef, secret: &str) -> Result<(), crate::secrets::CredentialStoreError> {
            self.0.set(credential_ref, secret)
        }
        fn get(&self, credential_ref: &CredentialRef) -> Result<Option<String>, crate::secrets::CredentialStoreError> {
            self.0.get(credential_ref)
        }
        fn delete(&self, _credential_ref: &CredentialRef) -> Result<(), crate::secrets::CredentialStoreError> {
            Err(crate::secrets::CredentialStoreError::Backend("simulated delete failure".to_string()))
        }
    }

    #[test]
    fn recover_update_where_deleting_the_now_unreferenced_credential_fails_leaves_the_entry_pending() {
        let journal = unique_journal("recover_delete_fails_fail_closed");
        let store = DeleteAlwaysFailsStore(FakeCredentialStore::new());
        let new_ref = CredentialRef::generate();
        let old_ref = CredentialRef::generate();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Update,
                state: JournalOperationState::SettingsSwapped,
                new_credential_ref: Some(new_ref.clone()),
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: 1,
            })
            .unwrap();

        // The new side won, but cleaning up the old credential fails: the
        // entry must not be silently completed as if cleanup had succeeded.
        let outcome = recover(&journal, Some(&new_ref), &store, |_| Ok(false)).unwrap();

        assert_eq!(outcome.completed, 0);
        assert_eq!(outcome.deletion_failed, 1);
        assert!(
            !journal.is_empty().unwrap(),
            "an entry whose cleanup delete failed must remain in the journal for a later retry"
        );
    }
}
