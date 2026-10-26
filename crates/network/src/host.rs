use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// A normalized host: lowercase DNS name without trailing dot, or an IP literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    Dns(String),
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
}

impl Host {
    /// Parse DNS / IPv4 / `[IPv6]`. A bare IPv6 literal is accepted only when
    /// `allow_bare_v6` is set. Returns `None` for anything else.
    pub(crate) fn parse(text: &str, allow_bare_v6: bool) -> Option<Host> {
        if let Some(inner) = text.strip_prefix('[') {
            let inner = inner.strip_suffix(']')?;
            return Ipv6Addr::from_str(inner).ok().map(Host::Ipv6);
        }
        if text.contains(':') {
            if !allow_bare_v6 {
                return None;
            }
            return Ipv6Addr::from_str(text).ok().map(Host::Ipv6);
        }
        if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return Ipv4Addr::from_str(text).ok().map(Host::Ipv4);
        }
        normalize_dns(text).map(Host::Dns)
    }

    /// `localhost`, `127.0.0.0/8` and `::1` literals. Subdomains of localhost
    /// are not loopback.
    pub fn is_loopback(&self) -> bool {
        match self {
            Host::Dns(d) => d == "localhost",
            Host::Ipv4(ip) => ip.octets()[0] == 127,
            Host::Ipv6(ip) => *ip == Ipv6Addr::LOCALHOST,
        }
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Dns(d) => f.write_str(d),
            Host::Ipv4(ip) => write!(f, "{ip}"),
            Host::Ipv6(ip) => write!(f, "[{ip}]"),
        }
    }
}

/// Lowercase and strip one trailing dot. Rejects empty labels, labels over 63
/// bytes, names over 253 bytes, non-ASCII, and characters other than
/// alphanumerics, `-` and `_`. Labels must not start or end with `-`.
pub(crate) fn normalize_dns(text: &str) -> Option<String> {
    let name = text.strip_suffix('.').unwrap_or(text);
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    for label in lower.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return None;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
    }
    Some(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table() {
        let cases: &[(&str, bool, Option<Host>)] = &[
            ("Example.COM", false, Some(Host::Dns("example.com".into()))),
            ("example.com.", false, Some(Host::Dns("example.com".into()))),
            ("10.0.0.1", false, Some(Host::Ipv4(Ipv4Addr::new(10, 0, 0, 1)))),
            ("[::1]", false, Some(Host::Ipv6(Ipv6Addr::LOCALHOST))),
            ("::1", false, None),
            ("::1", true, Some(Host::Ipv6(Ipv6Addr::LOCALHOST))),
            ("1.2.3", false, None),
            ("256.1.1.1", false, None),
            ("", false, None),
            (".", false, None),
            ("a..b", false, None),
            ("-a.com", false, None),
            ("a b.com", false, None),
            ("例え.jp", false, None),
            ("[::1", false, None),
            ("[1.2.3.4]", false, None),
            ("a:80", false, None),
        ];
        for (input, bare, want) in cases {
            assert_eq!(&Host::parse(input, *bare), want, "{input}");
        }
    }

    #[test]
    fn loopback_table() {
        let yes = ["localhost", "localhost.", "127.0.0.1", "127.255.0.9", "[::1]"];
        let no = ["a.localhost", "128.0.0.1", "[::2]", "example.com"];
        for h in yes {
            assert!(Host::parse(h, false).unwrap().is_loopback(), "{h}");
        }
        for h in no {
            assert!(!Host::parse(h, false).unwrap().is_loopback(), "{h}");
        }
    }
}
