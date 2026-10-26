use std::fmt;

use crate::error::UrlProblem;
use crate::host::Host;

const DEFAULT_PORT: u16 = 80;

/// An HTTP proxy endpoint (`http://host:port`). Never carries userinfo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyEndpoint {
    host: Host,
    port: u16,
}

impl ProxyEndpoint {
    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for ProxyEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}:{}", self.host, self.port)
    }
}

/// Result of parsing a proxy URL. `userinfo` is already percent-decoded once.
pub(crate) struct ParsedProxy {
    pub endpoint: ProxyEndpoint,
    pub userinfo: Option<(String, String)>,
}

/// Parse an `http://` proxy URL. `allow_userinfo` is false for manual settings.
pub(crate) fn parse_proxy_url(input: &str, allow_userinfo: bool) -> Result<ParsedProxy, UrlProblem> {
    let text = input.trim();
    if text.is_empty() {
        return Err(UrlProblem::Empty);
    }
    if text.chars().any(char::is_control) {
        return Err(UrlProblem::ControlCharacter);
    }
    let (scheme, rest) = text.split_once("://").ok_or(UrlProblem::MissingScheme)?;
    if !scheme.eq_ignore_ascii_case("http") {
        return Err(UrlProblem::UnsupportedScheme);
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match tail {
        "" | "/" => {}
        t if t.starts_with('?') => return Err(UrlProblem::QueryNotAllowed),
        t if t.starts_with('#') => return Err(UrlProblem::FragmentNotAllowed),
        _ => return Err(UrlProblem::PathNotAllowed),
    }

    let (userinfo_raw, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let userinfo = match userinfo_raw {
        None => None,
        Some(_) if !allow_userinfo => return Err(UrlProblem::UserinfoNotAllowed),
        Some(raw) => {
            let (user, pass) = raw.split_once(':').unwrap_or((raw, ""));
            if user.is_empty() {
                return Err(UrlProblem::InvalidUserinfo);
            }
            let user = percent_decode_once(user).ok_or(UrlProblem::InvalidUserinfo)?;
            let pass = percent_decode_once(pass).ok_or(UrlProblem::InvalidUserinfo)?;
            if user.chars().chain(pass.chars()).any(char::is_control) {
                return Err(UrlProblem::ControlCharacter);
            }
            Some((user, pass))
        }
    };

    let (host_text, port_text) = if hostport.starts_with('[') {
        let close = hostport.find(']').ok_or(UrlProblem::InvalidHost)?;
        let after = &hostport[close + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':').ok_or(UrlProblem::InvalidHost)?)
        };
        (&hostport[..=close], port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    if host_text.is_empty() {
        return Err(UrlProblem::EmptyHost);
    }
    let host = Host::parse(host_text, false).ok_or(UrlProblem::InvalidHost)?;
    let port = match port_text {
        None => DEFAULT_PORT,
        Some(p) => parse_port(p)?,
    };
    Ok(ParsedProxy {
        endpoint: ProxyEndpoint { host, port },
        userinfo,
    })
}

fn parse_port(text: &str) -> Result<u16, UrlProblem> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UrlProblem::InvalidPort);
    }
    match text.parse::<u16>() {
        Ok(p) if p != 0 => Ok(p),
        _ => Err(UrlProblem::InvalidPort),
    }
}

/// Percent-decode exactly once. `None` for malformed escapes or non-UTF-8.
fn percent_decode_once(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encode everything except RFC 3986 unreserved characters.
pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &str, allow: bool) -> ParsedProxy {
        match parse_proxy_url(input, allow) {
            Ok(p) => p,
            Err(e) => panic!("{input}: {e:?}"),
        }
    }

    fn err(input: &str, allow: bool) -> UrlProblem {
        match parse_proxy_url(input, allow) {
            Ok(p) => panic!("{input} accepted as {}", p.endpoint),
            Err(e) => e,
        }
    }

    #[test]
    fn accepted_endpoints() {
        let cases = [
            ("http://proxy.example.com:3128", "http://proxy.example.com:3128"),
            ("HTTP://Proxy.Example.com", "http://proxy.example.com:80"),
            ("http://proxy.example.com./", "http://proxy.example.com:80"),
            ("http://10.1.2.3:8080", "http://10.1.2.3:8080"),
            ("http://[2001:db8::1]:8080", "http://[2001:db8::1]:8080"),
            ("http://[::1]", "http://[::1]:80"),
            ("  http://p:65535  ", "http://p:65535"),
        ];
        for (input, want) in cases {
            assert_eq!(ok(input, false).endpoint.to_string(), want, "{input}");
        }
    }

    #[test]
    fn rejected_endpoints() {
        let cases = [
            ("", UrlProblem::Empty),
            ("   ", UrlProblem::Empty),
            ("proxy:3128", UrlProblem::MissingScheme),
            ("https://p:1", UrlProblem::UnsupportedScheme),
            ("socks5://p:1", UrlProblem::UnsupportedScheme),
            ("http://u:p@host:1", UrlProblem::UserinfoNotAllowed),
            ("http://@host", UrlProblem::UserinfoNotAllowed),
            ("http://host/path", UrlProblem::PathNotAllowed),
            ("http://host//", UrlProblem::PathNotAllowed),
            ("http://host/?a=1", UrlProblem::PathNotAllowed),
            ("http://host?a=1", UrlProblem::QueryNotAllowed),
            ("http://host#f", UrlProblem::FragmentNotAllowed),
            ("http://:80", UrlProblem::EmptyHost),
            ("http://", UrlProblem::EmptyHost),
            ("http://host:", UrlProblem::InvalidPort),
            ("http://host:0", UrlProblem::InvalidPort),
            ("http://host:65536", UrlProblem::InvalidPort),
            ("http://host:+80", UrlProblem::InvalidPort),
            ("http://host:80a", UrlProblem::InvalidPort),
            ("http://[::1", UrlProblem::InvalidHost),
            ("http://[::1]x", UrlProblem::InvalidHost),
            ("http://::1:80", UrlProblem::InvalidHost),
            ("http://1.2.3:80", UrlProblem::InvalidHost),
            ("http://ho st", UrlProblem::InvalidHost),
            ("http://host\r\nX: y", UrlProblem::ControlCharacter),
            ("http://ho\0st", UrlProblem::ControlCharacter),
        ];
        for (input, want) in cases {
            assert_eq!(err(input, false), want, "{input:?}");
        }
    }

    #[test]
    fn environment_userinfo_is_decoded_once() {
        let p = ok("http://us%40er:p%2540ss@proxy:3128", true);
        assert_eq!(
            p.userinfo,
            Some(("us@er".to_string(), "p%40ss".to_string()))
        );
        assert_eq!(p.endpoint.to_string(), "http://proxy:3128");

        let p = ok("http://user@proxy", true);
        assert_eq!(p.userinfo, Some(("user".to_string(), String::new())));

        // Last '@' separates userinfo from the host.
        let p = ok("http://a@b:c@proxy:1", true);
        assert_eq!(p.userinfo, Some(("a@b".to_string(), "c".to_string())));
    }

    #[test]
    fn environment_userinfo_rejections() {
        let cases = [
            ("http://u%0D:p@h", UrlProblem::ControlCharacter),
            ("http://u:p%0A@h", UrlProblem::ControlCharacter),
            ("http://u:p%00@h", UrlProblem::ControlCharacter),
            ("http://u:%zz@h", UrlProblem::InvalidUserinfo),
            ("http://u:%4@h", UrlProblem::InvalidUserinfo),
            ("http://u:%ff@h", UrlProblem::InvalidUserinfo),
            ("http://:p@h", UrlProblem::InvalidUserinfo),
        ];
        for (input, want) in cases {
            assert_eq!(err(input, true), want, "{input:?}");
        }
    }

    #[test]
    fn percent_round_trip() {
        for s in ["plain", "a b", "p@ss:w/rd%", "日本語"] {
            assert_eq!(percent_decode_once(&percent_encode(s)).as_deref(), Some(s));
        }
    }
}
