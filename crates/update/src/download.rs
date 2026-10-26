//! Bounded, endpoint-restricted fetching and staging of the archive.
//!
//! The network itself sits behind [`Transport`]; this module owns redirect
//! policy, byte and time limits, cancellation, hashing and the `.part` file
//! discipline. A real HTTP transport is wired in a later stage.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use semver::Version;
use url::Url;

use crate::clock::Clock;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::{
    CONNECT_TIMEOUT_MS, DOWNLOAD_TOTAL_MS, MAX_API_JSON_BYTES, MAX_ARCHIVE_BYTES,
    MAX_CATALOG_BYTES, MAX_REDIRECTS, MAX_SIGNATURE_BYTES, METADATA_TOTAL_MS, READ_STALL_MS,
};
use crate::model::{CATALOG_ASSET_NAME, REPOSITORY, SIGNATURE_ASSET_NAME, Target};
use crate::release::{ReleaseSelection, select_release};
use crate::util::{hex_encode, sha256_context};
use crate::version::parse_release_tag;

pub const USER_AGENT: &str = concat!("hane-update/", env!("CARGO_PKG_VERSION"));
pub const GITHUB_API_VERSION: &str = "2022-11-28";
const API_HOST: &str = "api.github.com";
/// Release delivery hosts allowed as redirect targets (exact match only).
pub const REDIRECT_HOSTS: [&str; 2] = [
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];

pub fn latest_release_url() -> String {
    format!("https://{API_HOST}/repos/{REPOSITORY}/releases/latest")
}

pub fn asset_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}")
}

/// What may be requested. The URL is built here, never taken from input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint<'a> {
    LatestRelease,
    Asset { tag: &'a str, name: &'a str },
}

impl Endpoint<'_> {
    fn url(&self) -> Result<String> {
        match *self {
            Endpoint::LatestRelease => Ok(latest_release_url()),
            Endpoint::Asset { tag, name } => {
                parse_release_tag(tag)?;
                let known = name == CATALOG_ASSET_NAME
                    || name == SIGNATURE_ASSET_NAME
                    || Target::ALL.iter().any(|t| t.asset_name() == name);
                if !known {
                    return Err(UpdateError::new(
                        ErrorCode::EndpointRejected,
                        format!("asset {name:?} is not a Hane update asset"),
                    ));
                }
                Ok(asset_url(tag, name))
            }
        }
    }

    fn accept(&self) -> &'static str {
        match self {
            Endpoint::LatestRelease => "application/vnd.github+json",
            Endpoint::Asset { .. } => "application/octet-stream",
        }
    }

    fn api_version(&self) -> Option<&'static str> {
        match self {
            Endpoint::LatestRelease => Some(GITHUB_API_VERSION),
            Endpoint::Asset { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub accept: &'static str,
    pub user_agent: &'static str,
    pub api_version: Option<&'static str>,
    pub connect_timeout_ms: u64,
    pub read_timeout_ms: u64,
    /// Upper bound for the whole call: connecting and receiving the response
    /// head together. It is what is left of the operation deadline, so it also
    /// bounds `connect_timeout_ms + read_timeout_ms` taken together.
    pub deadline_ms: u64,
}

/// Response body whose every read is bounded by the operation deadline.
pub trait BodyReader: Send {
    /// Reads into `buf`, returning within `timeout_ms` milliseconds. When no
    /// data arrives in time it fails with [`io::ErrorKind::TimedOut`]. The
    /// caller passes the smaller of the read-stall limit and the time left on
    /// the operation deadline.
    fn read_within(&mut self, buf: &mut [u8], timeout_ms: u64) -> io::Result<usize>;
}

pub struct Response {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    pub body: Box<dyn BodyReader>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    Timeout,
    Disconnected,
    Tls,
    Other,
    /// Proxy settings or credential unusable; never falls back to Direct.
    ProxyConfiguration,
    /// The proxy answered the CONNECT with a failure other than 407, or its
    /// response was malformed.
    ProxyConnect,
    /// The proxy answered 407.
    ProxyAuthentication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub kind: TransportFailure,
    pub detail: String,
}

/// One HTTP GET without redirect following, compression or retries. The
/// implementation applies the timeouts in the request, returns (or fails with
/// [`TransportFailure::Timeout`]) within `Request::deadline_ms`, hands back a
/// body that honors the timeout of each read, and verifies TLS.
pub trait Transport: Send + Sync {
    fn get(&self, request: &Request) -> std::result::Result<Response, TransportError>;
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn cancelled() -> UpdateError {
    UpdateError::new(ErrorCode::Cancelled, "operation was cancelled")
}

fn transport_error(err: TransportError) -> UpdateError {
    let code = match err.kind {
        TransportFailure::Timeout => ErrorCode::Timeout,
        TransportFailure::ProxyConfiguration => ErrorCode::ProxyConfiguration,
        TransportFailure::ProxyConnect => ErrorCode::ProxyConnect,
        TransportFailure::ProxyAuthentication => ErrorCode::ProxyAuthentication,
        TransportFailure::Disconnected | TransportFailure::Tls | TransportFailure::Other => {
            ErrorCode::NetworkUnavailable
        }
    };
    UpdateError::new(code, err.detail)
}

/// 404, rate limiting and server errors are different outcomes; none of them
/// may be reported as "already up to date".
pub fn status_error(status: u16) -> UpdateError {
    match status {
        404 => UpdateError::new(ErrorCode::NotFound, "HTTP 404"),
        403 | 429 => UpdateError::new(ErrorCode::RateLimited, format!("HTTP {status}")),
        _ => UpdateError::new(ErrorCode::NetworkUnavailable, format!("HTTP {status}")),
    }
}

/// Structured diagnostic suffix: the `ErrorKind` and, when present, the raw OS
/// error number. Kept separate from the human text so it survives rewording.
fn io_diagnostics(err: &io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => format!("io_kind={:?} os_error={code}", err.kind()),
        None => format!("io_kind={:?}", err.kind()),
    }
}

fn read_error(err: &io::Error) -> UpdateError {
    let diagnostics = io_diagnostics(err);
    match err.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => UpdateError::new(
            ErrorCode::Timeout,
            format!("read stalled: {err} [{diagnostics}]"),
        ),
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => UpdateError::new(
            ErrorCode::Truncated,
            format!("connection cut: {err} [{diagnostics}]"),
        ),
        _ => UpdateError::new(
            ErrorCode::NetworkUnavailable,
            format!("{err} [{diagnostics}]"),
        ),
    }
}

fn check_redirect_target(url: &Url) -> Result<()> {
    let reject = |why: &str| {
        UpdateError::new(
            ErrorCode::RedirectRejected,
            format!("redirect to {url} rejected: {why}"),
        )
    };
    if url.scheme() != "https" {
        return Err(reject("not https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(reject("userinfo present"));
    }
    if url.port().is_some() {
        return Err(reject("explicit port"));
    }
    match url.host_str() {
        Some(host) if REDIRECT_HOSTS.contains(&host) => Ok(()),
        _ => Err(reject("host is not an allowed release delivery host")),
    }
}

pub struct Fetcher<'a> {
    pub transport: &'a dyn Transport,
    pub clock: &'a dyn Clock,
}

/// A single monotonic deadline for one high-level operation. Every request,
/// redirect hop and body read of that operation draws from the same budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline {
    started_ms: u64,
    budget_ms: u64,
}

impl Deadline {
    /// Milliseconds left, or a timeout error once the budget is used up.
    fn remaining(&self, clock: &dyn Clock) -> Result<u64> {
        let elapsed = clock.monotonic_ms().saturating_sub(self.started_ms);
        match self.budget_ms.checked_sub(elapsed) {
            Some(left) if left > 0 => Ok(left),
            _ => Err(UpdateError::new(
                ErrorCode::Timeout,
                "total transfer time exceeded",
            )),
        }
    }
}

impl Fetcher<'_> {
    /// Starts the shared deadline for a metadata check ([`METADATA_TOTAL_MS`]).
    /// Pass it to [`Fetcher::check_latest_within`] and
    /// [`Fetcher::fetch_catalog_within`] so both share one budget.
    pub fn metadata_deadline(&self) -> Deadline {
        self.deadline(METADATA_TOTAL_MS)
    }

    fn deadline(&self, budget_ms: u64) -> Deadline {
        Deadline {
            started_ms: self.clock.monotonic_ms(),
            budget_ms,
        }
    }

    /// Opens the endpoint, following at most [`MAX_REDIRECTS`] hops and
    /// validating every hop's scheme, host and userinfo. Each request's
    /// timeouts are capped by what is left of `deadline`.
    fn open(
        &self,
        endpoint: &Endpoint<'_>,
        deadline: &Deadline,
        cancel: &CancelToken,
    ) -> Result<Response> {
        let mut current = Url::parse(&endpoint.url()?).map_err(|err| {
            UpdateError::new(ErrorCode::EndpointRejected, format!("bad endpoint: {err}"))
        })?;
        let mut hops = 0usize;
        loop {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let remaining = deadline.remaining(self.clock)?;
            let request = Request {
                url: current.to_string(),
                accept: endpoint.accept(),
                user_agent: USER_AGENT,
                api_version: if hops == 0 { endpoint.api_version() } else { None },
                connect_timeout_ms: CONNECT_TIMEOUT_MS.min(remaining),
                read_timeout_ms: READ_STALL_MS.min(remaining),
                deadline_ms: remaining,
            };
            let response = self.transport.get(&request).map_err(transport_error)?;
            // A transport that returned after the deadline broke its contract;
            // the late response is never used.
            deadline.remaining(self.clock)?;
            match response.status {
                200 => return Ok(response),
                301 | 302 | 303 | 307 | 308 => {
                    hops += 1;
                    if hops > MAX_REDIRECTS {
                        return Err(UpdateError::new(
                            ErrorCode::RedirectRejected,
                            "too many redirects",
                        ));
                    }
                    let location = response.location.as_deref().ok_or_else(|| {
                        UpdateError::new(
                            ErrorCode::RedirectRejected,
                            "redirect without a Location header",
                        )
                    })?;
                    let next = current.join(location).map_err(|err| {
                        UpdateError::new(
                            ErrorCode::RedirectRejected,
                            format!("bad redirect target: {err}"),
                        )
                    })?;
                    check_redirect_target(&next)?;
                    current = next;
                }
                status => return Err(status_error(status)),
            }
        }
    }

    /// Streams the body into `sink`, enforcing the byte ceiling, the total
    /// deadline and cancellation. Returns the byte count.
    fn pump(
        &self,
        mut response: Response,
        max_bytes: u64,
        deadline: &Deadline,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<u64> {
        if let Some(declared) = response.content_length
            && declared > max_bytes
        {
            return Err(UpdateError::new(
                ErrorCode::LimitExceeded,
                format!("declared length {declared} exceeds {max_bytes}"),
            ));
        }
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let remaining = deadline.remaining(self.clock)?;
            let read = match response
                .body
                .read_within(&mut buffer, READ_STALL_MS.min(remaining))
            {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(read_error(&err)),
            };
            // A read that returned after the deadline is not trusted, even if
            // it produced data or reported the end of the body.
            deadline.remaining(self.clock)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > max_bytes {
                return Err(UpdateError::new(
                    ErrorCode::LimitExceeded,
                    format!("body exceeds {max_bytes} bytes"),
                ));
            }
            sink(&buffer[..read])?;
        }
        if let Some(declared) = response.content_length
            && total != declared
        {
            return Err(UpdateError::new(
                ErrorCode::Truncated,
                format!("received {total} of {declared} bytes"),
            ));
        }
        Ok(total)
    }

    fn fetch_bytes(
        &self,
        endpoint: &Endpoint<'_>,
        max_bytes: u64,
        deadline: &Deadline,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        let response = self.open(endpoint, deadline, cancel)?;
        let mut data = Vec::new();
        self.pump(response, max_bytes, deadline, cancel, &mut |chunk| {
            data.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(data)
    }

    /// Reads `releases/latest` and selects the release for `target`, within
    /// its own metadata budget.
    pub fn check_latest(
        &self,
        current: &Version,
        target: Target,
        cancel: &CancelToken,
    ) -> Result<ReleaseSelection> {
        self.check_latest_within(current, target, &self.metadata_deadline(), cancel)
    }

    /// Like [`Fetcher::check_latest`], drawing from a caller-provided
    /// `deadline` so a whole metadata check shares one budget.
    pub fn check_latest_within(
        &self,
        current: &Version,
        target: Target,
        deadline: &Deadline,
        cancel: &CancelToken,
    ) -> Result<ReleaseSelection> {
        let body = self.fetch_bytes(
            &Endpoint::LatestRelease,
            MAX_API_JSON_BYTES,
            deadline,
            cancel,
        )?;
        select_release(&body, current, target)
    }

    /// Fetches the catalog and its signature for the pinned release within one
    /// metadata budget. A 404 here is `metadata_missing` (old release), not
    /// "up to date".
    pub fn fetch_catalog(
        &self,
        selection: &ReleaseSelection,
        cancel: &CancelToken,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        self.fetch_catalog_within(selection, &self.metadata_deadline(), cancel)
    }

    /// Like [`Fetcher::fetch_catalog`], drawing from a caller-provided
    /// `deadline`.
    pub fn fetch_catalog_within(
        &self,
        selection: &ReleaseSelection,
        deadline: &Deadline,
        cancel: &CancelToken,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let missing = |err: UpdateError| {
            if err.code == ErrorCode::NotFound {
                UpdateError::new(ErrorCode::MetadataMissing, "release has no update metadata")
            } else {
                err
            }
        };
        let catalog = self
            .fetch_bytes(
                &Endpoint::Asset {
                    tag: &selection.tag,
                    name: CATALOG_ASSET_NAME,
                },
                MAX_CATALOG_BYTES,
                deadline,
                cancel,
            )
            .map_err(missing)?;
        let signature = self
            .fetch_bytes(
                &Endpoint::Asset {
                    tag: &selection.tag,
                    name: SIGNATURE_ASSET_NAME,
                },
                MAX_SIGNATURE_BYTES,
                deadline,
                cancel,
            )
            .map_err(missing)?;
        Ok((catalog, signature))
    }

    /// Downloads the archive into `store`. Any failure discards the partial
    /// file; only a complete size- and hash-verified body is committed.
    pub fn download_archive(
        &self,
        request: &ArchiveDownload<'_>,
        store: &mut dyn PartStore,
        cancel: &CancelToken,
    ) -> Result<DownloadedArchive> {
        if request.size == 0 || request.size > MAX_ARCHIVE_BYTES {
            return Err(UpdateError::new(
                ErrorCode::LimitExceeded,
                "archive size is out of range",
            ));
        }
        let outcome = self.download_inner(request, store, cancel);
        if outcome.is_err() {
            store.discard_part();
        }
        outcome
    }

    fn download_inner(
        &self,
        request: &ArchiveDownload<'_>,
        store: &mut dyn PartStore,
        cancel: &CancelToken,
    ) -> Result<DownloadedArchive> {
        let deadline = self.deadline(DOWNLOAD_TOTAL_MS);
        let mut writer = store.begin_part().map_err(UpdateError::from)?;
        let response = self.open(
            &Endpoint::Asset {
                tag: request.tag,
                name: request.name,
            },
            &deadline,
            cancel,
        )?;
        let mut hash = sha256_context();
        let total = self.pump(response, request.size, &deadline, cancel, &mut |chunk| {
            hash.update(chunk);
            writer.write_all(chunk).map_err(UpdateError::from)
        })?;
        if total != request.size {
            return Err(UpdateError::new(
                ErrorCode::SizeMismatch,
                format!("archive is {total} bytes, expected {}", request.size),
            ));
        }
        let sha256 = hex_encode(hash.finish().as_ref());
        if sha256 != request.sha256 {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                "archive SHA-256 differs from the signed catalog",
            ));
        }
        writer.flush().map_err(UpdateError::from)?;
        drop(writer);
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        store.commit_part().map_err(UpdateError::from)?;
        Ok(DownloadedArchive {
            size: total,
            sha256,
        })
    }
}

/// Expected archive properties, taken from the verified catalog.
pub struct ArchiveDownload<'a> {
    pub tag: &'a str,
    pub name: &'a str,
    pub size: u64,
    pub sha256: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedArchive {
    pub size: u64,
    pub sha256: String,
}

/// Filesystem seam for the private `.part` file.
pub trait PartStore {
    fn begin_part(&mut self) -> io::Result<Box<dyn Write>>;
    /// Makes the complete part the candidate archive (sync, then rename).
    fn commit_part(&mut self) -> io::Result<()>;
    /// Removes any partial file; must not fail the caller.
    fn discard_part(&mut self);
}

#[cfg(test)]
mod io_error_tests {
    use super::*;

    #[test]
    fn read_error_keeps_class_kind_and_os_error() {
        let cases = [
            (io::ErrorKind::TimedOut, ErrorCode::Timeout),
            (io::ErrorKind::ConnectionReset, ErrorCode::Truncated),
            (
                io::ErrorKind::PermissionDenied,
                ErrorCode::NetworkUnavailable,
            ),
        ];
        for (kind, code) in cases {
            let error = read_error(&io::Error::new(kind, "synthetic"));
            assert_eq!(error.code, code);
            assert!(error.detail.contains("synthetic"), "{}", error.detail);
            assert!(
                error.detail.contains(&format!("io_kind={kind:?}")),
                "{}",
                error.detail
            );
            assert!(!error.detail.contains("os_error"), "{}", error.detail);
        }
    }

    #[test]
    fn proxy_failures_map_to_their_own_codes_not_to_a_generic_network_error() {
        for (kind, code, text) in [
            (
                TransportFailure::ProxyConfiguration,
                ErrorCode::ProxyConfiguration,
                "proxy_configuration",
            ),
            (
                TransportFailure::ProxyConnect,
                ErrorCode::ProxyConnect,
                "proxy_connect",
            ),
            (
                TransportFailure::ProxyAuthentication,
                ErrorCode::ProxyAuthentication,
                "proxy_authentication",
            ),
            (TransportFailure::Timeout, ErrorCode::Timeout, "timeout"),
            (TransportFailure::Tls, ErrorCode::NetworkUnavailable, "network_unavailable"),
        ] {
            let error = transport_error(TransportError {
                kind,
                detail: "fixed".to_owned(),
            });
            assert_eq!(error.code, code);
            assert_eq!(error.code.as_str(), text);
        }
    }

    #[test]
    fn read_error_reports_raw_os_error_number() {
        let error = read_error(&io::Error::from_raw_os_error(4242));
        assert_eq!(error.code, ErrorCode::NetworkUnavailable);
        assert!(error.detail.contains("os_error=4242"), "{}", error.detail);
        assert!(error.detail.contains("io_kind="), "{}", error.detail);
    }
}
