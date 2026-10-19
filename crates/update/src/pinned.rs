//! Handle-bound mutation of a verified install file (Windows).
//!
//! The old identity is checked through an open handle that denies other
//! processes write and delete sharing. While the handle is held the pathname
//! cannot be renamed, replaced or deleted, so the verified file is exactly the
//! one that is then moved aside or deleted *through that same handle*. Nothing
//! here resolves the install path a second time for a destructive operation.
//!
//! On Unix (macOS) the verified file is held open and remembered by device and
//! inode. A displacement moves whatever object currently has the install name
//! aside with one atomic `rename`, and only then compares the moved object with
//! the verified one. A different object is moved back without replacing
//! anything and the mutation fails, so a file swapped in after the check is
//! never overwritten or deleted.
//!
//! On other platforms the type exists only so the shared code compiles; it is
//! never constructed.

#[cfg(windows)]
pub(crate) use imp::{PinnedDir, PinnedFile, move_write_through, replace_write_through};

#[cfg(unix)]
pub(crate) use unix_imp::PinnedFile;

#[cfg(not(windows))]
#[allow(dead_code, reason = "only the Windows adapter constructs a pin")]
pub(crate) struct PinnedDir(());

#[cfg(not(any(windows, unix)))]
#[allow(dead_code, reason = "only the Windows and Unix adapters construct a pin")]
pub(crate) struct PinnedFile(());

#[cfg(unix)]
mod unix_imp {
    use std::fs::{self, File, Metadata};
    use std::io;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::path::Path;

    use crate::error::{ErrorCode, Result, UpdateError};
    use crate::identity::FileIdentity;
    use crate::util::{hex_encode, sha256_context};

    /// An install file whose identity was verified through this very handle.
    pub(crate) struct PinnedFile {
        file: File,
        dev: u64,
        ino: u64,
    }

    impl PinnedFile {
        /// Opens the regular file at `path` (never a symlink). `Ok(None)` when
        /// it does not exist.
        pub(crate) fn open(path: &Path) -> Result<Option<Self>> {
            let not_regular = || {
                UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{} is not a regular file", path.display()),
                )
            };
            let before = match fs::symlink_metadata(path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            if !before.is_file() {
                return Err(not_regular());
            }
            let file = match File::open(path) {
                Ok(file) => file,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let meta = file.metadata()?;
            if !meta.is_file() || meta.dev() != before.dev() || meta.ino() != before.ino() {
                return Err(UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{} changed while it was being opened", path.display()),
                ));
            }
            Ok(Some(Self {
                file,
                dev: meta.dev(),
                ino: meta.ino(),
            }))
        }

        /// Size and SHA-256 of the content behind this handle.
        pub(crate) fn identity(&self) -> Result<FileIdentity> {
            let mut context = sha256_context();
            let mut buffer = vec![0u8; 64 * 1024];
            let mut size = 0u64;
            loop {
                let n = self.file.read_at(&mut buffer, size)?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                context.update(&buffer[..n]);
            }
            Ok(FileIdentity {
                size,
                sha256: hex_encode(context.finish().as_ref()),
            })
        }

        fn is_this(&self, meta: &Metadata) -> bool {
            meta.is_file() && meta.dev() == self.dev && meta.ino() == self.ino
        }

        /// Moves whatever has the name `path` to `aside` (which must not
        /// exist) in one atomic step and succeeds only when that is the
        /// verified file. Any other object is moved back without replacing
        /// and the call fails.
        pub(crate) fn displace(&self, path: &Path, aside: &Path) -> Result<()> {
            match fs::rename(path, aside) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return Err(UpdateError::new(
                        ErrorCode::InstallModified,
                        format!("{} vanished before it could be replaced", path.display()),
                    ));
                }
                Err(err) => return Err(err.into()),
            }
            let moved = fs::symlink_metadata(aside)?;
            if self.is_this(&moved) {
                return Ok(());
            }
            let restored = if moved.is_file() {
                fs::hard_link(aside, path).and_then(|()| fs::remove_file(aside))
            } else {
                match fs::symlink_metadata(path) {
                    Err(err) if err.kind() == io::ErrorKind::NotFound => fs::rename(aside, path),
                    _ => Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "the name was taken again",
                    )),
                }
            };
            Err(UpdateError::new(
                ErrorCode::InstallModified,
                match restored {
                    Ok(()) => format!("{} was replaced after it was verified", path.display()),
                    Err(err) => format!(
                        "{} was replaced after it was verified; the other file is kept at {}: {err}",
                        path.display(),
                        aside.display()
                    ),
                },
            ))
        }

        /// Puts the displaced verified file back under `path`, failing rather
        /// than replacing anything that took the name meanwhile.
        pub(crate) fn put_back(aside: &Path, path: &Path) -> io::Result<()> {
            fs::hard_link(aside, path)?;
            fs::remove_file(aside)
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::OpenOptions;
    use std::io::{self, Read};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use crate::error::{ErrorCode, Result, UpdateError};
    use crate::identity::FileIdentity;
    use crate::util::{hex_encode, sha256_context};

    const DELETE: u32 = 0x0001_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// An install directory verified as a real (non-reparse) directory and
    /// held open without delete sharing: while the handle lives, the directory
    /// and therefore every ancestor path leading to it cannot be renamed,
    /// replaced or deleted, so names resolved beneath it stay inside it.
    pub(crate) struct PinnedDir {
        _dir: std::fs::File,
    }

    impl PinnedDir {
        /// Opens `path` without following a reparse point. `Ok(None)` when it
        /// does not exist; an error when it is a link or not a directory.
        pub(crate) fn open(path: &Path) -> Result<Option<Self>> {
            let dir = match OpenOptions::new()
                .access_mode(GENERIC_READ)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
            {
                Ok(dir) => dir,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let meta = dir.metadata()?;
            if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !meta.is_dir() {
                return Err(UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{} is not a regular directory", path.display()),
                ));
            }
            Ok(Some(Self { _dir: dir }))
        }
    }

    /// Durable no-replace move on one volume: `MoveFileExW` with
    /// `MOVEFILE_WRITE_THROUGH`, which does not return until the move has been
    /// written to disk. Without `MOVEFILE_REPLACE_EXISTING` it fails when `to`
    /// exists, and without `MOVEFILE_COPY_ALLOWED` it never degrades to a
    /// copy. Windows documents no flush for a directory handle (opening one
    /// for write is refused), so namespace changes are made durable by
    /// performing them through this call.
    #[allow(unsafe_code, reason = "MoveFileExW through windows-sys")]
    pub(crate) fn move_write_through(from: &Path, to: &Path) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

        let wide = |path: &Path| -> Vec<u16> {
            path.as_os_str().encode_wide().chain(Some(0)).collect()
        };
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated and live past the call.
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Like [`move_write_through`], but an existing `to` is replaced
    /// (`MOVEFILE_REPLACE_EXISTING`). The replacement is a single namespace
    /// operation, so a failure leaves the previous `to` untouched.
    #[allow(unsafe_code, reason = "MoveFileExW through windows-sys")]
    pub(crate) fn replace_write_through(from: &Path, to: &Path) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };

        let wide = |path: &Path| -> Vec<u16> {
            path.as_os_str().encode_wide().chain(Some(0)).collect()
        };
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated and live past the call.
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_WRITE_THROUGH | MOVEFILE_REPLACE_EXISTING,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// An install file whose identity was verified through this very handle.
    pub(crate) struct PinnedFile {
        file: std::fs::File,
    }

    impl PinnedFile {
        /// Opens `path` without following a reparse point, denying write and
        /// delete sharing. `Ok(None)` when it does not exist.
        pub(crate) fn open(path: &Path) -> Result<Option<Self>> {
            let file = match OpenOptions::new()
                .access_mode(GENERIC_READ | DELETE)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
            {
                Ok(file) => file,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.into()),
            };
            let meta = file.metadata()?;
            if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !meta.is_file() {
                return Err(UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{} is not a regular file", path.display()),
                ));
            }
            Ok(Some(Self { file }))
        }

        /// Size and SHA-256 of the content behind this handle.
        pub(crate) fn identity(&self) -> Result<FileIdentity> {
            let mut reader = &self.file;
            let mut context = sha256_context();
            let mut buffer = vec![0u8; 64 * 1024];
            let mut size = 0u64;
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                context.update(&buffer[..n]);
            }
            Ok(FileIdentity {
                size,
                sha256: hex_encode(context.finish().as_ref()),
            })
        }

        /// Renames the file behind the handle to `dest`, failing when `dest`
        /// exists (no replacement).
        pub(crate) fn rename_to(&self, dest: &Path) -> io::Result<()> {
            rename_by_handle(self, dest)
        }

    }

    #[allow(unsafe_code, reason = "SetFileInformationByHandle through windows-sys")]
    fn rename_by_handle(pin: &PinnedFile, dest: &Path) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
        };

        let name: Vec<u16> = dest.as_os_str().encode_wide().collect();
        let name_bytes = name.len() * 2;
        let name_offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let total = (name_offset + name_bytes + 2).max(std::mem::size_of::<FILE_RENAME_INFO>());
        // 8-byte aligned and zeroed: ReplaceIfExists is false, RootDirectory
        // is null, so FileName is the full destination path.
        let mut buffer = vec![0u64; total.div_ceil(8)];
        let base = buffer.as_mut_ptr().cast::<u8>();
        // SAFETY: `buffer` is large enough for the header plus `name_bytes` at
        // `name_offset`, is suitably aligned for FILE_RENAME_INFO, and lives
        // past the call. Only FileNameLength and the name bytes are written.
        let ok = unsafe {
            let info = base.cast::<FILE_RENAME_INFO>();
            (*info).FileNameLength = name_bytes as u32;
            std::ptr::copy_nonoverlapping(name.as_ptr(), base.add(name_offset).cast::<u16>(), name.len());
            SetFileInformationByHandle(
                pin.file.as_raw_handle().cast(),
                FileRenameInfo,
                base.cast_const().cast(),
                (buffer.len() * 8) as u32,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
