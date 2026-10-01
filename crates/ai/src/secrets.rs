//! Secret storage for AI connection credentials (Custom Provider API keys).
//!
//! Per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section 7.3,
//! `AiSettings` never holds a secret value directly: it holds a
//! [`CredentialRef`], an opaque identifier Hane assigns before the secret is
//! ever written anywhere. The secret itself lives only in the OS credential
//! store (macOS Keychain / Windows Credential Manager), addressed by that
//! identifier. There is no plaintext fallback: on a platform or environment
//! where the OS store is unavailable, [`CredentialStore::set`] fails
//! explicitly instead of writing the secret to disk unprotected.

use std::fmt;

/// An opaque, non-secret identifier for one stored credential. Hane assigns
/// this itself (see [`CredentialRef::generate`]) before the secret it names
/// is ever written to the credential store, so the identifier can be
/// persisted in `AiSettings` and the credential operation journal without
/// exposing the secret.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

impl CredentialRef {
    /// Generates a fresh, process-wide-unique identifier. Not a secret and
    /// not required to be cryptographically random: its only job is to never
    /// collide with another identifier this process (or, in practice, this
    /// machine) has handed out, so two in-flight credential operations can
    /// never address the same OS credential store entry.
    pub fn generate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        CredentialRef(format!("cred-{nanos:x}-{}-{counter:x}", std::process::id()))
    }

    /// Reconstructs a `CredentialRef` from a previously persisted identifier
    /// (loaded from `AiSettings` or the credential operation journal). Does
    /// not validate that a credential store entry with this name exists.
    pub fn from_persisted(id: impl Into<String>) -> Self {
        CredentialRef(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug)]
pub enum CredentialStoreError {
    /// No OS credential store is available on this platform/environment.
    /// Callers must surface this as an explicit error; they must never
    /// respond by writing the secret to disk in plaintext instead.
    Unavailable(String),
    /// The OS credential store rejected the operation (e.g. keychain access
    /// denied, corrupted entry).
    Backend(String),
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialStoreError::Unavailable(reason) => {
                write!(f, "OS credential store unavailable: {reason}")
            }
            CredentialStoreError::Backend(reason) => {
                write!(f, "OS credential store error: {reason}")
            }
        }
    }
}

impl std::error::Error for CredentialStoreError {}

/// Boundary between AI settings/credential logic and the OS credential
/// store. Implementations must never write a secret anywhere other than the
/// backing OS store (no plaintext fallback file, no log line containing the
/// secret).
pub trait CredentialStore: Send + Sync {
    fn set(&self, credential_ref: &CredentialRef, secret: &str)
    -> Result<(), CredentialStoreError>;
    /// Returns `Ok(None)` when the platform's OS credential store is
    /// available but has no entry for `credential_ref` (e.g. it was already
    /// deleted), as opposed to [`CredentialStoreError::Unavailable`] when
    /// there is no store to query at all.
    fn get(&self, credential_ref: &CredentialRef) -> Result<Option<String>, CredentialStoreError>;
    /// Deleting an already-absent entry is not an error: recovery paths
    /// retry deletes idempotently.
    fn delete(&self, credential_ref: &CredentialRef) -> Result<(), CredentialStoreError>;
}

/// OS-backed credential store. On macOS this is the Keychain, on Windows the
/// Credential Manager. On every other platform (including this Linux
/// development/CI environment), no native backend is wired in and every
/// operation fails with [`CredentialStoreError::Unavailable`] instead of
/// silently falling back to plaintext.
pub struct OsCredentialStore {
    /// The credential-store "service" namespace every entry for this Hane
    /// installation is stored under, combined with the entry's
    /// [`CredentialRef`] to form the full OS credential store key. Kept
    /// distinct per purpose (e.g. Custom Provider keys) so Hane's own
    /// entries never collide with, or get enumerated alongside, unrelated
    /// applications' entries.
    service: String,
}

impl OsCredentialStore {
    pub fn new(service: impl Into<String>) -> Self {
        OsCredentialStore {
            service: service.into(),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl CredentialStore for OsCredentialStore {
    fn set(
        &self,
        credential_ref: &CredentialRef,
        secret: &str,
    ) -> Result<(), CredentialStoreError> {
        let entry = keyring::Entry::new(&self.service, credential_ref.as_str())
            .map_err(|e| CredentialStoreError::Backend(e.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|e| CredentialStoreError::Backend(e.to_string()))
    }

    fn get(&self, credential_ref: &CredentialRef) -> Result<Option<String>, CredentialStoreError> {
        let entry = keyring::Entry::new(&self.service, credential_ref.as_str())
            .map_err(|e| CredentialStoreError::Backend(e.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialStoreError::Backend(e.to_string())),
        }
    }

    fn delete(&self, credential_ref: &CredentialRef) -> Result<(), CredentialStoreError> {
        let entry = keyring::Entry::new(&self.service, credential_ref.as_str())
            .map_err(|e| CredentialStoreError::Backend(e.to_string()))?;
        match entry.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CredentialStoreError::Backend(e.to_string())),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl CredentialStore for OsCredentialStore {
    fn set(
        &self,
        _credential_ref: &CredentialRef,
        _secret: &str,
    ) -> Result<(), CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(
            "no OS credential store backend is wired in on this platform".to_string(),
        ))
    }

    fn get(&self, _credential_ref: &CredentialRef) -> Result<Option<String>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(
            "no OS credential store backend is wired in on this platform".to_string(),
        ))
    }

    fn delete(&self, _credential_ref: &CredentialRef) -> Result<(), CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(
            "no OS credential store backend is wired in on this platform".to_string(),
        ))
    }
}

/// In-memory [`CredentialStore`] used by tests as the fake secret-store
/// boundary: it never touches a real OS credential store, so tests never
/// depend on (or pollute) the developer's or CI runner's actual Keychain /
/// Credential Manager.
#[derive(Default)]
pub struct FakeCredentialStore {
    entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl FakeCredentialStore {
    pub fn new() -> Self {
        FakeCredentialStore::default()
    }

    /// Test-only introspection: the number of entries currently stored,
    /// independent of any `CredentialRef` a test may have already dropped.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CredentialStore for FakeCredentialStore {
    fn set(
        &self,
        credential_ref: &CredentialRef,
        secret: &str,
    ) -> Result<(), CredentialStoreError> {
        self.entries
            .lock()
            .unwrap()
            .insert(credential_ref.as_str().to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, credential_ref: &CredentialRef) -> Result<Option<String>, CredentialStoreError> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(credential_ref.as_str())
            .cloned())
    }

    fn delete(&self, credential_ref: &CredentialRef) -> Result<(), CredentialStoreError> {
        self.entries.lock().unwrap().remove(credential_ref.as_str());
        Ok(())
    }
}

/// A [`CredentialStore`] that always reports itself unavailable, for tests
/// exercising the explicit-error-instead-of-plaintext-fallback contract.
#[derive(Default)]
pub struct UnavailableCredentialStore;

impl CredentialStore for UnavailableCredentialStore {
    fn set(
        &self,
        _credential_ref: &CredentialRef,
        _secret: &str,
    ) -> Result<(), CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(
            "test double: always unavailable".to_string(),
        ))
    }

    fn get(&self, _credential_ref: &CredentialRef) -> Result<Option<String>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(
            "test double: always unavailable".to_string(),
        ))
    }

    fn delete(&self, _credential_ref: &CredentialRef) -> Result<(), CredentialStoreError> {
        Err(CredentialStoreError::Unavailable(
            "test double: always unavailable".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_credential_refs_are_unique() {
        let a = CredentialRef::generate();
        let b = CredentialRef::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn fake_store_round_trips_a_secret() {
        let store = FakeCredentialStore::new();
        let reference = CredentialRef::generate();
        store.set(&reference, "sk-test-secret").unwrap();
        assert_eq!(
            store.get(&reference).unwrap().as_deref(),
            Some("sk-test-secret")
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn fake_store_get_of_missing_entry_is_none_not_an_error() {
        let store = FakeCredentialStore::new();
        let reference = CredentialRef::generate();
        assert_eq!(store.get(&reference).unwrap(), None);
    }

    #[test]
    fn fake_store_delete_is_idempotent() {
        let store = FakeCredentialStore::new();
        let reference = CredentialRef::generate();
        store.set(&reference, "sk-test-secret").unwrap();
        store.delete(&reference).unwrap();
        assert!(store.get(&reference).unwrap().is_none());
        // A second delete of an already-absent entry must not error: journal
        // recovery retries deletes unconditionally.
        store.delete(&reference).unwrap();
    }

    #[test]
    fn unavailable_store_never_falls_back_to_a_plaintext_write() {
        let store = UnavailableCredentialStore;
        let reference = CredentialRef::generate();
        let err = store.set(&reference, "sk-test-secret").unwrap_err();
        assert!(matches!(err, CredentialStoreError::Unavailable(_)));
    }

    #[test]
    fn credential_ref_display_matches_as_str() {
        let reference = CredentialRef::from_persisted("cred-fixed-id");
        assert_eq!(reference.to_string(), "cred-fixed-id");
        assert_eq!(reference.as_str(), "cred-fixed-id");
    }
}
