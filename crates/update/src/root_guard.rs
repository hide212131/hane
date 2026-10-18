//! Stable OS identity of an install root for the length of one operation
//! (Issue #510, U4).
//!
//! A pathname can be re-resolved to a different directory after the first
//! mutation (the root is renamed or replaced). The guard fixes the root's
//! volume and directory file ID when the operation starts and refuses every
//! later check that resolves to anything else. On Windows it also keeps a
//! directory handle open without `FILE_SHARE_DELETE`, so the OS itself refuses
//! to rename or delete the root while the operation runs; the identity check
//! remains as the detection for every case the handle does not cover.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};

/// Volume and directory file ID of one directory object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirId(u64, u64);

pub struct RootGuard {
    path: PathBuf,
    id: DirId,
    /// Held only to pin the directory (Windows: blocks rename and delete).
    _pin: File,
}

fn changed(what: &str) -> UpdateError {
    UpdateError::new(
        ErrorCode::InstallModified,
        format!("the install root {what}; nothing more is written"),
    )
}

impl RootGuard {
    /// Fixes the identity of the directory currently at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let pin = open_dir(path)?;
        let id = dir_id(&pin)?;
        Ok(Self {
            path: path.to_path_buf(),
            id,
            _pin: pin,
        })
    }

    /// Fails closed unless `path` still resolves to the directory fixed at
    /// [`RootGuard::open`] and the pinned object is still that directory.
    pub fn verify(&self) -> Result<()> {
        let current = open_dir(&self.path).map_err(|_| changed("is no longer at its path"))?;
        let now = dir_id(&current).map_err(|_| changed("identity cannot be read"))?;
        if now != self.id || !dir_id(&self._pin).is_ok_and(|pinned| pinned == self.id) {
            return Err(changed("was moved or replaced"));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_dir(path: &Path) -> Result<File> {
    let file = File::open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(UpdateError::new(ErrorCode::Io, "the install root is not a directory"));
    }
    Ok(file)
}

#[cfg(unix)]
fn dir_id(file: &File) -> Result<DirId> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(DirId(meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn open_dir(path: &Path) -> Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    // No FILE_SHARE_DELETE: renaming or deleting the directory while this
    // handle is open fails in the OS.
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(UpdateError::new(ErrorCode::Io, "the install root is not a directory"));
    }
    Ok(file)
}

#[cfg(windows)]
fn dir_id(file: &File) -> Result<DirId> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    // SAFETY: BY_HANDLE_FILE_INFORMATION is plain data for which all-zero is valid.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle belongs to `file`, which is alive, and `info` is a
    // live out-pointer.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(DirId(u64::from(info.dwVolumeSerialNumber), index))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn an_untouched_root_verifies_repeatedly() {
        let dir = TempDir::new("root-guard-same");
        let root = dir.path().join("install");
        fs::create_dir(&root).unwrap();
        let guard = RootGuard::open(&root).unwrap();
        guard.verify().unwrap();
        guard.verify().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_moved_root_or_a_replacement_at_its_path_is_refused() {
        let dir = TempDir::new("root-guard-moved");
        let root = dir.path().join("install");
        fs::create_dir(&root).unwrap();
        let guard = RootGuard::open(&root).unwrap();
        fs::rename(&root, dir.path().join("moved")).unwrap();
        assert!(guard.verify().is_err(), "a vanished path is refused");
        fs::create_dir(&root).unwrap();
        assert!(guard.verify().is_err(), "another directory at the path is refused");
    }

    #[test]
    fn a_file_is_not_a_root() {
        let dir = TempDir::new("root-guard-file");
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(RootGuard::open(&file).is_err());
    }
}
