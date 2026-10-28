//! Opt-in transport of the isolated update validation build (Issue #510).
//!
//! Compiled only with the `validation-feed` Cargo feature, which no standard
//! `debug` / `release` build enables. The standard build therefore keeps the
//! fixed GitHub endpoints, the HTTPS-only redirect policy and the production
//! trust policy, and has no code that reads an endpoint from the environment
//! or from state files.
//!
//! The update crate still builds the GitHub Release URLs, validates every
//! redirect hop, verifies the signed catalog, inspects the archive and applies
//! it. This transport only answers those unchanged requests from a fixture
//! server on a loopback ephemeral port:
//!
//! - the feed address must be `http://127.0.0.1:<port>` or `http://[::1]:<port>`
//!   with a port of 1024 or above; userinfo, names, paths and other hosts are
//!   rejected, and plain HTTP is used for this loopback peer only;
//! - the connection is made directly to that address, never through a proxy
//!   and never through name resolution;
//! - only the GitHub API / download hosts the update crate builds are mapped to
//!   the fixture. Any other host, including every redirect target host, is
//!   refused, so a redirect can never leave the loopback fixture;
//! - a missing or invalid address fails closed; it never falls back to GitHub.
//!
//! The matching throw-away public key is embedded through the existing
//! `HANE_UPDATE_KEY_ID` / `HANE_UPDATE_PUBLIC_KEY_BASE64` build values. The
//! private key stays with the fixture builder and is never part of the source.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use hane_update::download::{Request, Response, Transport, TransportError, TransportFailure};

use super::{
    BodyStream, DeadlineSocket, Framing, HttpBody, MAX_HEAD_BYTES, failure, map_io, parse_head,
    remaining_ms,
};

/// Environment variable of the validation build only.
pub const FEED_ENV_VAR: &str = "HANE_UPDATE_VALIDATION_FEED";

const API_PREFIX: &str = "https://api.github.com/";
const SITE_PREFIX: &str = "https://github.com/";
const MIN_EPHEMERAL_PORT: u16 = 1024;

/// Accepts only `http://127.0.0.1:<port>` and `http://[::1]:<port>`.
pub(crate) fn parse_feed(text: &str) -> Result<SocketAddr, String> {
    let rest = text
        .strip_prefix("http://")
        .ok_or_else(|| "validation feed must use http:// on a loopback address".to_owned())?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| "validation feed must be a loopback IP literal with a port".to_owned())?;
    let loopback = match address.ip() {
        IpAddr::V4(ip) => ip == Ipv4Addr::LOCALHOST,
        IpAddr::V6(ip) => ip == Ipv6Addr::LOCALHOST,
    };
    if !loopback {
        return Err("validation feed must be 127.0.0.1 or [::1]".to_owned());
    }
    if address.port() < MIN_EPHEMERAL_PORT {
        return Err("validation feed must use an ephemeral port".to_owned());
    }
    Ok(address)
}

/// The path the fixture serves for a request URL of the update crate, or a
/// refusal for every host that is not the GitHub API / download host.
fn feed_path(url: &str) -> Result<String, TransportError> {
    for prefix in [API_PREFIX, SITE_PREFIX] {
        if let Some(rest) = url.strip_prefix(prefix) {
            if rest.contains(['@', '\\', '\r', '\n', ' ']) {
                break;
            }
            return Ok(format!("/{rest}"));
        }
    }
    Err(failure(
        TransportFailure::Other,
        "the validation feed serves only the GitHub Release URLs of the update crate",
    ))
}

pub struct ValidationFeedTransport {
    feed: Result<SocketAddr, String>,
}

impl ValidationFeedTransport {
    pub fn from_environment() -> Self {
        let feed = match std::env::var(FEED_ENV_VAR) {
            Ok(text) => parse_feed(&text),
            Err(_) => Err(format!("{FEED_ENV_VAR} is not set")),
        };
        Self { feed }
    }

    #[cfg(test)]
    fn new(feed: SocketAddr) -> Self {
        Self { feed: Ok(feed) }
    }
}

struct PlainStream(DeadlineSocket);

impl Read for PlainStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl BodyStream for PlainStream {
    fn set_read_timeout_ms(&mut self, ms: u64) -> io::Result<()> {
        self.0.rearm(ms);
        Ok(())
    }
}

impl Transport for ValidationFeedTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        let feed = self
            .feed
            .as_ref()
            .map_err(|detail| failure(TransportFailure::Other, detail.clone()))?;
        let path = feed_path(&request.url)?;
        let started = Instant::now();
        let deadline = Duration::from_millis(request.deadline_ms);
        let budget = remaining_ms(started, deadline)?.min(request.connect_timeout_ms.max(1));
        let tcp = TcpStream::connect_timeout(feed, Duration::from_millis(budget))
            .map_err(|error| map_io(&error))?;
        let limit = started
            .checked_add(deadline)
            .unwrap_or_else(|| started + Duration::from_secs(60 * 60 * 24));
        let mut stream = PlainStream(DeadlineSocket::new(
            tcp,
            limit,
            Duration::from_millis(request.read_timeout_ms.max(1)),
        ));

        let mut head_text = format!(
            "GET {path} HTTP/1.1\r\nHost: {feed}\r\nUser-Agent: {}\r\nAccept: {}\r\n",
            request.user_agent, request.accept
        );
        if let Some(version) = request.api_version {
            head_text.push_str(&format!("X-GitHub-Api-Version: {version}\r\n"));
        }
        head_text.push_str("Accept-Encoding: identity\r\nConnection: close\r\n\r\n");
        stream
            .0
            .write_all(head_text.as_bytes())
            .and_then(|()| stream.0.flush())
            .map_err(|error| map_io(&error))?;

        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let read = stream.read(&mut byte).map_err(|error| map_io(&error))?;
            if read == 0 {
                return Err(failure(
                    TransportFailure::Disconnected,
                    "connection closed before the response head ended",
                ));
            }
            raw.push(byte[0]);
            if raw.ends_with(b"\r\n\r\n") {
                raw.truncate(raw.len() - 4);
                break;
            }
            if raw.len() > MAX_HEAD_BYTES {
                return Err(failure(TransportFailure::Other, "response head is too large"));
            }
        }
        let head = parse_head(&raw)?;
        let framing = if head.chunked {
            Framing::Chunked {
                chunk_remaining: 0,
                finished: false,
            }
        } else if let Some(length) = head.content_length {
            Framing::Length { remaining: length }
        } else {
            Framing::UntilClose
        };
        Ok(Response {
            status: head.status,
            location: head.location,
            content_length: if head.chunked { None } else { head.content_length },
            body: Box::new(HttpBody { stream, framing }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_update::download::BodyReader;
    use std::net::TcpListener;

    fn request(url: &str) -> Request {
        Request {
            url: url.to_owned(),
            accept: "application/octet-stream",
            user_agent: "hane-update/test",
            api_version: None,
            connect_timeout_ms: 2_000,
            read_timeout_ms: 2_000,
            deadline_ms: 5_000,
        }
    }

    #[test]
    fn only_loopback_ephemeral_ip_literals_are_accepted() {
        assert!(parse_feed("http://127.0.0.1:49152").is_ok());
        assert!(parse_feed("http://127.0.0.1:49152/").is_ok());
        assert!(parse_feed("http://[::1]:49152").is_ok());
        for rejected in [
            "https://127.0.0.1:49152",
            "http://localhost:49152",
            "http://127.0.0.2:49152",
            "http://192.168.0.1:49152",
            "http://example.com:49152",
            "http://user@127.0.0.1:49152",
            "http://user:pw@127.0.0.1:49152",
            "http://127.0.0.1:49152@example.com",
            "http://127.0.0.1:49152/path",
            "http://127.0.0.1:80",
            "http://127.0.0.1",
            "127.0.0.1:49152",
            "",
        ] {
            assert!(parse_feed(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn only_github_release_urls_map_to_the_fixture() {
        assert_eq!(
            feed_path("https://api.github.com/repos/hide212131/hane/releases/latest").unwrap(),
            "/repos/hide212131/hane/releases/latest"
        );
        assert_eq!(
            feed_path("https://github.com/hide212131/hane/releases/download/v1.0.0/a.zip")
                .unwrap(),
            "/hide212131/hane/releases/download/v1.0.0/a.zip"
        );
        for rejected in [
            "https://release-assets.githubusercontent.com/a",
            "https://objects.githubusercontent.com/a",
            "https://example.com/a",
            "http://github.com/a",
            "https://github.com@example.com/a",
            "https://github.com/a@b",
        ] {
            assert!(feed_path(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn a_missing_or_invalid_feed_fails_closed() {
        let transport = ValidationFeedTransport {
            feed: Err("not configured".to_owned()),
        };
        let error = transport
            .get(&request("https://github.com/hide212131/hane/x"))
            .err()
            .unwrap();
        assert_eq!(error.kind, TransportFailure::Other);
    }

    #[test]
    fn a_request_is_answered_by_the_loopback_fixture_and_external_hosts_are_not_contacted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut byte = [0u8; 1];
            while !received.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                received.push(byte[0]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
                .unwrap();
            String::from_utf8(received).unwrap()
        });
        let transport = ValidationFeedTransport::new(address);

        assert!(
            transport
                .get(&request("https://release-assets.githubusercontent.com/a"))
                .is_err()
        );
        let mut response = transport
            .get(&request(
                "https://github.com/hide212131/hane/releases/download/v1.0.0/a",
            ))
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.content_length, Some(5));
        let mut buffer = [0u8; 16];
        let read = response.body.read_within(&mut buffer, 2_000).unwrap();
        assert_eq!(&buffer[..read], b"hello");

        let sent = server.join().unwrap();
        assert!(
            sent.starts_with("GET /hide212131/hane/releases/download/v1.0.0/a HTTP/1.1\r\n"),
            "{sent}"
        );
        assert!(sent.contains(&format!("Host: {address}\r\n")), "{sent}");
    }
}
