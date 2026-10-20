//! Whole-archive ZIP inspection that runs before anything is extracted.
//!
//! This is a strict reader for the subset of ZIP the Hane release uses (single
//! volume, no ZIP64, `stored` or `deflate`). Everything else is rejected. The
//! `zip` crate is not in the resolved dependency set of this workspace, so the
//! container parsing here is intentionally minimal and refuses ambiguity:
//! the central directory must be contiguous with the end record, every local
//! header must agree with it, and entry data ranges must not overlap.
//! Inflation is delegated to `flate2`.
//!
//! Order of operations for [`stage_archive`]: structure -> layout -> declared
//! totals -> content hashes (full decompression with byte caps) -> signed
//! inventory -> only then extraction into an empty stage directory.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

use crate::catalog::VerifiedCandidate;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::identity::SignedInventory;
use crate::limits::{
    MAX_ARCHIVE_BYTES, MAX_CENTRAL_DIRECTORY_BYTES, MAX_COMPRESSION_RATIO, MAX_ENTRIES,
    MAX_ENTRY_BYTES, MAX_INVENTORY_BYTES, MAX_SIGNATURE_BYTES, MAX_UNPACKED_BYTES,
    RATIO_CHECK_MIN_BYTES,
};
use crate::model::{
    INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME, MACOS_APP_PREFIX, MACOS_INVENTORY_REL,
    MACOS_INVENTORY_SIGNATURE_REL, Target,
};
use crate::paths::{ancestors, collision_key, validate_rel_path};
use crate::trust::TrustPolicy;
use crate::util::{hex_encode, sha256_context};

const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_LOCAL: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;
const LOCAL_HEADER_LEN: usize = 30;
/// Encryption (bit 0, 6), data descriptor (3), deflate options (1, 2) and UTF-8
/// names (11) are the only general-purpose flags tolerated.
const ALLOWED_FLAGS: u16 = 0x0806 | 0x0008;
const FLAG_ENCRYPTED: u16 = 0x0001 | 0x0040 | 0x2000;
const FLAG_DATA_DESCRIPTOR: u16 = 0x0008;
const DOS_ATTR_DIRECTORY: u32 = 0x10;
const DOS_ATTR_REPARSE: u32 = 0x400;
const HOST_UNIX: u8 = 3;

fn reject(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::ArchiveRejected, detail)
}

fn io_reject(err: io::Error) -> UpdateError {
    reject(format!("archive read failed: {err}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_archive_bytes: u64,
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            max_entries: MAX_ENTRIES,
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_bytes: MAX_UNPACKED_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    /// Normalized relative path without trailing slash.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub compressed_size: u64,
    pub method: Method,
    pub crc32: u32,
    /// Owner-execute bit from Unix attributes (special bits are dropped).
    pub executable: bool,
    /// Content SHA-256; empty for directories and before content inspection.
    pub sha256: String,
    data_offset: u64,
}

struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(reject("truncated ZIP structure"));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn le16(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

/// Rejects ZIP64 and WinZip-AES extra fields; checks the TLV chain is exact.
fn check_extra(extra: &[u8]) -> Result<()> {
    let mut cursor = Bytes::new(extra);
    while cursor.remaining() > 0 {
        let id = cursor.u16()?;
        let len = usize::from(cursor.u16()?);
        cursor.take(len)?;
        if id == 0x0001 {
            return Err(reject("ZIP64 extra field is not supported"));
        }
        if id == 0x9901 {
            return Err(reject("AES-encrypted entry"));
        }
    }
    Ok(())
}

struct CentralEntry {
    name: String,
    name_bytes: Vec<u8>,
    flags: u16,
    method: Method,
    crc32: u32,
    compressed_size: u64,
    size: u64,
    local_offset: u64,
    is_dir: bool,
    executable: bool,
}

/// Parses and validates the container structure without reading entry data.
pub fn scan_archive<R: Read + Seek>(reader: &mut R, limits: &ArchiveLimits) -> Result<Vec<EntryInfo>> {
    let length = reader.seek(SeekFrom::End(0)).map_err(io_reject)?;
    if length > limits.max_archive_bytes {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "archive exceeds the size limit",
        ));
    }
    if length < EOCD_LEN as u64 {
        return Err(reject("not a ZIP archive"));
    }
    let tail_len = length.min(EOCD_LEN as u64 + u64::from(u16::MAX));
    let tail_start = length - tail_len;
    let mut tail = vec![0u8; tail_len as usize];
    reader.seek(SeekFrom::Start(tail_start)).map_err(io_reject)?;
    reader.read_exact(&mut tail).map_err(io_reject)?;

    let eocd_pos = (0..=tail.len() - EOCD_LEN)
        .rev()
        .find(|&pos| {
            le32(&tail, pos) == SIG_EOCD
                && pos + EOCD_LEN + usize::from(le16(&tail, pos + 20)) == tail.len()
        })
        .ok_or_else(|| reject("end of central directory not found"))?;
    let eocd = &tail[eocd_pos..];
    let disk = le16(eocd, 4);
    let cd_disk = le16(eocd, 6);
    let entries_on_disk = le16(eocd, 8);
    let total_entries = le16(eocd, 10);
    let cd_size = le32(eocd, 12);
    let cd_offset = le32(eocd, 16);
    if disk != 0 || cd_disk != 0 || entries_on_disk != total_entries {
        return Err(reject("multi-volume archive"));
    }
    if total_entries == u16::MAX || cd_size == u32::MAX || cd_offset == u32::MAX {
        return Err(reject("ZIP64 archive"));
    }
    if total_entries == 0 {
        return Err(reject("empty archive"));
    }
    if usize::from(total_entries) > limits.max_entries {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "too many archive entries",
        ));
    }
    let eocd_abs = tail_start + eocd_pos as u64;
    if u64::from(cd_offset) + u64::from(cd_size) != eocd_abs {
        return Err(reject("central directory is not contiguous with the end record"));
    }
    if u64::from(cd_size) > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "central directory too large",
        ));
    }
    let mut directory = vec![0u8; cd_size as usize];
    reader
        .seek(SeekFrom::Start(u64::from(cd_offset)))
        .map_err(io_reject)?;
    reader.read_exact(&mut directory).map_err(io_reject)?;

    let mut cursor = Bytes::new(&directory);
    let mut central = Vec::with_capacity(usize::from(total_entries));
    for _ in 0..total_entries {
        central.push(read_central_entry(&mut cursor, limits)?);
    }
    if cursor.remaining() != 0 {
        return Err(reject("trailing bytes in the central directory"));
    }
    check_names_and_totals(&central, limits)?;

    let mut entries = Vec::with_capacity(central.len());
    let mut ranges = Vec::with_capacity(central.len());
    for raw in &central {
        let data_offset = verify_local_header(reader, raw, u64::from(cd_offset))?;
        ranges.push((raw.local_offset, data_offset + raw.compressed_size));
        entries.push(EntryInfo {
            path: raw.name.trim_end_matches('/').to_string(),
            is_dir: raw.is_dir,
            size: raw.size,
            compressed_size: raw.compressed_size,
            method: raw.method,
            crc32: raw.crc32,
            executable: raw.executable,
            sha256: String::new(),
            data_offset,
        });
    }
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(reject("overlapping entry data"));
        }
    }
    Ok(entries)
}

fn read_central_entry(cursor: &mut Bytes<'_>, limits: &ArchiveLimits) -> Result<CentralEntry> {
    if cursor.u32()? != SIG_CENTRAL {
        return Err(reject("bad central directory record"));
    }
    let made_by = cursor.u16()?;
    let _needed = cursor.u16()?;
    let flags = cursor.u16()?;
    let method = cursor.u16()?;
    let _time = cursor.u16()?;
    let _date = cursor.u16()?;
    let crc32 = cursor.u32()?;
    let compressed = cursor.u32()?;
    let size = cursor.u32()?;
    let name_len = usize::from(cursor.u16()?);
    let extra_len = usize::from(cursor.u16()?);
    let comment_len = usize::from(cursor.u16()?);
    let disk = cursor.u16()?;
    let _internal = cursor.u16()?;
    let attributes = cursor.u32()?;
    let local_offset = cursor.u32()?;
    let name_bytes = cursor.take(name_len)?.to_vec();
    let extra = cursor.take(extra_len)?;
    cursor.take(comment_len)?;

    if flags & FLAG_ENCRYPTED != 0 {
        return Err(reject("encrypted entry"));
    }
    if flags & !ALLOWED_FLAGS != 0 {
        return Err(reject("unsupported general-purpose flags"));
    }
    let method = match method {
        0 => Method::Stored,
        8 => Method::Deflate,
        other => return Err(reject(format!("unsupported compression method {other}"))),
    };
    if disk != 0 {
        return Err(reject("entry on another volume"));
    }
    if compressed == u32::MAX || size == u32::MAX || local_offset == u32::MAX {
        return Err(reject("ZIP64 entry"));
    }
    check_extra(extra)?;

    let name = String::from_utf8(name_bytes.clone()).map_err(|_| reject("entry name is not UTF-8"))?;
    if name.is_empty() {
        return Err(reject("empty entry name"));
    }
    let is_dir = name.ends_with('/');
    validate_rel_path(name.trim_end_matches('/'))?;
    if name.ends_with("//") {
        return Err(reject("empty path component"));
    }

    if attributes & DOS_ATTR_REPARSE != 0 {
        return Err(reject("reparse point entry"));
    }
    if attributes & DOS_ATTR_DIRECTORY != 0 && !is_dir {
        return Err(reject("directory attribute on a file name"));
    }
    let mut executable = false;
    if (made_by >> 8) as u8 == HOST_UNIX {
        let mode = attributes >> 16;
        match mode & 0o170_000 {
            0 => {}
            0o100_000 => {
                if is_dir {
                    return Err(reject("regular-file mode on a directory name"));
                }
            }
            0o040_000 => {
                if !is_dir {
                    return Err(reject("directory mode on a file name"));
                }
            }
            0o120_000 => return Err(reject("symbolic link entry")),
            _ => return Err(reject("special file entry")),
        }
        // Setuid/setgid/sticky are never restored; only owner-execute is.
        executable = !is_dir && mode & 0o100 != 0;
    }

    let size = u64::from(size);
    let compressed = u64::from(compressed);
    if size > limits.max_entry_bytes {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "entry exceeds the per-entry size limit",
        ));
    }
    if method == Method::Stored && compressed != size {
        return Err(reject("stored entry with differing sizes"));
    }
    if is_dir && (size != 0 || compressed > 2) {
        return Err(reject("directory entry with data"));
    }
    if size >= RATIO_CHECK_MIN_BYTES && (compressed == 0 || size / compressed > MAX_COMPRESSION_RATIO) {
        return Err(reject("compression ratio looks like a decompression bomb"));
    }
    Ok(CentralEntry {
        name,
        name_bytes,
        flags,
        method,
        crc32,
        compressed_size: compressed,
        size,
        local_offset: u64::from(local_offset),
        is_dir,
        executable,
    })
}

/// Duplicate / case / file-versus-directory collisions and declared totals.
fn check_names_and_totals(entries: &[CentralEntry], limits: &ArchiveLimits) -> Result<()> {
    let mut keys: HashMap<String, bool> = HashMap::new();
    let mut file_keys: HashSet<String> = HashSet::new();
    let mut dir_keys: HashSet<String> = HashSet::new();
    let mut total = 0u64;
    for entry in entries {
        let path = entry.name.trim_end_matches('/');
        let key = collision_key(path);
        if keys.insert(key.clone(), entry.is_dir).is_some() {
            return Err(reject(format!("duplicate or colliding entry {path:?}")));
        }
        for ancestor in ancestors(path) {
            let ancestor_key = collision_key(ancestor);
            if file_keys.contains(&ancestor_key) {
                return Err(reject(format!("{path:?} lies below a file")));
            }
            dir_keys.insert(ancestor_key);
        }
        if entry.is_dir {
            dir_keys.insert(key);
        } else {
            if dir_keys.contains(&key) {
                return Err(reject(format!("file {path:?} collides with a directory")));
            }
            file_keys.insert(key);
        }
        total = total
            .checked_add(entry.size)
            .filter(|t| *t <= limits.max_total_bytes)
            .ok_or_else(|| {
                UpdateError::new(ErrorCode::LimitExceeded, "declared unpacked size too large")
            })?;
    }
    Ok(())
}

/// Cross-checks the local header against the central record; returns the
/// offset where entry data starts.
fn verify_local_header<R: Read + Seek>(reader: &mut R, raw: &CentralEntry, cd_offset: u64) -> Result<u64> {
    if raw.local_offset + LOCAL_HEADER_LEN as u64 > cd_offset {
        return Err(reject("local header lies outside the data area"));
    }
    reader
        .seek(SeekFrom::Start(raw.local_offset))
        .map_err(io_reject)?;
    let mut header = [0u8; LOCAL_HEADER_LEN];
    reader.read_exact(&mut header).map_err(io_reject)?;
    if le32(&header, 0) != SIG_LOCAL {
        return Err(reject("bad local header"));
    }
    if le16(&header, 6) != raw.flags {
        return Err(reject("local and central flags differ"));
    }
    let method = match raw.method {
        Method::Stored => 0,
        Method::Deflate => 8,
    };
    if le16(&header, 8) != method {
        return Err(reject("local and central compression differ"));
    }
    if raw.flags & FLAG_DATA_DESCRIPTOR == 0
        && (u64::from(le32(&header, 14)) != u64::from(raw.crc32)
            || u64::from(le32(&header, 18)) != raw.compressed_size
            || u64::from(le32(&header, 22)) != raw.size)
    {
        return Err(reject("local and central sizes differ"));
    }
    let name_len = usize::from(le16(&header, 26));
    let extra_len = usize::from(le16(&header, 28));
    if name_len != raw.name_bytes.len() {
        return Err(reject("local and central names differ"));
    }
    let mut variable = vec![0u8; name_len + extra_len];
    reader.read_exact(&mut variable).map_err(io_reject)?;
    if variable[..name_len] != raw.name_bytes[..] {
        return Err(reject("local and central names differ"));
    }
    check_extra(&variable[name_len..])?;
    let data_offset = raw.local_offset + LOCAL_HEADER_LEN as u64 + (name_len + extra_len) as u64;
    if data_offset + raw.compressed_size > cd_offset {
        return Err(reject("entry data overlaps the central directory"));
    }
    Ok(data_offset)
}

const WINDOWS_ROOT_FILES: [&str; 9] = [
    "hane.exe",
    "hane-updater.exe",
    "hane-install.json",
    "hane-install.json.sig",
    "Hane.ShellIntegration.msix",
    "Hane.ShellIntegration.cer",
    "Trust-HaneShellCertificate.ps1",
    "README.md",
    "LICENSE",
];
const WINDOWS_REQUIRED: [&str; 4] = [
    "hane.exe",
    "hane-updater.exe",
    "hane-install.json",
    "hane-install.json.sig",
];
const MACOS_ROOT: &str = "hane-macos-arm64";
const MACOS_APP: &str = "hane-macos-arm64/Hane.app";
const MACOS_REQUIRED: [&str; 5] = [
    "hane-macos-arm64/Hane.app/Contents/Info.plist",
    "hane-macos-arm64/Hane.app/Contents/MacOS/hane",
    "hane-macos-arm64/Hane.app/Contents/Helpers/hane-updater",
    "hane-macos-arm64/Hane.app/Contents/Resources/hane-install.json",
    "hane-macos-arm64/Hane.app/Contents/Resources/hane-install.json.sig",
];

fn windows_file_allowed(path: &str) -> bool {
    if WINDOWS_ROOT_FILES.contains(&path) {
        return true;
    }
    let parts: Vec<&str> = path.split('/').collect();
    parts.len() == 3 && parts[0] == "shell-integration" && parts[2] == "hane_shell_extension.dll"
}

fn windows_dir_allowed(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    (parts.len() == 1 || parts.len() == 2) && parts[0] == "shell-integration"
}

/// Only the documented layout for the target is accepted; any other file,
/// notably an extra executable, rejects the archive.
pub fn check_layout(target: Target, entries: &[EntryInfo]) -> Result<()> {
    let files: HashSet<&str> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.path.as_str())
        .collect();
    match target {
        Target::WindowsX64 => {
            for entry in entries {
                let allowed = if entry.is_dir {
                    windows_dir_allowed(&entry.path)
                } else {
                    windows_file_allowed(&entry.path)
                };
                if !allowed {
                    return Err(reject(format!("unexpected entry {:?}", entry.path)));
                }
            }
            for required in WINDOWS_REQUIRED {
                if !files.contains(required) {
                    return Err(reject(format!("missing required file {required:?}")));
                }
            }
        }
        Target::MacosArm64 => {
            for entry in entries {
                let path = entry.path.as_str();
                let in_app = path == MACOS_APP || path.starts_with("hane-macos-arm64/Hane.app/");
                let allowed = if entry.is_dir {
                    path == MACOS_ROOT || in_app
                } else {
                    in_app || path == "hane-macos-arm64/README.md" || path == "hane-macos-arm64/LICENSE"
                };
                if !allowed {
                    return Err(reject(format!("unexpected entry {path:?}")));
                }
                if entry.executable
                    && !(path.starts_with("hane-macos-arm64/Hane.app/Contents/MacOS/")
                        || path.starts_with("hane-macos-arm64/Hane.app/Contents/Helpers/"))
                {
                    return Err(reject(format!("unexpected executable {path:?}")));
                }
            }
            for required in MACOS_REQUIRED {
                if !files.contains(required) {
                    return Err(reject(format!("missing required file {required:?}")));
                }
            }
        }
    }
    Ok(())
}

/// Streams one entry through decompression with a hard byte cap, verifying
/// the declared size and CRC-32, handing every chunk to `sink`. Returns the
/// content SHA-256.
fn stream_entry<R: Read + Seek>(
    reader: &mut R,
    entry: &EntryInfo,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<String> {
    reader
        .seek(SeekFrom::Start(entry.data_offset))
        .map_err(io_reject)?;
    let raw = Read::take(&mut *reader, entry.compressed_size);
    let mut source: Box<dyn Read + '_> = match entry.method {
        Method::Stored => Box::new(raw),
        Method::Deflate => Box::new(DeflateDecoder::new(raw)),
    };
    // One byte beyond the declared size distinguishes "exactly" from "more".
    let mut limited = source.by_ref().take(entry.size + 1);
    let mut sha = sha256_context();
    let mut crc = flate2::Crc::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = match limited.read(&mut buffer) {
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(reject(format!("corrupt entry {:?}: {err}", entry.path))),
        };
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > entry.size {
            return Err(reject(format!(
                "entry {:?} is larger than its declared size",
                entry.path
            )));
        }
        sha.update(&buffer[..read]);
        crc.update(&buffer[..read]);
        sink(&buffer[..read])?;
    }
    if total != entry.size {
        return Err(reject(format!(
            "entry {:?} is smaller than its declared size",
            entry.path
        )));
    }
    if crc.sum() != entry.crc32 {
        return Err(reject(format!("CRC mismatch in {:?}", entry.path)));
    }
    Ok(hex_encode(sha.finish().as_ref()))
}

fn read_entry<R: Read + Seek>(reader: &mut R, entry: &EntryInfo, limit: u64) -> Result<Vec<u8>> {
    if entry.size > limit {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            format!("{:?} exceeds {limit} bytes", entry.path),
        ));
    }
    let mut data = Vec::new();
    stream_entry(reader, entry, &mut |chunk| {
        data.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(data)
}

#[derive(Debug, Clone)]
pub struct InspectedArchive {
    pub entries: Vec<EntryInfo>,
    pub total_unpacked: u64,
    /// Present for Windows archives (verified, matches the catalog).
    pub inventory: Option<SignedInventory>,
}

/// Inspects the whole archive against the verified candidate. Nothing is
/// written anywhere.
pub fn inspect_archive<R: Read + Seek>(
    reader: &mut R,
    candidate: &VerifiedCandidate,
    policy: &TrustPolicy,
    limits: &ArchiveLimits,
) -> Result<InspectedArchive> {
    let mut entries = scan_archive(reader, limits)?;
    check_layout(candidate.target(), &entries)?;
    let total_unpacked: u64 = entries.iter().map(|e| e.size).sum();
    if total_unpacked != candidate.target_entry().unpacked_size {
        return Err(UpdateError::new(
            ErrorCode::SizeMismatch,
            format!(
                "archive unpacks to {total_unpacked} bytes, catalog says {}",
                candidate.target_entry().unpacked_size
            ),
        ));
    }
    // Full decompression with caps: declared sizes are claims, not facts.
    for entry in entries.iter_mut().filter(|e| !e.is_dir) {
        let entry_ro = entry.clone();
        entry.sha256 = stream_entry(reader, &entry_ro, &mut |_| Ok(()))?;
    }
    let inventory = match candidate.target() {
        Target::WindowsX64 => Some(verify_inventory(
            reader,
            &entries,
            candidate,
            policy,
            &InventoryLocation {
                prefix: "",
                inventory: INVENTORY_FILE_NAME,
                signature: INVENTORY_SIGNATURE_FILE_NAME,
            },
        )?),
        Target::MacosArm64 => Some(verify_inventory(
            reader,
            &entries,
            candidate,
            policy,
            &InventoryLocation {
                prefix: MACOS_APP_PREFIX,
                inventory: MACOS_INVENTORY_REL,
                signature: MACOS_INVENTORY_SIGNATURE_REL,
            },
        )?),
    };
    Ok(InspectedArchive {
        entries,
        total_unpacked,
        inventory,
    })
}

/// Where a target keeps its signed inventory inside the archive. Inventory
/// paths are relative to `prefix`; files outside `prefix` (the macOS README and
/// LICENSE) are not owned and not listed.
struct InventoryLocation {
    prefix: &'static str,
    inventory: &'static str,
    signature: &'static str,
}

fn verify_inventory<R: Read + Seek>(
    reader: &mut R,
    entries: &[EntryInfo],
    candidate: &VerifiedCandidate,
    policy: &TrustPolicy,
    location: &InventoryLocation,
) -> Result<SignedInventory> {
    let inventory_path = format!("{}{}", location.prefix, location.inventory);
    let signature_path = format!("{}{}", location.prefix, location.signature);
    let find = |name: &str| entries.iter().find(|e| !e.is_dir && e.path == name);
    let inventory_entry =
        find(&inventory_path).ok_or_else(|| reject("archive has no signed inventory"))?;
    let signature_entry =
        find(&signature_path).ok_or_else(|| reject("archive has no inventory signature"))?;
    let raw = read_entry(reader, inventory_entry, MAX_INVENTORY_BYTES)?;
    let signature = read_entry(reader, signature_entry, MAX_SIGNATURE_BYTES)?;
    // The catalog binds the exact inventory bytes (check_candidate compares
    // the hash); the signature authenticates them independently.
    let signed = SignedInventory::verify(&raw, &signature, policy)?;
    signed.check_candidate(candidate)?;

    let listed: BTreeMap<&str, (u64, &str)> = signed
        .inventory()
        .files
        .iter()
        .map(|f| (f.path.as_str(), (f.size, f.sha256.as_str())))
        .collect();
    let present: BTreeMap<&str, &EntryInfo> = entries
        .iter()
        .filter(|e| !e.is_dir && e.path != inventory_path && e.path != signature_path)
        .filter_map(|e| Some((e.path.strip_prefix(location.prefix)?, e)))
        .collect();
    if listed.len() != present.len() || !listed.keys().all(|p| present.contains_key(p)) {
        return Err(reject("archive files differ from the signed inventory"));
    }
    for (path, (size, sha256)) in &listed {
        let entry = present[path];
        if entry.size != *size {
            return Err(UpdateError::new(
                ErrorCode::SizeMismatch,
                format!("{path:?} size differs from the signed inventory"),
            ));
        }
        if entry.sha256 != *sha256 {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("{path:?} content differs from the signed inventory"),
            ));
        }
    }
    Ok(signed)
}

/// Inspects, then extracts into `stage`. A rejected archive leaves `stage`
/// (and everything outside it) untouched.
pub fn stage_archive<R: Read + Seek>(
    reader: &mut R,
    candidate: &VerifiedCandidate,
    policy: &TrustPolicy,
    limits: &ArchiveLimits,
    stage: &Path,
) -> Result<InspectedArchive> {
    let inspected = inspect_archive(reader, candidate, policy, limits)?;
    extract_archive(reader, &inspected, stage)?;
    Ok(inspected)
}

fn prepare_stage(stage: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(stage)?;
    if !meta.is_dir() {
        return Err(UpdateError::new(
            ErrorCode::StageNotEmpty,
            "stage is not a real directory",
        ));
    }
    if fs::read_dir(stage)?.next().is_some() {
        return Err(UpdateError::new(
            ErrorCode::StageNotEmpty,
            "stage directory is not empty",
        ));
    }
    Ok(())
}

/// Extracts an inspected archive into an empty, pre-existing stage directory.
///
/// Every file is created exclusively (never overwriting or following an
/// existing entry), every directory component is checked not to be a link,
/// and content is re-hashed against the inspection result while writing.
pub fn extract_archive<R: Read + Seek>(
    reader: &mut R,
    inspected: &InspectedArchive,
    stage: &Path,
) -> Result<()> {
    prepare_stage(stage)?;
    let mut created: Vec<(PathBuf, bool)> = Vec::new();
    let result = extract_entries(reader, inspected, stage, &mut created);
    if result.is_err() {
        for (path, is_dir) in created.iter().rev() {
            let _ = if *is_dir {
                fs::remove_dir(path)
            } else {
                fs::remove_file(path)
            };
        }
    }
    result
}

fn extract_entries<R: Read + Seek>(
    reader: &mut R,
    inspected: &InspectedArchive,
    stage: &Path,
    created: &mut Vec<(PathBuf, bool)>,
) -> Result<()> {
    for entry in &inspected.entries {
        let parts: Vec<&str> = entry.path.split('/').collect();
        let (dirs, file_name) = if entry.is_dir {
            (&parts[..], None)
        } else {
            (&parts[..parts.len() - 1], parts.last().copied())
        };
        let mut current = stage.to_path_buf();
        for part in dirs {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(reject(format!("{:?} is not a plain directory", entry.path))),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&current)?;
                    created.push((current.clone(), true));
                }
                Err(err) => return Err(err.into()),
            }
        }
        let Some(file_name) = file_name else {
            continue;
        };
        current.push(file_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if entry.executable { 0o755 } else { 0o644 });
        }
        let mut file = options.open(&current)?;
        created.push((current.clone(), false));
        let sha = stream_entry(reader, entry, &mut |chunk| {
            file.write_all(chunk).map_err(UpdateError::from)
        })?;
        if sha != entry.sha256 {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("{:?} changed between inspection and extraction", entry.path),
            ));
        }
        file.sync_all()?;
    }
    Ok(())
}
