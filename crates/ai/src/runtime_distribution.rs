//! Resolves the absolute path to the Codex App Server executable that
//! `crate::runtime::RuntimeConfig`/`AiRuntime` spawn, and classifies every
//! way that resolution (or the subsequent `initialize` handshake) can fail
//! into a small, stable set of diagnostic codes a caller can surface to the
//! user (see `AiSnapshot`/the AI settings screen).
//!
//! Resolution has exactly two candidates, tried in this fixed order, with no
//! further fallback:
//!
//! 1. An explicit override via the `HANE_CODEX_APP_SERVER` environment
//!    variable. If set, it must be an absolute path to a valid executable;
//!    a relative, missing, or otherwise invalid override is reported as its
//!    own diagnostic and never falls back to the installed package.
//! 2. The installed package: a `manifest.json` (schema contract below) next
//!    to the files `crate::runtime_install` downloaded and verified on the
//!    user's first explicit "install" action, under a directory this
//!    crate's caller resolves (in production, `AiPaths::runtime_install_dir`
//!    under Hane's own app data, never next to the current executable and
//!    never requiring a build-time bundling step).
//!
//! Neither candidate is ever searched for via `PATH`, an adjacent legacy
//! flat executable, or another application's internals: every resolved path
//! is absolute and explicit.
//!
//! The installed package's `manifest.json` is a `schema_version = 1` document
//! naming a `bundle_id`, the exact target triple (one of
//! [`WINDOWS_X64_TARGET`]/[`WINDOWS_ARM64_TARGET`]/[`MACOS_ARM64_TARGET`])
//! it was built for, its upstream `codex_version` and `source_commit`, the
//! `archive_sha256` of its upstream archive, a `files`
//! inventory, an explicit `licenses` list (empty for Hane's direct-install
//! contract), and the `runtime_profile`/`config_profile`/`state_profile` the
//! package was created for. Every
//! `files` entry (not only the `entrypoint`) names a `path` relative to the
//! manifest, a `type`, its `size`, its `sha256`, and whether it is
//! `executable`; resolution verifies every entry's existence, type, size,
//! hash, and executable attribute, and that its `path` cannot resolve
//! outside the manifest's own directory -- lexically, and also on disk: the
//! bundle directory itself and every path component resolving a `files`
//! entry or the `entrypoint` is rejected if it is a symlink or (on Windows)
//! another reparse point (including a junction/mount point), so a bundled
//! symlink can never redirect a verified entry to a file outside the
//! package. Hane does not copy or redistribute upstream license documents;
//! archive fetching and extraction are handled by `crate::runtime_install`.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The explicit override environment variable. Highest priority; see the
/// module docs for why a bad override never falls back to the installed
/// package.
pub const EXPLICIT_OVERRIDE_ENV_VAR: &str = "HANE_CODEX_APP_SERVER";

const MANIFEST_FILE_NAME: &str = "manifest.json";
const MANIFEST_SCHEMA_VERSION: u32 = 1;

pub const WINDOWS_X64_TARGET: &str = "x86_64-pc-windows-msvc";
pub const WINDOWS_ARM64_TARGET: &str = "aarch64-pc-windows-msvc";
pub const MACOS_ARM64_TARGET: &str = "aarch64-apple-darwin";

/// Every target the installed package contract supports, for diagnostics.
pub const SUPPORTED_TARGETS: [&str; 3] = [
    WINDOWS_X64_TARGET,
    WINDOWS_ARM64_TARGET,
    MACOS_ARM64_TARGET,
];

/// The only recognized `files` entry `type`. Any other value is treated as
/// an unknown-schema inventory entry and rejected fail-closed.
const RUNTIME_MANIFEST_FILE_TYPE_FILE: &str = "file";

/// The current platform's manifest `target` identifier, or `None` when this
/// host is neither of the three P0 targets the installed package's manifest
/// contract supports.
pub fn current_target() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(WINDOWS_X64_TARGET)
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        Some(WINDOWS_ARM64_TARGET)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(MACOS_ARM64_TARGET)
    } else {
        None
    }
}

/// Every distinct, stable way runtime binary resolution (or, via
/// [`classify_spawn_error`]/[`classify_handshake_failure`], the subsequent
/// spawn/`initialize` handshake) can fail. Each variant is meant to be shown
/// to the user distinctly (see the AI settings screen's diagnostics
/// section), never collapsed into a single generic "AI unavailable" message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeDistributionErrorCode {
    /// `HANE_CODEX_APP_SERVER` was set to a non-absolute path.
    ExplicitPathNotAbsolute,
    /// The resolved candidate (explicit override, installed manifest, or a
    /// file the manifest names) does not exist.
    Missing,
    /// The installed manifest's `target` does not match this host, or this
    /// host is neither of the three P0 targets at all.
    UnsupportedTarget,
    /// A `files` inventory entry's computed `sha256` does not match the
    /// manifest's recorded value, or its actual size does not match the
    /// manifest's recorded `size`.
    IntegrityMismatch,
    /// A candidate file or the manifest could not be read because of
    /// filesystem permissions.
    AccessDenied,
    /// A candidate exists but is not a usable executable: not a regular
    /// file, empty, missing or unexpectedly carrying the executable
    /// permission bit (Unix) relative to its manifest `executable` flag, or
    /// (for the installed package) described by a `manifest.json` that is not
    /// valid JSON, not `schema_version = 1`, missing a required contract
    /// field, or whose `files` inventory has an unknown `type`, a duplicate
    /// or out-of-bundle `path`, or no entry for `entrypoint` marked
    /// `executable`.
    InvalidExecutable,
    /// The binary spawned, but the `initialize` JSON-RPC handshake did not
    /// complete (see `crate::runtime::RuntimeError::Handshake`).
    InitializeFailed,
}

impl RuntimeDistributionErrorCode {
    pub fn stable_code(self) -> &'static str {
        match self {
            Self::ExplicitPathNotAbsolute => "runtime_explicit_path_not_absolute",
            Self::Missing => "runtime_missing",
            Self::UnsupportedTarget => "runtime_target_unsupported",
            Self::IntegrityMismatch => "runtime_integrity_mismatch",
            Self::AccessDenied => "runtime_access_denied",
            Self::InvalidExecutable => "runtime_invalid_executable",
            Self::InitializeFailed => "runtime_initialize_failed",
        }
    }
}

/// Where a successfully resolved binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBinarySource {
    ExplicitOverride,
    BundledPackage,
}

/// One entry in a manifest's `files` inventory: a runtime-dependent file's
/// `path` relative to the manifest, its `type` (currently always `"file"`),
/// declared `size` in bytes, `sha256`, and whether it must carry the
/// executable attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeManifestFile {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

/// One of the `runtime_profile`/`config_profile`/`state_profile` fields a
/// manifest carries, identifying the sandbox/approval, configuration, or
/// state profile the installed App Server was packaged against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeManifestProfile {
    pub name: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// The installed package's `manifest.json` contract: `schema_version = 1`,
/// a `bundle_id`, naming the exact target triple, upstream
/// `codex_version`/`source_commit`, the `archive_sha256` of the upstream
/// archive it was extracted from, a `files` inventory, a `licenses` list, and
/// the `runtime_profile`/`config_profile`/`state_profile`. The
/// `licenses` field is retained as an explicit, possibly empty list for the
/// manifest schema; direct installs use an empty list because Hane does not
/// redistribute upstream documents. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeManifest {
    pub schema_version: u32,
    pub bundle_id: String,
    pub target: String,
    pub codex_version: String,
    pub entrypoint: String,
    pub source_commit: String,
    pub archive_sha256: String,
    pub files: Vec<RuntimeManifestFile>,
    pub licenses: Vec<String>,
    pub runtime_profile: RuntimeManifestProfile,
    pub config_profile: RuntimeManifestProfile,
    pub state_profile: RuntimeManifestProfile,
}

/// The outcome of a successful resolution: an absolute `binary_path` ready
/// to use as `crate::runtime::RuntimeConfig::binary_path`, where it came
/// from, and (for the bundled package only) the manifest that described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeBinary {
    pub binary_path: PathBuf,
    pub source: RuntimeBinarySource,
    pub manifest: Option<RuntimeManifest>,
}

/// Resolves the embedded Codex App Server binary: `explicit_override`
/// (typically `std::env::var_os(EXPLICIT_OVERRIDE_ENV_VAR)`) first, with no
/// fallback to `bundle_dir` if set; otherwise `bundle_dir`'s
/// `manifest.json`, validated against the current host's
/// [`current_target`]. Never consults `PATH` or any other location.
pub fn resolve_runtime_binary(
    explicit_override: Option<&OsStr>,
    bundle_dir: &Path,
) -> Result<ResolvedRuntimeBinary, RuntimeDistributionErrorCode> {
    match explicit_override {
        Some(explicit) => resolve_explicit_override(Path::new(explicit)),
        None => resolve_bundled_package(bundle_dir, current_target()),
    }
}

/// Maps an `io::Error` from a failed spawn attempt (`std::process::Command`
/// could not start the resolved binary at all) to the same diagnostic
/// taxonomy as resolution itself, so both surface through one set of codes.
pub fn classify_spawn_error(error: &io::Error) -> RuntimeDistributionErrorCode {
    match error.kind() {
        io::ErrorKind::NotFound => RuntimeDistributionErrorCode::Missing,
        io::ErrorKind::PermissionDenied => RuntimeDistributionErrorCode::AccessDenied,
        _ => RuntimeDistributionErrorCode::InvalidExecutable,
    }
}

/// The binary spawned but the `initialize` handshake itself did not
/// complete; always [`RuntimeDistributionErrorCode::InitializeFailed`]. A
/// tiny wrapper (rather than a bare constant at each call site) so every
/// call site's intent -- classifying a handshake failure, as opposed to a
/// spawn failure -- is explicit at a glance.
pub fn classify_handshake_failure() -> RuntimeDistributionErrorCode {
    RuntimeDistributionErrorCode::InitializeFailed
}

fn resolve_explicit_override(
    path: &Path,
) -> Result<ResolvedRuntimeBinary, RuntimeDistributionErrorCode> {
    if !path.is_absolute() {
        return Err(RuntimeDistributionErrorCode::ExplicitPathNotAbsolute);
    }
    validate_executable_file(path)?;
    Ok(ResolvedRuntimeBinary {
        binary_path: path.to_path_buf(),
        source: RuntimeBinarySource::ExplicitOverride,
        manifest: None,
    })
}

fn resolve_bundled_package(
    bundle_dir: &Path,
    target: Option<&'static str>,
) -> Result<ResolvedRuntimeBinary, RuntimeDistributionErrorCode> {
    let manifest_path = bundle_dir.join(MANIFEST_FILE_NAME);
    let manifest_bytes = read_file(&manifest_path)?;
    let manifest: RuntimeManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| RuntimeDistributionErrorCode::InvalidExecutable)?;
    validate_manifest_shape(&manifest)?;

    let Some(target) = target else {
        return Err(RuntimeDistributionErrorCode::UnsupportedTarget);
    };
    if manifest.target != target {
        return Err(RuntimeDistributionErrorCode::UnsupportedTarget);
    }

    // Every inventoried runtime-dependent file -- not only the entrypoint --
    // must exist, match its declared type/size/hash/executable attribute,
    // and resolve inside `bundle_dir`.
    for file in &manifest.files {
        let file_path = resolve_within_bundle(bundle_dir, &file.path)?;
        validate_inventory_file(&file_path, file)?;
    }

    let entrypoint_path = resolve_within_bundle(bundle_dir, &manifest.entrypoint)?;

    Ok(ResolvedRuntimeBinary {
        binary_path: entrypoint_path,
        source: RuntimeBinarySource::BundledPackage,
        manifest: Some(manifest),
    })
}

/// Required-contract-field presence/well-formedness, independent of the
/// current host: a manifest with a blank or missing contract field, an
/// unknown `files` entry `type`, a duplicate or out-of-bundle `files` path,
/// or an `entrypoint` that is not itself a `files` entry marked `executable`
/// is treated the same as a structurally invalid document, regardless of
/// whether this host would otherwise be able to use it.
fn validate_manifest_shape(
    manifest: &RuntimeManifest,
) -> Result<(), RuntimeDistributionErrorCode> {
    let non_blank = |s: &str| !s.trim().is_empty();
    let profile_ok = |profile: &RuntimeManifestProfile| non_blank(&profile.name);

    let top_level_ok = manifest.schema_version == MANIFEST_SCHEMA_VERSION
        && non_blank(&manifest.bundle_id)
        && non_blank(&manifest.target)
        && non_blank(&manifest.codex_version)
        && non_blank(&manifest.entrypoint)
        && non_blank(&manifest.source_commit)
        && is_sha256_hex(&manifest.archive_sha256)
        && !manifest.files.is_empty()
        && manifest.licenses.iter().all(|license| non_blank(license))
        && profile_ok(&manifest.runtime_profile)
        && profile_ok(&manifest.config_profile)
        && profile_ok(&manifest.state_profile);
    if !top_level_ok {
        return Err(RuntimeDistributionErrorCode::InvalidExecutable);
    }

    let mut seen_paths = std::collections::HashSet::with_capacity(manifest.files.len());
    for file in &manifest.files {
        let shape_ok = non_blank(&file.path)
            && has_valid_relative_path(&file.path)
            && file.kind == RUNTIME_MANIFEST_FILE_TYPE_FILE
            && is_sha256_hex(&file.sha256)
            && seen_paths.insert(file.path.as_str());
        if !shape_ok {
            return Err(RuntimeDistributionErrorCode::InvalidExecutable);
        }
    }

    match manifest
        .files
        .iter()
        .find(|file| file.path == manifest.entrypoint)
    {
        Some(file) if file.executable => Ok(()),
        _ => Err(RuntimeDistributionErrorCode::InvalidExecutable),
    }
}

/// Whether `relative` is a non-empty, relative path made only of normal
/// components (no absolute path, no `.`/`..`), so a manifest-provided path
/// can never resolve outside the directory it is joined onto.
fn has_valid_relative_path(relative: &str) -> bool {
    use std::path::Component;
    let relative_path = Path::new(relative);
    !relative_path.as_os_str().is_empty()
        && !relative_path.is_absolute()
        && relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Whether `value` is a well-formed lowercase-or-uppercase hex-encoded
/// SHA-256 digest (64 hex digits), independent of whether it matches any
/// particular file's actual content.
pub(crate) fn is_sha256_hex(value: &str) -> bool {
    let value = value.trim();
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Joins `relative` (a manifest-provided, package-relative path) onto
/// `bundle_dir`, rejecting anything that would resolve outside of it (e.g. a
/// `..` component), so a corrupted or tampered manifest can never point a
/// resolved file at an arbitrary filesystem location.
fn resolve_within_bundle(
    bundle_dir: &Path,
    relative: &str,
) -> Result<PathBuf, RuntimeDistributionErrorCode> {
    if !has_valid_relative_path(relative) {
        return Err(RuntimeDistributionErrorCode::InvalidExecutable);
    }
    reject_symlink_escape(bundle_dir, relative)?;
    Ok(bundle_dir.join(Path::new(relative)))
}

/// Rejects `bundle_dir` itself, or any path component resolving `relative`
/// onto it, that is a symlink or (on Windows) another reparse point --
/// including a junction/mount point, which Rust's `FileType::is_symlink`
/// does *not* report as a symlink (its Windows implementation only
/// recognizes the `IO_REPARSE_TAG_SYMLINK` reparse tag, not
/// `IO_REPARSE_TAG_MOUNT_POINT`), so [`check_not_symlink`] additionally
/// checks the raw `FILE_ATTRIBUTE_REPARSE_POINT` bit on Windows.
/// `has_valid_relative_path` already guarantees `relative` is lexically
/// contained (no `..`/absolute component); this additionally guarantees the
/// *resolved* path never traverses a symlinked or reparse-point directory or
/// file to a location outside `bundle_dir`, which a purely lexical check
/// cannot catch. A component that does not exist yet simply stops the walk
/// -- the subsequent existence check reports that case as
/// [`RuntimeDistributionErrorCode::Missing`], not as a symlink escape.
fn reject_symlink_escape(
    bundle_dir: &Path,
    relative: &str,
) -> Result<(), RuntimeDistributionErrorCode> {
    if !check_not_symlink(bundle_dir)? {
        return Ok(());
    }
    let mut current = bundle_dir.to_path_buf();
    for component in Path::new(relative).components() {
        current.push(component);
        if !check_not_symlink(&current)? {
            break;
        }
    }
    Ok(())
}

/// `Ok(true)` if `path` exists and is not a symlink/reparse point, `Ok(false)`
/// if it does not exist (yet), or an error classifying why it could not be
/// inspected (permission denied) or that it is a symlink/reparse point
/// (treated as [`RuntimeDistributionErrorCode::InvalidExecutable`], matching
/// the existing "not a regular file" classification).
///
/// On Windows this does not rely solely on `FileType::is_symlink()`: its
/// Windows implementation only recognizes the `IO_REPARSE_TAG_SYMLINK`
/// reparse tag, so a junction/mount point (`IO_REPARSE_TAG_MOUNT_POINT`) or
/// any other reparse point would otherwise pass through unrejected. Instead
/// this additionally checks the raw `FILE_ATTRIBUTE_REPARSE_POINT` bit via
/// [`std::os::windows::fs::MetadataExt::file_attributes`], which every
/// reparse point carries regardless of its specific tag.
pub(crate) fn check_not_symlink(path: &Path) -> Result<bool, RuntimeDistributionErrorCode> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(RuntimeDistributionErrorCode::InvalidExecutable);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(RuntimeDistributionErrorCode::InvalidExecutable);
                }
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(classify_io_error(&error)),
    }
}

/// Verifies one `files` inventory entry against the filesystem: it must
/// exist as a regular file of exactly the declared `size`, carry (or lack)
/// the executable attribute exactly as declared (Unix only; Windows has no
/// equivalent permission bit to check), and hash to the declared `sha256`.
pub(crate) fn validate_inventory_file(
    path: &Path,
    file: &RuntimeManifestFile,
) -> Result<(), RuntimeDistributionErrorCode> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return Err(classify_io_error(&error)),
    };
    if !metadata.is_file() {
        return Err(RuntimeDistributionErrorCode::InvalidExecutable);
    }
    if metadata.len() != file.size {
        return Err(RuntimeDistributionErrorCode::IntegrityMismatch);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let is_executable = metadata.permissions().mode() & 0o111 != 0;
        if is_executable != file.executable {
            return Err(RuntimeDistributionErrorCode::InvalidExecutable);
        }
    }
    verify_file_integrity(path, &file.sha256)
}

fn validate_executable_file(path: &Path) -> Result<(), RuntimeDistributionErrorCode> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return Err(classify_io_error(&error)),
    };
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(RuntimeDistributionErrorCode::InvalidExecutable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(RuntimeDistributionErrorCode::InvalidExecutable);
        }
    }
    Ok(())
}

fn verify_file_integrity(
    path: &Path,
    expected_sha256_hex: &str,
) -> Result<(), RuntimeDistributionErrorCode> {
    let actual = sha256_hex_of_file(path)?;
    if actual.eq_ignore_ascii_case(expected_sha256_hex.trim()) {
        Ok(())
    } else {
        Err(RuntimeDistributionErrorCode::IntegrityMismatch)
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>, RuntimeDistributionErrorCode> {
    std::fs::read(path).map_err(|error| classify_io_error(&error))
}

fn classify_io_error(error: &io::Error) -> RuntimeDistributionErrorCode {
    match error.kind() {
        io::ErrorKind::NotFound => RuntimeDistributionErrorCode::Missing,
        io::ErrorKind::PermissionDenied => RuntimeDistributionErrorCode::AccessDenied,
        _ => RuntimeDistributionErrorCode::InvalidExecutable,
    }
}

pub(crate) fn sha256_hex_of_file(path: &Path) -> Result<String, RuntimeDistributionErrorCode> {
    let mut file = std::fs::File::open(path).map_err(|error| classify_io_error(&error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| classify_io_error(&error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Minimal, dependency-free SHA-256 (FIPS 180-4), used only to verify the
/// bundled entrypoint's integrity against its manifest-recorded digest. Pure
/// Rust so this crate does not need a new external dependency (and its
/// matching `Cargo.lock` entry) just for one bundled-file integrity check.
struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[rustfmt::skip]
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            state: SHA256_INITIAL_STATE,
            buffer: [0u8; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        self.absorb(data);
    }

    fn absorb(&mut self, mut data: &[u8]) {
        if self.buffer_len > 0 {
            let needed = 64 - self.buffer_len;
            let take = needed.min(data.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                Self::process_block(&mut self.state, &block);
                self.buffer_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            Self::process_block(&mut self.state, &block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);
        self.absorb(&[0x80]);
        let zero_pad = if self.buffer_len <= 56 {
            56 - self.buffer_len
        } else {
            120 - self.buffer_len
        };
        let zeros = [0u8; 64];
        self.absorb(&zeros[..zero_pad]);
        self.absorb(&bit_len.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, value) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        out
    }

    fn process_block(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex_encode(&hasher.finalize())
    }

    #[test]
    fn sha256_matches_known_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn current_target_matches_this_build_platform() {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        assert_eq!(current_target(), Some(WINDOWS_X64_TARGET));
        #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
        assert_eq!(current_target(), Some(WINDOWS_ARM64_TARGET));
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(current_target(), Some(MACOS_ARM64_TARGET));
        #[cfg(not(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        assert_eq!(current_target(), None);
    }

    fn next_fixture_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-runtime-distribution-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_entrypoint(bundle_dir: &Path, name: &str, contents: &[u8]) -> (PathBuf, String, u64) {
        write_file(bundle_dir, name, contents, true)
    }

    /// Writes `contents` at `bundle_dir/name`, setting (Unix only) the
    /// executable permission bit according to `executable`, and returns the
    /// path, hex `sha256`, and byte length a manifest `files` entry for it
    /// would declare.
    fn write_file(
        bundle_dir: &Path,
        name: &str,
        contents: &[u8],
        executable: bool,
    ) -> (PathBuf, String, u64) {
        let path = bundle_dir.join(name);
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let _ = executable;
        }
        (path, sha256_hex(contents), contents.len() as u64)
    }

    /// A minimal, structurally valid manifest: `entrypoint_path` as the sole
    /// `files` entry (declared `executable: true`), with `entrypoint_sha256`
    /// and `entrypoint_size` as its `sha256`/`size`.
    fn base_manifest_value(
        target: &str,
        entrypoint_path: &str,
        entrypoint_sha256: &str,
        entrypoint_size: u64,
    ) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "bundle_id": "codex-app-server-bundle",
            "target": target,
            "codex_version": "1.2.3",
            "entrypoint": entrypoint_path,
            "source_commit": "deadbeef",
            "archive_sha256": "0".repeat(64),
            "files": [
                {
                    "path": entrypoint_path,
                    "type": "file",
                    "size": entrypoint_size,
                    "sha256": entrypoint_sha256,
                    "executable": true,
                }
            ],
            "licenses": [],
            "runtime_profile": {"name": "default"},
            "config_profile": {"name": "default"},
            "state_profile": {"name": "default"},
        })
    }

    fn add_file_entry(
        manifest: &mut serde_json::Value,
        path: &str,
        kind: &str,
        size: u64,
        sha256: &str,
        executable: bool,
    ) {
        manifest["files"].as_array_mut().unwrap().push(serde_json::json!({
            "path": path,
            "type": kind,
            "size": size,
            "sha256": sha256,
            "executable": executable,
        }));
    }

    fn write_manifest(dir: &Path, manifest: &serde_json::Value) {
        std::fs::write(dir.join(MANIFEST_FILE_NAME), manifest.to_string()).unwrap();
    }

    #[test]
    fn explicit_relative_override_is_reported_without_falling_back() {
        let result = resolve_runtime_binary(
            Some(OsStr::new("relative/codex-app-server")),
            Path::new("/nonexistent/bundle"),
        );
        assert_eq!(
            result,
            Err(RuntimeDistributionErrorCode::ExplicitPathNotAbsolute)
        );
    }

    #[test]
    fn explicit_missing_override_is_reported_without_falling_back() {
        let dir = next_fixture_dir("explicit-missing");
        let missing = dir.join("codex-app-server");
        let result = resolve_runtime_binary(Some(missing.as_os_str()), &dir.join("bundle"));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::Missing));
    }

    #[test]
    fn explicit_non_executable_override_is_invalid() {
        let dir = next_fixture_dir("explicit-non-exec");
        let path = dir.join("codex-app-server");
        std::fs::write(&path, b"").unwrap();
        let result = resolve_runtime_binary(Some(path.as_os_str()), &dir.join("bundle"));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn explicit_valid_override_resolves_and_bypasses_the_bundle() {
        let dir = next_fixture_dir("explicit-ok");
        let (path, _, _) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let resolved =
            resolve_runtime_binary(Some(path.as_os_str()), Path::new("/should/not/be/read"))
                .unwrap();
        assert_eq!(resolved.binary_path, path);
        assert_eq!(resolved.source, RuntimeBinarySource::ExplicitOverride);
        assert!(resolved.manifest.is_none());
    }

    #[test]
    fn bundled_package_missing_manifest_is_reported_as_missing() {
        let dir = next_fixture_dir("bundle-missing-manifest");
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::Missing));
    }

    #[test]
    fn bundled_package_malformed_manifest_json_is_invalid() {
        let dir = next_fixture_dir("bundle-malformed-manifest");
        std::fs::write(dir.join("manifest.json"), b"{not json").unwrap();
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_with_empty_license_list_is_valid() {
        let dir = next_fixture_dir("bundle-empty-licenses");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        write_manifest(&dir, &manifest);

        let resolved = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET)).unwrap();

        assert_eq!(resolved.source, RuntimeBinarySource::BundledPackage);
        assert!(resolved.manifest.unwrap().licenses.is_empty());
    }

    #[test]
    fn bundled_package_wrong_schema_version_is_invalid() {
        let dir = next_fixture_dir("bundle-wrong-schema");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        manifest["schema_version"] = serde_json::json!(2);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_missing_required_field_is_invalid() {
        let dir = next_fixture_dir("bundle-missing-field");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        for field in [
            "schema_version",
            "bundle_id",
            "target",
            "codex_version",
            "entrypoint",
            "source_commit",
            "archive_sha256",
            "files",
            "licenses",
            "runtime_profile",
            "config_profile",
            "state_profile",
        ] {
            let mut manifest =
                base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
            manifest.as_object_mut().unwrap().remove(field);
            write_manifest(&dir, &manifest);
            let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
            assert_eq!(
                result,
                Err(RuntimeDistributionErrorCode::InvalidExecutable),
                "removing {field} should be rejected"
            );
        }
    }

    #[test]
    fn bundled_package_blank_or_malformed_required_field_is_invalid() {
        let dir = next_fixture_dir("bundle-blank-field");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        for (field, value) in [
            ("bundle_id", serde_json::json!("")),
            ("codex_version", serde_json::json!("   ")),
            ("source_commit", serde_json::json!("")),
            ("archive_sha256", serde_json::json!("not-a-hash")),
        ] {
            let mut manifest =
                base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
            manifest
                .as_object_mut()
                .unwrap()
                .insert(field.to_string(), value);
            write_manifest(&dir, &manifest);
            let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
            assert_eq!(
                result,
                Err(RuntimeDistributionErrorCode::InvalidExecutable),
                "field {field}"
            );
        }
    }

    #[test]
    fn bundled_package_duplicate_file_path_is_invalid() {
        let dir = next_fixture_dir("bundle-duplicate-path");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(&mut manifest, "codex-app-server", "file", len, &sha, true);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_unknown_file_type_is_invalid() {
        let dir = next_fixture_dir("bundle-unknown-type");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        manifest["files"][0]["type"] = serde_json::json!("symlink");
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_entrypoint_not_marked_executable_is_invalid() {
        let dir = next_fixture_dir("bundle-entrypoint-not-exec-flag");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        manifest["files"][0]["executable"] = serde_json::json!(false);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_target_mismatch_is_unsupported() {
        let dir = next_fixture_dir("bundle-target-mismatch");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let manifest = base_manifest_value(MACOS_ARM64_TARGET, "codex-app-server", &sha, len);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::UnsupportedTarget));
    }

    #[test]
    fn bundled_package_on_an_unsupported_host_is_unsupported_regardless_of_manifest() {
        let dir = next_fixture_dir("bundle-unsupported-host");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, None);
        assert_eq!(result, Err(RuntimeDistributionErrorCode::UnsupportedTarget));
    }

    #[test]
    fn bundled_package_entrypoint_outside_the_bundle_is_invalid() {
        let dir = next_fixture_dir("bundle-escape");
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "../escaped", &"0".repeat(64), 16);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_dependent_file_path_escaping_is_invalid() {
        let dir = next_fixture_dir("bundle-dependent-escape");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(&mut manifest, "../escaped", "file", 10, &"a".repeat(64), false);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    #[cfg(unix)]
    fn bundled_package_entrypoint_symlink_escaping_the_bundle_is_invalid() {
        use std::os::unix::fs::symlink;
        let dir = next_fixture_dir("bundle-entrypoint-symlink-escape");
        let outside_dir = next_fixture_dir("bundle-entrypoint-symlink-escape-outside");
        let (_, sha, len) = write_file(&outside_dir, "secret", b"outside-contents", true);
        symlink(outside_dir.join("secret"), dir.join("codex-app-server")).unwrap();
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    #[cfg(unix)]
    fn bundled_package_dependent_file_symlink_escaping_the_bundle_is_invalid() {
        use std::os::unix::fs::symlink;
        let dir = next_fixture_dir("bundle-dependent-symlink-escape");
        let outside_dir = next_fixture_dir("bundle-dependent-symlink-escape-outside");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let (_, dep_sha, dep_len) = write_file(&outside_dir, "libcodex.so", b"outside-lib", false);
        symlink(
            outside_dir.join("libcodex.so"),
            dir.join("libcodex.so"),
        )
        .unwrap();
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(&mut manifest, "libcodex.so", "file", dep_len, &dep_sha, false);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    #[cfg(unix)]
    fn bundled_package_file_under_symlinked_directory_escaping_the_bundle_is_invalid() {
        use std::os::unix::fs::symlink;
        let dir = next_fixture_dir("bundle-symlinked-dir-escape");
        let outside_dir = next_fixture_dir("bundle-symlinked-dir-escape-outside");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let (_, dep_sha, dep_len) =
            write_file(&outside_dir, "libcodex.so", b"outside-lib", false);
        symlink(&outside_dir, dir.join("linked")).unwrap();
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(
            &mut manifest,
            "linked/libcodex.so",
            "file",
            dep_len,
            &dep_sha,
            false,
        );
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    /// Creates a directory junction at `junction` pointing at `target` via
    /// `mklink /J`, which (unlike a symbolic link) does not require
    /// administrator privileges or Developer Mode on Windows.
    #[cfg(windows)]
    fn create_junction(junction: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(junction)
            .arg(target)
            .status()
            .expect("failed to run mklink /J");
        assert!(status.success(), "mklink /J failed to create the junction fixture");
    }

    #[test]
    #[cfg(windows)]
    fn bundled_package_file_under_directory_junction_escaping_the_bundle_is_invalid() {
        let dir = next_fixture_dir("bundle-junction-dir-escape");
        let outside_dir = next_fixture_dir("bundle-junction-dir-escape-outside");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let (_, dep_sha, dep_len) = write_file(&outside_dir, "libcodex.so", b"outside-lib", false);
        create_junction(&dir.join("linked"), &outside_dir);
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(
            &mut manifest,
            "linked/libcodex.so",
            "file",
            dep_len,
            &dep_sha,
            false,
        );
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    #[cfg(windows)]
    fn bundled_package_entrypoint_as_a_junction_itself_is_invalid() {
        let dir = next_fixture_dir("bundle-junction-entrypoint");
        let outside_dir = next_fixture_dir("bundle-junction-entrypoint-outside");
        let manifest =
            base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &"0".repeat(64), 16);
        create_junction(&dir.join("codex-app-server"), &outside_dir);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_missing_entrypoint_file_is_missing() {
        let dir = next_fixture_dir("bundle-missing-entrypoint");
        let manifest =
            base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &"0".repeat(64), 16);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::Missing));
    }

    #[test]
    fn bundled_package_dependent_file_missing_is_missing() {
        let dir = next_fixture_dir("bundle-dependent-missing");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(&mut manifest, "libcodex.so", "file", 10, &"a".repeat(64), false);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::Missing));
    }

    #[test]
    fn bundled_package_integrity_mismatch_is_reported() {
        let dir = next_fixture_dir("bundle-integrity-mismatch");
        let (_, _, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let wrong_sha = "1".repeat(64);
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &wrong_sha, len);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::IntegrityMismatch));
    }

    #[test]
    fn bundled_package_size_mismatch_is_integrity_mismatch() {
        let dir = next_fixture_dir("bundle-size-mismatch");
        let (_, sha, len) = write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len + 1);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::IntegrityMismatch));
    }

    #[test]
    #[cfg(unix)]
    fn bundled_package_executable_attribute_mismatch_is_invalid() {
        let dir = next_fixture_dir("bundle-exec-attr-mismatch");
        let (_, sha, len) = write_file(&dir, "codex-app-server", b"binary-contents", false);
        let manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        write_manifest(&dir, &manifest);
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::InvalidExecutable));
    }

    #[test]
    fn bundled_package_valid_manifest_resolves_the_entrypoint() {
        let dir = next_fixture_dir("bundle-ok");
        let (entrypoint_path, sha, len) =
            write_entrypoint(&dir, "codex-app-server", b"binary-contents");
        let (_, dep_sha, dep_len) = write_file(&dir, "libcodex.so", b"shared-library", false);
        let mut manifest = base_manifest_value(WINDOWS_X64_TARGET, "codex-app-server", &sha, len);
        add_file_entry(&mut manifest, "libcodex.so", "file", dep_len, &dep_sha, false);
        write_manifest(&dir, &manifest);
        let resolved = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET)).unwrap();
        assert_eq!(resolved.binary_path, entrypoint_path);
        assert_eq!(resolved.source, RuntimeBinarySource::BundledPackage);
        let manifest = resolved.manifest.unwrap();
        assert_eq!(manifest.codex_version, "1.2.3");
        assert_eq!(manifest.runtime_profile.name, "default");
        assert_eq!(manifest.config_profile.name, "default");
        assert_eq!(manifest.state_profile.name, "default");
        assert_eq!(manifest.files.len(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn access_denied_is_reported_distinctly_from_missing() {
        // Running as root (common in some container-based CI images) bypasses
        // Unix permission bits entirely; skip rather than flake.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = next_fixture_dir("bundle-access-denied");
        let manifest_path = dir.join("manifest.json");
        std::fs::write(&manifest_path, b"{}").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = resolve_bundled_package(&dir, Some(WINDOWS_X64_TARGET));
        assert_eq!(result, Err(RuntimeDistributionErrorCode::AccessDenied));
        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn classify_spawn_error_maps_common_io_errors() {
        assert_eq!(
            classify_spawn_error(&io::Error::from(io::ErrorKind::NotFound)),
            RuntimeDistributionErrorCode::Missing
        );
        assert_eq!(
            classify_spawn_error(&io::Error::from(io::ErrorKind::PermissionDenied)),
            RuntimeDistributionErrorCode::AccessDenied
        );
        assert_eq!(
            classify_spawn_error(&io::Error::other("boom")),
            RuntimeDistributionErrorCode::InvalidExecutable
        );
    }
}
