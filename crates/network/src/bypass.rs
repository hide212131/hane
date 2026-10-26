use std::net::IpAddr;
use std::net::Ipv6Addr;
use std::str::FromStr;

use crate::error::BypassProblem;
use crate::host::{Host, normalize_dns};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    /// Matches the name itself and dot-bounded subdomains.
    Domain(String),
    /// Exact, normalized IP match.
    Ip(IpAddr),
}

/// A validated proxy bypass (no_proxy) list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BypassList {
    all: bool,
    entries: Vec<Entry>,
}

impl BypassList {
    /// Parse a comma separated list. Surrounding whitespace and empty
    /// elements are dropped. Any invalid element rejects the whole list.
    pub fn parse(text: &str) -> Result<Self, BypassProblem> {
        let mut list = BypassList::default();
        for raw in text.split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if item == "*" {
                list.all = true;
                continue;
            }
            let entry = parse_entry(item)?;
            if !list.entries.contains(&entry) {
                list.entries.push(entry);
            }
        }
        Ok(list)
    }

    pub fn is_empty(&self) -> bool {
        !self.all && self.entries.is_empty()
    }

    /// Whether the destination host is excluded from proxying. Pure string /
    /// address comparison; DNS results are never consulted.
    pub fn matches(&self, host: &Host) -> bool {
        if self.all {
            return true;
        }
        self.entries.iter().any(|e| match (e, host) {
            (Entry::Domain(d), Host::Dns(h)) => {
                h == d
                    || (h.len() > d.len()
                        && h.ends_with(d.as_str())
                        && h.as_bytes()[h.len() - d.len() - 1] == b'.')
            }
            (Entry::Ip(IpAddr::V4(a)), Host::Ipv4(b)) => a == b,
            (Entry::Ip(IpAddr::V6(a)), Host::Ipv6(b)) => a == b,
            _ => false,
        })
    }

    /// Canonical comma separated text. It parses back to an equal list, so
    /// it is what settings persistence and the settings screen use.
    pub fn to_text(&self) -> String {
        self.to_env_value()
    }

    /// Canonical comma separated form for child process environments.
    pub(crate) fn to_env_value(&self) -> String {
        if self.all {
            return "*".to_string();
        }
        self.entries
            .iter()
            .map(|e| match e {
                Entry::Domain(d) => d.clone(),
                Entry::Ip(ip) => ip.to_string(),
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn parse_entry(item: &str) -> Result<Entry, BypassProblem> {
    if item.contains('*') {
        return Err(BypassProblem::Wildcard);
    }
    if item.contains("://") || item.contains(['@', '?', '#']) {
        return Err(BypassProblem::UrlForm);
    }
    if item.contains(char::is_whitespace) {
        return Err(BypassProblem::Whitespace);
    }
    if item.contains('/') {
        return Err(BypassProblem::Cidr);
    }
    if let Some(inner) = item.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or(BypassProblem::PortNotAllowed)?;
        return Ipv6Addr::from_str(inner)
            .map(|ip| Entry::Ip(IpAddr::V6(ip)))
            .map_err(|_| BypassProblem::InvalidAddress);
    }
    if item.contains(':') {
        return match Ipv6Addr::from_str(item) {
            Ok(ip) => Ok(Entry::Ip(IpAddr::V6(ip))),
            Err(_) if item.matches(':').count() == 1 => Err(BypassProblem::PortNotAllowed),
            Err(_) => Err(BypassProblem::InvalidAddress),
        };
    }
    if item.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return match Host::parse(item, false) {
            Some(Host::Ipv4(ip)) => Ok(Entry::Ip(IpAddr::V4(ip))),
            _ => Err(BypassProblem::InvalidAddress),
        };
    }
    let name = item.strip_prefix('.').unwrap_or(item);
    normalize_dns(name)
        .map(Entry::Domain)
        .ok_or(BypassProblem::InvalidHost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(text: &str) -> Host {
        Host::parse(text, true).unwrap()
    }

    #[test]
    fn matching_table() {
        let cases: &[(&str, &str, bool)] = &[
            ("example.com", "example.com", true),
            ("example.com", "EXAMPLE.com.", true),
            ("example.com", "www.example.com", true),
            ("example.com", "badexample.com", false),
            ("example.com", "example.com.evil", false),
            ("example.com", "com", false),
            (".example.com", "example.com", true),
            (".example.com", "a.b.example.com", true),
            (".example.com", "badexample.com", false),
            ("example.com.", "a.example.com", true),
            ("10.0.0.1", "10.0.0.1", true),
            ("10.0.0.1", "10.0.0.10", false),
            ("10.0.0.1", "10.0.0.2", false),
            ("::1", "[::1]", true),
            ("[2001:db8::1]", "[2001:0db8:0:0:0:0:0:1]", true),
            ("2001:db8::1", "[2001:db8::2]", false),
            ("10.0.0.1", "ten.example", false),
            ("example.com", "10.0.0.1", false),
            ("*", "anything.example", true),
            ("*", "[2001:db8::1]", true),
            (" a.com , , b.com ", "x.b.com", true),
            (" a.com , , b.com ", "c.com", false),
            ("", "a.com", false),
            (",,", "a.com", false),
        ];
        for (list, dest, want) in cases {
            let l = BypassList::parse(list).unwrap_or_else(|e| panic!("{list:?}: {e:?}"));
            assert_eq!(l.matches(&host(dest)), *want, "{list:?} vs {dest}");
        }
    }

    #[test]
    fn invalid_lists_are_rejected_entirely() {
        let cases = [
            ("*.example.com", BypassProblem::Wildcard),
            ("exa*mple.com", BypassProblem::Wildcard),
            ("a.com,*.b.com", BypassProblem::Wildcard),
            ("10.0.0.0/8", BypassProblem::Cidr),
            ("::1/128", BypassProblem::Cidr),
            ("http://example.com", BypassProblem::UrlForm),
            ("user@example.com", BypassProblem::UrlForm),
            ("example.com:8080", BypassProblem::PortNotAllowed),
            ("[::1]:80", BypassProblem::PortNotAllowed),
            ("exa mple.com", BypassProblem::Whitespace),
            ("a_b!.com", BypassProblem::InvalidHost),
            ("a..com", BypassProblem::InvalidHost),
            ("-a.com", BypassProblem::InvalidHost),
            ("1.2.3", BypassProblem::InvalidAddress),
            ("999.1.1.1", BypassProblem::InvalidAddress),
            ("[zzz]", BypassProblem::InvalidAddress),
            ("1:2:3", BypassProblem::InvalidAddress),
        ];
        for (list, want) in cases {
            assert_eq!(BypassList::parse(list), Err(want), "{list:?}");
        }
    }

    #[test]
    fn env_value_is_canonical() {
        let l = BypassList::parse(" .Example.COM., 10.0.0.1,[::1],example.com").unwrap();
        // "example.com" is deduplicated against ".Example.COM.".
        assert_eq!(l.to_env_value(), "example.com,10.0.0.1,::1");
        assert_eq!(BypassList::parse("a.com,*").unwrap().to_env_value(), "*");
        assert!(BypassList::parse("").unwrap().is_empty());
    }
}
