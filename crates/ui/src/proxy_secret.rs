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

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::*;
    use hane_network::PROXY_CREDENTIAL_SERVICE;

    fn entry(reference: &CredentialRef) -> Option<keyring::Entry> {
        keyring::Entry::new(PROXY_CREDENTIAL_SERVICE, reference.as_str()).ok()
    }

    impl ProxySecretWriter for OsProxyCredentialStore {
        fn put(
            &self,
            reference: &CredentialRef,
            credentials: &BasicCredentials,
        ) -> Result<(), SecretStoreError> {
            let secret = encode(credentials)?;
            entry(reference)
                .ok_or(SecretStoreError)?
                .set_password(&secret)
                .map_err(|_| SecretStoreError)
        }

        fn delete(&self, reference: &CredentialRef) -> Result<(), SecretStoreError> {
            match entry(reference).ok_or(SecretStoreError)?.delete_password() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(SecretStoreError),
            }
        }
    }

    impl CredentialStore for OsProxyCredentialStore {
        fn load(&self, reference: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
            let entry = entry(reference).ok_or(CredentialError::Unavailable)?;
            match entry.get_password() {
                Ok(secret) => decode(&secret),
                Err(keyring::Error::NoEntry) => Err(CredentialError::NotFound),
                Err(_) => Err(CredentialError::Unavailable),
            }
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
