//! Minimal internal API wiring [`crate::settings::AiSettingsStore`],
//! [`crate::credential_journal::CredentialJournal`],
//! [`crate::secrets::CredentialStore`], [`crate::provider`] config
//! generation and [`crate::runtime::AiRuntime`] together, per
//! `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` sections 7.3
//! and 8.
//!
//! Every step section 7.3 describes as needing to happen under one
//! continuous AI settings **exclusive** lock acquisition (credential journal
//! begin, secret write, settings atomic replace, journal state update,
//! old-secret cleanup, journal completion) is performed here while holding
//! exactly one [`AiSettingsExclusiveGuard`] for the whole sequence, via
//! [`AiSettingsStore::write_while_locked`] rather than re-entering
//! [`AiSettingsStore::save`] (which would try to acquire the same
//! process-exclusive OS lock a second time and fail).
//!
//! [`call_with_generation_check`] implements section 7.3's shared-lock
//! contract for an inference turn or connectivity probe: it acquires the AI
//! settings lock in **shared** mode, confirms the persisted
//! `settings_generation` still matches the generation the caller's
//! connection was configured for, and holds that shared lock until the call
//! itself has completed, failed or been rejected — never released early and
//! never waited on by a concurrent settings save (which uses a non-blocking
//! exclusive try-lock and is rejected immediately instead).

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::credential_journal::{
    recover, CredentialJournal, CredentialOperation, JournalOperationKind, JournalOperationState, RecoveryOutcome,
};
use crate::paths::AiPaths;
use crate::provider::{
    build_custom_provider_material, write_codex_config, CustomProviderConfigError, ShellEnvironmentPolicyFormat,
};
use crate::runtime::{AiRuntime, RuntimeConfig, RuntimeError};
use crate::secrets::{CredentialRef, CredentialStore, CredentialStoreError};
use crate::settings::{ActiveConnection, AiSettings, AiSettingsStore, SaveError};
use crate::settings_lock::AiSettingsExclusiveGuard;

#[derive(Debug)]
pub enum ConnectError {
    /// A guarded `AiSettings` write itself was rejected or failed (busy,
    /// stale `expected_revision`, a pending credential journal entry, or
    /// I/O).
    Save(SaveError),
    /// A plain I/O operation outside of a guarded settings write failed
    /// (a credential journal entry, loading current settings, or writing
    /// the generated Codex config).
    Io(io::Error),
    CredentialStore(CredentialStoreError),
    Provider(CustomProviderConfigError),
    Runtime(RuntimeError),
    /// The AI settings lock is currently held by another operation (another
    /// exclusive write, or — for a shared acquisition — a settings save in
    /// progress) and was rejected immediately rather than waited on.
    SettingsBusy,
    /// The persisted `AiSettings.settings_generation` no longer matches the
    /// generation the caller's connection (`RuntimeConfig`/App Server) was
    /// configured for: a runtime-affecting settings change landed since,
    /// and this turn/probe must not proceed against a now-stale Base URL,
    /// API key or model.
    GenerationMismatch { persisted: u64, configured: u64 },
    MissingCustomConnection,
    MissingCredential,
    CredentialNotFound,
    /// `delete_custom_credential`'s `old_credential_ref` argument does not
    /// match the `credential_ref` the just-reloaded current `AiSettings`
    /// actually references, even though `expected_revision` matched. This
    /// can only happen if the caller derived `old_credential_ref` from
    /// something other than the settings snapshot at `expected_revision`;
    /// proceeding would risk deleting a secret the current settings still
    /// depend on, so this is rejected before any journal/credential side
    /// effect instead of trusting the caller-supplied ref.
    CredentialRefMismatch { current: AiSettings },
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Save(e) => write!(f, "{e}"),
            ConnectError::Io(e) => write!(f, "I/O error: {e}"),
            ConnectError::CredentialStore(e) => write!(f, "{e}"),
            ConnectError::Provider(e) => write!(f, "{e}"),
            ConnectError::Runtime(e) => write!(f, "{e}"),
            ConnectError::SettingsBusy => write!(f, "AI settings are locked by another AI operation"),
            ConnectError::GenerationMismatch { persisted, configured } => write!(
                f,
                "AI settings changed (generation {persisted}) since this connection was configured \
                 (generation {configured}); reconnect before proceeding"
            ),
            ConnectError::MissingCustomConnection => write!(f, "no Custom Provider connection is configured"),
            ConnectError::MissingCredential => {
                write!(f, "the Custom Provider connection has no credential configured")
            }
            ConnectError::CredentialNotFound => {
                write!(f, "the Custom Provider credential was not found in the OS credential store")
            }
            ConnectError::CredentialRefMismatch { .. } => write!(
                f,
                "the credential being deleted no longer matches the current Custom Provider connection; reload before retrying"
            ),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<SaveError> for ConnectError {
    fn from(e: SaveError) -> Self {
        ConnectError::Save(e)
    }
}

impl From<CredentialStoreError> for ConnectError {
    fn from(e: CredentialStoreError) -> Self {
        ConnectError::CredentialStore(e)
    }
}

impl From<CustomProviderConfigError> for ConnectError {
    fn from(e: CustomProviderConfigError) -> Self {
        ConnectError::Provider(e)
    }
}

fn acquire_exclusive(settings_store: &AiSettingsStore) -> Result<AiSettingsExclusiveGuard, ConnectError> {
    settings_store
        .settings_lock()
        .try_acquire_exclusive()
        .map_err(ConnectError::Io)?
        .ok_or(ConnectError::SettingsBusy)
}

fn reject_if_journal_pending(journal: &CredentialJournal) -> Result<(), ConnectError> {
    if !journal.is_empty().map_err(ConnectError::Io)? {
        return Err(ConnectError::Save(SaveError::PendingCredentialJournal));
    }
    Ok(())
}

/// Creates or rotates the Custom Provider credential and lands the settings
/// replace that references it, as one journal-guarded transaction (ADR-0032
/// section 7.3 steps 1-7, `update` shape). `build_new_settings` receives the
/// freshly generated [`CredentialRef`] and must return the complete new
/// `AiSettings` to persist (including that ref); it never sees the secret
/// itself. `old_credential_ref` is the connection's previously active
/// credential, if any (`None` for the very first key ever saved).
pub fn update_custom_credential(
    settings_store: &AiSettingsStore,
    journal: &CredentialJournal,
    credential_store: &dyn CredentialStore,
    expected_revision: u64,
    old_credential_ref: Option<CredentialRef>,
    new_secret: &str,
    build_new_settings: impl FnOnce(&CredentialRef) -> AiSettings,
) -> Result<AiSettings, ConnectError> {
    let guard = acquire_exclusive(settings_store)?;
    reject_if_journal_pending(journal)?;

    // Compare `expected_revision` against the just-reloaded current settings
    // *before* any journal/credential side effect. Checking this only deep
    // inside `write_while_locked` (after `journal.begin` and
    // `credential_store.set` already ran) would leave a `PendingNew` journal
    // entry and a freshly written secret behind on a stale request, which is
    // indistinguishable from a genuine crash-mid-operation to a later
    // `recover` pass and could see it retried/completed instead of
    // discarded.
    let current = settings_store.load().map_err(ConnectError::Io)?;
    if current.revision != expected_revision {
        return Err(SaveError::RevisionConflict { current }.into());
    }
    if let Some(old) = &old_credential_ref {
        let current_credential_ref = current.custom.as_ref().and_then(|c| c.credential_ref.clone());
        if current_credential_ref.as_ref() != Some(old) {
            return Err(ConnectError::CredentialRefMismatch { current });
        }
    }
    let settings_generation_before = current.settings_generation;

    // Step 1-2: assign the new identifier and durably record the pending
    // operation before the secret is written anywhere.
    let new_ref = CredentialRef::generate();
    journal
        .begin(CredentialOperation {
            kind: JournalOperationKind::Update,
            state: JournalOperationState::PendingNew,
            new_credential_ref: Some(new_ref.clone()),
            old_credential_ref: old_credential_ref.clone(),
            settings_generation_before,
        })
        .map_err(ConnectError::Io)?;

    // Step 3: write the new secret. A failure here leaves the `PendingNew`
    // entry for the next startup's `recover` to diagnose/retry; the old
    // credential and old settings are untouched.
    credential_store.set(&new_ref, new_secret)?;

    // Step 4: atomically replace the non-secret settings to reference the
    // new credential.
    let new_settings = build_new_settings(&new_ref);
    let saved = match settings_store.write_while_locked(&guard, expected_revision, new_settings) {
        Ok(saved) => saved,
        Err(SaveError::PersistedDurabilityUnconfirmed(e)) => {
            // The settings replace's `rename` already landed — persisted
            // settings may already reference `new_ref` — so deleting it here
            // would risk deleting a secret the durable settings now depend
            // on. Leave the journal's `PendingNew` entry in place: startup
            // recovery re-reads the (already-updated) persisted settings and
            // resolves this exactly like a normal "new side won" success.
            return Err(SaveError::PersistedDurabilityUnconfirmed(e).into());
        }
        Err(e) => {
            let _ = credential_store.delete(&new_ref);
            return Err(e.into());
        }
    };

    // Step 5: the settings replace durably landed.
    journal.mark_update_settings_swapped(&new_ref).map_err(ConnectError::Io)?;

    // Step 6-7: clean up the now-unreferenced old credential (if any) and
    // only then remove the journal entry. A failed delete leaves the entry
    // for the next `recover` to retry (fail-closed, mirroring `recover`
    // itself); the already-successful settings save is still returned.
    let delete_ok = match &old_credential_ref {
        Some(old) => credential_store.delete(old).is_ok(),
        None => true,
    };
    if delete_ok {
        journal.complete(Some(&new_ref), old_credential_ref.as_ref()).map_err(ConnectError::Io)?;
    }

    Ok(saved)
}

/// Removes the Custom Provider credential entirely and lands the settings
/// replace that drops `credential_ref`, as one journal-guarded transaction
/// (ADR-0032 section 7.3 `delete` shape). `new_settings_without_credential`
/// must already have `custom.credential_ref` cleared.
pub fn delete_custom_credential(
    settings_store: &AiSettingsStore,
    journal: &CredentialJournal,
    credential_store: &dyn CredentialStore,
    expected_revision: u64,
    old_credential_ref: CredentialRef,
    new_settings_without_credential: AiSettings,
) -> Result<AiSettings, ConnectError> {
    let guard = acquire_exclusive(settings_store)?;
    reject_if_journal_pending(journal)?;

    // Same pre-check as `update_custom_credential`: compare
    // `expected_revision`, and confirm `old_credential_ref` actually matches
    // what current settings reference, *before* the journal is touched. A
    // stale-revision delete that skipped this would still leave a `Delete`
    // `PendingNew` journal entry referencing `old_credential_ref` — and if
    // current settings happen to still reference exactly that credential
    // (e.g. the concurrent write that won was unrelated to it), a later
    // `recover` cannot tell that apart from a genuine crash mid-delete, and
    // would finish deleting a credential settings still depend on.
    let current = settings_store.load().map_err(ConnectError::Io)?;
    if current.revision != expected_revision {
        return Err(SaveError::RevisionConflict { current }.into());
    }
    let current_credential_ref = current.custom.as_ref().and_then(|c| c.credential_ref.clone());
    if current_credential_ref.as_ref() != Some(&old_credential_ref) {
        return Err(ConnectError::CredentialRefMismatch { current });
    }
    let settings_generation_before = current.settings_generation;

    journal
        .begin(CredentialOperation {
            kind: JournalOperationKind::Delete,
            state: JournalOperationState::PendingNew,
            new_credential_ref: None,
            old_credential_ref: Some(old_credential_ref.clone()),
            settings_generation_before,
        })
        .map_err(ConnectError::Io)?;

    let saved = settings_store.write_while_locked(&guard, expected_revision, new_settings_without_credential)?;

    journal.mark_delete_settings_swapped(&old_credential_ref).map_err(ConnectError::Io)?;

    if credential_store.delete(&old_credential_ref).is_ok() {
        journal.complete(None, Some(&old_credential_ref)).map_err(ConnectError::Io)?;
    }

    Ok(saved)
}

/// Runs credential operation journal recovery (ADR-0032 section 7.3 step 6)
/// against the current, already-durable `AiSettings`, under one continuous
/// exclusive AI settings lock acquisition covering both the recovery itself
/// and any settings replace it needs to retry on a pending `delete`'s
/// behalf.
pub fn recover_at_startup(
    settings_store: &AiSettingsStore,
    journal: &CredentialJournal,
    credential_store: &dyn CredentialStore,
) -> Result<RecoveryOutcome, ConnectError> {
    let guard = acquire_exclusive(settings_store)?;
    let current = settings_store.load().map_err(ConnectError::Io)?;
    let current_credential_ref = current.custom.as_ref().and_then(|c| c.credential_ref.clone());

    recover(journal, current_credential_ref.as_ref(), credential_store, |_op| {
        // A pending `delete` whose settings replace never durably landed:
        // retry that specific write (dropping `credential_ref`), still
        // under the same exclusive lock this whole recovery pass holds.
        let mut retried = current.clone();
        if let Some(custom) = retried.custom.as_mut() {
            custom.credential_ref = None;
        }
        match settings_store.write_while_locked(&guard, current.revision, retried) {
            Ok(_) => Ok(true),
            Err(SaveError::Io(e)) => Err(e),
            // Conservative/fail-closed for every other outcome: none of
            // these confirm the replace landed, so this pass leaves the
            // journal entry in place for a later `recover` to re-derive from
            // a fresh read instead of guessing. In particular,
            // `PersistedDurabilityUnconfirmed` is *not* treated as success
            // here even though its `rename` may well have landed: the next
            // `recover_at_startup` call re-reads `current` from disk, so if
            // it did land, `current_credential_ref` will already show that
            // and this closure will not even be invoked on that next pass.
            Err(SaveError::RevisionConflict { .. }
            | SaveError::Busy
            | SaveError::PendingCredentialJournal
            | SaveError::PersistedDurabilityUnconfirmed(_)) => Ok(false),
        }
    })
    .map_err(ConnectError::Io)
}

/// Everything needed to spawn/reconfigure an [`AiRuntime`] for the
/// currently active connection: the `RuntimeConfig` itself, and the
/// `settings_generation` it was built for (per ADR-0032 section 8, needed to
/// later confirm via [`call_with_generation_check`] that persisted settings
/// have not moved on since). `Debug` is safe to derive: `RuntimeConfig`'s own
/// manual `Debug` impl already redacts `extra_env` values, so this never
/// prints a secret either.
#[derive(Debug)]
pub struct ConfiguredRuntime {
    pub config: RuntimeConfig,
    pub settings_generation: u64,
}

/// Builds the `RuntimeConfig` for `settings.active_connection`: for
/// ChatGPT, just the dedicated `CODEX_HOME`; for Custom, additionally
/// retrieves the secret from `credential_store`, generates and writes the
/// Custom Provider `config.toml`, and injects the secret into `extra_env`
/// for that child process only.
pub fn build_runtime_config_for_active_connection(
    settings: &AiSettings,
    credential_store: &dyn CredentialStore,
    paths: &AiPaths,
    binary_path: PathBuf,
    owner_lock_path: PathBuf,
    shell_env_format: ShellEnvironmentPolicyFormat,
) -> Result<ConfiguredRuntime, ConnectError> {
    let mut config = RuntimeConfig::new(binary_path, owner_lock_path);
    match settings.active_connection {
        ActiveConnection::ChatGpt => {
            config.codex_home = Some(paths.chatgpt_codex_home());
        }
        ActiveConnection::Custom => {
            let custom = settings.custom.as_ref().ok_or(ConnectError::MissingCustomConnection)?;
            let credential_ref = custom.credential_ref.as_ref().ok_or(ConnectError::MissingCredential)?;
            let secret = credential_store.get(credential_ref)?.ok_or(ConnectError::CredentialNotFound)?;
            let material = build_custom_provider_material(
                &custom.name,
                &custom.base_url,
                &custom.model_id,
                &secret,
                shell_env_format,
            )?;
            let codex_home = paths.custom_codex_home();
            write_codex_config(&codex_home, &material.config_toml).map_err(ConnectError::Io)?;
            config.codex_home = Some(codex_home);
            config.extra_env = material.extra_env;
        }
    }
    Ok(ConfiguredRuntime { config, settings_generation: settings.settings_generation })
}

/// Runs one inference turn or fixed connectivity probe against `runtime`,
/// gated on `settings_generation` per ADR-0032 section 7.3: acquires the AI
/// settings lock in shared mode, confirms the persisted
/// `settings_generation` still matches `configured_settings_generation` (the
/// generation `runtime`'s current `RuntimeConfig` was built for), and holds
/// that shared lock until the call itself has resolved. A concurrent
/// settings save is therefore either already reflected here (generation
/// mismatch, rejected before the call is ever sent) or is itself rejected
/// immediately while this call is in flight — never both silently racing.
pub fn call_with_generation_check(
    settings_store: &AiSettingsStore,
    configured_settings_generation: u64,
    runtime: &AiRuntime,
    method: &str,
    params: Option<Value>,
    timeout: Duration,
) -> Result<Value, ConnectError> {
    let _shared =
        settings_store.settings_lock().try_acquire_shared().map_err(ConnectError::Io)?.ok_or(ConnectError::SettingsBusy)?;

    let persisted = settings_store.load().map_err(ConnectError::Io)?.settings_generation;
    if persisted != configured_settings_generation {
        return Err(ConnectError::GenerationMismatch { persisted, configured: configured_settings_generation });
    }

    runtime.call(method, params, timeout).map_err(ConnectError::Runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::RejectAllServerRequests;
    use crate::secrets::{FakeCredentialStore, UnavailableCredentialStore};
    use crate::settings::{ChatGptConnectionSettings, CustomConnectionSettings};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn unique_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("hane-ai-connect-test-{}-{name}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_and_journal(name: &str) -> (AiSettingsStore, CredentialJournal, PathBuf) {
        let dir = unique_dir(name);
        (
            AiSettingsStore::new(dir.join("ai-settings.json"), dir.join("ai-settings.lock")),
            CredentialJournal::new(dir.join("credential-journal.json")),
            dir,
        )
    }

    fn custom_settings(credential_ref: Option<CredentialRef>) -> AiSettings {
        AiSettings {
            schema_version: crate::settings::AI_SETTINGS_SCHEMA_VERSION,
            revision: 0,
            settings_generation: 0,
            active_connection: ActiveConnection::Custom,
            chatgpt: ChatGptConnectionSettings::default(),
            custom: Some(CustomConnectionSettings {
                id: "conn-1".to_string(),
                name: "My Provider".to_string(),
                base_url: "https://provider.example/v1".to_string(),
                model_id: "gpt-test".to_string(),
                credential_ref,
            }),
        }
    }

    #[test]
    fn update_then_rotate_then_delete_leaves_no_journal_entries_and_no_stray_secrets() {
        let (store, journal, _dir) = store_and_journal("full_cycle");
        let credential_store = FakeCredentialStore::new();

        let saved = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap();
        assert!(journal.is_empty().unwrap());
        assert_eq!(credential_store.len(), 1);
        let first_ref = saved.custom.as_ref().unwrap().credential_ref.clone().unwrap();
        assert_eq!(credential_store.get(&first_ref).unwrap().as_deref(), Some("sk-first"));

        let rotated = update_custom_credential(
            &store,
            &journal,
            &credential_store,
            saved.revision,
            Some(first_ref.clone()),
            "sk-second",
            |new_ref| custom_settings(Some(new_ref.clone())),
        )
        .unwrap();
        assert!(journal.is_empty().unwrap());
        assert_eq!(credential_store.len(), 1, "the old credential must be cleaned up after rotation");
        let second_ref = rotated.custom.as_ref().unwrap().credential_ref.clone().unwrap();
        assert_ne!(second_ref, first_ref);
        assert_eq!(credential_store.get(&second_ref).unwrap().as_deref(), Some("sk-second"));

        let mut without_credential = rotated.clone();
        without_credential.custom.as_mut().unwrap().credential_ref = None;
        let deleted =
            delete_custom_credential(&store, &journal, &credential_store, rotated.revision, second_ref.clone(), without_credential)
                .unwrap();
        assert!(journal.is_empty().unwrap());
        assert!(credential_store.is_empty(), "no secret should remain after deleting the only credential");
        assert!(deleted.custom.as_ref().unwrap().credential_ref.is_none());
    }

    #[test]
    fn update_leaves_a_pending_journal_entry_and_untouched_settings_when_writing_the_secret_fails() {
        let (store, journal, _dir) = store_and_journal("set_failure");
        let credential_store = UnavailableCredentialStore;

        let err = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap_err();
        assert!(matches!(err, ConnectError::CredentialStore(_)));

        assert!(!journal.is_empty().unwrap(), "the PendingNew entry must survive for startup recovery to diagnose");
        assert_eq!(store.load().unwrap(), AiSettings::default(), "settings must be untouched when the secret write fails");
    }

    #[test]
    fn a_second_update_is_rejected_while_a_journal_entry_from_a_failed_operation_is_still_pending() {
        let (store, journal, _dir) = store_and_journal("pending_blocks_next");
        let credential_store = UnavailableCredentialStore;
        let _ = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        });
        assert!(!journal.is_empty().unwrap());

        let fake_store = FakeCredentialStore::new();
        let err = update_custom_credential(&store, &journal, &fake_store, 0, None, "sk-second", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap_err();
        assert!(matches!(err, ConnectError::Save(SaveError::PendingCredentialJournal)));
    }

    #[test]
    fn update_with_a_stale_expected_revision_touches_neither_the_journal_nor_the_credential_store() {
        let (store, journal, _dir) = store_and_journal("update_stale_revision");
        let credential_store = FakeCredentialStore::new();

        let saved = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap();
        assert!(journal.is_empty().unwrap());
        assert_eq!(credential_store.len(), 1);

        // A second writer still thinks the revision is 0 (stale): its
        // rotation attempt must be rejected before any journal entry is
        // begun or any new secret is written.
        let err = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-stale", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap_err();
        assert!(matches!(err, ConnectError::Save(SaveError::RevisionConflict { .. })));

        assert!(journal.is_empty().unwrap(), "a stale request must never leave a journal entry behind");
        assert_eq!(credential_store.len(), 1, "a stale request must never write a new secret");
        assert_eq!(store.load().unwrap(), saved, "a stale request must never touch persisted settings");
    }

    #[test]
    fn delete_with_a_stale_expected_revision_touches_neither_the_journal_nor_the_credential_store() {
        let (store, journal, _dir) = store_and_journal("delete_stale_revision");
        let credential_store = FakeCredentialStore::new();

        let saved = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap();
        let credential_ref = saved.custom.as_ref().unwrap().credential_ref.clone().unwrap();

        // A concurrent, unrelated change lands first (e.g. a display-name
        // rename), bumping `revision` without touching `credential_ref`.
        let mut renamed = saved.clone();
        renamed.custom.as_mut().unwrap().name = "Renamed".to_string();
        let renamed_saved = store.save(saved.revision, renamed, || Ok(true)).unwrap();

        // The delete caller still holds the stale `expected_revision` (and,
        // notably, `credential_ref` here still matches current settings --
        // this is exactly the case a revision-only check could miss).
        let mut without_credential = saved.clone();
        without_credential.custom.as_mut().unwrap().credential_ref = None;
        let err = delete_custom_credential(
            &store,
            &journal,
            &credential_store,
            saved.revision,
            credential_ref.clone(),
            without_credential,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::Save(SaveError::RevisionConflict { .. })));

        assert!(journal.is_empty().unwrap(), "a stale request must never leave a journal entry behind");
        assert_eq!(credential_store.len(), 1, "a stale request must never delete the still-referenced credential");
        assert_eq!(
            store.load().unwrap(),
            renamed_saved,
            "a stale request must never touch persisted settings"
        );
    }

    #[test]
    fn delete_whose_credential_ref_no_longer_matches_current_settings_is_rejected_without_side_effects() {
        let (store, journal, _dir) = store_and_journal("delete_credential_ref_mismatch");
        let credential_store = FakeCredentialStore::new();

        let saved = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap();

        // `expected_revision` matches, but the caller's `old_credential_ref`
        // does not match what current settings actually reference.
        let wrong_ref = CredentialRef::generate();
        let mut without_credential = saved.clone();
        without_credential.custom.as_mut().unwrap().credential_ref = None;
        let err = delete_custom_credential(
            &store,
            &journal,
            &credential_store,
            saved.revision,
            wrong_ref,
            without_credential,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::CredentialRefMismatch { .. }));

        assert!(journal.is_empty().unwrap());
        assert_eq!(credential_store.len(), 1, "the actually-referenced credential must not be deleted");
        assert_eq!(store.load().unwrap(), saved);
    }

    #[test]
    fn update_whose_settings_replace_lands_but_cannot_confirm_durability_does_not_delete_the_new_secret() {
        let (store, journal, _dir) = store_and_journal("update_ambiguous_durability");
        let credential_store = FakeCredentialStore::new();

        crate::atomic_file::fault_injection::fail_next_parent_dir_sync_for(store.path());
        let err = update_custom_credential(&store, &journal, &credential_store, 0, None, "sk-first", |new_ref| {
            custom_settings(Some(new_ref.clone()))
        })
        .unwrap_err();
        assert!(
            matches!(err, ConnectError::Save(SaveError::PersistedDurabilityUnconfirmed(_))),
            "expected PersistedDurabilityUnconfirmed, got {err:?}"
        );

        // The settings replace's `rename` actually landed: persisted
        // settings already reference the new credential.
        let reloaded = store.load().unwrap();
        let new_ref = reloaded.custom.as_ref().unwrap().credential_ref.clone().unwrap();
        assert_eq!(
            credential_store.get(&new_ref).unwrap().as_deref(),
            Some("sk-first"),
            "the new secret must not have been deleted: persisted settings already reference it"
        );

        // The journal's `PendingNew` entry is left in place for startup
        // recovery, which re-derives the outcome from the already-updated
        // persisted settings exactly like a normal success.
        assert!(!journal.is_empty().unwrap());
        let outcome = recover_at_startup(&store, &journal, &credential_store).unwrap();
        assert_eq!(outcome.completed, 1);
        assert!(journal.is_empty().unwrap());
        assert_eq!(credential_store.get(&new_ref).unwrap().as_deref(), Some("sk-first"));
    }

    #[test]
    fn recover_at_startup_retries_a_pending_delete_settings_swap_and_cleans_up() {
        let (store, journal, _dir) = store_and_journal("recover_delete_pending");
        let credential_store = FakeCredentialStore::new();
        let old_ref = CredentialRef::generate();
        credential_store.set(&old_ref, "old-secret").unwrap();

        // Settings still reference the old credential: simulates a crash
        // between the journal's `Delete` `begin` and the settings replace
        // that would have dropped `credential_ref`.
        let saved = store.save(0, custom_settings(Some(old_ref.clone())), || Ok(true)).unwrap();
        journal
            .begin(CredentialOperation {
                kind: JournalOperationKind::Delete,
                state: JournalOperationState::PendingNew,
                new_credential_ref: None,
                old_credential_ref: Some(old_ref.clone()),
                settings_generation_before: saved.settings_generation,
            })
            .unwrap();

        let outcome = recover_at_startup(&store, &journal, &credential_store).unwrap();
        assert_eq!(outcome.completed, 1);
        assert!(journal.is_empty().unwrap());
        assert!(credential_store.get(&old_ref).unwrap().is_none());

        let reloaded = store.load().unwrap();
        assert!(reloaded.custom.as_ref().unwrap().credential_ref.is_none());
    }

    #[test]
    fn build_runtime_config_for_chatgpt_uses_the_chatgpt_codex_home_and_no_extra_env() {
        let dir = unique_dir("build_config_chatgpt");
        let paths = AiPaths::new(&dir);
        let settings = AiSettings::default();
        let credential_store = FakeCredentialStore::new();

        let configured = build_runtime_config_for_active_connection(
            &settings,
            &credential_store,
            &paths,
            PathBuf::from("/opt/hane/codex"),
            dir.join("runtime.lock"),
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();

        assert_eq!(configured.config.codex_home, Some(paths.chatgpt_codex_home()));
        assert!(configured.config.extra_env.is_empty());
        assert_eq!(configured.settings_generation, settings.settings_generation);
    }

    #[test]
    fn build_runtime_config_for_custom_generates_config_and_injects_only_the_secret() {
        let dir = unique_dir("build_config_custom");
        let paths = AiPaths::new(&dir);
        let credential_store = FakeCredentialStore::new();
        let credential_ref = CredentialRef::generate();
        credential_store.set(&credential_ref, "sk-super-secret-value").unwrap();
        let settings = custom_settings(Some(credential_ref));

        let configured = build_runtime_config_for_active_connection(
            &settings,
            &credential_store,
            &paths,
            PathBuf::from("/opt/hane/codex"),
            dir.join("runtime.lock"),
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();

        assert_eq!(configured.config.codex_home, Some(paths.custom_codex_home()));
        assert_eq!(
            configured.config.extra_env,
            vec![(crate::provider::CUSTOM_PROVIDER_ENV_KEY.to_string(), "sk-super-secret-value".to_string())]
        );
        let written = std::fs::read_to_string(paths.custom_codex_home().join("config.toml")).unwrap();
        assert!(!written.contains("sk-super-secret-value"));
    }

    #[test]
    fn build_runtime_config_for_custom_without_a_stored_secret_is_rejected() {
        let dir = unique_dir("build_config_missing_secret");
        let paths = AiPaths::new(&dir);
        let credential_store = FakeCredentialStore::new();
        // A credential_ref is set, but nothing was ever stored for it.
        let settings = custom_settings(Some(CredentialRef::generate()));

        let err = build_runtime_config_for_active_connection(
            &settings,
            &credential_store,
            &paths,
            PathBuf::from("/opt/hane/codex"),
            dir.join("runtime.lock"),
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap_err();
        assert!(matches!(err, ConnectError::CredentialNotFound));
    }

    #[test]
    fn call_with_generation_check_rejects_a_stale_configured_generation_before_ever_touching_the_runtime() {
        let (store, _journal, dir) = store_and_journal("gated_call_stale");
        let mut first = AiSettings::default();
        first.chatgpt.model_id = Some("gpt-a".to_string());
        let saved = store.save(0, first, || Ok(true)).unwrap();
        assert_eq!(saved.settings_generation, 1);

        let mut changed = saved.clone();
        changed.chatgpt.model_id = Some("gpt-b".to_string());
        store.save(saved.revision, changed, || Ok(true)).unwrap();

        let (events_tx, _events_rx) = std::sync::mpsc::sync_channel(8);
        let handler = Arc::new(RejectAllServerRequests);
        // Never started: the generation check must reject this before
        // `AiRuntime::call` is ever reached, so an unreachable binary path
        // is safe here.
        let config = RuntimeConfig::new("/nonexistent/hane-ai-test-binary", dir.join("runtime.lock"));
        let runtime = AiRuntime::spawn(config, handler, events_tx);

        let result =
            call_with_generation_check(&store, 1, &runtime, "turn/start", None, Duration::from_secs(1));
        assert!(matches!(result, Err(ConnectError::GenerationMismatch { persisted: 2, configured: 1 })));

        let _ = runtime.shutdown();
    }
}
