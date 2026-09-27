//! Versioned, non-secret AI connection settings (`AiSettings`), separate
//! from Hane's general settings (`hane-session`'s `settings.conf`).
//!
//! Per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section
//! 7.3, this file never holds a secret: Custom Provider API keys live only
//! in the OS credential store, addressed here by [`crate::secrets::CredentialRef`].
//!
//! Two counters serve different purposes and must not be conflated:
//! - `revision`: bumped on every successful `AiSettings` write, including a
//!   display-name-only change. Used for optimistic-concurrency conflict
//!   detection (`save`'s `expected_revision`).
//! - `settings_generation`: bumped only when a write changes something that
//!   affects the running App Server's startup environment/generated config
//!   (connection method, model selection, Custom Provider base URL or
//!   credential). A display-name-only change leaves it untouched. The
//!   runtime lifecycle coordinator compares its own running generation
//!   against this value (under the settings lock's shared mode) before
//!   accepting a new inference turn or connectivity probe.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

use crate::atomic_file::{atomic_write_bytes, read_to_string_if_exists, AtomicWriteError};
use crate::secrets::CredentialRef;
use crate::settings_lock::{AiSettingsExclusiveGuard, AiSettingsLock};

pub const AI_SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveConnection {
    ChatGpt,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChatGptConnectionSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

/// A Custom Provider connection. `id` is an internal identifier distinct
/// from `name` (the user-facing display name, per section 1: "internal
/// identifiers are kept separate from display names"), stable across a
/// `name` rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomConnectionSettings {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<CredentialRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    pub schema_version: u32,
    pub revision: u64,
    pub settings_generation: u64,
    pub active_connection: ActiveConnection,
    #[serde(default)]
    pub chatgpt: ChatGptConnectionSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomConnectionSettings>,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            schema_version: AI_SETTINGS_SCHEMA_VERSION,
            revision: 0,
            settings_generation: 0,
            active_connection: ActiveConnection::ChatGpt,
            chatgpt: ChatGptConnectionSettings::default(),
            custom: None,
        }
    }
}

impl AiSettings {
    /// Whether `self`'s runtime-affecting fields differ from `previous`'s.
    /// `custom.name` is deliberately excluded: a display-name-only change
    /// must not bump `settings_generation` (and therefore must not trigger a
    /// runtime restart).
    fn is_runtime_affecting_change_from(&self, previous: &AiSettings) -> bool {
        if self.active_connection != previous.active_connection {
            return true;
        }
        if self.chatgpt.model_id != previous.chatgpt.model_id {
            return true;
        }
        match (&self.custom, &previous.custom) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(next), Some(prev)) => {
                next.base_url != prev.base_url
                    || next.model_id != prev.model_id
                    || next.credential_ref != prev.credential_ref
            }
        }
    }
}

#[derive(Debug)]
pub enum SaveError {
    Io(io::Error),
    /// The settings lock could not be acquired in exclusive mode without
    /// blocking: either another save is in progress, or an inference turn /
    /// connectivity probe currently holds the shared mode. Never waited on;
    /// callers must surface this as an immediate rejection rather than
    /// implicitly cancelling whatever holds the lock.
    Busy,
    /// The `expected_revision` the caller read no longer matches the
    /// persisted `revision`: another save landed in between. The caller must
    /// reload and must not blindly overwrite fields with a stale snapshot.
    RevisionConflict { current: AiSettings },
    /// A credential operation journal entry is still unresolved. Per section
    /// 7.3, no ordinary `AiSettings` save may proceed until journal recovery
    /// has completed and removed it.
    PendingCredentialJournal,
    /// The atomic replace's `rename` durably landed — any reader opening the
    /// settings file now observes `new_settings` — but this process could
    /// not confirm the parent directory entry's own crash-durability fsync.
    /// Callers must treat the write as having happened (e.g.
    /// `update_custom_credential` must not delete the credential the new,
    /// already-visible settings may now reference): the credential
    /// operation journal entry guarding this write is left in place so
    /// startup recovery re-derives the outcome from the already-updated
    /// persisted settings, exactly as it would for a normal success.
    PersistedDurabilityUnconfirmed(io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "I/O error saving AI settings: {e}"),
            SaveError::Busy => write!(f, "AI settings are locked by another AI operation"),
            SaveError::RevisionConflict { .. } => {
                write!(f, "AI settings were changed concurrently; reload before saving")
            }
            SaveError::PendingCredentialJournal => {
                write!(f, "a credential operation journal entry must be recovered before saving AI settings")
            }
            SaveError::PersistedDurabilityUnconfirmed(e) => write!(
                f,
                "AI settings replace may have already taken effect, but its crash-durability could not be confirmed: {e}"
            ),
        }
    }
}

impl std::error::Error for SaveError {}

/// Reads/writes `AiSettings` from/to a single JSON file, guarded by an
/// [`AiSettingsLock`] for every write.
pub struct AiSettingsStore {
    path: PathBuf,
    lock: AiSettingsLock,
}

impl AiSettingsStore {
    pub fn new(path: impl Into<PathBuf>, lock_path: impl Into<PathBuf>) -> Self {
        AiSettingsStore {
            path: path.into(),
            lock: AiSettingsLock::new(lock_path),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn settings_lock(&self) -> &AiSettingsLock {
        &self.lock
    }

    /// Loads the current settings, or `AiSettings::default()` if none have
    /// ever been saved. Does not itself take the settings lock: because
    /// writes are atomic file replaces (see `crate::atomic_file`), a
    /// concurrent save can never be observed as a torn/partial file — at
    /// worst this reads the previous, still fully valid file if the save's
    /// `rename` has not yet landed.
    pub fn load(&self) -> io::Result<AiSettings> {
        match read_to_string_if_exists(&self.path)? {
            Some(contents) => {
                serde_json::from_str(&contents).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            None => Ok(AiSettings::default()),
        }
    }

    /// Saves `new_settings`, enforcing the exclusive-lock, journal-empty and
    /// `revision` optimistic-concurrency contracts from ADR-0032 section
    /// 7.3. `journal_is_empty` is invoked under the freshly acquired
    /// exclusive lock, so a concurrent credential operation cannot begin
    /// between that check and this write landing.
    pub fn save(
        &self,
        expected_revision: u64,
        new_settings: AiSettings,
        journal_is_empty: impl FnOnce() -> io::Result<bool>,
    ) -> Result<AiSettings, SaveError> {
        let guard = self.lock.try_acquire_exclusive().map_err(SaveError::Io)?.ok_or(SaveError::Busy)?;

        if !journal_is_empty().map_err(SaveError::Io)? {
            return Err(SaveError::PendingCredentialJournal);
        }

        self.write_while_locked(&guard, expected_revision, new_settings)
    }

    /// Performs the read-current/check-`revision`/atomic-replace write
    /// itself, assuming the caller already holds `guard` (the exclusive AI
    /// settings lock). This exists for callers that must keep that lock held
    /// across more than just this one write — per
    /// `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section
    /// 7.3, a credential update/delete's journal-begin, secret write,
    /// settings replace, journal-mark-swapped, old-secret cleanup and
    /// journal-complete steps must all happen under one continuous exclusive
    /// acquisition, not one lock acquisition per step. [`Self::save`] itself
    /// uses this after acquiring its own guard and checking the journal is
    /// empty, so plain (non-transactional) saves go through the same
    /// revision/`settings_generation` logic.
    ///
    /// Deliberately does not check `journal_is_empty`: callers that need
    /// that invariant (every ordinary [`Self::save`]) enforce it themselves;
    /// the credential journal recovery path intentionally calls this while
    /// its own journal entry is still pending, since recovering that entry
    /// is exactly what is retrying this write.
    pub fn write_while_locked(
        &self,
        guard: &AiSettingsExclusiveGuard,
        expected_revision: u64,
        mut new_settings: AiSettings,
    ) -> Result<AiSettings, SaveError> {
        let _ = guard;
        let current = self.load().map_err(SaveError::Io)?;
        if current.revision != expected_revision {
            return Err(SaveError::RevisionConflict { current });
        }

        new_settings.schema_version = AI_SETTINGS_SCHEMA_VERSION;
        new_settings.revision = current.revision + 1;
        new_settings.settings_generation = if new_settings.is_runtime_affecting_change_from(&current) {
            current.settings_generation + 1
        } else {
            current.settings_generation
        };

        let bytes = serde_json::to_vec_pretty(&new_settings)
            .map_err(|e| SaveError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        match atomic_write_bytes(&self.path, &bytes) {
            Ok(()) => Ok(new_settings),
            Err(AtomicWriteError::NotPersisted(e)) => Err(SaveError::Io(e)),
            Err(AtomicWriteError::RenameSucceededSyncFailed(e)) => {
                Err(SaveError::PersistedDurabilityUnconfirmed(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("hane-ai-settings-test-{}-{name}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(name: &str) -> AiSettingsStore {
        let dir = unique_dir(name);
        AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock"))
    }

    fn always_empty_journal() -> io::Result<bool> {
        Ok(true)
    }

    #[test]
    fn load_without_a_saved_file_returns_defaults() {
        let store = store("defaults");
        let loaded = store.load().unwrap();
        assert_eq!(loaded, AiSettings::default());
    }

    #[test]
    fn save_persists_and_advances_revision() {
        let store = store("persist");
        let mut settings = AiSettings::default();
        settings.chatgpt.model_id = Some("gpt-test".to_string());

        let saved = store.save(0, settings, always_empty_journal).unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.settings_generation, 1);

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded, saved);
    }

    #[test]
    fn display_name_only_change_advances_revision_but_not_settings_generation() {
        let store = store("display_name_only");
        let custom = CustomConnectionSettings {
            id: "conn-1".to_string(),
            name: "My Provider".to_string(),
            base_url: "https://provider.example/v1".to_string(),
            model_id: "gpt-test".to_string(),
            credential_ref: None,
        };
        let mut settings = AiSettings::default();
        settings.active_connection = ActiveConnection::Custom;
        settings.custom = Some(custom.clone());
        let saved = store.save(0, settings, always_empty_journal).unwrap();
        assert_eq!(saved.settings_generation, 1);

        let mut renamed = saved.clone();
        renamed.custom.as_mut().unwrap().name = "Renamed Provider".to_string();
        let saved_again = store.save(saved.revision, renamed, always_empty_journal).unwrap();

        assert_eq!(saved_again.revision, 2);
        assert_eq!(saved_again.settings_generation, 1, "a display-name-only change must not bump settings_generation");
    }

    #[test]
    fn base_url_change_advances_settings_generation() {
        let store = store("base_url_change");
        let custom = CustomConnectionSettings {
            id: "conn-1".to_string(),
            name: "My Provider".to_string(),
            base_url: "https://provider.example/v1".to_string(),
            model_id: "gpt-test".to_string(),
            credential_ref: None,
        };
        let mut settings = AiSettings::default();
        settings.active_connection = ActiveConnection::Custom;
        settings.custom = Some(custom.clone());
        let saved = store.save(0, settings, always_empty_journal).unwrap();

        let mut changed = saved.clone();
        changed.custom.as_mut().unwrap().base_url = "https://other.example/v1".to_string();
        let saved_again = store.save(saved.revision, changed, always_empty_journal).unwrap();

        assert_eq!(saved_again.settings_generation, saved.settings_generation + 1);
    }

    #[test]
    fn stale_expected_revision_is_rejected_and_does_not_overwrite() {
        let store = store("stale_revision");
        let mut first = AiSettings::default();
        first.chatgpt.model_id = Some("gpt-a".to_string());
        let saved_first = store.save(0, first, always_empty_journal).unwrap();

        // A second writer reads the same starting point (revision 0) and
        // tries to save with a snapshot that is now stale.
        let mut stale = AiSettings::default();
        stale.chatgpt.model_id = Some("gpt-b".to_string());
        let err = store.save(0, stale, always_empty_journal).unwrap_err();
        match err {
            SaveError::RevisionConflict { current } => assert_eq!(current, saved_first),
            other => panic!("expected RevisionConflict, got {other:?}"),
        }

        // The first writer's save must be untouched by the rejected attempt.
        assert_eq!(store.load().unwrap(), saved_first);
    }

    #[test]
    fn save_is_rejected_immediately_while_a_shared_holder_exists() {
        let store = store("busy_shared_holder");
        let shared = store.settings_lock().try_acquire_shared().unwrap().unwrap();

        let err = store.save(0, AiSettings::default(), always_empty_journal).unwrap_err();
        assert!(matches!(err, SaveError::Busy));

        drop(shared);
        store.save(0, AiSettings::default(), always_empty_journal).unwrap();
    }

    #[test]
    fn save_is_rejected_while_a_credential_journal_entry_is_pending() {
        let store = store("pending_journal");
        let err = store.save(0, AiSettings::default(), || Ok(false)).unwrap_err();
        assert!(matches!(err, SaveError::PendingCredentialJournal));
        // Nothing was written.
        assert_eq!(store.load().unwrap(), AiSettings::default());
    }

    #[test]
    fn a_rename_that_landed_but_could_not_confirm_parent_dir_sync_durability_still_persists_and_is_reported_distinctly()
    {
        // Regression coverage for the "rename succeeded, parent directory
        // fsync failed" case: the write must still take effect (the settings
        // file already has the new content) and the error returned must be
        // distinguishable from an outright failed write, so a caller like
        // `crate::connect::update_custom_credential` does not mistake this
        // for "nothing happened" and delete a resource the new settings
        // already reference.
        let store = store("ambiguous_durability");
        crate::atomic_file::fault_injection::fail_next_parent_dir_sync_for(store.path());

        let mut settings = AiSettings::default();
        settings.chatgpt.model_id = Some("gpt-test".to_string());
        let err = store.save(0, settings.clone(), always_empty_journal).unwrap_err();
        match err {
            SaveError::PersistedDurabilityUnconfirmed(_) => {}
            other => panic!("expected PersistedDurabilityUnconfirmed, got {other:?}"),
        }

        // The write actually landed despite the reported error.
        let reloaded = store.load().unwrap();
        assert_eq!(reloaded.chatgpt.model_id, settings.chatgpt.model_id);
        assert_eq!(reloaded.revision, 1);
    }
}
