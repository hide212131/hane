//! Blocking HTTPS GET behind the update crate's [`Transport`] contract.
//!
//! This is the only network code of the application update. It is constructed
//! and used solely from the update service's background thread after an
//! explicit user action. It verifies the server certificate against the
//! operating system's trusted roots (rustls, `ring` provider), never follows
//! redirects (the caller validates each hop), never negotiates compression, and
//! bounds every connect, handshake, head read and body read by the deadlines in
//! the [`Request`]. DNS resolution uses the system resolver, which `std` cannot
//! interrupt; it runs on a single helper thread that the request stops waiting
//! for at the deadline, and no second lookup starts while it is still running.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use hane_update::download::{
    BodyReader, Request, Response, Transport, TransportError, TransportFailure,
};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_LINE_BYTES: usize = 1024;

pub struct HttpsTransport {
    config: Arc<ClientConfig>,
    dns_in_flight: Arc<AtomicBool>,
}

impl HttpsTransport {
    /// Builds the TLS configuration from the system trust store. Fails when no
    /// root certificate could be loaded, so a broken store never turns into an
    /// unverified connection.
    pub fn new() -> Result<Self, String> {
        let mut roots = RootCertStore::empty();
        let loaded = rustls_native_certs::load_native_certs();
        for certificate in loaded.certs {
            let _ = roots.add(certificate);
        }
        if roots.is_empty() {
            return Err("no trusted root certificates are available".to_owned());
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
            dns_in_flight: Arc::new(AtomicBool::new(false)),
        })
    }
}

/// Builds the TLS configuration (reading the system trust store) on the first
/// request instead of at startup, so launching the app does no update work.
#[derive(Default)]
pub struct LazyHttpsTransport {
    inner: std::sync::OnceLock<Result<HttpsTransport, String>>,
}

impl Transport for LazyHttpsTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        match self.inner.get_or_init(HttpsTransport::new) {
            Ok(transport) => transport.get(request),
            Err(detail) => Err(failure(TransportFailure::Tls, detail.clone())),
        }
    }
}

fn failure(kind: TransportFailure, detail: impl Into<String>) -> TransportError {
    TransportError {
        kind,
        detail: detail.into(),
    }
}

/// Human text plus the structured `ErrorKind` and, when present, the raw OS
/// error number.
fn io_detail(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("{error} [io_kind={:?} os_error={code}]", error.kind()),
        None => format!("{error} [io_kind={:?}]", error.kind()),
    }
}

fn map_io(error: &io::Error) -> TransportError {
    let detail = io_detail(error);
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        return failure(TransportFailure::Timeout, detail);
    }
    if error
        .get_ref()
        .is_some_and(|inner| inner.is::<rustls::Error>())
    {
        return failure(TransportFailure::Tls, detail);
    }
    match error.kind() {
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => failure(TransportFailure::Disconnected, detail),
        _ => failure(TransportFailure::Other, detail),
    }
}

/// Splits `https://host/path?query` into `(host, path_and_query)`. Anything
/// with userinfo, an explicit port or a control character is refused.
pub(crate) fn split_https_url(url: &str) -> Result<(&str, &str), TransportError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| failure(TransportFailure::Other, "only https is supported"))?;
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if host.is_empty()
        || host.contains('@')
        || host.contains(':')
        || host.chars().any(|c| c.is_ascii_control() || c == ' ')
        || path.chars().any(|c| c.is_ascii_control() || c == ' ')
    {
        return Err(failure(TransportFailure::Other, "unsupported URL form"));
    }
    Ok((host, path))
}

/// Looks the host up with the system resolver (blocking, not interruptible).
fn system_lookup(host: &str) -> io::Result<Vec<SocketAddr>> {
    (host, 443u16).to_socket_addrs().map(Iterator::collect)
}

/// Clears the in-flight flag when the resolver thread ends, even on panic.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Runs `lookup` on a helper thread and waits at most `budget` for it. `std`
/// cannot cancel a system resolver call, so at most one lookup thread exists
/// per `in_flight` flag: while a timed-out lookup is still running, further
/// lookups fail with a timeout instead of piling up more threads.
fn resolve_within<F>(
    in_flight: &Arc<AtomicBool>,
    host: &str,
    budget: Duration,
    lookup: F,
) -> Result<Vec<SocketAddr>, TransportError>
where
    F: FnOnce(&str) -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(failure(
            TransportFailure::Timeout,
            "a previous DNS lookup is still running",
        ));
    }
    let guard = InFlightGuard(Arc::clone(in_flight));
    let (sender, receiver) = mpsc::channel();
    let host = host.to_owned();
    let spawned = std::thread::Builder::new()
        .name("hane-dns".to_owned())
        .spawn(move || {
            let result = lookup(&host);
            drop(guard);
            let _ = sender.send(result);
        });
    if let Err(error) = spawned {
        // The closure (and its guard) was dropped, which cleared the flag.
        return Err(failure(TransportFailure::Other, error.to_string()));
    }
    match receiver.recv_timeout(budget) {
        Ok(Ok(addresses)) => Ok(addresses),
        Ok(Err(error)) => Err(failure(TransportFailure::Disconnected, error.to_string())),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(failure(
            TransportFailure::Timeout,
            "DNS resolution exceeded the request deadline",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(failure(TransportFailure::Other, "DNS resolver thread ended"))
        }
    }
}

/// A TCP stream whose every read and write is bounded by the time left until
/// `limit` (and by `step` per call), so multi-step operations such as the TLS
/// handshake or a chunked body line sequence cannot outlast one overall budget.
struct DeadlineSocket {
    sock: TcpStream,
    limit: Instant,
    step: Duration,
}

impl DeadlineSocket {
    fn new(sock: TcpStream, limit: Instant, step: Duration) -> Self {
        Self { sock, limit, step }
    }

    /// Starts a new overall budget of `ms` milliseconds from now.
    fn rearm(&mut self, ms: u64) {
        let budget = Duration::from_millis(ms.max(1));
        self.limit = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(60 * 60 * 24 * 365));
        self.step = budget;
    }

    fn budget(&self) -> io::Result<Duration> {
        match self.limit.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => Ok(left.min(self.step)),
            _ => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "operation deadline reached",
            )),
        }
    }
}

impl Read for DeadlineSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let budget = self.budget()?;
        self.sock.set_read_timeout(Some(budget))?;
        self.sock.read(buf)
    }
}

impl Write for DeadlineSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let budget = self.budget()?;
        self.sock.set_write_timeout(Some(budget))?;
        self.sock.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sock.flush()
    }
}

struct TlsStream {
    inner: StreamOwned<ClientConnection, DeadlineSocket>,
}

/// A readable response stream whose overall read budget can be reset.
pub(crate) trait BodyStream: Read + Send {
    /// Bounds everything read until the next call (including every internal
    /// read of one `read_within`) to `ms` milliseconds in total.
    fn set_read_timeout_ms(&mut self, ms: u64) -> io::Result<()>;
}

impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl BodyStream for TlsStream {
    fn set_read_timeout_ms(&mut self, ms: u64) -> io::Result<()> {
        self.inner.sock.rearm(ms);
        Ok(())
    }
}

fn read_line<S: Read>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed inside a header or chunk line",
            ));
        }
        if byte[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        line.push(byte[0]);
        if line.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header or chunk line is too long",
            ));
        }
    }
}

enum Framing {
    Length { remaining: u64 },
    Chunked { chunk_remaining: u64, finished: bool },
    UntilClose,
}

pub(crate) struct HttpBody<S: BodyStream> {
    stream: S,
    framing: Framing,
}

impl<S: BodyStream> HttpBody<S> {
    fn truncated() -> io::Error {
        io::Error::new(io::ErrorKind::UnexpectedEof, "response body ended early")
    }

    fn next_chunk_size(&mut self) -> io::Result<u64> {
        let line = read_line(&mut self.stream)?;
        let text = std::str::from_utf8(&line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        let digits = text.split(';').next().unwrap_or("").trim();
        if digits.is_empty() || digits.len() > 16 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"));
        }
        u64::from_str_radix(digits, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))
    }

    fn expect_crlf(&mut self) -> io::Result<()> {
        if read_line(&mut self.stream)?.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing chunk terminator",
            ))
        }
    }
}

impl<S: BodyStream> BodyReader for HttpBody<S> {
    fn read_within(&mut self, buf: &mut [u8], timeout_ms: u64) -> io::Result<usize> {
        self.stream.set_read_timeout_ms(timeout_ms)?;
        if buf.is_empty() {
            return Ok(0);
        }
        match self.framing {
            Framing::Length { remaining } => {
                if remaining == 0 {
                    return Ok(0);
                }
                let want = buf.len().min(usize::try_from(remaining).unwrap_or(usize::MAX));
                let read = self.stream.read(&mut buf[..want])?;
                if read == 0 {
                    return Err(Self::truncated());
                }
                self.framing = Framing::Length {
                    remaining: remaining - read as u64,
                };
                Ok(read)
            }
            Framing::Chunked {
                mut chunk_remaining,
                finished,
            } => {
                if finished {
                    return Ok(0);
                }
                if chunk_remaining == 0 {
                    let size = self.next_chunk_size()?;
                    if size == 0 {
                        // Trailers (if any) end at an empty line.
                        while !read_line(&mut self.stream)?.is_empty() {}
                        self.framing = Framing::Chunked {
                            chunk_remaining: 0,
                            finished: true,
                        };
                        return Ok(0);
                    }
                    chunk_remaining = size;
                }
                let want = buf
                    .len()
                    .min(usize::try_from(chunk_remaining).unwrap_or(usize::MAX));
                let read = self.stream.read(&mut buf[..want])?;
                if read == 0 {
                    return Err(Self::truncated());
                }
                chunk_remaining -= read as u64;
                if chunk_remaining == 0 {
                    self.expect_crlf()?;
                }
                self.framing = Framing::Chunked {
                    chunk_remaining,
                    finished: false,
                };
                Ok(read)
            }
            Framing::UntilClose => self.stream.read(buf),
        }
    }
}

struct Head {
    status: u16,
    location: Option<String>,
    content_length: Option<u64>,
    chunked: bool,
}

fn parse_head(raw: &[u8]) -> Result<Head, TransportError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| failure(TransportFailure::Other, "response head is not UTF-8"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.split(' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(failure(TransportFailure::Other, "not an HTTP/1.x response"));
    }
    let status = parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| failure(TransportFailure::Other, "bad status line"))?;
    let mut head = Head {
        status,
        location: None,
        content_length: None,
        chunked: false,
    };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "location" => head.location = Some(value.to_owned()),
            "content-length" => {
                let parsed = value
                    .parse::<u64>()
                    .map_err(|_| failure(TransportFailure::Other, "bad Content-Length"))?;
                if head.content_length.is_some_and(|existing| existing != parsed) {
                    return Err(failure(
                        TransportFailure::Other,
                        "conflicting Content-Length headers",
                    ));
                }
                head.content_length = Some(parsed);
            }
            "transfer-encoding" => {
                if value.eq_ignore_ascii_case("chunked") {
                    head.chunked = true;
                } else if !value.eq_ignore_ascii_case("identity") {
                    return Err(failure(
                        TransportFailure::Other,
                        "unsupported Transfer-Encoding",
                    ));
                }
            }
            "content-encoding" if !value.eq_ignore_ascii_case("identity") => {
                return Err(failure(
                    TransportFailure::Other,
                    "compressed responses are not accepted",
                ));
            }
            _ => {}
        }
    }
    Ok(head)
}

fn remaining_ms(started: Instant, deadline: Duration) -> Result<u64, TransportError> {
    match deadline.checked_sub(started.elapsed()) {
        Some(left) if !left.is_zero() => {
            Ok(u64::try_from(left.as_millis()).unwrap_or(u64::MAX).max(1))
        }
        _ => Err(failure(TransportFailure::Timeout, "request deadline reached")),
    }
}

impl Transport for HttpsTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        let started = Instant::now();
        let deadline = Duration::from_millis(request.deadline_ms);
        let (host, path) = split_https_url(&request.url)?;

        let addresses = resolve_within(
            &self.dns_in_flight,
            host,
            Duration::from_millis(remaining_ms(started, deadline)?),
            system_lookup,
        )?;
        let mut socket = None;
        let mut last_error = failure(TransportFailure::Disconnected, "no address resolved");
        for address in addresses {
            let budget = remaining_ms(started, deadline)?.min(request.connect_timeout_ms.max(1));
            match TcpStream::connect_timeout(&address, Duration::from_millis(budget)) {
                Ok(stream) => {
                    socket = Some(stream);
                    break;
                }
                Err(error) => last_error = map_io(&error),
            }
        }
        let socket = socket.ok_or(last_error)?;

        // Every read and write below (TLS handshake, request write, response
        // head) goes through this socket, which re-applies the time left until
        // the operation deadline before each I/O call.
        let operation_end = started
            .checked_add(deadline)
            .unwrap_or_else(|| started + Duration::from_secs(60 * 60 * 24 * 365));
        let socket = DeadlineSocket::new(
            socket,
            operation_end,
            Duration::from_millis(request.read_timeout_ms.max(1)),
        );

        let server_name = ServerName::try_from(host.to_owned())
            .map_err(|_| failure(TransportFailure::Tls, "invalid server name"))?;
        let connection = ClientConnection::new(Arc::clone(&self.config), server_name)
            .map_err(|error| failure(TransportFailure::Tls, error.to_string()))?;
        let mut tls = TlsStream {
            inner: StreamOwned::new(connection, socket),
        };

        let mut head_text = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {}\r\nAccept: {}\r\n",
            request.user_agent, request.accept
        );
        if let Some(version) = request.api_version {
            head_text.push_str(&format!("X-GitHub-Api-Version: {version}\r\n"));
        }
        head_text.push_str("Accept-Encoding: identity\r\nConnection: close\r\n\r\n");
        tls.inner
            .write_all(head_text.as_bytes())
            .and_then(|()| tls.inner.flush())
            .map_err(|error| map_io(&error))?;

        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let read = tls.read(&mut byte).map_err(|error| map_io(&error))?;
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
            body: Box::new(HttpBody {
                stream: tls,
                framing,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn map_io_keeps_class_kind_and_os_error() {
        let cases = [
            (io::ErrorKind::TimedOut, TransportFailure::Timeout),
            (io::ErrorKind::ConnectionReset, TransportFailure::Disconnected),
            (io::ErrorKind::PermissionDenied, TransportFailure::Other),
        ];
        for (kind, expected) in cases {
            let mapped = map_io(&io::Error::new(kind, "synthetic"));
            assert_eq!(mapped.kind, expected);
            assert!(mapped.detail.contains("synthetic"), "{}", mapped.detail);
            assert!(
                mapped.detail.contains(&format!("io_kind={kind:?}")),
                "{}",
                mapped.detail
            );
            assert!(!mapped.detail.contains("os_error"), "{}", mapped.detail);
        }
        let tls = map_io(&io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::HandshakeNotComplete,
        ));
        assert_eq!(tls.kind, TransportFailure::Tls);
        assert!(tls.detail.contains("io_kind=InvalidData"), "{}", tls.detail);
    }

    #[test]
    fn map_io_reports_raw_os_error_number() {
        let mapped = map_io(&io::Error::from_raw_os_error(4242));
        assert_eq!(mapped.kind, TransportFailure::Other);
        assert!(mapped.detail.contains("os_error=4242"), "{}", mapped.detail);
        assert!(mapped.detail.contains("io_kind="), "{}", mapped.detail);
    }

    struct Fake(Cursor<Vec<u8>>);

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl BodyStream for Fake {
        fn set_read_timeout_ms(&mut self, _: u64) -> io::Result<()> {
            Ok(())
        }
    }

    fn body(data: &[u8], framing: Framing) -> HttpBody<Fake> {
        HttpBody {
            stream: Fake(Cursor::new(data.to_vec())),
            framing,
        }
    }

    fn drain(body: &mut HttpBody<Fake>) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4];
        loop {
            let read = body.read_within(&mut buf, 1000)?;
            if read == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..read]);
        }
    }

    #[test]
    fn urls_with_userinfo_ports_or_other_schemes_are_refused() {
        assert_eq!(
            split_https_url("https://api.github.com/repos/x?y=1").unwrap(),
            ("api.github.com", "/repos/x?y=1")
        );
        for url in [
            "http://api.github.com/",
            "https://user@api.github.com/",
            "https://api.github.com:444/",
            "https:///path",
            "https://api.github.com/a b",
        ] {
            assert!(split_https_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn heads_keep_status_location_and_framing_and_refuse_compression() {
        let head = parse_head(
            b"HTTP/1.1 302 Found\r\nLocation: https://objects.githubusercontent.com/x\r\nContent-Length: 0",
        )
        .unwrap();
        assert_eq!(head.status, 302);
        assert_eq!(
            head.location.as_deref(),
            Some("https://objects.githubusercontent.com/x")
        );
        assert_eq!(head.content_length, Some(0));
        assert!(
            parse_head(b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip").is_err(),
            "a compressed body must never reach the hash check"
        );
        assert!(parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2").is_err());
        assert!(parse_head(b"garbage").is_err());
        assert!(
            parse_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked")
                .unwrap()
                .chunked
        );
    }

    #[test]
    fn a_length_body_stops_at_the_declared_length_and_flags_truncation() {
        let mut complete = body(b"abcdefgh-extra", Framing::Length { remaining: 8 });
        assert_eq!(drain(&mut complete).unwrap(), b"abcdefgh");
        let mut cut = body(b"abc", Framing::Length { remaining: 8 });
        let error = drain(&mut cut).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_chunked_body_is_decoded_and_its_trailer_consumed() {
        let wire = b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nX-Trailer: 1\r\n\r\n";
        let mut chunked = body(
            wire,
            Framing::Chunked {
                chunk_remaining: 0,
                finished: false,
            },
        );
        assert_eq!(drain(&mut chunked).unwrap(), b"hello world");
    }

    struct Plain(DeadlineSocket);

    impl Read for Plain {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl BodyStream for Plain {
        fn set_read_timeout_ms(&mut self, ms: u64) -> io::Result<()> {
            self.0.rearm(ms);
            Ok(())
        }
    }

    /// Connects to a loopback server that sends `drip` one byte at a time,
    /// `gap` apart, and then idles until the client goes away.
    fn dripping_client(drip: Vec<u8>, gap: Duration) -> (TcpStream, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            for byte in drip {
                std::thread::sleep(gap);
                if peer.write_all(&[byte]).is_err() {
                    return;
                }
            }
            let mut sink = [0u8; 1];
            let _ = peer.read(&mut sink);
        });
        (TcpStream::connect(address).unwrap(), server)
    }

    fn is_timeout(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        )
    }

    #[test]
    fn a_dns_lookup_past_the_deadline_times_out_and_never_stacks_threads() {
        let in_flight = Arc::new(AtomicBool::new(false));
        let (release, gate) = mpsc::channel::<()>();
        let started = Instant::now();
        let error = resolve_within(&in_flight, "example.test", Duration::from_millis(50), move |_| {
            let _ = gate.recv();
            Ok(Vec::new())
        })
        .unwrap_err();
        assert_eq!(error.kind, TransportFailure::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5));

        // The first lookup is still blocked: a retry must not start another.
        let error = resolve_within(&in_flight, "example.test", Duration::from_secs(5), |_| {
            panic!("a second resolver thread must not start")
        })
        .unwrap_err();
        assert_eq!(error.kind, TransportFailure::Timeout);

        drop(release);
        let waiting = Instant::now();
        while in_flight.load(Ordering::Acquire) {
            assert!(waiting.elapsed() < Duration::from_secs(5), "flag never cleared");
            std::thread::sleep(Duration::from_millis(5));
        }
        let address: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let resolved = resolve_within(&in_flight, "example.test", Duration::from_secs(5), move |_| {
            Ok(vec![address])
        })
        .unwrap();
        assert_eq!(resolved, vec![address]);
    }

    #[test]
    fn a_silent_peer_hits_the_overall_deadline_across_many_reads() {
        // One byte every 40 ms keeps each single read well inside the per-call
        // step, yet the 250 ms overall limit must still end the loop.
        let (client, server) = dripping_client(vec![b'x'; 200], Duration::from_millis(40));
        let mut socket = DeadlineSocket::new(
            client,
            Instant::now() + Duration::from_millis(250),
            Duration::from_secs(30),
        );
        let started = Instant::now();
        let mut byte = [0u8; 1];
        let error = loop {
            if let Err(error) = socket.read(&mut byte) {
                break error;
            }
        };
        assert!(is_timeout(&error), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(socket);
        server.join().unwrap();
    }

    #[test]
    fn a_slow_drip_chunked_body_returns_within_the_read_timeout() {
        // The chunk-size line never completes in time: 60 extension bytes at
        // 40 ms each take far longer than the 200 ms given to read_within.
        let mut wire = b"1;".to_vec();
        wire.extend_from_slice(&[b'a'; 60]);
        wire.extend_from_slice(b"\r\nZ\r\n0\r\n\r\n");
        let (client, server) = dripping_client(wire, Duration::from_millis(40));
        let socket = DeadlineSocket::new(
            client,
            Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let mut chunked = HttpBody {
            stream: Plain(socket),
            framing: Framing::Chunked {
                chunk_remaining: 0,
                finished: false,
            },
        };
        let started = Instant::now();
        let mut buf = [0u8; 8];
        let error = chunked.read_within(&mut buf, 200).unwrap_err();
        assert!(is_timeout(&error), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(chunked);
        server.join().unwrap();
    }

    #[test]
    fn a_chunked_body_cut_short_is_an_error_not_a_clean_end() {
        let mut chunked = body(
            b"5\r\nhel",
            Framing::Chunked {
                chunk_remaining: 0,
                finished: false,
            },
        );
        assert!(drain(&mut chunked).is_err());
    }
}
