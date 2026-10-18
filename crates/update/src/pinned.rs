//! Handle-bound mutation of a verified install file (Windows).
//!
//! The old identity is checked through an open handle that denies other
//! processes write and delete sharing. While the handle is held the pathname
//! cannot be renamed, replaced or deleted, so the verified file is exactly the
//! one that is then moved aside or deleted *through that same handle*. Nothing
//! here resolves the install path a second time for a destructive operation.
//!
//! On other platforms the type exists only so the shared code compiles; it is
//! never constructed.

#[cfg(windows)]
pub(crate) use imp::PinnedFile;

#[cfg(not(windows))]
#[allow(dead_code, reason = "only the Windows adapter constructs a pin")]
pub(crate) struct PinnedFile(());

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

        /// Marks the file behind the handle for deletion; it disappears when
        /// the handle is dropped.
        pub(crate) fn delete(&self) -> io::Result<()> {
            delete_by_handle(self)
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

    #[allow(unsafe_code, reason = "SetFileInformationByHandle through windows-sys")]
    fn delete_by_handle(pin: &PinnedFile) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
        };

        let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        // SAFETY: `info` is a valid FILE_DISPOSITION_INFO for the whole call.
        let ok = unsafe {
            SetFileInformationByHandle(
                pin.file.as_raw_handle().cast(),
                FileDispositionInfo,
                (&raw const info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
