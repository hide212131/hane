use crate::credential::{BasicCredentials, CredentialSource};
use crate::endpoint::ProxyEndpoint;
use crate::environment::ProxyEnvironment;
use crate::error::NetworkError;
use crate::host::Host;
use crate::settings::{NetworkSettings, ProxyAuth, ProxyMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

/// A request destination: scheme plus a normalized host. The port is not
/// part of route selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    scheme: Scheme,
    host: Host,
}

impl Destination {
    /// `host` is a DNS name, IPv4 address, `[IPv6]` or a bare IPv6 address.
    pub fn new(scheme: Scheme, host: &str) -> Result<Self, NetworkError> {
        let host = Host::parse(host, true).ok_or(NetworkError::InvalidDestination)?;
        Ok(Self { scheme, host })
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    pub fn host(&self) -> &Host {
        &self.host
    }
}

/// Outcome of route selection. Contains no fetched secret: a manual Basic
/// credential is only a reference until `load_credentials` is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedRoute {
    Direct,
    HttpProxy {
        endpoint: ProxyEndpoint,
        credentials: CredentialSource,
    },
}

/// Decide the route for a destination without any I/O.
///
/// Loopback destinations are always `Direct`. Configuration problems are
/// errors; they are never converted into `Direct`.
pub fn resolve(
    settings: &NetworkSettings,
    environment: &ProxyEnvironment,
    destination: &Destination,
) -> Result<ResolvedRoute, NetworkError> {
    settings.check_schema()?;
    if destination.host.is_loopback() {
        return Ok(ResolvedRoute::Direct);
    }
    match settings.mode {
        ProxyMode::Direct => Ok(ResolvedRoute::Direct),
        ProxyMode::Manual => {
            let manual = settings
                .manual
                .as_ref()
                .ok_or(NetworkError::ManualNotConfigured)?;
            if manual.bypass().matches(&destination.host) {
                return Ok(ResolvedRoute::Direct);
            }
            let credentials = match manual.auth() {
                ProxyAuth::None => CredentialSource::None,
                ProxyAuth::Basic(r) => CredentialSource::Stored(r.clone()),
            };
            Ok(ResolvedRoute::HttpProxy {
                endpoint: manual.endpoint().clone(),
                credentials,
            })
        }
        ProxyMode::Environment => {
            let Some(proxy) = environment.proxy_for(destination.scheme)? else {
                return Ok(ResolvedRoute::Direct);
            };
            let bypass = environment.bypass()?;
            if bypass.matches(&destination.host) {
                return Ok(ResolvedRoute::Direct);
            }
            let credentials = match proxy.userinfo {
                Some((user, pass)) => CredentialSource::Inline(BasicCredentials::new(user, pass)),
                None => CredentialSource::None,
            };
            Ok(ResolvedRoute::HttpProxy {
                endpoint: proxy.endpoint,
                credentials,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::CredentialRef;
    use crate::environment::EnvPlatform;
    use crate::error::{ProxySource, UrlProblem};
    use crate::settings::ManualProxy;

    fn env(vars: &[(&str, &str)]) -> ProxyEnvironment {
        ProxyEnvironment::from_vars(EnvPlatform::MacOs, vars.iter().copied())
    }

    fn dest(scheme: Scheme, host: &str) -> Destination {
        Destination::new(scheme, host).unwrap()
    }

    fn proxy_endpoint(route: &ResolvedRoute) -> Option<String> {
        match route {
            ResolvedRoute::Direct => None,
            ResolvedRoute::HttpProxy { endpoint, .. } => Some(endpoint.to_string()),
        }
    }

    #[test]
    fn environment_mode_table() {
        let e = env(&[
            ("https_proxy", "http://secure:3128"),
            ("http_proxy", "http://plain:3128"),
            ("no_proxy", "internal.example, 10.0.0.5"),
        ]);
        let s = NetworkSettings::default();
        let cases = [
            (Scheme::Https, "example.com", Some("http://secure:3128")),
            (Scheme::Http, "example.com", Some("http://plain:3128")),
            (Scheme::Https, "api.internal.example", None),
            (Scheme::Https, "internal.example", None),
            (
                Scheme::Https,
                "notinternal.example",
                Some("http://secure:3128"),
            ),
            (Scheme::Https, "10.0.0.5", None),
            (Scheme::Https, "10.0.0.6", Some("http://secure:3128")),
            (Scheme::Https, "localhost", None),
            (Scheme::Http, "127.0.0.1", None),
            (Scheme::Https, "[::1]", None),
            (Scheme::Https, "a.localhost", Some("http://secure:3128")),
        ];
        for (scheme, host, want) in cases {
            let r = resolve(&s, &e, &dest(scheme, host)).unwrap();
            assert_eq!(proxy_endpoint(&r).as_deref(), want, "{scheme:?} {host}");
        }
    }

    #[test]
    fn environment_without_variables_is_direct() {
        let r = resolve(
            &NetworkSettings::default(),
            &env(&[]),
            &dest(Scheme::Https, "a.com"),
        );
        assert_eq!(r, Ok(ResolvedRoute::Direct));
    }

    #[test]
    fn invalid_environment_fails_closed() {
        let s = NetworkSettings::default();
        let d = dest(Scheme::Https, "a.com");
        let e = env(&[
            ("https_proxy", "http://bad host"),
            ("all_proxy", "http://ok:1"),
        ]);
        assert_eq!(
            resolve(&s, &e, &d),
            Err(NetworkError::InvalidProxyUrl {
                source: ProxySource::Environment("https_proxy"),
                problem: UrlProblem::InvalidHost,
            })
        );
        let e = env(&[("https_proxy", "http://ok:1"), ("no_proxy", "*.a.com")]);
        assert!(matches!(
            resolve(&s, &e, &d),
            Err(NetworkError::InvalidBypass { .. })
        ));
        // Loopback stays Direct even when the environment is broken.
        let loopback = dest(Scheme::Https, "localhost");
        assert_eq!(resolve(&s, &e, &loopback), Ok(ResolvedRoute::Direct));
    }

    #[test]
    fn environment_userinfo_becomes_inline_credentials() {
        let e = env(&[("https_proxy", "http://u%3Aser:pw@p:1")]);
        let r = resolve(
            &NetworkSettings::default(),
            &e,
            &dest(Scheme::Https, "a.com"),
        )
        .unwrap();
        let ResolvedRoute::HttpProxy { credentials, .. } = &r else {
            panic!("expected proxy");
        };
        assert_eq!(
            credentials,
            &CredentialSource::Inline(BasicCredentials::new("u:ser", "pw"))
        );
        assert!(!format!("{r:?}").contains("pw"));
    }

    #[test]
    fn direct_mode_ignores_environment() {
        let e = env(&[("https_proxy", "http://p:1"), ("no_proxy", "*.bad")]);
        let r = resolve(
            &NetworkSettings::direct(),
            &e,
            &dest(Scheme::Https, "a.com"),
        );
        assert_eq!(r, Ok(ResolvedRoute::Direct));
    }

    #[test]
    fn manual_mode_ignores_environment_and_uses_manual_bypass() {
        let e = env(&[("https_proxy", "http://env:1"), ("no_proxy", "a.com")]);
        let m = ManualProxy::new("http://manual:8080", "skip.example", ProxyAuth::None).unwrap();
        let s = NetworkSettings::manual(m);
        let r = resolve(&s, &e, &dest(Scheme::Https, "a.com")).unwrap();
        assert_eq!(proxy_endpoint(&r).as_deref(), Some("http://manual:8080"));
        let r = resolve(&s, &e, &dest(Scheme::Http, "x.skip.example")).unwrap();
        assert_eq!(r, ResolvedRoute::Direct);
        let r = resolve(&s, &e, &dest(Scheme::Https, "[::1]")).unwrap();
        assert_eq!(r, ResolvedRoute::Direct);
    }

    #[test]
    fn manual_basic_returns_reference_only_and_bypass_has_none() {
        let reference = CredentialRef::new("cred-1").unwrap();
        let m = ManualProxy::new(
            "http://[2001:db8::1]",
            "skip.example",
            ProxyAuth::Basic(reference.clone()),
        )
        .unwrap();
        let s = NetworkSettings::manual(m);
        let r = resolve(&s, &env(&[]), &dest(Scheme::Https, "a.com")).unwrap();
        assert_eq!(
            r,
            ResolvedRoute::HttpProxy {
                endpoint: match &r {
                    ResolvedRoute::HttpProxy { endpoint, .. } => endpoint.clone(),
                    ResolvedRoute::Direct => unreachable!(),
                },
                credentials: CredentialSource::Stored(reference),
            }
        );
        assert_eq!(
            proxy_endpoint(&r).as_deref(),
            Some("http://[2001:db8::1]:80")
        );
        // Bypassed destinations carry no credential reference to fetch.
        let r = resolve(&s, &env(&[]), &dest(Scheme::Https, "skip.example")).unwrap();
        assert_eq!(r, ResolvedRoute::Direct);
    }

    #[test]
    fn broken_settings_are_errors_not_direct() {
        let d = dest(Scheme::Https, "a.com");
        let manual_missing = NetworkSettings {
            mode: ProxyMode::Manual,
            ..NetworkSettings::default()
        };
        assert_eq!(
            resolve(&manual_missing, &env(&[]), &d),
            Err(NetworkError::ManualNotConfigured)
        );
        let bad_schema = NetworkSettings {
            schema: 7,
            mode: ProxyMode::Direct,
            manual: None,
        };
        assert_eq!(
            resolve(&bad_schema, &env(&[]), &d),
            Err(NetworkError::UnsupportedSchema(7))
        );
    }

    #[test]
    fn destination_validation() {
        for bad in ["", "a b", "http://a.com", "a.com:443", "1.2.3"] {
            assert_eq!(
                Destination::new(Scheme::Https, bad),
                Err(NetworkError::InvalidDestination),
                "{bad}"
            );
        }
        assert!(Destination::new(Scheme::Https, "2001:db8::1").is_ok());
        assert!(Destination::new(Scheme::Https, "[2001:db8::1]").is_ok());
    }
}
