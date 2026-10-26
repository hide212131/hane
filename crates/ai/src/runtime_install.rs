//! First-use network download and local install of the Codex App Server
//! runtime.
//!
//! Hane never contacts the network at startup or while editing a document.
//! The only way this module's download pipeline ever runs is the user
//! explicitly choosing "install" in the AI settings screen
//! (`crate::service::AiCommand::InstallRuntime`). Once a candidate has been
//! downloaded, hash-verified, safely extracted, matched file-for-file
//! against a pinned inventory, and launched once in an isolated, throwaway
//! configuration to
//! confirm it actually starts, stops the App Server process, and publishes
//! a `manifest.json` under `AiPaths::runtime_install_dir`, this module is
//! never consulted again: every later AI operation resolves the installed
//! binary purely locally via `crate::runtime_distribution::resolve_runtime_binary`.
//!
//! # Where the pinned target/URL/size/hash data comes from
//!
//! The exact upstream URL, archive size, and SHA-256 for each of the three P0
//! targets are the single source of truth already checked into this
//! repository at `assets/ai/codex-runtime-lock.json`, together with a
//! per-target pinned file-level inventory under `assets/ai/inventory/`.
//! this module reads the lock to drive the first-use download. The downloaded
//! files are installed directly under Hane's app-data runtime directory and
//! produce the `manifest.json` contract `crate::runtime_distribution` resolves.
//!
//! # Why extraction shells out to the system `tar`, not a from-scratch parser
//!
//! This module downloads the archive and verifies its whole-file SHA-256
//! against the pinned `archive_sha256` *before* ever extracting it. Because
//! that pinned hash is checked before extraction. This module still defends in
//! depth: it diffs the extracted file tree against the pinned inventory
//! path-for-path (rejecting anything missing, extra, or mismatched in
//! size/hash/executable-bit) and rejects any symlink/reparse point found on
//! disk (reusing `crate::runtime_distribution::check_not_symlink`), but it
//! does not reimplement a tar/gzip entry-level parser in Rust: both P0
//! target platforms (Windows and macOS) ship a `tar` capable of `-xzf` out of
//! the box, and reusing it keeps this module free of new third-party
//! dependencies.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hane_network::{BasicCredentials, NetworkContext, ProxyEndpoint, ResolvedRoute, Scheme};
use serde::Deserialize;
use url::Url;

use crate::rpc::RejectAllServerRequests;
use crate::runtime::{AiRuntime, RuntimeConfig, RuntimeError, RuntimeState};
use crate::runtime_distribution::{
    self, MACOS_ARM64_TARGET, RuntimeManifest, RuntimeManifestFile, RuntimeManifestProfile,
    WINDOWS_ARM64_TARGET, WINDOWS_X64_TARGET, current_target,
};

const LOCK_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ai/codex-runtime-lock.json"
));
const WINDOWS_X64_INVENTORY_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ai/inventory/x86_64-pc-windows-msvc-0.157.1.json"
));
const WINDOWS_ARM64_INVENTORY_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ai/inventory/aarch64-pc-windows-msvc-0.157.1.json"
));
const MACOS_ARM64_INVENTORY_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/ai/inventory/aarch64-apple-darwin-0.157.1.json"
));

const MAX_ENTRY_SIZE: u64 = 1024 * 1024 * 1024; // 1 GiB, matching the build-time tool's cap.
const MAX_TOTAL_SIZE: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB, matching the build-time tool's cap.
const MAX_REDIRECT_HOPS: u32 = 5;
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_SPEED_LIMIT: u64 = 1024;
const DOWNLOAD_SPEED_TIME: Duration = Duration::from_secs(60);
const VERIFY_START_TIMEOUT: Duration = Duration::from_secs(20);
const SUBPROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

const WINDOWS_X64_ARCHIVE_SIZE: u64 = 122_840_599;
const WINDOWS_X64_ARCHIVE_SHA256: &str =
    "7ca926f4fc443c0ead83109314e9a97bf43d2da276be147f1919a4b5e62dfa8b";
const WINDOWS_ARM64_ARCHIVE_SIZE: u64 = 113_873_506;
const WINDOWS_ARM64_ARCHIVE_SHA256: &str =
    "e94f89d28bc81eb4a98110c4513d7713e7985a9d91c9d9d5627de92e030e9d73";
const MACOS_ARM64_ARCHIVE_SIZE: u64 = 98_491_706;
const MACOS_ARM64_ARCHIVE_SHA256: &str =
    "9a6033c9fe30260e71d784d003ad1e2de2a55c93e24a24856e8433029a3f811a";

#[derive(Debug, Deserialize)]
struct LockFile {
    schema_version: u32,
    codex_version: String,
    source_commit: String,
    targets: std::collections::HashMap<String, LockTargetEntry>,
}

#[derive(Debug, Deserialize)]
struct LockTargetEntry {
    url: String,
    archive_size: u64,
    archive_sha256: String,
    entrypoint_name: String,
    runtime_profile: LockProfile,
    config_profile: LockProfile,
    state_profile: LockProfile,
}

#[derive(Debug, Deserialize)]
struct LockProfile {
    name: String,
}

/// A pinned, fixed-at-compile-time description of the exact artifact this
/// build will ever install for one target: the upstream URL, its exact size
/// and SHA-256, the host allowlist every request and redirect hop must stay
/// within, the entrypoint file name, and the full pinned file-level inventory
/// the extracted archive must match exactly. Everything here either comes
/// straight from
/// `assets/ai/codex-runtime-lock.json`/`assets/ai/inventory/*.json`, or is a
/// fixed constant; nothing here is ever fetched from the network.
#[derive(Debug, Clone)]
pub struct RuntimeInstallSpec {
    pub target: String,
    pub codex_version: String,
    pub source_commit: String,
    pub url: String,
    pub archive_size: u64,
    pub archive_sha256: String,
    pub entrypoint_name: String,
    pub allowed_hosts: Vec<String>,
    pub pinned_inventory: Vec<RuntimeManifestFile>,
    pub runtime_profile: String,
    pub config_profile: String,
    pub state_profile: String,
}

/// The fixed hosts allowed to ever serve the archive's bytes, on the initial
/// request or any redirect hop: the GitHub release-asset page and the
/// official storage hosts it redirects downloads to. Matches the GitHub
/// release hosts allowed for the direct download.
const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Builds [`RuntimeInstallSpec`] for this host's current target
/// ([`current_target`]), reading the pinned lock/inventory data embedded at
/// compile time. `None` when this host is neither P0 target, or (defensively)
/// when the embedded lock/inventory data this build was compiled with does
/// not actually describe that target.
pub fn install_spec_for_current_target() -> Option<RuntimeInstallSpec> {
    install_spec_for_target(current_target()?)
}

fn install_spec_for_target(target: &str) -> Option<RuntimeInstallSpec> {
    let lock: LockFile = serde_json::from_str(LOCK_JSON).ok()?;
    if lock.schema_version != 1 {
        return None;
    }
    if lock.codex_version.trim().is_empty() || lock.source_commit.trim().is_empty() {
        return None;
    }
    let entry = lock.targets.get(target)?;
    let (expected_size, expected_sha256) = match target {
        WINDOWS_X64_TARGET => (WINDOWS_X64_ARCHIVE_SIZE, WINDOWS_X64_ARCHIVE_SHA256),
        WINDOWS_ARM64_TARGET => (WINDOWS_ARM64_ARCHIVE_SIZE, WINDOWS_ARM64_ARCHIVE_SHA256),
        MACOS_ARM64_TARGET => (MACOS_ARM64_ARCHIVE_SIZE, MACOS_ARM64_ARCHIVE_SHA256),
        _ => return None,
    };
    let allowed_hosts: Vec<String> = ALLOWED_HOSTS.iter().map(|host| host.to_string()).collect();
    if entry.archive_size != expected_size
        || !entry.archive_sha256.eq_ignore_ascii_case(expected_sha256)
        || !runtime_distribution::is_sha256_hex(&entry.archive_sha256)
        || check_allowed_url(&entry.url, &allowed_hosts).is_err()
        || entry.entrypoint_name.trim().is_empty()
        || entry.runtime_profile.name.trim().is_empty()
        || entry.config_profile.name.trim().is_empty()
        || entry.state_profile.name.trim().is_empty()
        || entry.archive_size > MAX_TOTAL_SIZE
    {
        return None;
    }
    let inventory_json = match target {
        WINDOWS_X64_TARGET => WINDOWS_X64_INVENTORY_JSON,
        WINDOWS_ARM64_TARGET => WINDOWS_ARM64_INVENTORY_JSON,
        MACOS_ARM64_TARGET => MACOS_ARM64_INVENTORY_JSON,
        _ => return None,
    };
    let pinned_inventory: Vec<RuntimeManifestFile> = serde_json::from_str(inventory_json).ok()?;
    if pinned_inventory.is_empty() {
        return None;
    }
    let mut seen_inventory_paths = std::collections::HashSet::new();
    if pinned_inventory.iter().any(|file| {
        file.path.trim().is_empty()
            || !file.path.is_ascii()
            || !has_valid_relative_path(&file.path)
            || file.kind != "file"
            || !runtime_distribution::is_sha256_hex(&file.sha256)
            || !seen_inventory_paths.insert(file.path.to_ascii_lowercase())
    }) || !pinned_inventory.iter().any(|file| {
        file.path.rsplit('/').next() == Some(entry.entrypoint_name.as_str()) && file.executable
    }) {
        return None;
    }
    Some(RuntimeInstallSpec {
        target: target.to_owned(),
        codex_version: lock.codex_version.clone(),
        source_commit: lock.source_commit.clone(),
        url: entry.url.clone(),
        archive_size: entry.archive_size,
        archive_sha256: entry.archive_sha256.clone(),
        entrypoint_name: entry.entrypoint_name.clone(),
        allowed_hosts,
        pinned_inventory,
        runtime_profile: entry.runtime_profile.name.clone(),
        config_profile: entry.config_profile.name.clone(),
        state_profile: entry.state_profile.name.clone(),
    })
}

/// Every distinct, stable way the first-use install pipeline can fail.
/// Mirrors `RuntimeDistributionErrorCode`'s philosophy: each variant is
/// meant to be shown to the user distinctly, not collapsed into one generic
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeInstallErrorCode {
    /// This host is neither P0 target, or this build's embedded lock/
    /// inventory data does not describe it.
    UnsupportedTarget,
    /// The initial request or a redirect hop resolved to a host outside the
    /// fixed allowlist, a non-HTTPS scheme, a non-default port, or a URL
    /// carrying embedded credentials.
    DisallowedRedirectHost,
    /// Too many redirect hops, or the final response was never a plain
    /// `200`.
    NetworkDownloadFailed,
    /// The downloaded archive's size does not match the pinned
    /// `archive_size`.
    SizeMismatch,
    /// The downloaded archive's SHA-256 does not match the pinned
    /// `archive_sha256`.
    IntegrityMismatch,
    /// Extraction itself failed, or produced something other than exactly
    /// the pinned file-level inventory (missing/extra/mismatched entry,
    /// case-fold collision, oversized entry, or a symlink/reparse point
    /// found on disk).
    ExtractionFailed,
    /// Zero or more than one extracted file matches the pinned
    /// `entrypoint_name` as an executable entry.
    AmbiguousEntrypoint,
    /// The candidate spawned in an isolated, throwaway configuration, but
    /// the `initialize` handshake or the following stop could not be
    /// confirmed.
    VerificationFailed,
    /// A staging directory could not be created, or the verified candidate
    /// could not be published to `AiPaths::runtime_install_dir` for a reason
    /// other than permission denial.
    StagingFailed,
    /// An install-related filesystem or child-process operation was rejected
    /// by the operating system due to insufficient permissions.
    PermissionDenied,
    /// The user canceled the install, or Hane is shutting down.
    Canceled,
}

impl RuntimeInstallErrorCode {
    pub fn stable_code(self) -> &'static str {
        match self {
            Self::UnsupportedTarget => "runtime_install_unsupported_target",
            Self::DisallowedRedirectHost => "runtime_install_disallowed_redirect_host",
            Self::NetworkDownloadFailed => "runtime_install_network_failed",
            Self::SizeMismatch => "runtime_install_size_mismatch",
            Self::IntegrityMismatch => "runtime_install_integrity_mismatch",
            Self::ExtractionFailed => "runtime_install_extraction_failed",
            Self::AmbiguousEntrypoint => "runtime_install_ambiguous_entrypoint",
            Self::VerificationFailed => "runtime_install_verification_failed",
            Self::StagingFailed => "runtime_install_staging_failed",
            Self::PermissionDenied => "runtime_install_permission_denied",
            Self::Canceled => "runtime_install_canceled",
        }
    }

    /// Whether repeating the same explicit install can plausibly succeed.
    /// Deterministic failures (an unsupported host, an archive whose layout
    /// the pinned lock cannot resolve) return `false` so the UI does not
    /// offer a retry that cannot change the outcome.
    pub fn is_retryable(self) -> bool {
        !matches!(self, Self::UnsupportedTarget | Self::AmbiguousEntrypoint)
    }

    /// The pipeline stage that failed, as a short user-facing label.
    pub fn failure_stage(self) -> &'static str {
        match self {
            Self::UnsupportedTarget => "対応環境の判定",
            Self::DisallowedRedirectHost | Self::NetworkDownloadFailed => "ダウンロード",
            Self::SizeMismatch | Self::IntegrityMismatch => "取得物の検証",
            Self::ExtractionFailed | Self::AmbiguousEntrypoint => "展開と内容検査",
            Self::VerificationFailed => "導入候補の起動確認",
            Self::StagingFailed | Self::PermissionDenied => "Hane専用領域への公開",
            Self::Canceled => "利用者による取り消し",
        }
    }
}

pub(crate) fn map_io_error(
    error: &io::Error,
    fallback: RuntimeInstallErrorCode,
) -> RuntimeInstallErrorCode {
    if error.kind() == io::ErrorKind::PermissionDenied {
        RuntimeInstallErrorCode::PermissionDenied
    } else {
        fallback
    }
}

pub(crate) fn map_distribution_error(
    error: runtime_distribution::RuntimeDistributionErrorCode,
    fallback: RuntimeInstallErrorCode,
) -> RuntimeInstallErrorCode {
    if error == runtime_distribution::RuntimeDistributionErrorCode::AccessDenied {
        RuntimeInstallErrorCode::PermissionDenied
    } else {
        fallback
    }
}

/// The install pipeline's current phase, surfaced to the UI
/// (`AiSnapshot::runtime_install`). `NotInstalled` and `Installed` are also
/// this module's answer, independent of any in-progress attempt, to "is a
/// runtime currently installed" (see `crate::service`'s startup check, which
/// calls `crate::runtime_distribution::resolve_runtime_binary` against
/// `AiPaths::runtime_install_dir` directly rather than through this type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeInstallState {
    NotInstalled,
    Downloading,
    Verifying,
    Installed,
    Failed(RuntimeInstallErrorCode),
}

/// Downloads `spec.url` into `dest`, enforcing HTTPS and `spec.allowed_hosts`
/// on the initial request and every redirect hop, and the exact
/// `spec.archive_size`. Implementations must return
/// `Err(RuntimeInstallErrorCode::Canceled)` promptly once `is_canceled`
/// starts returning `true`, without leaving a partially written `dest`
/// behind on any error (the caller's staging directory is removed as a
/// whole on failure regardless, but a downloader should not rely on that).
pub trait ArchiveDownloader: Send + Sync {
    fn download(
        &self,
        spec: &RuntimeInstallSpec,
        dest: &Path,
        is_canceled: &dyn Fn() -> bool,
    ) -> Result<(), RuntimeInstallErrorCode>;
}

/// Extracts the archive at `archive` directly into `dest_dir` (already
/// created, empty), reproducing the pinned inventory's paths verbatim
/// (no extra leading directory component).
pub trait ArchiveExtractor: Send + Sync {
    fn extract(
        &self,
        archive: &Path,
        dest_dir: &Path,
        is_canceled: &dyn Fn() -> bool,
    ) -> Result<(), RuntimeInstallErrorCode>;
}

/// Downloads over HTTPS by shelling out to the system `curl`, manually
/// following redirects one hop at a time (never `curl -L`) so every hop's
/// target host can be checked against `spec.allowed_hosts` before it is
/// ever requested.
///
/// Every hop resolves its route from the shared [`NetworkContext`] snapshot;
/// curl itself is never left to pick a proxy from its own environment or
/// configuration files.
#[derive(Debug)]
pub struct CurlArchiveDownloader {
    network: NetworkContext,
    last_failure: Mutex<Option<DownloadFailure>>,
    /// Test seam, empty in production (only `with_test_seams` can fill it):
    /// extra curl arguments (CA bundle, `--connect-to`) and parent-environment
    /// entries. The production command construction and route decision are
    /// unchanged.
    seam_args: Vec<OsString>,
    seam_envs: Vec<(OsString, OsString)>,
}

/// Secret-free facts about why the last download attempt failed, so a proxy
/// refusal, an origin status and a curl failure stay distinguishable. It never
/// holds curl's stderr, headers, the proxy URL or a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFailure {
    /// The proxy settings or credential could not be used.
    ProxyConfiguration,
    /// The proxy answered the CONNECT with 407.
    ProxyAuthentication,
    /// The proxy answered the CONNECT with another failure status.
    ProxyConnect(u16),
    /// The origin (not the proxy) answered with this status.
    OriginStatus(u16),
    /// curl failed with this exit code (`-1` when it had none).
    CurlExit(i32),
    /// curl could not be started or driven.
    CurlIo,
}

impl Default for CurlArchiveDownloader {
    fn default() -> Self {
        Self::new(NetworkContext::default())
    }
}

impl CurlArchiveDownloader {
    pub fn new(network: NetworkContext) -> Self {
        Self {
            network,
            last_failure: Mutex::new(None),
            seam_args: Vec::new(),
            seam_envs: Vec::new(),
        }
    }

    /// Test-only: adds curl arguments (for example `--cacert` and
    /// `--connect-to` to reach a local fixture) and environment entries that
    /// stand in for the parent process environment. Everything else, in
    /// particular the route decision and the proxy options, is the production
    /// code path.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_seams(
        mut self,
        args: Vec<OsString>,
        envs: Vec<(OsString, OsString)>,
    ) -> Self {
        self.seam_args = args;
        self.seam_envs = envs;
        self
    }

    /// The failure recorded by the most recent `download` call, if it failed.
    pub fn last_failure(&self) -> Option<DownloadFailure> {
        *self.last_failure.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_failure(&self, failure: Option<DownloadFailure>) {
        *self.last_failure.lock().unwrap_or_else(|e| e.into_inner()) = failure;
    }

    fn configuration_failure(&self) -> RuntimeInstallErrorCode {
        self.set_failure(Some(DownloadFailure::ProxyConfiguration));
        RuntimeInstallErrorCode::NetworkDownloadFailed
    }

    /// Resolves the route for `url` and builds the curl invocation for one
    /// hop, plus the secret config (if any) to feed through stdin.
    fn prepare_hop(
        &self,
        url: &str,
        spec: &RuntimeInstallSpec,
        dest: &Path,
        header_path: &Path,
    ) -> Result<CurlHop, RuntimeInstallErrorCode> {
        let parsed = Url::parse(url).map_err(|_| RuntimeInstallErrorCode::DisallowedRedirectHost)?;
        let host = parsed.host_str().unwrap_or_default();
        let route = self
            .network
            .route(Scheme::Https, host)
            .map_err(|_| self.configuration_failure())?;
        let (proxy, credentials) = match route {
            ResolvedRoute::Direct => (None, None),
            ResolvedRoute::HttpProxy {
                endpoint,
                credentials,
            } => {
                let credentials = self
                    .network
                    .credentials(&credentials)
                    .map_err(|_| self.configuration_failure())?;
                (Some(endpoint), credentials)
            }
        };
        let mut command = build_curl_command(
            proxy.as_ref(),
            credentials.is_some(),
            spec.archive_size,
            url,
            dest,
            header_path,
            &self.seam_args,
        );
        let mut parent_names: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
        for (name, value) in &self.seam_envs {
            command.env(name, value);
            parent_names.push(name.clone());
        }
        strip_proxy_environment(&mut command, parent_names);
        Ok(CurlHop {
            command,
            stdin: credentials.as_ref().map(curl_proxy_user_config),
        })
    }

    fn download_hops(
        &self,
        spec: &RuntimeInstallSpec,
        dest: &Path,
        is_canceled: &dyn Fn() -> bool,
    ) -> Result<(), RuntimeInstallErrorCode> {
        let mut current_url = spec.url.clone();
        for _ in 0..=MAX_REDIRECT_HOPS {
            if is_canceled() {
                return Err(RuntimeInstallErrorCode::Canceled);
            }
            check_allowed_url(&current_url, &spec.allowed_hosts)?;
            let header_path = dest.with_extension("headers");
            let hop = self.prepare_hop(&current_url, spec, dest, &header_path)?;
            let outcome = match run_cancelable_io(hop.command, hop.stdin, true, is_canceled) {
                Ok(outcome) => outcome,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Err(RuntimeInstallErrorCode::Canceled);
                }
                Err(error) => {
                    self.set_failure(Some(DownloadFailure::CurlIo));
                    return Err(map_io_error(
                        &error,
                        RuntimeInstallErrorCode::NetworkDownloadFailed,
                    ));
                }
            };
            if !outcome.status.success() {
                self.set_failure(Some(classify_curl_failure(
                    outcome.status.code(),
                    &outcome.write_out,
                )));
                return Err(RuntimeInstallErrorCode::NetworkDownloadFailed);
            }
            let headers = match std::fs::read_to_string(&header_path) {
                Ok(headers) => headers,
                Err(error) => {
                    return Err(map_io_error(
                        &error,
                        RuntimeInstallErrorCode::NetworkDownloadFailed,
                    ));
                }
            };
            let _ = std::fs::remove_file(&header_path);
            let status_code = parse_http_status(&headers)
                .ok_or(RuntimeInstallErrorCode::NetworkDownloadFailed)?;
            if status_code == 200 {
                return verify_downloaded_size(dest, spec.archive_size);
            }
            if matches!(status_code, 301 | 302 | 303 | 307 | 308) {
                let location = find_header_value(&headers, "location")
                    .ok_or(RuntimeInstallErrorCode::NetworkDownloadFailed)?;
                current_url = resolve_redirect_url(&current_url, &location)?;
                continue;
            }
            self.set_failure(Some(DownloadFailure::OriginStatus(status_code)));
            return Err(RuntimeInstallErrorCode::NetworkDownloadFailed);
        }
        Err(RuntimeInstallErrorCode::NetworkDownloadFailed)
    }
}

impl ArchiveDownloader for CurlArchiveDownloader {
    fn download(
        &self,
        spec: &RuntimeInstallSpec,
        dest: &Path,
        is_canceled: &dyn Fn() -> bool,
    ) -> Result<(), RuntimeInstallErrorCode> {
        self.set_failure(None);
        let result = self.download_hops(spec, dest, is_canceled);
        if result.is_err() {
            // Never leave a partial archive or a header dump behind.
            let _ = std::fs::remove_file(dest);
            let _ = std::fs::remove_file(dest.with_extension("headers"));
        }
        result
    }
}

/// One prepared curl invocation. `stdin` holds the secret `--config -` text
/// when the proxy needs Basic credentials; it is never part of the argv.
struct CurlHop {
    command: Command,
    stdin: Option<Vec<u8>>,
}

/// Proxy-related variable names removed from curl's inherited environment
/// (compared case-insensitively). Certificate and PATH variables stay.
const CURL_PROXY_VARS: [&str; 6] = [
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ftp_proxy",
    "socks_proxy",
    "no_proxy",
];

fn strip_proxy_environment(command: &mut Command, names: impl IntoIterator<Item = OsString>) {
    for name in names {
        let is_proxy = name
            .to_str()
            .is_some_and(|text| CURL_PROXY_VARS.iter().any(|p| text.eq_ignore_ascii_case(p)));
        if is_proxy {
            command.env_remove(&name);
        }
    }
}

/// Builds the argv of one curl hop. The first option is `--disable` so no
/// curlrc can add a proxy or disable verification; the proxy is chosen only
/// here (`--proxy ""` for a direct route) and `--noproxy ""` stops curl from
/// applying a bypass list of its own. Neither `-k`, `--proxy-insecure` nor
/// `-L` is ever added. Credentials are not an argument: `--config -` reads
/// them from stdin.
fn build_curl_command(
    proxy: Option<&ProxyEndpoint>,
    has_credentials: bool,
    max_filesize: u64,
    url: &str,
    dest: &Path,
    header_path: &Path,
    extra_args: &[OsString],
) -> Command {
    let mut command = Command::new("curl");
    apply_no_console_window(&mut command);
    command.arg("--disable");
    command.arg("--silent");
    // Keep the proxy's CONNECT response out of the -D header dump.
    // The response is still parsed defensively below because curl can
    // expose more than one HTTP response block in proxy setups.
    command.arg("--suppress-connect-headers");
    command.arg("--proto").arg("=https");
    command.arg("--noproxy").arg("");
    match proxy {
        None => {
            command.arg("--proxy").arg("");
        }
        Some(endpoint) => {
            command.arg("--proxy").arg(endpoint.to_string());
            if has_credentials {
                command.arg("--proxy-basic").arg("--config").arg("-");
            }
        }
    }
    apply_download_timeout_args(&mut command);
    command.arg("--max-filesize").arg(max_filesize.to_string());
    command.arg("-D").arg(header_path);
    command.arg("-o").arg(dest);
    // The CONNECT status and the origin status are reported separately so a
    // proxy refusal is not mistaken for an origin answer.
    command.arg("-w").arg("%{http_connect} %{http_code}");
    command.arg("-A").arg("hane-runtime-install/1");
    command.args(extra_args);
    command.arg("--").arg(url);
    command
}

/// The curl config text that carries the proxy credential. Backslash and
/// double quote are escaped as curl's config parser requires; control
/// characters and a `:` in the user name were rejected before this point.
fn curl_proxy_user_config(credentials: &BasicCredentials) -> Vec<u8> {
    fn push_escaped(out: &mut String, text: &str) {
        for c in text.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                other => out.push(other),
            }
        }
    }
    let mut config = String::from("proxy-user = \"");
    push_escaped(&mut config, credentials.username().expose());
    config.push(':');
    push_escaped(&mut config, credentials.password().expose());
    config.push_str("\"\n");
    config.into_bytes()
}

/// Sorts a failed curl run into the secret-free categories using only the
/// exit code and the `%{http_connect}` value that `-w` printed.
fn classify_curl_failure(exit_code: Option<i32>, write_out: &str) -> DownloadFailure {
    let connect = write_out
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    match connect {
        407 => DownloadFailure::ProxyAuthentication,
        0 | 200 => DownloadFailure::CurlExit(exit_code.unwrap_or(-1)),
        other => DownloadFailure::ProxyConnect(other),
    }
}

/// Extracts by shelling out to the system `tar -xzf`. See the module docs
/// for why this does not reimplement a tar/gzip parser in Rust.
#[derive(Debug, Default)]
pub struct TarArchiveExtractor;

impl ArchiveExtractor for TarArchiveExtractor {
    fn extract(
        &self,
        archive: &Path,
        dest_dir: &Path,
        is_canceled: &dyn Fn() -> bool,
    ) -> Result<(), RuntimeInstallErrorCode> {
        if is_canceled() {
            return Err(RuntimeInstallErrorCode::Canceled);
        }
        let mut command = Command::new("tar");
        apply_no_console_window(&mut command);
        command
            .arg("--no-same-owner")
            .arg("--no-same-permissions")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(dest_dir);
        let status = match run_cancelable(command, is_canceled) {
            Ok(status) => status,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return Err(RuntimeInstallErrorCode::Canceled);
            }
            Err(error) => {
                return Err(map_io_error(
                    &error,
                    RuntimeInstallErrorCode::ExtractionFailed,
                ));
            }
        };
        if status.success() {
            Ok(())
        } else {
            Err(RuntimeInstallErrorCode::ExtractionFailed)
        }
    }
}

#[cfg(windows)]
fn apply_no_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // Matches `RuntimeConfig::spawn_child`: suppress the console window
    // Windows would otherwise flash for these console-subsystem helper
    // executables.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn apply_no_console_window(_command: &mut Command) {}

/// Bounds connection setup and detects stalled transfers without limiting the
/// total time for a transfer that continues to make progress.
fn apply_download_timeout_args(command: &mut Command) {
    command
        .arg("--connect-timeout")
        .arg(DOWNLOAD_CONNECT_TIMEOUT.as_secs().to_string())
        .arg("--speed-limit")
        .arg(DOWNLOAD_SPEED_LIMIT.to_string())
        .arg("--speed-time")
        .arg(DOWNLOAD_SPEED_TIME.as_secs().to_string());
}

/// Spawns `command` (with stdin/stdout/stderr all discarded -- this module
/// only cares about `-o`/`-D` file outputs and the exit status) and polls
/// for completion, killing it promptly once `is_canceled` returns `true`.
fn run_cancelable(
    command: Command,
    is_canceled: &dyn Fn() -> bool,
) -> io::Result<std::process::ExitStatus> {
    run_cancelable_io(command, None, false, is_canceled).map(|outcome| outcome.status)
}

struct RunOutcome {
    status: std::process::ExitStatus,
    /// What the child wrote to stdout (only captured on request, and only the
    /// first few bytes: it is curl's `-w` text, never a body or a secret).
    write_out: String,
}

/// Like [`run_cancelable`], optionally feeding `input` to the child's stdin.
/// The input is written on its own thread and the pipe is closed after the
/// write, so a child that never reads, exits early or is cancelled can not
/// block this function: the child is always reaped and the pipe released
/// before it returns. stderr is always discarded.
fn run_cancelable_io(
    mut command: Command,
    input: Option<Vec<u8>>,
    capture_stdout: bool,
    is_canceled: &dyn Fn() -> bool,
) -> io::Result<RunOutcome> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let mut writer: Option<std::thread::JoinHandle<io::Result<()>>> = None;
    if let Some(bytes) = input
        && let Some(mut stdin) = child.stdin.take()
    {
        writer = Some(std::thread::spawn(move || {
            let result = stdin.write_all(&bytes).and_then(|()| stdin.flush());
            drop(stdin);
            result
        }));
    }
    loop {
        if let Err(error) = take_writer_result(&mut writer, false) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // The child's end of the pipe is closed, so the writer ends.
                let _ = take_writer_result(&mut writer, true);
                let mut write_out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = (&mut stdout).take(256).read_to_string(&mut write_out);
                }
                return Ok(RunOutcome { status, write_out });
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = take_writer_result(&mut writer, true);
                return Err(error);
            }
        }
        if is_canceled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = take_writer_result(&mut writer, true);
            return Err(io::Error::new(io::ErrorKind::Interrupted, "canceled"));
        }
        std::thread::sleep(SUBPROCESS_POLL_INTERVAL);
    }
}

/// Collects the stdin writer's result. A broken pipe only means the child
/// stopped reading (it reports its own outcome through the exit status), so it
/// is not an error; any other write failure is.
fn take_writer_result(
    writer: &mut Option<std::thread::JoinHandle<io::Result<()>>>,
    wait: bool,
) -> io::Result<()> {
    let Some(handle) = writer.as_ref() else {
        return Ok(());
    };
    if !wait && !handle.is_finished() {
        return Ok(());
    }
    match writer.take().map(std::thread::JoinHandle::join) {
        Some(Ok(Err(error))) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
        _ => Ok(()),
    }
}

/// Validates `raw_url` is `https`, carries no embedded credentials, uses the
/// default HTTPS port (443) if any, and resolves to a host in
/// `allowed_hosts` -- checked before the initial request and before
/// following every redirect hop, exactly like
/// the direct-download host allowlist.
fn check_allowed_url(
    raw_url: &str,
    allowed_hosts: &[String],
) -> Result<(), RuntimeInstallErrorCode> {
    let parsed =
        Url::parse(raw_url).map_err(|_| RuntimeInstallErrorCode::DisallowedRedirectHost)?;
    let host = parsed.host_str().unwrap_or_default();
    let host_allowed = allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host));
    let safe = parsed.scheme() == "https"
        && host_allowed
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port_or_known_default() == Some(443);
    if safe {
        Ok(())
    } else {
        Err(RuntimeInstallErrorCode::DisallowedRedirectHost)
    }
}

fn resolve_redirect_url(current: &str, location: &str) -> Result<String, RuntimeInstallErrorCode> {
    let base = Url::parse(current).map_err(|_| RuntimeInstallErrorCode::DisallowedRedirectHost)?;
    let resolved = base
        .join(location)
        .map_err(|_| RuntimeInstallErrorCode::DisallowedRedirectHost)?;
    Ok(resolved.into())
}

/// Returns the final HTTP response block from a curl `-D`-dumped header.
///
/// A proxy can prepend a `200 Connection established` block before the
/// release response, so the first block is not necessarily the response for
/// the requested URL. Curl uses CRLF, but accepting LF as well keeps this
/// parser straightforward to exercise and defensive for alternate transports.
fn last_http_response_block(headers: &str) -> Option<String> {
    let normalized = headers.replace("\r\n", "\n");
    let mut last_block = None;
    for block in normalized.split("\n\n") {
        if block
            .lines()
            .next()
            .is_some_and(|line| line.trim_start().starts_with("HTTP/"))
        {
            last_block = Some(block.to_owned());
        }
    }
    last_block
}

/// Parses the status code from the final response block in a curl `-D`-dumped
/// header (`HTTP/1.1 302 Found\r\n...`).
fn parse_http_status(headers: &str) -> Option<u16> {
    let block = last_http_response_block(headers)?;
    let first_line = block.lines().next()?.trim();
    first_line.split_whitespace().nth(1)?.parse().ok()
}

/// Finds the value of the header named `name` (case-insensitive) in the final
/// response block of a curl `-D`-dumped header.
fn find_header_value(headers: &str, name: &str) -> Option<String> {
    last_http_response_block(headers)?.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case(name) {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}

fn verify_downloaded_size(path: &Path, expected_size: u64) -> Result<(), RuntimeInstallErrorCode> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        map_io_error(&error, RuntimeInstallErrorCode::NetworkDownloadFailed)
    })?;
    if metadata.len() == expected_size {
        Ok(())
    } else {
        Err(RuntimeInstallErrorCode::SizeMismatch)
    }
}

fn verify_archive_integrity(
    path: &Path,
    expected_sha256_hex: &str,
) -> Result<(), RuntimeInstallErrorCode> {
    let actual = runtime_distribution::sha256_hex_of_file(path).map_err(|error| {
        map_distribution_error(error, RuntimeInstallErrorCode::IntegrityMismatch)
    })?;
    if actual.eq_ignore_ascii_case(expected_sha256_hex.trim()) {
        Ok(())
    } else {
        Err(RuntimeInstallErrorCode::IntegrityMismatch)
    }
}

fn has_valid_relative_path(relative: &str) -> bool {
    use std::path::Component;
    let path = Path::new(relative);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Walks the extracted tree at `payload_dir`, building a `path`-sorted
/// inventory of every regular file found (POSIX-style `/`-joined relative
/// paths), rejecting any symlink/reparse point, case-fold path collision, or
/// per-entry/total size over the fixed caps. See the module docs for why
/// this walk is a defense-in-depth check, not this module's primary
/// integrity guarantee (the whole-archive SHA-256 check already is).
fn build_inventory_from_extracted_tree(
    payload_dir: &Path,
) -> Result<Vec<RuntimeManifestFile>, RuntimeInstallErrorCode> {
    let mut inventory = Vec::new();
    let mut seen_casefold = std::collections::HashSet::new();
    let mut total_size: u64 = 0;
    let mut stack = vec![PathBuf::new()];
    while let Some(relative_dir) = stack.pop() {
        let absolute_dir = payload_dir.join(&relative_dir);
        let entries = std::fs::read_dir(&absolute_dir).map_err(|error| {
            map_io_error(&error, RuntimeInstallErrorCode::ExtractionFailed)
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                map_io_error(&error, RuntimeInstallErrorCode::ExtractionFailed)
            })?;
            let relative_path = relative_dir.join(entry.file_name());
            let absolute_path = payload_dir.join(&relative_path);
            let not_symlink = runtime_distribution::check_not_symlink(&absolute_path)
                .map_err(|error| {
                    map_distribution_error(error, RuntimeInstallErrorCode::ExtractionFailed)
                })?;
            if !not_symlink {
                // Vanished between `read_dir` and this check; treat as not
                // present rather than racily reporting a false positive.
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|error| {
                    map_io_error(&error, RuntimeInstallErrorCode::ExtractionFailed)
                })?;
            if file_type.is_dir() {
                stack.push(relative_path);
                continue;
            }
            if !file_type.is_file() {
                return Err(RuntimeInstallErrorCode::ExtractionFailed);
            }
            let posix_path = relative_path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !seen_casefold.insert(posix_path.to_lowercase()) {
                return Err(RuntimeInstallErrorCode::ExtractionFailed);
            }
            let metadata = std::fs::metadata(&absolute_path).map_err(|error| {
                map_io_error(&error, RuntimeInstallErrorCode::ExtractionFailed)
            })?;
            let size = metadata.len();
            if size > MAX_ENTRY_SIZE {
                return Err(RuntimeInstallErrorCode::ExtractionFailed);
            }
            total_size = total_size.saturating_add(size);
            if total_size > MAX_TOTAL_SIZE {
                return Err(RuntimeInstallErrorCode::ExtractionFailed);
            }
            let sha256 = runtime_distribution::sha256_hex_of_file(&absolute_path).map_err(
                |error| map_distribution_error(error, RuntimeInstallErrorCode::ExtractionFailed),
            )?;
            let executable = is_executable(&metadata);
            inventory.push(RuntimeManifestFile {
                path: posix_path,
                kind: "file".to_string(),
                size,
                sha256,
                executable,
            });
        }
    }
    inventory.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(inventory)
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    // Windows has no equivalent permission bit; the pinned inventory's
    // `executable` flag is matched below only on Unix, mirroring
    // `crate::runtime_distribution::validate_inventory_file`.
    true
}

/// Compares `actual` (built from the real, just-extracted tree) against
/// `pinned` (from the repository's own pinned inventory file) path-for-path:
/// every pinned entry must be present with exactly the same size/sha256, and
/// `actual` must not contain anything pinned does not also list.
fn diff_against_pinned_inventory(
    pinned: &[RuntimeManifestFile],
    actual: &[RuntimeManifestFile],
) -> Result<(), RuntimeInstallErrorCode> {
    if pinned.len() != actual.len() {
        return Err(RuntimeInstallErrorCode::ExtractionFailed);
    }
    let actual_by_path: std::collections::HashMap<&str, &RuntimeManifestFile> =
        actual.iter().map(|file| (file.path.as_str(), file)).collect();
    for expected in pinned {
        let Some(found) = actual_by_path.get(expected.path.as_str()) else {
            return Err(RuntimeInstallErrorCode::ExtractionFailed);
        };
        let matches = found.kind == expected.kind
            && found.size == expected.size
            && found.sha256.eq_ignore_ascii_case(&expected.sha256)
            && (!cfg!(unix) || found.executable == expected.executable);
        if !matches {
            return Err(RuntimeInstallErrorCode::ExtractionFailed);
        }
    }
    Ok(())
}

/// Finds the single extracted entry whose final path component equals
/// `entrypoint_name` and is marked executable. Mirrors
/// the pinned entrypoint contract.
fn resolve_entrypoint(
    inventory: &[RuntimeManifestFile],
    entrypoint_name: &str,
) -> Result<String, RuntimeInstallErrorCode> {
    let mut matches = inventory
        .iter()
        .filter(|file| file.executable && file.path.rsplit('/').next() == Some(entrypoint_name));
    let first = matches.next().ok_or(RuntimeInstallErrorCode::AmbiguousEntrypoint)?;
    if matches.next().is_some() {
        return Err(RuntimeInstallErrorCode::AmbiguousEntrypoint);
    }
    Ok(first.path.clone())
}

/// Spawns the candidate entrypoint once, through the existing
/// `crate::runtime::AiRuntime`/`RuntimeConfig` coordinator, scoped entirely
/// to a dedicated, throwaway `CODEX_HOME`/working directory/owner-lock path
/// under this attempt's own staging directory -- never the real app data,
/// the real runtime owner lock, or the user's actual credentials/`HOME` (see
/// `RuntimeConfig::spawn_child`, which this reuses unchanged). Confirms the
/// `initialize` handshake completes and the subsequent stop can be
/// confirmed; returns `Ok(())` only if both hold.
fn verify_candidate_runtime(
    entrypoint: &Path,
    verify_root: &Path,
) -> Result<(), RuntimeInstallErrorCode> {
    std::fs::create_dir_all(verify_root).map_err(|error| {
        map_io_error(&error, RuntimeInstallErrorCode::VerificationFailed)
    })?;
    let codex_home = verify_root.join("codex-home");
    std::fs::create_dir_all(&codex_home).map_err(|error| {
        map_io_error(&error, RuntimeInstallErrorCode::VerificationFailed)
    })?;
    let mut config = RuntimeConfig::new(entrypoint, verify_root.join("owner.lock"));
    config.codex_home = Some(codex_home);
    config.inherit_parent_home = false;
    config.working_directory = Some(verify_root.join("workspace"));
    config.start_timeout = VERIFY_START_TIMEOUT;
    let (events_tx, _events_rx) = mpsc::sync_channel(16);
    let runtime = AiRuntime::spawn(config, Arc::new(RejectAllServerRequests), events_tx);
    let start_result = runtime.start();
    let start_permission_denied = matches!(
        &start_result,
        Err(RuntimeError::OwnerLock(error) | RuntimeError::Spawn(error))
            if error.kind() == io::ErrorKind::PermissionDenied
    );
    let started = matches!(start_result, Ok(status) if status.state == RuntimeState::Ready);
    let shutdown_status = runtime.shutdown();
    let stopped = shutdown_status.state == RuntimeState::Stopped;
    if started && stopped {
        Ok(())
    } else if start_permission_denied {
        Err(RuntimeInstallErrorCode::PermissionDenied)
    } else {
        Err(RuntimeInstallErrorCode::VerificationFailed)
    }
}

/// Runs the full first-use install pipeline for `spec` into `staging_dir`
/// (already created by the caller, empty), downloading via `downloader` and
/// extracting via `extractor`. `progress` is called at each phase boundary
/// so a caller can mirror it into `AiSnapshot::runtime_install`.
///
/// On success, returns the path (always `staging_dir.join("payload")`) of
/// the completed, manifest-described install -- still inside `staging_dir`,
/// not yet published. The caller atomically publishes it by replacing
/// `AiPaths::runtime_install_dir` with exactly this directory (e.g. via
/// `std::fs::rename`) and is responsible for removing whatever remains of
/// `staging_dir` afterward. On any error or cancellation, the caller must
/// remove `staging_dir` as a whole: this function never does so itself, so
/// the caller can always find the exact same path it passed in.
#[allow(clippy::too_many_arguments)]
pub fn install_runtime(
    spec: &RuntimeInstallSpec,
    staging_dir: &Path,
    downloader: &dyn ArchiveDownloader,
    extractor: &dyn ArchiveExtractor,
    is_canceled: &dyn Fn() -> bool,
    mut progress: impl FnMut(RuntimeInstallState),
) -> Result<PathBuf, RuntimeInstallErrorCode> {
    if is_canceled() {
        return Err(RuntimeInstallErrorCode::Canceled);
    }
    progress(RuntimeInstallState::Downloading);
    let archive_path = staging_dir.join("archive.tar.gz");
    downloader.download(spec, &archive_path, is_canceled)?;
    if is_canceled() {
        return Err(RuntimeInstallErrorCode::Canceled);
    }
    verify_archive_integrity(&archive_path, &spec.archive_sha256)?;

    progress(RuntimeInstallState::Verifying);
    if is_canceled() {
        return Err(RuntimeInstallErrorCode::Canceled);
    }
    let payload_dir = staging_dir.join("payload");
    std::fs::create_dir_all(&payload_dir).map_err(|error| {
        map_io_error(&error, RuntimeInstallErrorCode::StagingFailed)
    })?;
    extractor.extract(&archive_path, &payload_dir, is_canceled)?;
    let _ = std::fs::remove_file(&archive_path);
    if is_canceled() {
        return Err(RuntimeInstallErrorCode::Canceled);
    }

    let actual_inventory = build_inventory_from_extracted_tree(&payload_dir)?;
    diff_against_pinned_inventory(&spec.pinned_inventory, &actual_inventory)?;
    let entrypoint_relative = resolve_entrypoint(&actual_inventory, &spec.entrypoint_name)?;

    if is_canceled() {
        return Err(RuntimeInstallErrorCode::Canceled);
    }
    let entrypoint_absolute = payload_dir.join(&entrypoint_relative);
    let verify_root = staging_dir.join("verify");
    let verify_result = verify_candidate_runtime(&entrypoint_absolute, &verify_root);
    let _ = std::fs::remove_dir_all(&verify_root);
    verify_result?;

    let manifest = RuntimeManifest {
        schema_version: 1,
        bundle_id: format!("codex-app-server-{}", spec.target),
        target: spec.target.clone(),
        codex_version: spec.codex_version.clone(),
        entrypoint: entrypoint_relative,
        source_commit: spec.source_commit.clone(),
        archive_sha256: spec.archive_sha256.clone(),
        files: actual_inventory,
        // Hane downloads and installs the upstream runtime directly into its
        // own app-data directory; it does not redistribute license documents.
        licenses: Vec::new(),
        runtime_profile: RuntimeManifestProfile {
            name: spec.runtime_profile.clone(),
            notes: None,
        },
        config_profile: RuntimeManifestProfile {
            name: spec.config_profile.clone(),
            notes: None,
        },
        state_profile: RuntimeManifestProfile {
            name: spec.state_profile.clone(),
            notes: None,
        },
    };
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|_| RuntimeInstallErrorCode::StagingFailed)?;
    std::fs::write(payload_dir.join("manifest.json"), manifest_json).map_err(|error| {
        map_io_error(&error, RuntimeInstallErrorCode::StagingFailed)
    })?;
    Ok(payload_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_download_uses_connection_and_stall_timeouts_without_total_time_limit() {
        let mut command = Command::new("curl");
        apply_download_timeout_args(&mut command);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "--connect-timeout".to_owned(),
                "30".to_owned(),
                "--speed-limit".to_owned(),
                "1024".to_owned(),
                "--speed-time".to_owned(),
                "60".to_owned(),
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--max-time"));
    }

    #[test]
    fn permission_denied_install_errors_stay_distinct_from_generic_failures() {
        assert_eq!(
            map_io_error(
                &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                RuntimeInstallErrorCode::StagingFailed,
            ),
            RuntimeInstallErrorCode::PermissionDenied
        );
        assert_eq!(
            map_io_error(
                &std::io::Error::other("generic install failure"),
                RuntimeInstallErrorCode::StagingFailed,
            ),
            RuntimeInstallErrorCode::StagingFailed
        );
        assert_eq!(
            map_distribution_error(
                runtime_distribution::RuntimeDistributionErrorCode::AccessDenied,
                RuntimeInstallErrorCode::ExtractionFailed,
            ),
            RuntimeInstallErrorCode::PermissionDenied
        );
        assert_eq!(
            map_distribution_error(
                runtime_distribution::RuntimeDistributionErrorCode::InvalidExecutable,
                RuntimeInstallErrorCode::ExtractionFailed,
            ),
            RuntimeInstallErrorCode::ExtractionFailed
        );
        assert_eq!(
            RuntimeInstallErrorCode::PermissionDenied.stable_code(),
            "runtime_install_permission_denied"
        );
    }

    #[test]
    fn lock_keeps_all_fixed_archive_sizes_and_hashes() {
        let lock: LockFile = serde_json::from_str(LOCK_JSON).unwrap();
        let windows = lock.targets.get(WINDOWS_X64_TARGET).unwrap();
        assert_eq!(windows.archive_size, WINDOWS_X64_ARCHIVE_SIZE);
        assert_eq!(windows.archive_sha256, WINDOWS_X64_ARCHIVE_SHA256);
        let windows_arm64 = lock.targets.get(WINDOWS_ARM64_TARGET).unwrap();
        assert_eq!(windows_arm64.archive_size, WINDOWS_ARM64_ARCHIVE_SIZE);
        assert_eq!(windows_arm64.archive_sha256, WINDOWS_ARM64_ARCHIVE_SHA256);
        let macos = lock.targets.get(MACOS_ARM64_TARGET).unwrap();
        assert_eq!(macos.archive_size, MACOS_ARM64_ARCHIVE_SIZE);
        assert_eq!(macos.archive_sha256, MACOS_ARM64_ARCHIVE_SHA256);
    }

    #[test]
    fn windows_arm64_install_spec_uses_the_pinned_native_package() {
        let spec = install_spec_for_target(WINDOWS_ARM64_TARGET).unwrap();
        assert_eq!(spec.target, WINDOWS_ARM64_TARGET);
        assert_eq!(spec.archive_size, WINDOWS_ARM64_ARCHIVE_SIZE);
        assert_eq!(spec.archive_sha256, WINDOWS_ARM64_ARCHIVE_SHA256);
        assert!(spec.url.ends_with("codex-app-server-package-aarch64-pc-windows-msvc.tar.gz"));
        assert_eq!(spec.entrypoint_name, "codex-app-server.exe");
        assert_eq!(spec.pinned_inventory.len(), 6);
        assert!(spec.pinned_inventory.iter().any(|file| {
            file.path == "bin/codex-app-server.exe" && file.executable
        }));
    }

    #[test]
    fn initial_and_redirect_urls_are_limited_to_https_release_hosts() {
        let allowed = ALLOWED_HOSTS.iter().map(|host| (*host).to_owned()).collect::<Vec<_>>();
        assert!(check_allowed_url(
            "https://github.com/openai/codex/releases/download/rust-v0.157.1/package.tar.gz",
            &allowed,
        )
        .is_ok());
        assert!(check_allowed_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/123456789/abcdef/package.tar.gz",
            &allowed,
        )
        .is_ok());
        assert!(check_allowed_url("http://github.com/package.tar.gz", &allowed).is_err());
        assert!(check_allowed_url("https://evil.example/package.tar.gz", &allowed).is_err());
        assert!(check_allowed_url(
            "https://release-assets.githubusercontent.com.evil.example/package.tar.gz",
            &allowed,
        )
        .is_err());
        assert!(check_allowed_url("https://github.com:8443/package.tar.gz", &allowed).is_err());
        assert!(check_allowed_url("https://user:secret@github.com/package.tar.gz", &allowed).is_err());
    }

    #[test]
    fn proxy_connect_response_is_ignored_when_parsing_a_release_redirect() {
        let headers = concat!(
            "HTTP/1.1 200 Connection established\r\n",
            "Proxy-Agent: test-proxy\r\n",
            "\r\n",
            "HTTP/1.1 302 Found\r\n",
            "Location: https://release-assets.githubusercontent.com/archive.tar.gz\r\n",
            "\r\n",
        );

        assert_eq!(parse_http_status(headers), Some(302));
        assert_eq!(
            find_header_value(headers, "location"),
            Some("https://release-assets.githubusercontent.com/archive.tar.gz".to_owned())
        );
    }

    #[test]
    fn proxy_connect_response_is_ignored_when_parsing_the_final_success() {
        let headers = concat!(
            "HTTP/1.1 200 Connection established\n",
            "\n",
            "HTTP/2 200\n",
            "content-length: 123\n",
            "\n",
        );

        assert_eq!(parse_http_status(headers), Some(200));
        assert_eq!(find_header_value(headers, "location"), None);
    }

    #[test]
    fn canceled_install_stops_before_a_downloader_or_extractor_runs() {
        struct UnexpectedDownloader;
        impl ArchiveDownloader for UnexpectedDownloader {
            fn download(
                &self,
                _spec: &RuntimeInstallSpec,
                _dest: &Path,
                _is_canceled: &dyn Fn() -> bool,
            ) -> Result<(), RuntimeInstallErrorCode> {
                panic!("downloader must not run after cancellation");
            }
        }
        struct UnexpectedExtractor;
        impl ArchiveExtractor for UnexpectedExtractor {
            fn extract(
                &self,
                _archive: &Path,
                _dest_dir: &Path,
                _is_canceled: &dyn Fn() -> bool,
            ) -> Result<(), RuntimeInstallErrorCode> {
                panic!("extractor must not run after cancellation");
            }
        }

        let spec = install_spec_for_target(WINDOWS_X64_TARGET).unwrap();
        let staging = std::env::temp_dir().join(format!(
            "hane-runtime-install-canceled-{}",
            std::process::id()
        ));
        let result = install_runtime(
            &spec,
            &staging,
            &UnexpectedDownloader,
            &UnexpectedExtractor,
            &|| true,
            |_| panic!("progress must not run after cancellation"),
        );
        assert_eq!(result, Err(RuntimeInstallErrorCode::Canceled));
    }

    #[test]
    fn candidate_verification_fails_closed_when_codex_home_cannot_be_created() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let verify_root = std::env::temp_dir().join(format!(
            "hane-runtime-install-codex-home-failure-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&verify_root).unwrap();
        std::fs::write(verify_root.join("codex-home"), b"not a directory").unwrap();

        let result = verify_candidate_runtime(
            Path::new("/missing/codex-app-server"),
            &verify_root,
        );

        assert_eq!(result, Err(RuntimeInstallErrorCode::VerificationFailed));
        let _ = std::fs::remove_dir_all(verify_root);
    }

    #[cfg(unix)]
    #[test]
    fn install_runtime_accepts_a_verified_package_without_license_documents() {
        const EMPTY_ARCHIVE_SHA256: &str =
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        const FAKE_APP_SERVER: &[u8] = b"#!/bin/sh\nset -eu\ntest -d \"${CODEX_HOME:?}\"\nwhile IFS= read -r line; do\n    case \"$line\" in\n        *'\"method\":\"initialize\"'*)\n            id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9][0-9]*\\).*/\\1/p')\n            printf '{\"id\":%s,\"result\":{\"ok\":true}}\\n' \"$id\"\n            ;;\n    esac\ndone\n";

        struct EmptyArchiveDownloader;
        impl ArchiveDownloader for EmptyArchiveDownloader {
            fn download(
                &self,
                _spec: &RuntimeInstallSpec,
                dest: &Path,
                _is_canceled: &dyn Fn() -> bool,
            ) -> Result<(), RuntimeInstallErrorCode> {
                std::fs::write(dest, b"")
                    .map_err(|_| RuntimeInstallErrorCode::NetworkDownloadFailed)
            }
        }

        struct FakeAppServerExtractor(&'static [u8]);
        impl ArchiveExtractor for FakeAppServerExtractor {
            fn extract(
                &self,
                _archive: &Path,
                dest_dir: &Path,
                _is_canceled: &dyn Fn() -> bool,
            ) -> Result<(), RuntimeInstallErrorCode> {
                let path = dest_dir.join("codex-app-server");
                std::fs::write(&path, self.0)
                    .map_err(|_| RuntimeInstallErrorCode::ExtractionFailed)?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .map_err(|_| RuntimeInstallErrorCode::ExtractionFailed)
            }
        }

        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-runtime-install-empty-licenses-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let hash_source = root.join("hash-source");
        std::fs::write(&hash_source, FAKE_APP_SERVER).unwrap();
        let executable_sha256 = runtime_distribution::sha256_hex_of_file(&hash_source).unwrap();
        let _ = std::fs::remove_file(&hash_source);

        let spec = RuntimeInstallSpec {
            target: "test-target".to_owned(),
            codex_version: "0.157.1".to_owned(),
            source_commit: "test-commit".to_owned(),
            url: "https://github.com/example/runtime.tar.gz".to_owned(),
            archive_size: 0,
            archive_sha256: EMPTY_ARCHIVE_SHA256.to_owned(),
            entrypoint_name: "codex-app-server".to_owned(),
            allowed_hosts: vec!["github.com".to_owned()],
            pinned_inventory: vec![RuntimeManifestFile {
                path: "codex-app-server".to_owned(),
                kind: "file".to_owned(),
                size: FAKE_APP_SERVER.len() as u64,
                sha256: executable_sha256,
                executable: true,
            }],
            runtime_profile: "default".to_owned(),
            config_profile: "default".to_owned(),
            state_profile: "default".to_owned(),
        };
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let mut progress = Vec::new();
        let result = install_runtime(
            &spec,
            &staging,
            &EmptyArchiveDownloader,
            &FakeAppServerExtractor(FAKE_APP_SERVER),
            &|| false,
            |state| progress.push(state),
        )
        .unwrap();

        let manifest: RuntimeManifest =
            serde_json::from_slice(&std::fs::read(result.join("manifest.json")).unwrap())
                .unwrap();
        assert!(manifest.licenses.is_empty());
        assert!(!result.join("licenses").exists());
        assert_eq!(
            progress,
            vec![RuntimeInstallState::Downloading, RuntimeInstallState::Verifying]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn inventory_comparison_rejects_extra_files_and_case_collisions() {
        let file = RuntimeManifestFile {
            path: "bin/codex-app-server".to_owned(),
            kind: "file".to_owned(),
            size: 1,
            sha256: "0".repeat(64),
            executable: true,
        };
        assert_eq!(
            diff_against_pinned_inventory(
                std::slice::from_ref(&file),
                &[file.clone(), file.clone()],
            ),
            Err(RuntimeInstallErrorCode::ExtractionFailed)
        );
        assert_eq!(
            resolve_entrypoint(
                &[
                    file.clone(),
                    RuntimeManifestFile {
                        path: "other/codex-app-server".to_owned(),
                        ..file
                    },
                ],
                "codex-app-server",
            ),
            Err(RuntimeInstallErrorCode::AmbiguousEntrypoint)
        );
    }

    fn argv(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn proxy_endpoint(url: &str) -> ProxyEndpoint {
        use hane_network::{
            EnvPlatform, NetworkContext, ProxyEnvironment, ResolvedRoute, Scheme,
        };
        let context = NetworkContext::from_environment(ProxyEnvironment::from_vars(
            EnvPlatform::MacOs,
            [("https_proxy", url)],
        ));
        match context.route(Scheme::Https, "github.com").unwrap() {
            ResolvedRoute::HttpProxy { endpoint, .. } => endpoint,
            ResolvedRoute::Direct => panic!("expected a proxy route"),
        }
    }

    #[test]
    fn curl_command_starts_with_disable_and_never_weakens_tls_or_follows_redirects() {
        let endpoint = proxy_endpoint("http://proxy.example:3128");
        for (proxy, credentials) in [(None, false), (Some(&endpoint), false), (Some(&endpoint), true)]
        {
            let command = build_curl_command(
                proxy,
                credentials,
                10,
                "https://github.com/a",
                Path::new("dest"),
                Path::new("dest.headers"),
                &[],
            );
            let args = argv(&command);
            assert_eq!(args[0], "--disable");
            for forbidden in ["-k", "--insecure", "--proxy-insecure", "-L", "--location"] {
                assert!(!args.iter().any(|arg| arg == forbidden), "{forbidden}");
            }
            let position = |name: &str| args.iter().position(|arg| arg == name).unwrap();
            assert_eq!(args[position("--proto") + 1], "=https");
            assert_eq!(args[position("--noproxy") + 1], "");
            assert!(args.contains(&"--suppress-connect-headers".to_owned()));
            assert!(args.contains(&"--connect-timeout".to_owned()));
            assert!(args.contains(&"--speed-time".to_owned()));
            assert_eq!(args[args.len() - 2], "--");
            assert_eq!(args[args.len() - 1], "https://github.com/a");
            match proxy {
                None => assert_eq!(args[position("--proxy") + 1], ""),
                Some(_) => assert_eq!(args[position("--proxy") + 1], "http://proxy.example:3128"),
            }
            assert_eq!(args.contains(&"--config".to_owned()), credentials);
            if credentials {
                assert_eq!(args[position("--config") + 1], "-");
            }
        }
    }

    #[test]
    fn proxy_credentials_never_appear_in_the_argv_and_are_escaped_for_the_config() {
        let credentials = BasicCredentials::new("user", "p:\"\\ss word#");
        let config = String::from_utf8(curl_proxy_user_config(&credentials)).unwrap();
        assert_eq!(config, "proxy-user = \"user:p:\\\"\\\\ss word#\"\n");

        let endpoint = proxy_endpoint("http://proxy.example:3128");
        let command = build_curl_command(
            Some(&endpoint),
            true,
            10,
            "https://github.com/a",
            Path::new("dest"),
            Path::new("dest.headers"),
            &[],
        );
        let joined = argv(&command).join(" ");
        assert!(!joined.contains("ss word"));
        assert!(!joined.contains("user:"));
        assert!(!format!("{command:?}").contains("ss word"));
    }

    #[test]
    fn proxy_variables_are_removed_from_the_child_in_any_case() {
        let mut command = Command::new("curl");
        strip_proxy_environment(
            &mut command,
            [
                "HTTPS_PROXY",
                "https_proxy",
                "All_Proxy",
                "NO_PROXY",
                "ftp_proxy",
                "SOCKS_PROXY",
                "SSL_CERT_FILE",
                "PATH",
            ]
            .map(OsString::from),
        );
        let removed: Vec<String> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(removed.len(), 6, "{removed:?}");
        assert!(!removed.iter().any(|name| name == "SSL_CERT_FILE" || name == "PATH"));
    }

    #[test]
    fn curl_failures_are_sorted_without_secret_text() {
        assert_eq!(
            classify_curl_failure(Some(56), "407 000"),
            DownloadFailure::ProxyAuthentication
        );
        assert_eq!(
            classify_curl_failure(Some(56), "403 000"),
            DownloadFailure::ProxyConnect(403)
        );
        assert_eq!(
            classify_curl_failure(Some(60), "200 000"),
            DownloadFailure::CurlExit(60)
        );
        assert_eq!(
            classify_curl_failure(None, ""),
            DownloadFailure::CurlExit(-1)
        );
    }

    #[test]
    fn an_unusable_credential_never_reaches_curl() {
        use hane_network::{
            CredentialError, CredentialRef, CredentialStore, EnvPlatform, ManualProxy,
            NetworkSettings, ProxyAuth, ProxyEnvironment,
        };
        struct Missing;
        impl CredentialStore for Missing {
            fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
                Err(CredentialError::NotFound)
            }
        }
        let manual = ManualProxy::new(
            "http://proxy.example:3128",
            "",
            ProxyAuth::Basic(CredentialRef::new("c1").unwrap()),
        )
        .unwrap();
        let downloader = CurlArchiveDownloader::new(NetworkContext::new(
            NetworkSettings::manual(manual),
            ProxyEnvironment::from_vars(EnvPlatform::MacOs, std::iter::empty::<(&str, &str)>()),
            Arc::new(Missing),
        ));
        let spec = install_spec_for_target(WINDOWS_X64_TARGET).unwrap();
        let dir = std::env::temp_dir().join(format!("hane-curl-config-{}", std::process::id()));
        let result = downloader.download(&spec, &dir.join("archive"), &|| false);
        assert_eq!(result, Err(RuntimeInstallErrorCode::NetworkDownloadFailed));
        assert_eq!(
            downloader.last_failure(),
            Some(DownloadFailure::ProxyConfiguration)
        );
    }

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        command
    }

    #[cfg(unix)]
    #[test]
    fn stdin_input_to_a_child_that_exits_without_reading_does_not_block() {
        // 4 MiB is far more than a pipe buffer, so the write can only finish
        // because the child's exit closes the pipe.
        let started = std::time::Instant::now();
        let outcome =
            run_cancelable_io(shell("exit 3"), Some(vec![b'x'; 4 << 20]), true, &|| false).unwrap();
        assert_eq!(outcome.status.code(), Some(3));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_reads_its_stdin_to_eof_sees_the_whole_input() {
        let outcome = run_cancelable_io(
            shell("wc -c | tr -d ' '"),
            Some(b"proxy-user = \"a:b\"\n".to_vec()),
            true,
            &|| false,
        )
        .unwrap();
        assert!(outcome.status.success());
        assert_eq!(outcome.write_out.trim(), "19");
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_child_that_never_reads_stdin_kills_it_and_returns() {
        let started = std::time::Instant::now();
        let canceled = move || started.elapsed() > Duration::from_millis(300);
        let error = run_cancelable_io(
            shell("sleep 30"),
            Some(vec![b'x'; 4 << 20]),
            false,
            &canceled,
        )
        .err()
        .expect("the run was cancelled");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(20));
    }
}
