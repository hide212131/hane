use std::error::Error;
use std::fmt;

use crate::error::NetworkError;

/// OS secret store service name reserved for proxy credentials. Kept apart
/// from the AI provider key namespace.
pub const PROXY_CREDENTIAL_SERVICE: &str = "hane.network.proxy";

/// A secret string. Debug is redacted; there is intentionally no `Display`
/// and no `Serialize` implementation.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Proxy Basic credentials. Both parts are treated as secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicCredentials {
    username: Secret,
    password: Secret,
}

impl BasicCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: Secret::new(username),
            password: Secret::new(password),
        }
    }

    pub fn username(&self) -> &Secret {
        &self.username
    }

    pub fn password(&self) -> &Secret {
        &self.password
    }
}

/// Opaque id of a credential in the OS secret store (service
/// [`PROXY_CREDENTIAL_SERVICE`]). The id itself is not a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn new(id: impl Into<String>) -> Result<Self, NetworkError> {
        let id = id.into();
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if valid {
            Ok(Self(id))
        } else {
            Err(NetworkError::InvalidCredentialRef)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Failure to obtain a stored credential. Callers must surface it; it never
/// means "connect without authentication" or "connect directly".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialError {
    NotFound,
    Unavailable,
    Invalid,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "資格情報が見つかりません",
            Self::Unavailable => "秘密ストアにアクセスできません",
            Self::Invalid => "保存された資格情報が不正です",
        })
    }
}

impl Error for CredentialError {}

/// Backend contract implemented later by the Keychain / Credential Manager
/// adapters.
pub trait CredentialStore {
    fn load(&self, reference: &CredentialRef) -> Result<BasicCredentials, CredentialError>;
}

/// Credentials attached to a resolved route. Route decision and secret
/// retrieval are separate: a bypassed destination never produces a
/// `Stored` source, so nothing is fetched for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    None,
    /// Decoded from an environment proxy URL.
    Inline(BasicCredentials),
    /// Manual Basic credentials still to be loaded from the secret store.
    Stored(CredentialRef),
}

/// Obtain the credentials for a route. A store failure is an error, never
/// `Ok(None)`.
pub fn load_credentials(
    source: &CredentialSource,
    store: &dyn CredentialStore,
) -> Result<Option<BasicCredentials>, NetworkError> {
    match source {
        CredentialSource::None => Ok(None),
        CredentialSource::Inline(c) => Ok(Some(c.clone())),
        CredentialSource::Stored(r) => store.load(r).map(Some).map_err(NetworkError::Credential),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failing(CredentialError);
    impl CredentialStore for Failing {
        fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
            Err(self.0)
        }
    }

    #[test]
    fn debug_hides_secret_values() {
        let c = BasicCredentials::new("alice", "p@ss");
        let text = format!("{c:?} {:?}", CredentialSource::Inline(c.clone()));
        assert!(!text.contains("alice"));
        assert!(!text.contains("p@ss"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn credential_ref_validation() {
        for ok in ["a", "abc-123_x.y"] {
            assert!(CredentialRef::new(ok).is_ok(), "{ok}");
        }
        let long = "x".repeat(129);
        for bad in ["", "a b", "a/b", "日本", long.as_str()] {
            assert_eq!(
                CredentialRef::new(bad),
                Err(NetworkError::InvalidCredentialRef),
                "{bad}"
            );
        }
    }

    #[test]
    fn store_failure_is_an_error_not_unauthenticated() {
        let r = CredentialSource::Stored(CredentialRef::new("id1").unwrap());
        for e in [
            CredentialError::NotFound,
            CredentialError::Unavailable,
            CredentialError::Invalid,
        ] {
            assert_eq!(
                load_credentials(&r, &Failing(e)),
                Err(NetworkError::Credential(e))
            );
        }
        assert_eq!(
            load_credentials(&CredentialSource::None, &Failing(CredentialError::NotFound)),
            Ok(None)
        );
    }
}
