use std::collections::BTreeMap;
use std::fmt;

use crate::bypass::BypassList;
use crate::endpoint::{ParsedProxy, parse_proxy_url};
use crate::error::{NetworkError, ProxySource};
use crate::route::Scheme;

/// How the OS treats environment variable names. Injected so tests cover both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvPlatform {
    /// Case-sensitive names; the upper-case-only `HTTP_PROXY` is ignored
    /// because it may be set by CGI environments.
    MacOs,
    /// Names are case-insensitive: `HTTP_PROXY` is `http_proxy`.
    Windows,
}

impl EnvPlatform {
    pub fn current() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::MacOs }
    }
}

const RECOGNIZED: [&str; 8] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
];

const HTTPS_MACOS: &[&str] = &["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"];
const HTTP_MACOS: &[&str] = &["http_proxy", "all_proxy", "ALL_PROXY"];
const NO_PROXY_MACOS: &[&str] = &["no_proxy", "NO_PROXY"];
const HTTPS_WINDOWS: &[&str] = &["https_proxy", "all_proxy"];
const HTTP_WINDOWS: &[&str] = &["http_proxy", "all_proxy"];
const NO_PROXY_WINDOWS: &[&str] = &["no_proxy"];

/// Immutable snapshot of the proxy-related variables captured at startup.
/// Values are never shown by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyEnvironment {
    platform: EnvPlatform,
    vars: BTreeMap<String, String>,
}

impl ProxyEnvironment {
    /// Capture only the proxy variables from injected name/value pairs.
    /// This never reads the process environment.
    pub fn from_vars<I, K, V>(platform: EnvPlatform, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut map = BTreeMap::new();
        for (name, value) in vars {
            let name = name.as_ref();
            match platform {
                EnvPlatform::MacOs => {
                    if RECOGNIZED.contains(&name) {
                        map.insert(name.to_string(), value.as_ref().to_string());
                    }
                }
                EnvPlatform::Windows => {
                    let lower = name.to_ascii_lowercase();
                    if RECOGNIZED.contains(&lower.as_str()) {
                        // Windows has a single value per name; if duplicates
                        // are injected, the exact lower-case spelling wins.
                        if name == lower || !map.contains_key(&lower) {
                            map.insert(lower, value.as_ref().to_string());
                        }
                    }
                }
            }
        }
        Self {
            platform,
            vars: map,
        }
    }

    /// First non-empty variable in priority order. Empty / whitespace-only
    /// values count as unset; a non-empty value is final even if it is
    /// invalid, so a broken higher-priority value never falls through.
    fn first_set(&self, names: &'static [&'static str]) -> Option<(&'static str, &str)> {
        names.iter().find_map(|&n| {
            let v = self.vars.get(n)?;
            (!v.trim().is_empty()).then_some((n, v.as_str()))
        })
    }

    fn proxy_names(&self, scheme: Scheme) -> &'static [&'static str] {
        match (self.platform, scheme) {
            (EnvPlatform::MacOs, Scheme::Https) => HTTPS_MACOS,
            (EnvPlatform::MacOs, Scheme::Http) => HTTP_MACOS,
            (EnvPlatform::Windows, Scheme::Https) => HTTPS_WINDOWS,
            (EnvPlatform::Windows, Scheme::Http) => HTTP_WINDOWS,
        }
    }

    /// The proxy selected for a destination scheme, or `None` when no
    /// variable is set. Invalid selected values are errors.
    pub(crate) fn proxy_for(
        &self,
        scheme: Scheme,
    ) -> Result<Option<ParsedProxy>, NetworkError> {
        let Some((name, value)) = self.first_set(self.proxy_names(scheme)) else {
            return Ok(None);
        };
        parse_proxy_url(value, true)
            .map(Some)
            .map_err(|problem| NetworkError::InvalidProxyUrl {
                source: ProxySource::Environment(name),
                problem,
            })
    }

    /// The selected `no_proxy` list. Absent means an empty list; an invalid
    /// list is an error.
    pub(crate) fn bypass(&self) -> Result<BypassList, NetworkError> {
        let names = match self.platform {
            EnvPlatform::MacOs => NO_PROXY_MACOS,
            EnvPlatform::Windows => NO_PROXY_WINDOWS,
        };
        match self.first_set(names) {
            None => Ok(BypassList::default()),
            Some((name, value)) => {
                BypassList::parse(value).map_err(|problem| NetworkError::InvalidBypass {
                    source: ProxySource::Environment(name),
                    problem,
                })
            }
        }
    }
}

impl fmt::Debug for ProxyEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyEnvironment")
            .field("platform", &self.platform)
            .field("variables", &self.vars.len())
            .field("values", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UrlProblem;

    fn env(platform: EnvPlatform, vars: &[(&str, &str)]) -> ProxyEnvironment {
        ProxyEnvironment::from_vars(platform, vars.iter().copied())
    }

    fn selected(e: &ProxyEnvironment, scheme: Scheme) -> Option<String> {
        e.proxy_for(scheme)
            .unwrap()
            .map(|p| p.endpoint.to_string())
    }

    #[test]
    fn https_priority_macos() {
        let all = [
            ("https_proxy", "http://a:1"),
            ("HTTPS_PROXY", "http://b:1"),
            ("all_proxy", "http://c:1"),
            ("ALL_PROXY", "http://d:1"),
        ];
        for (drop, want) in [(0, "a"), (1, "b"), (2, "c"), (3, "d")] {
            let vars: Vec<_> = all[drop..].to_vec();
            let e = env(EnvPlatform::MacOs, &vars);
            assert_eq!(
                selected(&e, Scheme::Https),
                Some(format!("http://{want}:1")),
                "drop {drop}"
            );
        }
    }

    #[test]
    fn http_priority_and_uppercase_http_proxy() {
        let e = env(
            EnvPlatform::MacOs,
            &[("HTTP_PROXY", "http://cgi:1"), ("ALL_PROXY", "http://d:1")],
        );
        assert_eq!(selected(&e, Scheme::Http), Some("http://d:1".into()));

        let e = env(EnvPlatform::MacOs, &[("HTTP_PROXY", "http://cgi:1")]);
        assert_eq!(selected(&e, Scheme::Http), None);

        let e = env(
            EnvPlatform::MacOs,
            &[("http_proxy", "http://a:1"), ("all_proxy", "http://c:1")],
        );
        assert_eq!(selected(&e, Scheme::Http), Some("http://a:1".into()));
        // http_proxy never serves HTTPS destinations.
        assert_eq!(selected(&e, Scheme::Https), Some("http://c:1".into()));
    }

    #[test]
    fn windows_names_are_case_insensitive() {
        let e = env(EnvPlatform::Windows, &[("HTTP_PROXY", "http://w:1")]);
        assert_eq!(selected(&e, Scheme::Http), Some("http://w:1".into()));
        let e = env(EnvPlatform::Windows, &[("Https_Proxy", "http://w:2")]);
        assert_eq!(selected(&e, Scheme::Https), Some("http://w:2".into()));
        let e = env(EnvPlatform::Windows, &[("ALL_PROXY", "http://w:3")]);
        assert_eq!(selected(&e, Scheme::Https), Some("http://w:3".into()));
    }

    #[test]
    fn empty_values_are_unset_but_invalid_values_fail_closed() {
        let e = env(
            EnvPlatform::MacOs,
            &[("https_proxy", ""), ("HTTPS_PROXY", "  "), ("all_proxy", "http://c:1")],
        );
        assert_eq!(selected(&e, Scheme::Https), Some("http://c:1".into()));

        let e = env(
            EnvPlatform::MacOs,
            &[("https_proxy", "socks5://x:1"), ("all_proxy", "http://c:1")],
        );
        assert_eq!(
            e.proxy_for(Scheme::Https).err(),
            Some(NetworkError::InvalidProxyUrl {
                source: ProxySource::Environment("https_proxy"),
                problem: UrlProblem::UnsupportedScheme,
            })
        );
    }

    #[test]
    fn unrelated_variables_are_not_captured() {
        let e = env(
            EnvPlatform::MacOs,
            &[("PATH", "/bin"), ("Http_Proxy", "http://x:1"), ("SSL_CERT_FILE", "/c")],
        );
        assert_eq!(e.vars.len(), 0);
    }

    #[test]
    fn no_proxy_priority_and_errors() {
        let e = env(
            EnvPlatform::MacOs,
            &[("no_proxy", "a.com"), ("NO_PROXY", "b.com")],
        );
        assert_eq!(e.bypass().unwrap().to_env_value(), "a.com");
        let e = env(EnvPlatform::MacOs, &[("NO_PROXY", "b.com")]);
        assert_eq!(e.bypass().unwrap().to_env_value(), "b.com");
        let e = env(EnvPlatform::MacOs, &[("no_proxy", "*.a.com")]);
        assert!(e.bypass().is_err());
        let e = env(EnvPlatform::Windows, &[("NO_PROXY", "w.com")]);
        assert_eq!(e.bypass().unwrap().to_env_value(), "w.com");
    }

    #[test]
    fn debug_hides_values() {
        let e = env(EnvPlatform::MacOs, &[("https_proxy", "http://user:hunter2@p:1")]);
        let text = format!("{e:?}");
        assert!(!text.contains("hunter2") && !text.contains("user"));
    }
}
