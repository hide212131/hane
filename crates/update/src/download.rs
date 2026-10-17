//! Bounded, endpoint-restricted fetching and staging of the archive.
//!
//! The network itself sits behind [`Transport`]; this module owns redirect
//! policy, byte and time limits, cancellation, hashing and the `.part` file
//! discipline. A real HTTP transport is wired in a later stage.

use std::io::{self, Read, Write};
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
}

pub struct Response {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    Timeout,
    Disconnected,
    Tls,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    pub kind: TransportFailure,
    pub detail: String,
}

/// One HTTP GET without redirect following, compression or retries. The
/// implementation applies the timeouts in the request and verifies TLS.
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

fn read_error(err: &io::Error) -> UpdateError {
    match err.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            UpdateError::new(ErrorCode::Timeout, format!("read stalled: {err}"))
        }
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => {
            UpdateError::new(ErrorCode::Truncated, format!("connection cut: {err}"))
        }
        _ => UpdateError::new(ErrorCode::NetworkUnavailable, err.to_string()),
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

impl Fetcher<'_> {
    /// Opens the endpoint, following at most [`MAX_REDIRECTS`] hops and
    /// validating every hop's scheme, host and userinfo.
    fn open(&self, endpoint: &Endpoint<'_>, cancel: &CancelToken) -> Result<Response> {
        let mut current = Url::parse(&endpoint.url()?).map_err(|err| {
            UpdateError::new(ErrorCode::EndpointRejected, format!("bad endpoint: {err}"))
        })?;
        let mut hops = 0usize;
        loop {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let request = Request {
                url: current.to_string(),
                accept: endpoint.accept(),
                user_agent: USER_AGENT,
                api_version: if hops == 0 { endpoint.api_version() } else { None },
                connect_timeout_ms: CONNECT_TIMEOUT_MS,
                read_timeout_ms: READ_STALL_MS,
            };
            let response = self.transport.get(&request).map_err(transport_error)?;
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
        budget_ms: u64,
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
        let started = self.clock.monotonic_ms();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            if self.clock.monotonic_ms().saturating_sub(started) > budget_ms {
                return Err(UpdateError::new(
                    ErrorCode::Timeout,
                    "total transfer time exceeded",
                ));
            }
            let read = match response.body.read(&mut buffer) {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(read_error(&err)),
            };
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
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        let response = self.open(endpoint, cancel)?;
        let mut data = Vec::new();
        self.pump(response, max_bytes, METADATA_TOTAL_MS, cancel, &mut |chunk| {
            data.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(data)
    }

    /// Reads `releases/latest` and selects the release for `target`.
    pub fn check_latest(
        &self,
        current: &Version,
        target: Target,
        cancel: &CancelToken,
    ) -> Result<ReleaseSelection> {
        let body = self.fetch_bytes(&Endpoint::LatestRelease, MAX_API_JSON_BYTES, cancel)?;
        select_release(&body, current, target)
    }

    /// Fetches the catalog and its signature for the pinned release. A 404
    /// here is `metadata_missing` (old release), not "up to date".
    pub fn fetch_catalog(
        &self,
        selection: &ReleaseSelection,
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
        let mut writer = store.begin_part().map_err(UpdateError::from)?;
        let response = self.open(
            &Endpoint::Asset {
                tag: request.tag,
                name: request.name,
            },
            cancel,
        )?;
        let mut hash = sha256_context();
        let total = self.pump(response, request.size, DOWNLOAD_TOTAL_MS, cancel, &mut |chunk| {
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
