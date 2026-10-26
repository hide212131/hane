//! The OS-backed [`TransientSecretStore`] for the proxy userinfo that crosses an
//! update restart: the macOS Keychain or the Windows Credential Manager, under
//! the `hane.network.proxy` service reserved for proxy credentials.
//!
//! An entry is addressed by the opaque, transaction-bound reference recorded in
//! `child-env.json`; the secret itself exists only in the OS store. There is no
//! file or plaintext fallback: where no native backend exists every `put` and
//! `take` fails, so the handoff aborts instead of continuing without the
//! credential. Errors carry fixed text only, never the secret or an OS message.

#[cfg(any(target_os = "macos", target_os = "windows"))]
use hane_network::PROXY_CREDENTIAL_SERVICE;

use crate::error::{ErrorCode, Result, UpdateError};
use crate::state_root::TransientSecretStore;

/// A backend failure (no native store, or the OS store rejected the
/// operation). Deliberately without any detail text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendError;

/// The raw OS operations, so the one-time and error behaviour is testable
/// without touching a real Keychain / Credential Manager.
pub trait SecretBackend {
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), BackendError>;
    fn get(&self, account: &str) -> std::result::Result<Option<String>, BackendError>;
    /// Removing an absent entry is not an error.
    fn delete(&self, account: &str) -> std::result::Result<(), BackendError>;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct KeyringBackend;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl KeyringBackend {
    fn entry(account: &str) -> std::result::Result<keyring::Entry, BackendError> {
        keyring::Entry::new(PROXY_CREDENTIAL_SERVICE, account).map_err(|_| BackendError)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SecretBackend for KeyringBackend {
    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), BackendError> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|_| BackendError)
    }

    fn get(&self, account: &str) -> std::result::Result<Option<String>, BackendError> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(BackendError),
        }
    }

    fn delete(&self, account: &str) -> std::result::Result<(), BackendError> {
        match Self::entry(account)?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(BackendError),
        }
    }
}

/// No native backend: nothing can have been stored, so removing is a no-op and
/// storing or reading fails.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct KeyringBackend;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl SecretBackend for KeyringBackend {
    fn set(&self, _: &str, _: &str) -> std::result::Result<(), BackendError> {
        Err(BackendError)
    }

    fn get(&self, _: &str) -> std::result::Result<Option<String>, BackendError> {
        Err(BackendError)
    }

    fn delete(&self, _: &str) -> std::result::Result<(), BackendError> {
        Ok(())
    }
}

/// The production store.
pub struct OsSecretStore<B = KeyringBackend> {
    backend: B,
}

impl OsSecretStore {
    /// The store of the proxy credentials of an update restart.
    pub fn proxy() -> Self {
        Self::with_backend(KeyringBackend)
    }
}

impl<B: SecretBackend> OsSecretStore<B> {
    pub(crate) fn with_backend(backend: B) -> Self {
        Self { backend }
    }
}

/// The account name of a reference. It lives next to the other proxy entries
/// but can never equal a Manual-mode credential id.
fn account(reference: &str) -> Result<String> {
    let well_formed = !reference.is_empty()
        && reference.len() <= 128
        && reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if well_formed {
        Ok(format!("update-restart/{reference}"))
    } else {
        Err(failure("the temporary proxy credential reference is malformed"))
    }
}

fn failure(text: &'static str) -> UpdateError {
    UpdateError::new(ErrorCode::Io, text)
}

fn backend_failure(_: BackendError) -> UpdateError {
    failure("the OS secret store cannot handle the proxy credential")
}

impl<B: SecretBackend> TransientSecretStore for OsSecretStore<B> {
    fn put(&self, reference: &str, secret: &str) -> Result<()> {
        self.backend
            .set(&account(reference)?, secret)
            .map_err(backend_failure)
    }

    /// Reads the entry and removes it. When the removal fails the secret is
    /// not handed out (it would stay claimable); the record that names the
    /// reference stays too, so the terminal cleanup retries the removal.
    fn take(&self, reference: &str) -> Result<Option<String>> {
        let account = account(reference)?;
        let Some(secret) = self.backend.get(&account).map_err(backend_failure)? else {
            return Ok(None);
        };
        self.backend.delete(&account).map_err(backend_failure)?;
        Ok(Some(secret))
    }

    fn delete(&self, reference: &str) -> Result<()> {
        self.backend
            .delete(&account(reference)?)
            .map_err(backend_failure)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    const DUMMY_SECRET: &str = "dummy-secret-user:dummy-secret-pass";

    #[derive(Default)]
    struct FakeBackend {
        entries: RefCell<BTreeMap<String, String>>,
        fail_set: bool,
        fail_get: bool,
        fail_delete: bool,
    }

    impl SecretBackend for FakeBackend {
        fn set(&self, account: &str, secret: &str) -> std::result::Result<(), BackendError> {
            if self.fail_set {
                return Err(BackendError);
            }
            self.entries.borrow_mut().insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn get(&self, account: &str) -> std::result::Result<Option<String>, BackendError> {
            if self.fail_get {
                return Err(BackendError);
            }
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn delete(&self, account: &str) -> std::result::Result<(), BackendError> {
            if self.fail_delete {
                return Err(BackendError);
            }
            self.entries.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn store(backend: FakeBackend) -> OsSecretStore<FakeBackend> {
        OsSecretStore::with_backend(backend)
    }

    #[test]
    fn a_secret_is_claimed_exactly_once_under_the_proxy_service_account() {
        let store = store(FakeBackend::default());
        store.put("txn.0", DUMMY_SECRET).unwrap();
        assert_eq!(
            store.backend.entries.borrow().keys().collect::<Vec<_>>(),
            ["update-restart/txn.0"]
        );
        assert_eq!(store.take("txn.0").unwrap().as_deref(), Some(DUMMY_SECRET));
        assert!(store.backend.entries.borrow().is_empty());
        assert_eq!(store.take("txn.0").unwrap(), None);
    }

    #[test]
    fn delete_is_idempotent_and_removes_the_entry() {
        let store = store(FakeBackend::default());
        store.put("txn.1", DUMMY_SECRET).unwrap();
        store.delete("txn.1").unwrap();
        store.delete("txn.1").unwrap();
        assert!(store.backend.entries.borrow().is_empty());
    }

    #[test]
    fn write_read_and_delete_failures_are_errors_without_the_secret() {
        let failing_set = store(FakeBackend { fail_set: true, ..Default::default() });
        let err = failing_set.put("txn.0", DUMMY_SECRET).unwrap_err();
        assert!(!format!("{err:?} {err}").contains("dummy-secret"));

        let failing_get = store(FakeBackend { fail_get: true, ..Default::default() });
        assert!(failing_get.take("txn.0").is_err());

        let failing_delete = store(FakeBackend { fail_delete: true, ..Default::default() });
        failing_delete
            .backend
            .entries
            .borrow_mut()
            .insert("update-restart/txn.0".to_owned(), DUMMY_SECRET.to_owned());
        // The secret is not handed out when it could not be removed, and the
        // entry stays for the cleanup to retry.
        let err = failing_delete.take("txn.0").unwrap_err();
        assert!(!format!("{err:?} {err}").contains("dummy-secret"));
        assert!(failing_delete.delete("txn.0").is_err());
        assert_eq!(failing_delete.backend.entries.borrow().len(), 1);
    }

    #[test]
    fn a_malformed_reference_never_reaches_the_os_store() {
        let store = store(FakeBackend::default());
        for reference in ["", "../escape", "a b", "txn.0\n"] {
            assert!(store.put(reference, DUMMY_SECRET).is_err(), "{reference:?}");
            assert!(store.take(reference).is_err());
            assert!(store.delete(reference).is_err());
        }
        assert!(store.backend.entries.borrow().is_empty());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_native_branch_uses_the_reserved_proxy_service() {
        assert_eq!(PROXY_CREDENTIAL_SERVICE, "hane.network.proxy");
        // Absent entry: the account is well formed and the lookup is not an error
        // of the reference handling (no secret is written).
        assert!(account("00000000.0").is_ok());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn without_a_native_backend_nothing_is_stored_and_the_handoff_fails() {
        let store = OsSecretStore::proxy();
        assert!(store.put("txn.0", DUMMY_SECRET).is_err());
        assert!(store.take("txn.0").is_err());
        // Nothing could be stored, so the cleanup has nothing to fail on.
        store.delete("txn.0").unwrap();
    }
}
