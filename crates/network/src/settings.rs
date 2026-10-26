use crate::bypass::BypassList;
use crate::credential::CredentialRef;
use crate::endpoint::{ProxyEndpoint, parse_proxy_url};
use crate::error::{NetworkError, ProxySource};

/// Current persisted schema version of [`NetworkSettings`].
pub const SETTINGS_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyMode {
    /// Use the proxy variables captured at startup (default).
    #[default]
    Environment,
    /// Ignore proxy environment and connect directly.
    Direct,
    /// Use the manual HTTP proxy.
    Manual,
}

/// Manual proxy authentication method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyAuth {
    None,
    /// Basic auth; the secret lives in the OS store under this reference.
    Basic(CredentialRef),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualProxy {
    endpoint: ProxyEndpoint,
    bypass: BypassList,
    auth: ProxyAuth,
}

impl ManualProxy {
    /// Validate user input. The URL must be `http://host[:port]` without
    /// userinfo, path, query or fragment.
    pub fn new(url: &str, bypass: &str, auth: ProxyAuth) -> Result<Self, NetworkError> {
        let parsed = parse_proxy_url(url, false).map_err(|problem| NetworkError::InvalidProxyUrl {
            source: ProxySource::Manual,
            problem,
        })?;
        let bypass = BypassList::parse(bypass).map_err(|problem| NetworkError::InvalidBypass {
            source: ProxySource::Manual,
            problem,
        })?;
        Ok(Self {
            endpoint: parsed.endpoint,
            bypass,
            auth,
        })
    }

    pub fn endpoint(&self) -> &ProxyEndpoint {
        &self.endpoint
    }

    pub fn bypass(&self) -> &BypassList {
        &self.bypass
    }

    pub fn auth(&self) -> &ProxyAuth {
        &self.auth
    }
}

/// App-wide network settings. Holds no secret values, only a credential
/// reference. Invalid persisted state is reported as an error by the caller
/// and is never converted to `Direct`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkSettings {
    pub schema: u32,
    pub mode: ProxyMode,
    pub manual: Option<ManualProxy>,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
            mode: ProxyMode::Environment,
            manual: None,
        }
    }
}

impl NetworkSettings {
    pub fn manual(manual: ManualProxy) -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
            mode: ProxyMode::Manual,
            manual: Some(manual),
        }
    }

    pub fn direct() -> Self {
        Self {
            mode: ProxyMode::Direct,
            ..Self::default()
        }
    }

    pub(crate) fn check_schema(&self) -> Result<(), NetworkError> {
        if self.schema == SETTINGS_SCHEMA {
            Ok(())
        } else {
            Err(NetworkError::UnsupportedSchema(self.schema))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{BypassProblem, UrlProblem};

    #[test]
    fn default_is_environment() {
        let s = NetworkSettings::default();
        assert_eq!(s.mode, ProxyMode::Environment);
        assert!(s.manual.is_none());
        assert!(s.check_schema().is_ok());
    }

    #[test]
    fn unknown_schema_is_an_error() {
        let s = NetworkSettings {
            schema: 99,
            ..NetworkSettings::default()
        };
        assert_eq!(s.check_schema(), Err(NetworkError::UnsupportedSchema(99)));
    }

    #[test]
    fn manual_validation() {
        assert!(ManualProxy::new("http://p:3128", "a.com", ProxyAuth::None).is_ok());
        assert_eq!(
            ManualProxy::new("http://u:p@p:3128", "", ProxyAuth::None),
            Err(NetworkError::InvalidProxyUrl {
                source: ProxySource::Manual,
                problem: UrlProblem::UserinfoNotAllowed,
            })
        );
        assert_eq!(
            ManualProxy::new("http://p:3128", "*.a.com", ProxyAuth::None),
            Err(NetworkError::InvalidBypass {
                source: ProxySource::Manual,
                problem: BypassProblem::Wildcard,
            })
        );
    }

    #[test]
    fn errors_do_not_echo_input() {
        let e = ManualProxy::new("http://user:secret@p", "", ProxyAuth::None).unwrap_err();
        assert!(!format!("{e} {e:?}").contains("secret"));
    }
}
