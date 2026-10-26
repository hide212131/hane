use std::fmt;

use crate::credential::{BasicCredentials, CredentialSource, CredentialStore, load_credentials};
use crate::endpoint::{ProxyEndpoint, percent_encode};
use crate::environment::ProxyEnvironment;
use crate::error::NetworkError;
use crate::route::Scheme;
use crate::settings::{NetworkSettings, ProxyAuth, ProxyMode};

/// Proxy-related variable names removed from a child's parent environment,
/// compared case-insensitively. Certificate, PATH and provider variables are
/// deliberately not listed.
const PROXY_VARS: [&str; 6] = [
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ftp_proxy",
    "socks_proxy",
    "no_proxy",
];

/// Loopback destinations always stay direct (Custom Provider loopback HTTP,
/// OAuth callback). Appended to the child's no_proxy when a proxy is set.
const LOOPBACK_BYPASS: &str = "localhost,127.0.0.1,::1";

#[derive(Clone, PartialEq, Eq)]
struct ChildProxy {
    endpoint: ProxyEndpoint,
    credentials: Option<BasicCredentials>,
}

impl ChildProxy {
    fn url(&self) -> String {
        match &self.credentials {
            None => self.endpoint.to_string(),
            Some(c) => format!(
                "http://{}:{}@{}:{}",
                percent_encode(c.username().expose()),
                percent_encode(c.password().expose()),
                self.endpoint.host(),
                self.endpoint.port()
            ),
        }
    }
}

/// The normalized proxy configuration for a child process, derived from one
/// settings/environment snapshot. Debug does not show endpoints or secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct ChildProxyPlan {
    http: Option<ChildProxy>,
    https: Option<ChildProxy>,
    no_proxy: Option<String>,
}

impl ChildProxyPlan {
    /// Build the plan. Credentials are loaded only for manual Basic, via the
    /// injected store; a load failure is an error, never an unauthenticated
    /// or direct plan.
    pub fn from_settings(
        settings: &NetworkSettings,
        environment: &ProxyEnvironment,
        store: &dyn CredentialStore,
    ) -> Result<Self, NetworkError> {
        settings.check_schema()?;
        match settings.mode {
            ProxyMode::Direct => Ok(Self::direct()),
            ProxyMode::Manual => {
                let manual = settings
                    .manual
                    .as_ref()
                    .ok_or(NetworkError::ManualNotConfigured)?;
                let source = match manual.auth() {
                    ProxyAuth::None => CredentialSource::None,
                    ProxyAuth::Basic(r) => CredentialSource::Stored(r.clone()),
                };
                let credentials = load_credentials(&source, store)?;
                let proxy = ChildProxy {
                    endpoint: manual.endpoint().clone(),
                    credentials,
                };
                Ok(Self {
                    http: Some(proxy.clone()),
                    https: Some(proxy),
                    no_proxy: Some(with_loopback(&manual.bypass().to_env_value())),
                })
            }
            ProxyMode::Environment => {
                let http = Self::from_env(environment, Scheme::Http)?;
                let https = Self::from_env(environment, Scheme::Https)?;
                if http.is_none() && https.is_none() {
                    return Ok(Self::direct());
                }
                let bypass = environment.bypass()?;
                Ok(Self {
                    http,
                    https,
                    no_proxy: Some(with_loopback(&bypass.to_env_value())),
                })
            }
        }
    }

    fn from_env(
        environment: &ProxyEnvironment,
        scheme: Scheme,
    ) -> Result<Option<ChildProxy>, NetworkError> {
        Ok(environment.proxy_for(scheme)?.map(|p| ChildProxy {
            endpoint: p.endpoint,
            credentials: p
                .userinfo
                .map(|(user, pass)| BasicCredentials::new(user, pass)),
        }))
    }

    fn direct() -> Self {
        Self {
            http: None,
            https: None,
            no_proxy: None,
        }
    }

    /// Whether the child gets any proxy variable.
    pub fn uses_proxy(&self) -> bool {
        self.http.is_some() || self.https.is_some()
    }

    /// Strip every proxy variable from `parent` (case-insensitively, so no
    /// conflicting `ALL_PROXY` etc. survives) and add the normalized values.
    /// All other variables, such as certificates, PATH and provider settings,
    /// are passed through unchanged and in order.
    pub fn apply<I, K, V>(&self, parent: I) -> ChildEnvironment
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut vars: Vec<(String, String)> = parent
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .filter(|(k, _)| !PROXY_VARS.iter().any(|p| k.eq_ignore_ascii_case(p)))
            .collect();
        if let Some(p) = &self.http {
            let url = p.url();
            vars.push(("http_proxy".into(), url.clone()));
            vars.push(("HTTP_PROXY".into(), url));
        }
        if let Some(p) = &self.https {
            let url = p.url();
            vars.push(("https_proxy".into(), url.clone()));
            vars.push(("HTTPS_PROXY".into(), url));
        }
        if let Some(list) = &self.no_proxy {
            vars.push(("no_proxy".into(), list.clone()));
            vars.push(("NO_PROXY".into(), list.clone()));
        }
        ChildEnvironment { vars }
    }
}

fn with_loopback(list: &str) -> String {
    if list == "*" {
        "*".to_string()
    } else if list.is_empty() {
        LOOPBACK_BYPASS.to_string()
    } else {
        format!("{list},{LOOPBACK_BYPASS}")
    }
}

impl fmt::Debug for ChildProxyPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChildProxyPlan")
            .field("http", &self.http.is_some())
            .field("https", &self.https.is_some())
            .field("values", &"<redacted>")
            .finish()
    }
}

/// Child process environment. Values may contain proxy credentials, so Debug
/// shows variable names only.
#[derive(Clone, PartialEq, Eq)]
pub struct ChildEnvironment {
    vars: Vec<(String, String)>,
}

impl ChildEnvironment {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn into_pairs(self) -> Vec<(String, String)> {
        self.vars
    }
}

impl fmt::Debug for ChildEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.vars.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("ChildEnvironment")
            .field("names", &names)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{CredentialError, CredentialRef};
    use crate::environment::EnvPlatform;
    use crate::settings::ManualProxy;

    struct Store(Result<BasicCredentials, CredentialError>);
    impl CredentialStore for Store {
        fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
            self.0.clone()
        }
    }

    fn no_store() -> Store {
        Store(Err(CredentialError::Unavailable))
    }

    fn env(vars: &[(&str, &str)]) -> ProxyEnvironment {
        ProxyEnvironment::from_vars(EnvPlatform::MacOs, vars.iter().copied())
    }

    fn parent() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PATH", "/usr/bin"),
            ("SSL_CERT_FILE", "/certs/ca.pem"),
            ("OPENAI_API_KEY", "k"),
            ("ALL_PROXY", "http://conflict:1"),
            ("Http_Proxy", "http://conflict:2"),
            ("NO_PROXY", "conflict"),
            ("socks_proxy", "socks5://x:1"),
        ]
    }

    fn assert_others_untouched(out: &ChildEnvironment) {
        assert_eq!(out.get("PATH"), Some("/usr/bin"));
        assert_eq!(out.get("SSL_CERT_FILE"), Some("/certs/ca.pem"));
        assert_eq!(out.get("OPENAI_API_KEY"), Some("k"));
    }

    #[test]
    fn direct_strips_all_proxy_variables() {
        let plan = ChildProxyPlan::from_settings(
            &NetworkSettings::direct(),
            &env(&[("https_proxy", "http://p:1")]),
            &no_store(),
        )
        .unwrap();
        assert!(!plan.uses_proxy());
        let out = plan.apply(parent());
        assert_eq!(out.iter().count(), 3);
        assert_others_untouched(&out);
    }

    #[test]
    fn environment_normalizes_and_drops_conflicts() {
        let e = env(&[
            ("https_proxy", "http://secure:3128"),
            ("all_proxy", "http://fallback:80"),
            ("no_proxy", "internal.example"),
        ]);
        let plan =
            ChildProxyPlan::from_settings(&NetworkSettings::default(), &e, &no_store()).unwrap();
        let out = plan.apply(parent());
        assert_others_untouched(&out);
        assert_eq!(out.get("https_proxy"), Some("http://secure:3128"));
        assert_eq!(out.get("HTTPS_PROXY"), Some("http://secure:3128"));
        // all_proxy applies to HTTP as well, resolved into http_proxy.
        assert_eq!(out.get("http_proxy"), Some("http://fallback:80"));
        assert_eq!(out.get("ALL_PROXY"), None);
        assert_eq!(out.get("Http_Proxy"), None);
        assert_eq!(out.get("socks_proxy"), None);
        assert_eq!(
            out.get("no_proxy"),
            Some("internal.example,localhost,127.0.0.1,::1")
        );
        assert_eq!(out.get("NO_PROXY"), out.get("no_proxy"));
    }

    #[test]
    fn environment_without_proxy_is_just_stripped() {
        let plan =
            ChildProxyPlan::from_settings(&NetworkSettings::default(), &env(&[]), &no_store())
                .unwrap();
        let out = plan.apply(parent());
        assert_eq!(out.iter().count(), 3);
    }

    #[test]
    fn environment_credentials_are_re_encoded() {
        let e = env(&[("https_proxy", "http://u%40:p%2540@p:1")]);
        let plan =
            ChildProxyPlan::from_settings(&NetworkSettings::default(), &e, &no_store()).unwrap();
        let out = plan.apply(Vec::<(String, String)>::new());
        assert_eq!(out.get("https_proxy"), Some("http://u%40:p%2540@p:1"));
        assert_eq!(out.get("http_proxy"), None);
        assert!(!format!("{plan:?} {out:?}").contains("p%2540"));
    }

    #[test]
    fn manual_uses_manual_values_and_store() {
        let reference = CredentialRef::new("c1").unwrap();
        let m = ManualProxy::new(
            "http://[2001:db8::1]:8080",
            "a.com",
            ProxyAuth::Basic(reference),
        )
        .unwrap();
        let s = NetworkSettings::manual(m);
        let e = env(&[("https_proxy", "http://env:1"), ("no_proxy", "env.example")]);

        let store = Store(Ok(BasicCredentials::new("user", "p w")));
        let out = ChildProxyPlan::from_settings(&s, &e, &store)
            .unwrap()
            .apply(parent());
        assert_others_untouched(&out);
        let want = "http://user:p%20w@[2001:db8::1]:8080";
        assert_eq!(out.get("https_proxy"), Some(want));
        assert_eq!(out.get("http_proxy"), Some(want));
        assert_eq!(out.get("no_proxy"), Some("a.com,localhost,127.0.0.1,::1"));
    }

    #[test]
    fn credential_failure_is_an_error() {
        let m = ManualProxy::new(
            "http://p:1",
            "",
            ProxyAuth::Basic(CredentialRef::new("c1").unwrap()),
        )
        .unwrap();
        let r = ChildProxyPlan::from_settings(&NetworkSettings::manual(m), &env(&[]), &no_store());
        assert_eq!(
            r.err(),
            Some(NetworkError::Credential(CredentialError::Unavailable))
        );
    }

    #[test]
    fn wildcard_bypass_is_kept() {
        let e = env(&[("https_proxy", "http://p:1"), ("no_proxy", "*")]);
        let plan =
            ChildProxyPlan::from_settings(&NetworkSettings::default(), &e, &no_store()).unwrap();
        assert_eq!(
            plan.apply(Vec::<(String, String)>::new()).get("no_proxy"),
            Some("*")
        );
    }

    #[test]
    fn invalid_environment_is_an_error() {
        let e = env(&[("https_proxy", "http://p:1"), ("no_proxy", "10.0.0.0/8")]);
        assert!(
            ChildProxyPlan::from_settings(&NetworkSettings::default(), &e, &no_store()).is_err()
        );
        let e = env(&[("http_proxy", "ftp://p:1")]);
        assert!(
            ChildProxyPlan::from_settings(&NetworkSettings::default(), &e, &no_store()).is_err()
        );
    }
}
