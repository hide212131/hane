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
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use hane_network::{
    BasicCredentials, Host, NetworkContext, NetworkError, ResolvedRoute, Scheme,
};
use hane_update::download::{
    BodyReader, Request, Response, Transport, TransportError, TransportFailure,
};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_LINE_BYTES: usize = 1024;
const HTTPS_PORT: u16 = 443;
/// Interim (1xx) answers tolerated before the final CONNECT answer.
const MAX_INTERIM_RESPONSES: usize = 4;

/// Resolves `(host, port)` to socket addresses. Production uses the system
/// resolver; tests substitute a fixed table.
type Lookup = Arc<dyn Fn(&str, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

pub struct HttpsTransport {
    config: Arc<ClientConfig>,
    dns_in_flight: Arc<AtomicBool>,
    /// Immutable settings / proxy environment / credential snapshot. The route
    /// for every request (and so for every redirect hop) is derived from it.
    network: NetworkContext,
    lookup: Lookup,
    /// Port of a direct connection to the origin. Always 443 in production.
    direct_port: u16,
}

/// Builds the TLS configuration from the system trust store. Fails when no
/// root certificate could be loaded, so a broken store never turns into an
/// unverified connection.
fn native_tls_config() -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for certificate in loaded.certs {
        let _ = roots.add(certificate);
    }
    if roots.is_empty() {
        return Err("no trusted root certificates are available".to_owned());
    }
    tls_config(roots)
}

fn tls_config(roots: RootCertStore) -> Result<Arc<ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// What the first request caches: the TLS configuration and the one-resolver
/// flag. The route is deliberately not cached here; it is decided per request.
struct SharedParts {
    config: Arc<ClientConfig>,
    dns_in_flight: Arc<AtomicBool>,
}

/// Builds the TLS configuration (reading the system trust store) on the first
/// request instead of at startup, so launching the app does no update work.
pub struct LazyHttpsTransport {
    network: NetworkContext,
    shared: std::sync::OnceLock<Result<SharedParts, String>>,
}

impl LazyHttpsTransport {
    pub fn new(network: NetworkContext) -> Self {
        Self {
            network,
            shared: std::sync::OnceLock::new(),
        }
    }
}

impl Transport for LazyHttpsTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        let parts = self.shared.get_or_init(|| {
            native_tls_config().map(|config| SharedParts {
                config,
                dns_in_flight: Arc::new(AtomicBool::new(false)),
            })
        });
        match parts {
            Ok(parts) => HttpsTransport {
                config: Arc::clone(&parts.config),
                dns_in_flight: Arc::clone(&parts.dns_in_flight),
                network: self.network.clone(),
                lookup: Arc::new(system_lookup),
                direct_port: HTTPS_PORT,
            }
            .get(request),
            Err(detail) => Err(failure(TransportFailure::Tls, detail.clone())),
        }
    }
}

/// A resolved connection target: a name that needs the system resolver, or an
/// IP literal that does not.
enum Peer<'a> {
    Name(&'a str),
    Ip(IpAddr),
}

fn proxy_configuration(error: NetworkError) -> TransportError {
    // `NetworkError`'s text never contains an input value or a secret.
    failure(TransportFailure::ProxyConfiguration, error.to_string())
}

/// A failure while reaching the proxy itself. A timeout stays a timeout; any
/// other failure is a proxy connection failure (never retried directly).
fn proxy_peer_error(error: TransportError) -> TransportError {
    if error.kind == TransportFailure::Timeout {
        error
    } else {
        failure(TransportFailure::ProxyConnect, error.detail)
    }
}

/// Reads one byte of the CONNECT answer. Only one byte is ever requested, so
/// bytes that belong to the TLS tunnel are never consumed.
fn read_connect_byte<S: Read>(stream: &mut S) -> Result<u8, TransportError> {
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(failure(
                    TransportFailure::ProxyConnect,
                    "the proxy closed the connection before the CONNECT answer ended",
                ));
            }
            Ok(_) => return Ok(byte[0]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(failure(TransportFailure::Timeout, io_detail(&error)));
            }
            Err(error) => return Err(failure(TransportFailure::ProxyConnect, io_detail(&error))),
        }
    }
}

fn connect_malformed(why: &str) -> TransportError {
    failure(
        TransportFailure::ProxyConnect,
        format!("malformed CONNECT answer: {why}"),
    )
}

/// Reads one CRLF-terminated line. A bare LF or CR is refused, the line text
/// is limited to [`MAX_LINE_BYTES`], and every byte counts against the
/// [`MAX_HEAD_BYTES`] budget of the whole CONNECT answer.
fn read_connect_line<S: Read>(stream: &mut S, used: &mut usize) -> Result<Vec<u8>, TransportError> {
    let mut line = Vec::new();
    loop {
        let byte = read_connect_byte(stream)?;
        *used += 1;
        if *used > MAX_HEAD_BYTES {
            return Err(connect_malformed("answer is too large"));
        }
        match byte {
            b'\n' => return Err(connect_malformed("bare LF line ending")),
            b'\r' => {
                let next = read_connect_byte(stream)?;
                *used += 1;
                if next != b'\n' {
                    return Err(connect_malformed("bare CR in a line"));
                }
                if *used > MAX_HEAD_BYTES {
                    return Err(connect_malformed("answer is too large"));
                }
                return Ok(line);
            }
            other => {
                line.push(other);
                if line.len() > MAX_LINE_BYTES {
                    return Err(connect_malformed("line is too long"));
                }
            }
        }
    }
}

/// `HTTP/1.0` or `HTTP/1.1`, one space, a three-digit status, then nothing or
/// a space and a reason phrase.
fn parse_connect_status(line: &[u8]) -> Result<u16, TransportError> {
    let text = std::str::from_utf8(line).map_err(|_| connect_malformed("status line"))?;
    let rest = text
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| text.strip_prefix("HTTP/1.0 "))
        .ok_or_else(|| connect_malformed("status line"))?;
    let digits = rest.get(..3).ok_or_else(|| connect_malformed("status code"))?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(connect_malformed("status code"));
    }
    let after = &rest[3..];
    if !(after.is_empty() || after.starts_with(' ')) {
        return Err(connect_malformed("status code"));
    }
    let status: u16 = digits
        .parse()
        .map_err(|_| connect_malformed("status code"))?;
    if !(100..=599).contains(&status) {
        return Err(connect_malformed("status code"));
    }
    Ok(status)
}

/// Reads the header lines up to the empty line. Any framing that implies a
/// body (`Transfer-Encoding`, or a non-zero `Content-Length`) is refused: an
/// answer that opens a tunnel has none.
fn read_connect_headers<S: Read>(stream: &mut S, used: &mut usize) -> Result<(), TransportError> {
    loop {
        let line = read_connect_line(stream, used)?;
        if line.is_empty() {
            return Ok(());
        }
        let text = std::str::from_utf8(&line).map_err(|_| connect_malformed("header"))?;
        let Some((name, value)) = text.split_once(':') else {
            return Err(connect_malformed("header"));
        };
        if name.is_empty()
            || name
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            || text
                .bytes()
                .any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err(connect_malformed("header"));
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(connect_malformed("unexpected body framing"));
        }
        if name.eq_ignore_ascii_case("content-length") && value.trim() != "0" {
            return Err(connect_malformed("unexpected body framing"));
        }
    }
}

/// Reads the answer to a CONNECT and succeeds only for a final `200`.
/// Interim 1xx answers (at most four, never 101) are skipped. A failing final
/// status is classified from the status line alone; its body is not read.
fn read_connect_answer<S: Read>(stream: &mut S) -> Result<(), TransportError> {
    let mut used = 0usize;
    let mut interim = 0usize;
    loop {
        let line = read_connect_line(stream, &mut used)?;
        let status = parse_connect_status(&line)?;
        match status {
            101 => return Err(connect_malformed("protocol switch")),
            100..=199 => {
                interim += 1;
                if interim > MAX_INTERIM_RESPONSES {
                    return Err(connect_malformed("too many interim answers"));
                }
                read_connect_headers(stream, &mut used)?;
            }
            200 => return read_connect_headers(stream, &mut used),
            407 => {
                return Err(failure(
                    TransportFailure::ProxyAuthentication,
                    "the proxy requires authentication (407)",
                ));
            }
            other => {
                return Err(failure(
                    TransportFailure::ProxyConnect,
                    format!("the proxy refused the CONNECT ({other})"),
                ));
            }
        }
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(BASE64[(n >> 18) as usize & 63] as char);
        out.push(BASE64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The CONNECT request. The Basic credential, when present, appears only
/// here; the later origin GET never carries it.
fn connect_request(host: &str, credentials: Option<&BasicCredentials>) -> String {
    let mut text = format!("CONNECT {host}:{HTTPS_PORT} HTTP/1.1\r\nHost: {host}:{HTTPS_PORT}\r\n");
    if let Some(credentials) = credentials {
        let pair = format!(
            "{}:{}",
            credentials.username().expose(),
            credentials.password().expose()
        );
        text.push_str("Proxy-Authorization: Basic ");
        text.push_str(&base64_encode(pair.as_bytes()));
        text.push_str("\r\n");
    }
    text.push_str("\r\n");
    text
}

fn establish_tunnel(
    socket: &mut DeadlineSocket,
    host: &str,
    credentials: Option<&BasicCredentials>,
) -> Result<(), TransportError> {
    let request = connect_request(host, credentials);
    socket
        .write_all(request.as_bytes())
        .and_then(|()| socket.flush())
        .map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) {
                failure(TransportFailure::Timeout, io_detail(&error))
            } else {
                failure(TransportFailure::ProxyConnect, io_detail(&error))
            }
        })?;
    read_connect_answer(socket)
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
fn system_lookup(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    (host, port).to_socket_addrs().map(Iterator::collect)
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

impl HttpsTransport {
    /// Resolves `peer` (unless it is an IP literal) and connects, drawing
    /// every step from the one request deadline. The resolver is the shared
    /// single-flight one, so this limit holds for proxy hosts and ports too.
    fn connect(
        &self,
        peer: Peer<'_>,
        port: u16,
        request: &Request,
        started: Instant,
        deadline: Duration,
    ) -> Result<TcpStream, TransportError> {
        let addresses = match peer {
            Peer::Ip(ip) => vec![SocketAddr::new(ip, port)],
            Peer::Name(name) => {
                let lookup = Arc::clone(&self.lookup);
                resolve_within(
                    &self.dns_in_flight,
                    name,
                    Duration::from_millis(remaining_ms(started, deadline)?),
                    move |host| lookup(host, port),
                )?
            }
        };
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
        socket.ok_or(last_error)
    }
}

impl Transport for HttpsTransport {
    fn get(&self, request: &Request) -> Result<Response, TransportError> {
        let started = Instant::now();
        let deadline = Duration::from_millis(request.deadline_ms);
        let (host, path) = split_https_url(&request.url)?;

        // The route comes from the shared snapshot for this very host, so a
        // redirect hop is decided again. A settings or secret problem ends the
        // request here; it never turns into a direct connection.
        let route = self
            .network
            .route(Scheme::Https, host)
            .map_err(proxy_configuration)?;
        let proxy = match route {
            ResolvedRoute::Direct => None,
            ResolvedRoute::HttpProxy {
                endpoint,
                credentials,
            } => {
                let credentials = self
                    .network
                    .credentials(&credentials)
                    .map_err(proxy_configuration)?;
                Some((endpoint, credentials))
            }
        };
        let tcp = match &proxy {
            None => self.connect(Peer::Name(host), self.direct_port, request, started, deadline)?,
            Some((endpoint, _)) => {
                let peer = match endpoint.host() {
                    Host::Dns(name) => Peer::Name(name),
                    Host::Ipv4(ip) => Peer::Ip(IpAddr::V4(*ip)),
                    Host::Ipv6(ip) => Peer::Ip(IpAddr::V6(*ip)),
                };
                self.connect(peer, endpoint.port(), request, started, deadline)
                    .map_err(proxy_peer_error)?
            }
        };

        // Every read and write below (CONNECT, TLS handshake, request write,
        // response head) goes through this socket, which re-applies the time
        // left until the operation deadline before each I/O call.
        let operation_end = started
            .checked_add(deadline)
            .unwrap_or_else(|| started + Duration::from_secs(60 * 60 * 24 * 365));
        let mut socket = DeadlineSocket::new(
            tcp,
            operation_end,
            Duration::from_millis(request.read_timeout_ms.max(1)),
        );
        if let Some((_, credentials)) = &proxy {
            // The Proxy-Authorization header exists only in this request.
            establish_tunnel(&mut socket, host, credentials.as_ref())?;
        }

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

    // ---- CONNECT answer ---------------------------------------------------

    fn answer(bytes: &[u8]) -> Result<(), TransportError> {
        read_connect_answer(&mut Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn connect_answer_accepts_a_final_200_after_at_most_four_interim_answers() {
        assert!(answer(b"HTTP/1.1 200 Connection established\r\n\r\n").is_ok());
        assert!(answer(b"HTTP/1.0 200\r\nProxy-Agent: x\r\nContent-Length: 0\r\n\r\n").is_ok());
        let mut four = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(4);
        four.extend_from_slice(b"HTTP/1.1 200 OK\r\n\r\n");
        assert!(answer(&four).is_ok());
        let mut five = b"HTTP/1.1 102 Processing\r\n\r\n".repeat(5);
        five.extend_from_slice(b"HTTP/1.1 200 OK\r\n\r\n");
        assert_eq!(answer(&five).unwrap_err().kind, TransportFailure::ProxyConnect);
        // A header line of exactly the limit is fine.
        let edge = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(MAX_LINE_BYTES - 3));
        assert!(answer(edge.as_bytes()).is_ok());
    }

    #[test]
    fn connect_answer_rejects_malformed_oversized_and_unexpected_framing() {
        let too_long = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(1100));
        let mut too_big = b"HTTP/1.1 200 OK\r\n".to_vec();
        for _ in 0..200 {
            too_big.extend_from_slice(format!("X-Pad: {}\r\n", "a".repeat(90)).as_bytes());
        }
        too_big.extend_from_slice(b"\r\n");
        let cases: Vec<Vec<u8>> = vec![
            b"HTTP/1.1 200 OK\n\n".to_vec(),
            b"HTTP/1.1 200 OK\r\n\n".to_vec(),
            b"HTTP/2 200 OK\r\n\r\n".to_vec(),
            b"HTTP/1.1 20 OK\r\n\r\n".to_vec(),
            b"HTTP/1.1 2000 OK\r\n\r\n".to_vec(),
            b"HTTP/1.1 200OK\r\n\r\n".to_vec(),
            b"HTTP/1.1 099 Low\r\n\r\n".to_vec(),
            b"HTTP/1.1 101 Switching Protocols\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nbad header\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nX: a\rb\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nX: y".to_vec(),
            b"HTTP/1.1 200 OK\r\n".to_vec(),
            Vec::new(),
            too_long.into_bytes(),
            too_big,
        ];
        for (index, raw) in cases.iter().enumerate() {
            let error = answer(raw).unwrap_err();
            assert_eq!(error.kind, TransportFailure::ProxyConnect, "case {index}");
        }
    }

    #[test]
    fn connect_answer_classifies_final_statuses_and_stops_reading_a_failure() {
        for (status, expected) in [
            ("407 Proxy Authentication Required", TransportFailure::ProxyAuthentication),
            ("403 Forbidden", TransportFailure::ProxyConnect),
            ("502 Bad Gateway", TransportFailure::ProxyConnect),
            ("500 Oops", TransportFailure::ProxyConnect),
            ("201 Created", TransportFailure::ProxyConnect),
            ("301 Moved", TransportFailure::ProxyConnect),
        ] {
            let line = format!("HTTP/1.1 {status}\r\n");
            let mut input = Cursor::new(format!("{line}\r\nBODYBODYBODY").into_bytes());
            let error = read_connect_answer(&mut input).unwrap_err();
            assert_eq!(error.kind, expected, "{status}");
            assert_eq!(input.position(), line.len() as u64, "{status}: body must stay unread");
        }
    }

    #[test]
    fn connect_answer_leaves_the_tunnel_bytes_unread() {
        let mut input = Cursor::new(b"HTTP/1.1 200 OK\r\n\r\n\x16\x03\x01TUNNEL".to_vec());
        read_connect_answer(&mut input).unwrap();
        let mut rest = Vec::new();
        input.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"\x16\x03\x01TUNNEL");
    }

    #[test]
    fn connect_request_carries_basic_credentials_only_when_given() {
        let credentials = BasicCredentials::new("user", "pa:ss\"\\");
        let text = connect_request("api.github.com", Some(&credentials));
        assert!(
            text.starts_with("CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com:443\r\n"),
            "{text}"
        );
        let expected = base64_encode("user:pa:ss\"\\".as_bytes());
        assert!(text.contains(&format!("Proxy-Authorization: Basic {expected}\r\n")));
        assert!(text.ends_with("\r\n\r\n"));
        assert!(!connect_request("api.github.com", None).contains("Proxy-Authorization"));
        for (input, want) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input), want);
        }
    }

    // ---- proxy + TLS fixtures ---------------------------------------------

    use crate::test_tls::{
        ProxyBehavior, TestPki, ok_response, recorded, spawn_origin, spawn_proxy,
    };
    use hane_network::{
        CredentialError, CredentialRef, CredentialStore, EnvPlatform, ManualProxy, NetworkSettings,
        ProxyAuth, ProxyEnvironment,
    };
    use std::sync::Mutex;

    struct Harness {
        transport: HttpsTransport,
        lookups: Arc<Mutex<Vec<(String, u16)>>>,
    }

    /// A transport that trusts only `pki`, resolves only the names in `table`
    /// (every other lookup fails, which blocks a direct connection) and
    /// records each lookup.
    fn harness(
        pki: &TestPki,
        network: NetworkContext,
        table: Vec<(&'static str, SocketAddr)>,
        direct_port: u16,
    ) -> Harness {
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&lookups);
        let lookup: Lookup = Arc::new(move |host: &str, port: u16| {
            seen.lock().unwrap().push((host.to_owned(), port));
            table
                .iter()
                .find(|(name, _)| *name == host)
                .map(|(_, address)| vec![*address])
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "blocked in test"))
        });
        Harness {
            transport: HttpsTransport {
                config: tls_config(pki.roots()).unwrap(),
                dns_in_flight: Arc::new(AtomicBool::new(false)),
                network,
                lookup,
                direct_port,
            },
            lookups,
        }
    }

    fn env_network(vars: &[(&str, &str)]) -> NetworkContext {
        NetworkContext::from_environment(ProxyEnvironment::from_vars(
            EnvPlatform::MacOs,
            vars.iter().copied(),
        ))
    }

    fn request(url: &str, deadline_ms: u64) -> Request {
        Request {
            url: url.to_owned(),
            accept: "application/octet-stream",
            user_agent: "hane-test",
            api_version: None,
            connect_timeout_ms: 2000,
            read_timeout_ms: 2000,
            deadline_ms,
        }
    }

    fn failure_of(result: Result<Response, TransportError>) -> TransportError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("the request unexpectedly succeeded"),
        }
    }

    fn read_all(mut response: Response) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buffer = [0u8; 64];
        loop {
            let read = response.body.read_within(&mut buffer, 2000).unwrap();
            if read == 0 {
                return out;
            }
            out.extend_from_slice(&buffer[..read]);
        }
    }

    #[test]
    fn a_request_reaches_the_origin_only_through_the_proxy_tunnel() {
        let pki = TestPki::new(&["api.github.com"]);
        let origin = spawn_origin(&pki, |_| ok_response(b"hello"));
        let credential = base64_encode(b"user:pa:ss");
        let proxy = spawn_proxy(ProxyBehavior::Tunnel {
            origin: origin.addr,
            require_basic: Some(credential.clone()),
        });
        let url = format!("http://user:pa%3Ass@proxy.test:{}", proxy.addr.port());
        let harness = harness(
            &pki,
            env_network(&[("https_proxy", url.as_str())]),
            vec![("proxy.test", proxy.addr)],
            HTTPS_PORT,
        );

        let response = harness
            .transport
            .get(&request("https://api.github.com/repos/x", 5000))
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(response.status, 200);
        assert_eq!(read_all(response), b"hello");

        let connects = recorded(&proxy.requests);
        assert_eq!(connects.len(), 1, "{connects:?}");
        assert!(connects[0].starts_with("CONNECT api.github.com:443 HTTP/1.1\r\n"));
        assert!(connects[0].contains(&format!("Proxy-Authorization: Basic {credential}\r\n")));
        let origin_requests = recorded(&origin.requests);
        assert_eq!(origin_requests.len(), 1);
        assert!(origin_requests[0].starts_with("GET /repos/x HTTP/1.1\r\n"));
        assert!(
            !origin_requests[0]
                .to_ascii_lowercase()
                .contains("proxy-authorization"),
            "the proxy credential must not reach the origin"
        );
        // Only the proxy name was looked up, and only on the proxy's port.
        assert_eq!(
            *harness.lookups.lock().unwrap(),
            vec![("proxy.test".to_owned(), proxy.addr.port())]
        );
    }

    #[test]
    fn proxy_refusals_are_classified_and_never_retried_directly() {
        let pki = TestPki::new(&["api.github.com"]);
        let origin = spawn_origin(&pki, |_| ok_response(b"x"));
        let cases: [(&[u8], TransportFailure); 4] = [
            (
                b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                TransportFailure::ProxyAuthentication,
            ),
            (
                b"HTTP/1.1 403 Forbidden\r\n\r\n",
                TransportFailure::ProxyConnect,
            ),
            (
                b"HTTP/1.1 502 Bad Gateway\r\n\r\n",
                TransportFailure::ProxyConnect,
            ),
            (b"garbage\r\n\r\n", TransportFailure::ProxyConnect),
        ];
        for (raw, expected) in cases {
            let proxy = spawn_proxy(ProxyBehavior::Raw(vec![raw.to_vec()]));
            let url = format!("http://u:pa%3Ass@proxy.test:{}", proxy.addr.port());
            let harness = harness(
                &pki,
                env_network(&[("https_proxy", url.as_str())]),
                vec![("proxy.test", proxy.addr), ("api.github.com", origin.addr)],
                origin.addr.port(),
            );
            let error = failure_of(
                harness
                    .transport
                    .get(&request("https://api.github.com/repos/x", 5000)),
            );
            assert_eq!(error.kind, expected, "{error:?}");
            assert!(!error.detail.contains("pa:ss"), "{}", error.detail);
            // The origin is resolvable here, so a direct retry would show up.
            assert_eq!(harness.lookups.lock().unwrap().len(), 1);
            assert!(recorded(&origin.requests).is_empty());
        }
    }

    #[test]
    fn a_silent_proxy_ends_at_the_request_deadline() {
        let pki = TestPki::new(&["api.github.com"]);
        let proxy = spawn_proxy(ProxyBehavior::Silent);
        let url = format!("http://proxy.test:{}", proxy.addr.port());
        let harness = harness(
            &pki,
            env_network(&[("https_proxy", url.as_str())]),
            vec![("proxy.test", proxy.addr)],
            HTTPS_PORT,
        );
        let started = Instant::now();
        let error = failure_of(
            harness
                .transport
                .get(&request("https://api.github.com/repos/x", 400)),
        );
        assert_eq!(error.kind, TransportFailure::Timeout, "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn an_origin_certificate_the_client_does_not_trust_is_a_tls_failure() {
        let trusted = TestPki::new(&["api.github.com"]);
        let rogue = TestPki::new(&["api.github.com"]);
        let origin = spawn_origin(&rogue, |_| ok_response(b"x"));
        let proxy = spawn_proxy(ProxyBehavior::Tunnel {
            origin: origin.addr,
            require_basic: None,
        });
        let url = format!("http://proxy.test:{}", proxy.addr.port());
        let harness = harness(
            &trusted,
            env_network(&[("https_proxy", url.as_str())]),
            vec![("proxy.test", proxy.addr)],
            HTTPS_PORT,
        );
        let error = failure_of(
            harness
                .transport
                .get(&request("https://api.github.com/repos/x", 5000)),
        );
        assert_eq!(error.kind, TransportFailure::Tls, "{error:?}");
        assert!(recorded(&origin.requests).is_empty());
    }

    #[test]
    fn each_hop_is_routed_again_so_a_bypassed_host_connects_directly() {
        let pki = TestPki::new(&["api.github.com", "objects.githubusercontent.com"]);
        let origin = spawn_origin(&pki, |_| ok_response(b"ok"));
        let proxy = spawn_proxy(ProxyBehavior::Tunnel {
            origin: origin.addr,
            require_basic: None,
        });
        let url = format!("http://proxy.test:{}", proxy.addr.port());
        let harness = harness(
            &pki,
            env_network(&[
                ("https_proxy", url.as_str()),
                ("no_proxy", "objects.githubusercontent.com"),
            ]),
            vec![
                ("proxy.test", proxy.addr),
                ("objects.githubusercontent.com", origin.addr),
            ],
            origin.addr.port(),
        );
        for target in [
            "https://api.github.com/repos/x",
            "https://objects.githubusercontent.com/asset",
        ] {
            let response = harness
                .transport
                .get(&request(target, 5000))
                .unwrap_or_else(|error| panic!("{target}: {error:?}"));
            assert_eq!(read_all(response), b"ok");
        }
        let connects = recorded(&proxy.requests);
        assert_eq!(connects.len(), 1, "{connects:?}");
        assert!(connects[0].starts_with("CONNECT api.github.com:443 "));
        assert_eq!(
            *harness.lookups.lock().unwrap(),
            vec![
                ("proxy.test".to_owned(), proxy.addr.port()),
                ("objects.githubusercontent.com".to_owned(), origin.addr.port()),
            ]
        );
    }

    #[test]
    fn unusable_proxy_settings_fail_without_any_connection() {
        struct Missing;
        impl CredentialStore for Missing {
            fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
                Err(CredentialError::NotFound)
            }
        }
        let pki = TestPki::new(&["api.github.com"]);
        let manual = ManualProxy::new(
            "http://proxy.test:3128",
            "",
            ProxyAuth::Basic(CredentialRef::new("c1").unwrap()),
        )
        .unwrap();
        let networks = [
            NetworkContext::new(
                NetworkSettings::manual(manual),
                ProxyEnvironment::from_vars(EnvPlatform::MacOs, std::iter::empty::<(&str, &str)>()),
                Arc::new(Missing),
            ),
            env_network(&[("https_proxy", "socks5://proxy.test:1080")]),
            env_network(&[("https_proxy", "http://u:p%0D%0A@proxy.test:3128")]),
        ];
        for network in networks {
            let harness = harness(&pki, network, vec![], HTTPS_PORT);
            let error = failure_of(
                harness
                    .transport
                    .get(&request("https://api.github.com/repos/x", 5000)),
            );
            assert_eq!(error.kind, TransportFailure::ProxyConfiguration, "{error:?}");
            assert!(harness.lookups.lock().unwrap().is_empty());
        }
    }
}
