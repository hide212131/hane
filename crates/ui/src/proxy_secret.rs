//! OS secret store for the manual proxy's Basic credentials: the macOS Keychain
//! or the Windows Credential Manager, under the `hane.network.proxy` service
//! (kept apart from the AI provider keys). The settings file only ever holds
//! the opaque [`CredentialRef`]; the user name and password exist only here.
//!
//! There is no file or plaintext fallback. Where no native backend exists
//! every operation fails, and a failed read is a [`CredentialError`] that
//! callers must surface instead of connecting without authentication.
//! Errors carry fixed text only, never a secret or an OS message.

use hane_network::{BasicCredentials, CredentialError, CredentialRef, CredentialStore};

/// A write or delete in the OS secret store did not succeed. Deliberately
/// without detail text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretStoreError;

impl std::fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OSの秘密ストアに保存または削除できませんでした")
    }
}

impl std::error::Error for SecretStoreError {}

/// Write side of the proxy credential store. Reading goes through
/// [`CredentialStore`].
pub trait ProxySecretWriter: Send + Sync {
    /// Stores (or replaces) the credentials under `reference`.
    fn put(
        &self,
        reference: &CredentialRef,
        credentials: &BasicCredentials,
    ) -> Result<(), SecretStoreError>;

    /// Removes the entry. Removing an absent entry is not an error.
    fn delete(&self, reference: &CredentialRef) -> Result<(), SecretStoreError>;
}

/// The production store.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsProxyCredentialStore;

/// `user:password` as one secret. The user name never contains `:`
/// ([`BasicCredentials::check_for_basic`]), so the first `:` splits it.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(dead_code, reason = "only the native backends encode secrets")
)]
fn encode(credentials: &BasicCredentials) -> Result<String, SecretStoreError> {
    credentials
        .check_for_basic()
        .map_err(|_| SecretStoreError)?;
    Ok(format!(
        "{}:{}",
        credentials.username().expose(),
        credentials.password().expose()
    ))
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(dead_code, reason = "only the native backends decode secrets")
)]
fn decode(secret: &str) -> Result<BasicCredentials, CredentialError> {
    let (user, password) = secret.split_once(':').ok_or(CredentialError::Invalid)?;
    let credentials = BasicCredentials::new(user, password);
    credentials
        .check_for_basic()
        .map_err(|_| CredentialError::Invalid)?;
    Ok(credentials)
}

/// The OS service that holds the proxy credentials. The isolated
/// `validation-feed` build (Issue #510) uses its own service so it can never
/// read, replace or delete the credentials of the standard Hane.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", test)),
    allow(dead_code, reason = "only the native backends select a service")
)]
const fn proxy_credential_service(validation_feed: bool) -> &'static str {
    if validation_feed {
        "hane.network.proxy.validation-feed"
    } else {
        hane_network::PROXY_CREDENTIAL_SERVICE
    }
}

/// A failure of the underlying secret backend. `NoEntry` is the only case that
/// carries meaning; every other failure is opaque.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", test)),
    allow(dead_code, reason = "only the native backends use a secret backend")
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendError {
    NoEntry,
    Failed,
}

/// Raw access to an OS secret store, addressed by service and account. It is
/// the seam that lets tests check the service choice without touching the
/// native keyring.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", test)),
    allow(dead_code, reason = "only the native backends use a secret backend")
)]
trait SecretBackend {
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), BackendError>;
    fn get(&self, service: &str, account: &str) -> Result<String, BackendError>;
    fn delete(&self, service: &str, account: &str) -> Result<(), BackendError>;
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", test)),
    allow(dead_code, reason = "only the native backends use a secret backend")
)]
fn put_in<B: SecretBackend>(
    backend: &B,
    service: &str,
    reference: &CredentialRef,
    credentials: &BasicCredentials,
) -> Result<(), SecretStoreError> {
    let secret = encode(credentials)?;
    backend
        .set(service, reference.as_str(), &secret)
        .map_err(|_| SecretStoreError)
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", test)),
    allow(dead_code, reason = "only the native backends use a secret backend")
)]
fn delete_in<B: SecretBackend>(
    backend: &B,
    service: &str,
    reference: &CredentialRef,
) -> Result<(), SecretStoreError> {
    match backend.delete(service, reference.as_str()) {
        Ok(()) | Err(BackendError::NoEntry) => Ok(()),
        Err(BackendError::Failed) => Err(SecretStoreError),
    }
}

#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows", test)),
    allow(dead_code, reason = "only the native backends use a secret backend")
)]
fn load_from<B: SecretBackend>(
    backend: &B,
    service: &str,
    reference: &CredentialRef,
) -> Result<BasicCredentials, CredentialError> {
    match backend.get(service, reference.as_str()) {
        Ok(secret) => decode(&secret),
        Err(BackendError::NoEntry) => Err(CredentialError::NotFound),
        Err(BackendError::Failed) => Err(CredentialError::Unavailable),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::*;

    const SERVICE: &str = proxy_credential_service(cfg!(feature = "validation-feed"));

    struct Keyring;

    impl Keyring {
        fn entry(service: &str, account: &str) -> Result<keyring::Entry, BackendError> {
            keyring::Entry::new(service, account).map_err(|_| BackendError::Failed)
        }
    }

    impl SecretBackend for Keyring {
        fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), BackendError> {
            Self::entry(service, account)?
                .set_password(secret)
                .map_err(|_| BackendError::Failed)
        }

        fn get(&self, service: &str, account: &str) -> Result<String, BackendError> {
            Self::entry(service, account)?
                .get_password()
                .map_err(|error| match error {
                    keyring::Error::NoEntry => BackendError::NoEntry,
                    _ => BackendError::Failed,
                })
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), BackendError> {
            Self::entry(service, account)?
                .delete_password()
                .map_err(|error| match error {
                    keyring::Error::NoEntry => BackendError::NoEntry,
                    _ => BackendError::Failed,
                })
        }
    }

    impl ProxySecretWriter for OsProxyCredentialStore {
        fn put(
            &self,
            reference: &CredentialRef,
            credentials: &BasicCredentials,
        ) -> Result<(), SecretStoreError> {
            put_in(&Keyring, SERVICE, reference, credentials)
        }

        fn delete(&self, reference: &CredentialRef) -> Result<(), SecretStoreError> {
            delete_in(&Keyring, SERVICE, reference)
        }
    }

    impl CredentialStore for OsProxyCredentialStore {
        fn load(&self, reference: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
            load_from(&Keyring, SERVICE, reference)
        }
    }
}

/// No native backend: nothing can have been stored, so removing is a no-op and
/// storing or reading fails.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod native {
    use super::*;

    impl ProxySecretWriter for OsProxyCredentialStore {
        fn put(&self, _: &CredentialRef, _: &BasicCredentials) -> Result<(), SecretStoreError> {
            Err(SecretStoreError)
        }

        fn delete(&self, _: &CredentialRef) -> Result<(), SecretStoreError> {
            Ok(())
        }
    }

    impl CredentialStore for OsProxyCredentialStore {
        fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
            Err(CredentialError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_round_trips_with_colons_in_the_password() {
        let credentials = BasicCredentials::new("alice", "pa:ss:word");
        let secret = encode(&credentials).unwrap();
        assert_eq!(decode(&secret).unwrap(), credentials);
    }

    #[test]
    fn unusable_credentials_are_never_encoded() {
        for (user, password) in [("", "p"), ("a:b", "p"), ("a\r\nX: y", "p"), ("a", "p\0")] {
            assert_eq!(encode(&BasicCredentials::new(user, password)), Err(SecretStoreError));
        }
    }

    #[test]
    fn a_malformed_stored_secret_is_invalid_not_unauthenticated() {
        assert_eq!(decode("no-separator"), Err(CredentialError::Invalid));
        assert_eq!(decode(":only-password"), Err(CredentialError::Invalid));
        assert_eq!(decode("user:pa\nss"), Err(CredentialError::Invalid));
    }

    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeBackend {
        entries: Mutex<HashMap<(String, String), String>>,
    }

    impl SecretBackend for FakeBackend {
        fn set(&self, service: &str, account: &str, secret: &str) -> Result<(), BackendError> {
            self.entries
                .lock()
                .unwrap()
                .insert((service.into(), account.into()), secret.into());
            Ok(())
        }

        fn get(&self, service: &str, account: &str) -> Result<String, BackendError> {
            self.entries
                .lock()
                .unwrap()
                .get(&(service.to_string(), account.to_string()))
                .cloned()
                .ok_or(BackendError::NoEntry)
        }

        fn delete(&self, service: &str, account: &str) -> Result<(), BackendError> {
            self.entries
                .lock()
                .unwrap()
                .remove(&(service.to_string(), account.to_string()))
                .map(|_| ())
                .ok_or(BackendError::NoEntry)
        }
    }

    #[test]
    fn validation_and_standard_builds_select_different_services() {
        assert_eq!(proxy_credential_service(false), "hane.network.proxy");
        assert_eq!(
            proxy_credential_service(true),
            "hane.network.proxy.validation-feed"
        );
        assert_ne!(proxy_credential_service(false), proxy_credential_service(true));
    }

    #[test]
    fn validation_service_never_reads_replaces_or_deletes_standard_credentials() {
        let backend = FakeBackend::default();
        let reference = CredentialRef::new("proxy-1").unwrap();
        let standard = proxy_credential_service(false);
        let validation = proxy_credential_service(true);
        let normal_credentials = BasicCredentials::new("normal", "secret");
        let test_credentials = BasicCredentials::new("validation", "other");

        put_in(&backend, standard, &reference, &normal_credentials).unwrap();

        // The validation service cannot see the standard entry.
        assert_eq!(
            load_from(&backend, validation, &reference),
            Err(CredentialError::NotFound)
        );

        // Saving and loading in the validation service leaves the standard entry.
        put_in(&backend, validation, &reference, &test_credentials).unwrap();
        assert_eq!(
            load_from(&backend, validation, &reference).unwrap(),
            test_credentials
        );
        assert_eq!(
            load_from(&backend, standard, &reference).unwrap(),
            normal_credentials
        );

        // Deleting in the validation service leaves the standard entry.
        delete_in(&backend, validation, &reference).unwrap();
        assert_eq!(
            load_from(&backend, validation, &reference),
            Err(CredentialError::NotFound)
        );
        assert_eq!(
            load_from(&backend, standard, &reference).unwrap(),
            normal_credentials
        );

        // Deleting an absent entry is not an error.
        delete_in(&backend, validation, &reference).unwrap();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn without_a_native_backend_nothing_is_stored_and_reads_fail() {
        let reference = CredentialRef::new("proxy-1").unwrap();
        let store = OsProxyCredentialStore;
        assert!(store.put(&reference, &BasicCredentials::new("a", "b")).is_err());
        assert_eq!(store.load(&reference), Err(CredentialError::Unavailable));
        assert!(store.delete(&reference).is_ok());
    }
}
