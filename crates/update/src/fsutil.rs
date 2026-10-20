//! Real-filesystem helpers behind the seams used by the core.
//!
//! Durability here is "write temp, fsync, rename, fsync parent directory" as
//! far as `std` exposes it. On Windows the rename is a write-through
//! `MoveFileExW` (see `replace_file`) instead of a directory flush. Behaviour
//! across a real power loss on Windows or
//! macOS is NOT verified by this crate; recovery therefore always reconciles
//! the journal with the actual on-disk identity (see `journal`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::download::PartStore;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::MAX_JOURNAL_BYTES;
use crate::paths::validate_component;
use crate::trust::{SeenCatalog, SeenStore};
use crate::util::{hex_encode, parse_strict, sha256_context};

#[cfg(unix)]
fn private_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_options(_options: &mut OpenOptions) {}

pub(crate) fn sync_directory(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        // Directories cannot be opened for flushing through std on Windows.
        let _ = dir;
        Ok(())
    }
}

/// Flushes the file at `path` and its parent directory entry, so a rename that
/// produced `path` survives a power loss. Directory flushing is a no-op where
/// `std` cannot open directories (Windows); there only the file is flushed.
pub(crate) fn sync_renamed_file(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    OpenOptions::new().write(true).open(path)?.sync_all()?;
    sync_directory(parent)
}

/// Moves the already-flushed `temp` over `dest`, whether or not `dest` exists.
///
/// Windows: one `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH`. The replace is an atomic same-volume rename (no
/// copy fallback, because `MOVEFILE_COPY_ALLOWED` is not set), so on failure
/// `dest` keeps its previous content or stays missing. The call returns only
/// after the namespace change is flushed to disk. This guarantee covers the
/// rename of `temp` onto `dest` itself; Windows has no supported directory
/// flush, so `sync_directory` is not part of the success condition there.
/// Other platforms: `rename` replaces atomically and the caller flushes the
/// parent directory.
pub(crate) fn replace_file(temp: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        replace_write_through_windows(temp, dest)
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp, dest)
    }
}

/// The single `MoveFileExW` call, isolated so the crate-wide
/// `deny(unsafe_code)` stays in force everywhere else.
#[cfg(windows)]
#[allow(unsafe_code, reason = "one MoveFileExW call through windows-sys")]
fn replace_write_through_windows(temp: &Path, dest: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
    move_write_through_windows(temp, dest, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
}

/// Renames `from` to a `to` that must not exist, once and atomically, and
/// returns only after the namespace change is flushed (Windows:
/// `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` and no replace flag, so a
/// concurrent second claimer fails instead of winning the same name). Other
/// platforms: `rename`.
pub(crate) fn rename_new_durable(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
        move_write_through_windows(from, to, MOVEFILE_WRITE_THROUGH)
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)
    }
}

#[cfg(windows)]
#[allow(unsafe_code, reason = "one MoveFileExW call through windows-sys")]
fn move_write_through_windows(temp: &Path, dest: &Path, flags: u32) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let wide = |p: &Path| -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    };
    let temp_w = wide(temp);
    let dest_w = wide(dest);
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    let ok = unsafe { MoveFileExW(temp_w.as_ptr(), dest_w.as_ptr(), flags) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Removes the file at `path` and returns only after the removal is durable.
/// A missing file is an error: the caller verified it exists.
///
/// Windows: the file is first renamed to a retired name with a write-through
/// `MoveFileExW` (the record's own name is gone durably once that returns), then
/// the retired file is deleted. Other platforms: unlink, then flush the parent
/// directory.
pub(crate) fn remove_file_durable(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    #[cfg(windows)]
    {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
        let retired = parent.join(format!(".{name}.retired"));
        match fs::remove_file(&retired) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        rename_new_durable(path, &retired)?;
        fs::remove_file(&retired)
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(path)?;
        sync_directory(parent)
    }
}

/// Atomically replaces `path` with `bytes` via a sibling temp file.
pub fn durable_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = parent.join(format!(".{name}.tmp"));
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    private_options(&mut options);
    let mut file = options.open(&temp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = replace_file(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    sync_directory(parent)
}

/// Reads at most `limit` bytes; larger files are an error, not truncated.
pub fn read_limited(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            format!("{} exceeds {limit} bytes", path.display()),
        ));
    }
    Ok(Some(data))
}

/// True for symlinks and, on Windows, any reparse point (junctions included).
fn is_link_like(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Hashes the regular file at `root/<components>`. Every component below
/// `root` must be a real directory (the last one a regular file); a symlink or
/// reparse point anywhere on the way is rejected without being followed.
/// `root` itself is the configured install root and is used as given.
pub fn hash_regular_file_under(root: &Path, components: &[&str]) -> Result<Option<(u64, String)>> {
    let mut path = root.to_path_buf();
    for (index, part) in components.iter().enumerate() {
        path.push(part);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        if is_link_like(&meta) {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is a link", path.display()),
            ));
        }
        if index + 1 < components.len() && !meta.is_dir() {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not a directory", path.display()),
            ));
        }
    }
    hash_regular_file(&path)
}

/// SHA-256 and size of a regular file, never following a symlink.
pub fn hash_regular_file(path: &Path) -> Result<Option<(u64, String)>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if is_link_like(&meta) || !meta.is_file() {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut file = File::open(path)?;
    let mut context = sha256_context();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        context.update(&buffer[..n]);
    }
    Ok(Some((size, hex_encode(context.finish().as_ref()))))
}

/// Downloads into `<final_name>.part` inside a private directory and renames
/// it to `<final_name>` only when complete.
pub struct DirPartStore {
    part: PathBuf,
    complete: PathBuf,
}

impl DirPartStore {
    pub fn new(dir: &Path, final_name: &str) -> Result<Self> {
        validate_component(final_name, final_name)?;
        Ok(Self {
            part: dir.join(format!("{final_name}.part")),
            complete: dir.join(final_name),
        })
    }

    pub fn complete_path(&self) -> &Path {
        &self.complete
    }
}

impl PartStore for DirPartStore {
    fn begin_part(&mut self) -> io::Result<Box<dyn Write>> {
        match fs::remove_file(&self.part) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        private_options(&mut options);
        Ok(Box::new(options.open(&self.part)?))
    }

    fn commit_part(&mut self) -> io::Result<()> {
        OpenOptions::new().write(true).open(&self.part)?.sync_all()?;
        if fs::symlink_metadata(&self.complete).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "candidate archive already exists",
            ));
        }
        fs::rename(&self.part, &self.complete)?;
        if let Some(parent) = self.complete.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    fn discard_part(&mut self) {
        let _ = fs::remove_file(&self.part);
    }
}

/// File-backed sequence floor (`receipts/seen-catalog.json`).
pub struct FileSeenStore {
    path: PathBuf,
}

impl FileSeenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

/// Holds the OS file lock until dropped (closing the handle releases it).
struct SeenLockGuard {
    file: File,
}

impl Drop for SeenLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl SeenStore for FileSeenStore {
    fn lock(&self) -> Result<Box<dyn Send>> {
        let lock_path = self.path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(UpdateError::from)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        private_options(&mut options);
        let file = options.open(&lock_path).map_err(UpdateError::from)?;
        file.lock().map_err(UpdateError::from)?;
        Ok(Box::new(SeenLockGuard { file }))
    }

    fn load(&self) -> Result<Option<SeenCatalog>> {
        let Some(bytes) = read_limited(&self.path, MAX_JOURNAL_BYTES)? else {
            return Ok(None);
        };
        let seen: SeenCatalog = parse_strict(&bytes)?;
        seen.validate()?;
        Ok(Some(seen))
    }

    fn store(&mut self, seen: &SeenCatalog) -> Result<()> {
        let bytes = serde_json::to_vec(seen)
            .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
        durable_write(&self.path, &bytes).map_err(UpdateError::from)
    }
}

#[cfg(test)]
mod durable_write_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hane-fsutil-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    #[cfg(windows)]
    fn a_durable_new_rename_is_one_shot_and_never_replaces() {
        let dir = scratch_dir("claim");
        let ticket = dir.join("ticket");
        let claimed = dir.join("claimed");
        fs::write(&ticket, b"ticket").unwrap();
        rename_new_durable(&ticket, &claimed).unwrap();
        assert!(!ticket.exists());
        assert_eq!(fs::read(&claimed).unwrap(), b"ticket");
        // A second claimer of the same name loses and the winner is intact.
        fs::write(&ticket, b"other").unwrap();
        assert!(rename_new_durable(&ticket, &claimed).is_err());
        assert_eq!(fs::read(&claimed).unwrap(), b"ticket");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_missing_destination() {
        let dir = scratch_dir("create");
        let path = dir.join("journal.json");
        durable_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert!(!dir.join(".journal.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaces_existing_destination() {
        let dir = scratch_dir("replace");
        let path = dir.join("journal.json");
        durable_write(&path, b"first").unwrap();
        durable_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert!(!dir.join(".journal.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_replace_keeps_existing_data_and_reports_error() {
        let dir = scratch_dir("fail");
        let path = dir.join("journal.json");
        // A directory cannot be replaced by a file on any platform.
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"data").unwrap();
        assert!(durable_write(&path, b"new").is_err());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"data");
        assert!(!dir.join(".journal.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_file_overwrites_or_creates() {
        let dir = scratch_dir("move");
        let dest = dir.join("dest");
        let temp = dir.join("temp");
        fs::write(&temp, b"a").unwrap();
        replace_file(&temp, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"a");
        fs::write(&temp, b"b").unwrap();
        replace_file(&temp, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"b");
        assert!(!temp.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
