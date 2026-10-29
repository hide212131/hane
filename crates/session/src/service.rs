use crate::identity::{ContentFingerprint, FileIdentity, FileStamp};
use hane_document::RopeBuffer;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// A document read from the I/O boundary, with everything the session needs to
/// track the file afterwards.
pub struct LoadedFile {
    pub document: RopeBuffer,
    pub identity: FileIdentity,
    pub stamp: Option<FileStamp>,
    /// Fingerprint of the disk content that `document` was read from,
    /// computed on the loading (background) path. It becomes the session's
    /// baseline so a same-stamp external rewrite is still detected before the
    /// first background check. `None` when the service cannot provide one.
    pub fingerprint: Option<ContentFingerprint>,
}

/// A read-only file handle used by streaming features such as content search.
/// It captures metadata before reading and can query both the same open
/// handle and the originating path again after reading, without
/// constructing a document session.
pub struct ReadFile {
    reader: Box<dyn StampedRead>,
    stamp_at_open: Option<FileStamp>,
    identity: FileIdentity,
}

impl ReadFile {
    pub(crate) fn from_reader(
        reader: impl StampedRead + 'static,
        identity: FileIdentity,
    ) -> io::Result<Self> {
        let reader: Box<dyn StampedRead> = Box::new(reader);
        let stamp_at_open = reader.current_stamp()?;
        Ok(Self {
            reader,
            stamp_at_open,
            identity,
        })
    }

    /// Metadata captured from the opened handle before its first read.
    #[must_use]
    pub fn stamp_at_open(&self) -> Option<FileStamp> {
        self.stamp_at_open
    }

    /// Metadata from the same open handle after a read has completed. A
    /// renamed-over path does not change the inode this handle already has
    /// open, so this alone cannot see an atomic replacement of `path`.
    pub fn current_stamp(&self) -> io::Result<Option<FileStamp>> {
        self.reader.current_stamp()
    }

    /// Metadata looked up by the path this handle was opened from, independent
    /// of the open handle itself. An atomic rename that replaces `path` while
    /// this handle is open is only visible through this path-based lookup.
    pub fn path_stamp(&self) -> io::Result<Option<FileStamp>> {
        self.reader.path_stamp()
    }

    /// File identity associated with the path used to open this handle.
    #[must_use]
    pub fn identity(&self) -> &FileIdentity {
        &self.identity
    }
}

impl Read for ReadFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buffer)
    }
}

pub(crate) trait StampedRead: Read + Send {
    fn current_stamp(&self) -> io::Result<Option<FileStamp>>;
    fn path_stamp(&self) -> io::Result<Option<FileStamp>>;
}

struct OsStampedRead {
    file: fs::File,
    path: PathBuf,
}

impl Read for OsStampedRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

impl StampedRead for OsStampedRead {
    fn current_stamp(&self) -> io::Result<Option<FileStamp>> {
        self.file
            .metadata()
            .map(|metadata| stamp_from_metadata(Some(metadata)))
    }

    fn path_stamp(&self) -> io::Result<Option<FileStamp>> {
        Ok(stamp_from_metadata(fs::metadata(&self.path).ok()))
    }
}

/// The result of one successful write.
#[derive(Clone, Debug)]
pub struct SavedFile {
    pub identity: FileIdentity,
    pub stamp: Option<FileStamp>,
    /// Fingerprint of the bytes that were written, so the session's baseline
    /// follows what actually reached the disk.
    pub fingerprint: Option<ContentFingerprint>,
}

/// One consistent read of a file for external-change checks: the document, the
/// fingerprint of exactly those bytes, and the stamp of the handle they came
/// from.
pub struct ObservedFile {
    pub document: RopeBuffer,
    pub fingerprint: ContentFingerprint,
    pub stamp: Option<FileStamp>,
}

/// What a background check found at a path. `Unreadable` is never "no change":
/// the caller keeps its document and reports the reason.
pub enum DiskVersion {
    Present(Box<ObservedFile>),
    Missing,
    Unreadable(String),
}

/// How many times a read that raced a writer is retried before the check
/// gives up and reports the file as unstable rather than accepting a mix.
const OBSERVE_ATTEMPTS: usize = 3;

/// Reads `path` as one consistent version. The stamp of the opened handle must
/// still equal the path's stamp after the read, otherwise the file changed or
/// was atomically replaced mid-read and the read is retried (bounded). Runs on
/// a background thread; it reads and hashes the whole file.
pub fn observe_disk(service: &dyn FileService, path: &Path) -> DiskVersion {
    for _ in 0..OBSERVE_ATTEMPTS {
        if service.stamp(path).is_none() {
            return DiskVersion::Missing;
        }
        let loaded = match service.load(path) {
            Ok(loaded) => loaded,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return DiskVersion::Missing,
            Err(error) => return DiskVersion::Unreadable(error.to_string()),
        };
        let after = service.stamp(path);
        if loaded.stamp.is_some() && loaded.stamp != after {
            continue;
        }
        let fingerprint = loaded
            .fingerprint
            .unwrap_or_else(|| ContentFingerprint::of_document(&loaded.document));
        return DiskVersion::Present(Box::new(ObservedFile {
            document: loaded.document,
            fingerprint,
            stamp: loaded.stamp,
        }));
    }
    DiskVersion::Unreadable("ファイルが更新され続けているため、整合した内容を読めませんでした".to_owned())
}

/// Why a save did not happen.
#[derive(Debug)]
pub enum SaveFailure {
    /// The file changed on disk since this session last read or wrote it.
    /// Overwriting would discard someone else's edit, so the caller has to
    /// decide before the bytes are replaced.
    ExternalChange,
    Io(io::Error),
}

impl std::fmt::Display for SaveFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExternalChange => formatter.write_str("file changed on disk"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

/// The whole filesystem surface the editor is allowed to touch.
///
/// Every method blocks, and every caller runs them off the input path. Keeping
/// the trait synchronous lets the UI pick its own executor and lets tests swap
/// in an in-memory implementation without an async runtime.
pub trait FileService: Send + Sync + 'static {
    fn load(&self, path: &Path) -> io::Result<LoadedFile>;

    /// Opens a read-only stream without loading the document into a session.
    fn open_reader(&self, path: &Path) -> io::Result<ReadFile>;

    /// Writes `document` to `path` atomically: a complete temporary file is
    /// renamed over the target, so a crash never leaves a half-written document.
    fn save(&self, path: &Path, document: &RopeBuffer) -> io::Result<SavedFile>;

    /// Like `save`, but never creates the parent directory: a destination
    /// directory that no longer exists fails with `ErrorKind::NotFound`
    /// instead of being recreated. The refusal happens in the same
    /// filesystem operation that creates the temporary file, so a directory
    /// deleted at any earlier moment cannot be resurrected by the write.
    fn save_in_existing_directory(
        &self,
        path: &Path,
        document: &RopeBuffer,
    ) -> io::Result<SavedFile> {
        self.save(path, document)
    }

    /// Saves like `save` (or `save_in_existing_directory` when
    /// `create_parent` is false) after running `check`. `check` is given the
    /// path whose contents to inspect. Implementations that write through a
    /// temporary file must check again on the exact version being replaced
    /// (moved aside atomically first), so an external write that lands before
    /// the replace is reported instead of overwritten. The default only
    /// checks once, before writing.
    fn save_checked(
        &self,
        path: &Path,
        document: &RopeBuffer,
        create_parent: bool,
        check: &dyn Fn(&Path) -> Result<(), SaveFailure>,
    ) -> Result<SavedFile, SaveFailure> {
        check(path)?;
        let saved = if create_parent {
            self.save(path, document)
        } else {
            self.save_in_existing_directory(path, document)
        };
        saved.map_err(SaveFailure::Io)
    }

    /// Current stamp, or `None` when the file does not exist.
    fn stamp(&self, path: &Path) -> Option<FileStamp>;

    /// Renames `from` to `to`. Refuses when `to` already exists, so a caller
    /// racing another writer for a name never silently clobbers it; the
    /// caller picks a different target and retries instead.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Renames a directory without replacing an existing file or directory at
    /// `to`. The operation stays at the filesystem boundary so callers never
    /// need to compose a check and `std::fs::rename` themselves.
    fn rename_folder(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Creates `path` as a directory, including any missing parent
    /// directories. Creating a directory that already exists is not an
    /// error, the same as `std::fs::create_dir_all`.
    fn create_dir(&self, path: &Path) -> io::Result<()>;

    /// Creates the single directory `path` without creating its parent: a
    /// parent that no longer exists fails with `ErrorKind::NotFound` instead
    /// of being recreated, the directory counterpart of
    /// `save_in_existing_directory`. The default falls back to `create_dir`
    /// for services whose parents cannot vanish (in-memory fakes).
    fn create_dir_in_existing_parent(&self, path: &Path) -> io::Result<()> {
        self.create_dir(path)
    }
}

/// Runs one save job against a service, enforcing the external-change rule the
/// job carries. This is the only place a document is allowed to overwrite a file.
pub fn run_save_job(
    service: &dyn FileService,
    path: &Path,
    document: &RopeBuffer,
    guard: OverwriteGuard,
) -> Result<SavedFile, SaveFailure> {
    service.save_checked(path, document, true, &|inspect| {
        check_overwrite_guard(service, inspect, guard)
    })
}

/// Shared `ExpectStamp` check for every job that writes to a path. `Force`
/// needs no check; `ExpectStamp` fails with `SaveFailure::ExternalChange` when
/// the disk no longer matches what the session recorded.
fn check_overwrite_guard(
    service: &dyn FileService,
    path: &Path,
    guard: OverwriteGuard,
) -> Result<(), SaveFailure> {
    let expected = match guard {
        OverwriteGuard::Force => return Ok(()),
        OverwriteGuard::ExpectContent { stamp, baseline } => {
            return check_content_guard(service, path, stamp, baseline);
        }
        OverwriteGuard::ExpectStamp(expected) => expected,
    };
    let unchanged = match (expected, service.stamp(path)) {
        (Some(expected), Some(current)) => expected == current,
        // The file we expected to update is gone: recreating it is the
        // session's own content, not an overwrite of someone else's.
        (Some(_), None) | (None, None) => true,
        // We believed the path was free and it is not.
        (None, Some(_)) => false,
    };
    if unchanged {
        Ok(())
    } else {
        Err(SaveFailure::ExternalChange)
    }
}

/// The content-based guard: the disk must still hold the version the session
/// believes it is replacing. It does not depend on a watcher notification
/// having arrived, and it does not trust the stamp alone.
///
/// - The file is gone: allowed only when the session never knew a stamp (a
///   file it has not written yet). A file that existed and vanished is not
///   silently recreated.
/// - The file cannot be read (permission, invalid text, unstable): refused as
///   an I/O error, never treated as unchanged.
/// - A known baseline fingerprint must equal the fingerprint of what is on
///   disk now; without one the stamp is the best remaining evidence.
fn check_content_guard(
    service: &dyn FileService,
    path: &Path,
    stamp: Option<FileStamp>,
    baseline: Option<ContentFingerprint>,
) -> Result<(), SaveFailure> {
    match observe_disk(service, path) {
        DiskVersion::Missing => {
            if stamp.is_none() && baseline.is_none() {
                Ok(())
            } else {
                Err(SaveFailure::ExternalChange)
            }
        }
        DiskVersion::Unreadable(reason) => Err(SaveFailure::Io(io::Error::other(reason))),
        DiskVersion::Present(file) => match (baseline, stamp) {
            (Some(baseline), _) if file.fingerprint == baseline => Ok(()),
            (Some(_), _) | (None, None) => Err(SaveFailure::ExternalChange),
            (None, Some(stamp)) if file.stamp == Some(stamp) => Ok(()),
            (None, Some(_)) => Err(SaveFailure::ExternalChange),
        },
    }
}

/// Runs the first write of a `SaveIntent::CreateNew` job: the same
/// stamp-based collision guard as `run_save_job`, but the destination
/// directory must already exist. A directory deleted after the target name
/// was picked yields `ErrorKind::NotFound` rather than being recreated.
pub fn run_create_new_job(
    service: &dyn FileService,
    path: &Path,
    document: &RopeBuffer,
    guard: OverwriteGuard,
) -> Result<SavedFile, SaveFailure> {
    service.save_checked(path, document, false, &|inspect| {
        check_overwrite_guard(service, inspect, guard)
    })
}

/// Whether a write must first prove the file has not changed underneath it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverwriteGuard {
    /// Write unconditionally: the user picked this target explicitly, or already
    /// answered the conflict prompt.
    Force,
    /// Write only if the disk still matches the stamp the session recorded.
    ExpectStamp(Option<FileStamp>),
    /// Write only if the disk still holds the recorded content: the baseline
    /// fingerprint when known, else the stamp. Used for every ordinary save of
    /// a tracked file and, with the version the user was shown, for a
    /// confirmed overwrite.
    ExpectContent {
        stamp: Option<FileStamp>,
        baseline: Option<ContentFingerprint>,
    },
}

/// `FileService` backed by the real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsFileService;

impl FileService for OsFileService {
    fn open_reader(&self, path: &Path) -> io::Result<ReadFile> {
        let file = fs::File::open(path)?;
        ReadFile::from_reader(
            OsStampedRead {
                file,
                path: path.to_path_buf(),
            },
            identity_for(path),
        )
    }

    fn load(&self, path: &Path) -> io::Result<LoadedFile> {
        let file = fs::File::open(path)?;
        let stamp = stamp_from_metadata(file.metadata().ok());
        let document = RopeBuffer::from_reader(io::BufReader::new(file))?;
        let fingerprint = Some(ContentFingerprint::of_document(&document));
        Ok(LoadedFile {
            document,
            identity: identity_for(path),
            stamp,
            fingerprint,
        })
    }

    fn save(&self, path: &Path, document: &RopeBuffer) -> io::Result<SavedFile> {
        atomic_write(path, true, |writer| document.write_to(writer))?;
        Ok(SavedFile {
            identity: identity_for(path),
            stamp: self.stamp(path),
            fingerprint: Some(ContentFingerprint::of_document(document)),
        })
    }

    fn save_in_existing_directory(
        &self,
        path: &Path,
        document: &RopeBuffer,
    ) -> io::Result<SavedFile> {
        atomic_write(path, false, |writer| document.write_to(writer))?;
        Ok(SavedFile {
            identity: identity_for(path),
            stamp: self.stamp(path),
            fingerprint: Some(ContentFingerprint::of_document(document)),
        })
    }

    fn save_checked(
        &self,
        path: &Path,
        document: &RopeBuffer,
        create_parent: bool,
        check: &dyn Fn(&Path) -> Result<(), SaveFailure>,
    ) -> Result<SavedFile, SaveFailure> {
        check(path)?;
        // The version being replaced is checked again after it is atomically
        // moved aside, so what is verified is exactly what gets replaced.
        atomic_write_checked(
            path,
            create_parent,
            |writer| document.write_to(writer),
            check,
        )?;
        Ok(SavedFile {
            identity: identity_for(path),
            stamp: self.stamp(path),
            fingerprint: Some(ContentFingerprint::of_document(document)),
        })
    }

    fn stamp(&self, path: &Path) -> Option<FileStamp> {
        stamp_from_metadata(fs::metadata(path).ok())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if from == to {
            return Ok(());
        }
        #[cfg(any(target_os = "macos", windows))]
        if is_case_only_name(from, to) && same_filesystem_object(from, to)? {
            return rename_case_only_file(from, to);
        }
        // `fs::rename` may replace an existing destination, and this boundary
        // must not. Use the platform's atomic no-replace primitive so the
        // operation does not depend on hard links, which are unavailable on
        // some filesystems where an ordinary rename is supported.
        rename_file_without_replace(from, to)
    }

    fn rename_folder(&self, from: &Path, to: &Path) -> io::Result<()> {
        if from == to {
            return Ok(());
        }
        #[cfg(any(target_os = "macos", windows))]
        if is_case_only_name(from, to) && same_filesystem_object(from, to)? {
            return rename_case_only_folder(from, to);
        }
        rename_directory_without_replace(from, to)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn create_dir_in_existing_parent(&self, path: &Path) -> io::Result<()> {
        create_dir_under_parent_handle(path, || {})
    }
}

/// Splits `path` into the parent directory to open and the single child name.
fn split_parent_and_name(path: &Path) -> io::Result<(&Path, &std::ffi::OsStr)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no folder name"))?;
    Ok((parent, name))
}

/// Creates the single directory `path` relative to a handle on its parent that
/// is opened first and refuses symlinks / reparse points. `before_create` runs
/// between opening the parent and creating the child; it exists so tests can
/// replace the parent at exactly that point without timing.
///
/// On Unix the child is made with `mkdirat` on the opened parent descriptor, so
/// a later swap of the parent path cannot redirect the creation. On Windows the
/// parent handle is held without `FILE_SHARE_DELETE`, so the parent (and its
/// ancestors) cannot be renamed, deleted or replaced while the child is made.
fn create_dir_under_parent_handle(path: &Path, before_create: impl FnOnce()) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;

        let (parent, name) = split_parent_and_name(path)?;
        let parent = CString::new(parent.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "parent contains NUL"))?;
        let name = CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

        // SAFETY: `parent` is a valid NUL-terminated string; the returned
        // descriptor is checked and then owned exclusively by `OwnedFd`.
        let raw = unsafe { libc::open(parent.as_ptr(), flags) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let parent_fd = unsafe { OwnedFd::from_raw_fd(raw) };

        before_create();

        // SAFETY: `parent_fd` is a live directory descriptor and `name` is a
        // valid NUL-terminated single path component.
        if unsafe { libc::mkdirat(parent_fd.as_raw_fd(), name.as_ptr(), 0o777) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::AlreadyExists {
            // `O_NOFOLLOW` keeps a symlink to a directory from being mistaken
            // for an existing directory.
            // SAFETY: same invariants as above.
            let existing = unsafe { libc::openat(parent_fd.as_raw_fd(), name.as_ptr(), flags) };
            if existing >= 0 {
                drop(unsafe { OwnedFd::from_raw_fd(existing) });
                return Ok(());
            }
        }
        Err(error)
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

        let (parent, _) = split_parent_and_name(path)?;
        // Held until the end of this function: without FILE_SHARE_DELETE the
        // parent cannot be renamed, deleted or swapped for a reparse point.
        let parent_handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(parent)?;
        let attributes = parent_handle.metadata()?.file_attributes();
        if attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "parent is not a plain directory",
            ));
        }

        before_create();

        let result = create_dir_rejecting_links(path);
        drop(parent_handle);
        result
    }

    #[cfg(not(any(unix, windows)))]
    {
        before_create();
        create_dir_rejecting_links(path)
    }
}

#[cfg(not(unix))]
fn create_dir_rejecting_links(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // `symlink_metadata` does not follow links, so a symlink to a
            // directory is not mistaken for an existing directory.
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
                _ => Err(error),
            }
        }
        result => result,
    }
}

fn rename_file_without_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        rename_path_without_replace(from, to)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        // Keep a conservative fallback for platforms without a native
        // no-replace rename primitive. It preserves the old collision-safe
        // behavior, though it requires hard-link support there.
        fs::hard_link(from, to)?;
        fs::remove_file(from)
    }
}

#[cfg(any(target_os = "macos", windows))]
fn is_case_only_name(from: &Path, to: &Path) -> bool {
    // Do not restrict this to ASCII. On a case-insensitive volume the
    // filesystem-object check below is the authoritative answer, including
    // names such as `Ä.md` -> `ä.md` that NTFS/APFS may fold together.
    from.parent() == to.parent() && from.file_name() != to.file_name()
}

/// A case-only rename looks like a collision on case-insensitive filesystems:
/// the source and target paths resolve to the same directory entry. Move the
/// entry to a unique sibling first, then use the normal no-replace boundary to
/// install the requested spelling. If the second step loses a race, restore
/// the original spelling before returning the error.
#[cfg(any(target_os = "macos", windows))]
fn rename_case_only_file(from: &Path, to: &Path) -> io::Result<()> {
    let temporary = loop {
        let candidate = temporary_rename_path(to);
        match rename_file_without_replace(from, &candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    match rename_file_without_replace(&temporary, to) {
        Ok(()) => Ok(()),
        Err(error) => match rename_file_without_replace(&temporary, from) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(io::Error::new(
                error.kind(),
                format!("{error}; restoring the original name failed: {restore_error}"),
            )),
        },
    }
}

#[cfg(any(target_os = "macos", windows))]
fn rename_case_only_folder(from: &Path, to: &Path) -> io::Result<()> {
    let temporary = loop {
        let candidate = temporary_rename_path(to);
        match rename_directory_without_replace(from, &candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    match rename_directory_without_replace(&temporary, to) {
        Ok(()) => Ok(()),
        Err(error) => match rename_directory_without_replace(&temporary, from) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(io::Error::new(
                error.kind(),
                format!("{error}; restoring the original name failed: {restore_error}"),
            )),
        },
    }
}

#[cfg(any(target_os = "macos", windows))]
fn temporary_rename_path(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".hane-rename-{}-{sequence}.tmp",
        std::process::id()
    ))
}

#[cfg(any(target_os = "macos", windows))]
fn same_filesystem_object(from: &Path, to: &Path) -> io::Result<bool> {
    if fs::symlink_metadata(from).is_ok_and(|metadata| metadata.file_type().is_symlink())
        || fs::symlink_metadata(to).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Ok(false);
    }
    same_filesystem_object_platform(from, to)
}

#[cfg(target_os = "macos")]
fn same_filesystem_object_platform(from: &Path, to: &Path) -> io::Result<bool> {
    let source = fs::metadata(from)?;
    let target = match fs::metadata(to) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    use std::os::unix::fs::MetadataExt;

    Ok(source.dev() == target.dev() && source.ino() == target.ino())
}

#[cfg(windows)]
fn same_filesystem_object_platform(from: &Path, to: &Path) -> io::Result<bool> {
    // `std::os::windows::fs::MetadataExt` exposes the native file-index
    // fields only through unstable APIs. Canonicalizing both existing paths
    // uses the stable filesystem boundary and gives the same result needed
    // here: case aliases on a case-insensitive volume resolve to one spelling,
    // while distinct entries on a case-sensitive volume do not. A normal
    // rename target does not exist yet, so that is the non-alias case rather
    // than an error from this predicate.
    let source = fs::canonicalize(from)?;
    let target = match fs::canonicalize(to) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(source == target)
}

/// Renames a directory using the platform's no-replace primitive. Plain
/// `fs::rename` is intentionally not used here: on Unix it can replace an
/// existing empty directory, which would violate the same collision rule as
/// file rename. If a platform cannot provide an atomic no-replace directory
/// rename, fail closed rather than risking data loss.
fn rename_directory_without_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        rename_path_without_replace(from, to)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory rename without replacement is unsupported on this platform",
        ))
    }
}

/// Renames either a file or a directory without replacing an existing target.
/// The platform primitive is shared by both boundaries so regular file rename
/// does not impose the hard-link requirement used by the portable fallback.
fn rename_path_without_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source contains NUL"))?;
        let to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target contains NUL"))?;
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        (result == 0)
            .then_some(())
            .ok_or_else(io::Error::last_os_error)
    }

    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source contains NUL"))?;
        let to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "target contains NUL"))?;
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        (result == 0)
            .then_some(())
            .ok_or_else(io::Error::last_os_error)
    }

    #[cfg(target_os = "windows")]
    {
        // Windows' MoveFileEx implementation, which backs std::fs::rename,
        // refuses an existing destination unless the replace flag is passed.
        // Keep the explicit existence check for a useful, deterministic error
        // and retain the no-replace call at the OS boundary.
        if to.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "rename target already exists",
            ));
        }
        fs::rename(from, to)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rename without replacement is unsupported on this platform",
        ))
    }
}

fn identity_for(path: &Path) -> FileIdentity {
    fs::canonicalize(path).map_or_else(
        |_| FileIdentity::lexical(path),
        |canonical| FileIdentity::new(path, canonical),
    )
}

fn stamp_from_metadata(metadata: Option<fs::Metadata>) -> Option<FileStamp> {
    metadata.map(|metadata| FileStamp::new(metadata.len(), metadata.modified().ok()))
}

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, true, |writer| writer.write_all(bytes))
}

fn atomic_write(
    path: &Path,
    create_parent: bool,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
) -> io::Result<()> {
    atomic_write_checked(path, create_parent, write, &|_| Ok(())).map_err(|failure| match failure {
        SaveFailure::Io(error) => error,
        SaveFailure::ExternalChange => io::Error::other("file changed on disk"),
    })
}

/// Like `atomic_write`, but checks the version being replaced after the
/// temporary file is durable. `check` receives the location to inspect: the
/// target moved aside at the moment of replacement. A failing check removes
/// the temporary file and leaves the target in place.
fn atomic_write_checked(
    path: &Path,
    create_parent: bool,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
    check: &dyn Fn(&Path) -> Result<(), SaveFailure>,
) -> Result<(), SaveFailure> {
    atomic_write_checked_with_hook(path, create_parent, write, check, &|| {})
}

/// `before_displace` runs after the temporary file is durable and right
/// before the target is moved aside; tests use it to place an external
/// version in exactly that window.
fn atomic_write_checked_with_hook(
    path: &Path,
    create_parent: bool,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
    check: &dyn Fn(&Path) -> Result<(), SaveFailure>,
    before_displace: &dyn Fn(),
) -> Result<(), SaveFailure> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if create_parent {
        fs::create_dir_all(parent).map_err(SaveFailure::Io)?;
    }
    // A directory is never a save target: the move-aside install below would
    // otherwise rename and replace it.
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(SaveFailure::Io(io::Error::new(
            io::ErrorKind::IsADirectory,
            format!("{} is a directory", path.display()),
        )));
    }
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{stem}.hane-{}-{sequence}.tmp",
        std::process::id()
    ));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(SaveFailure::Io)?;
        let mut writer = BufWriter::new(file);
        write(&mut writer).map_err(SaveFailure::Io)?;
        writer.flush().map_err(SaveFailure::Io)?;
        writer.get_ref().sync_all().map_err(SaveFailure::Io)?;
        drop(writer);
        before_displace();
        install_checked(&temporary, path, check)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Installs `temporary` at `path` without ever replacing a version that was
/// not checked. A plain `rename` cannot compare-and-swap, so the current
/// target is first moved aside atomically: whatever sits at `path` at that
/// instant is exactly what gets checked (at its new name), and a file that an
/// external app places later is never overwritten because the final install
/// refuses to replace. A failed check puts the displaced version back.
fn install_checked(
    temporary: &Path,
    path: &Path,
    check: &dyn Fn(&Path) -> Result<(), SaveFailure>,
) -> Result<(), SaveFailure> {
    // Each retry means another writer created a new file at `path` between
    // the move-aside and the install; that file is judged on the next round.
    for _ in 0..MAX_INSTALL_ATTEMPTS {
        let aside = displaced_path(path);
        match rename_file_without_replace(path, &aside) {
            Ok(()) => {
                if let Err(failure) = check(&aside) {
                    return Err(restore_displaced(&aside, path, failure));
                }
                match rename_file_without_replace(temporary, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&aside);
                        return Ok(());
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let _ = fs::remove_file(&aside);
                    }
                    Err(error) => {
                        return Err(restore_displaced(&aside, path, SaveFailure::Io(error)));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                check(path)?;
                match rename_file_without_replace(temporary, path) {
                    Ok(()) => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(SaveFailure::Io(error)),
                }
            }
            Err(error) => return Err(SaveFailure::Io(error)),
        }
    }
    Err(SaveFailure::ExternalChange)
}

/// Puts a displaced version back at `path` and returns `failure`. If another
/// file has appeared there meanwhile the displaced version is kept at its
/// aside name and reported, never deleted or overwritten.
fn restore_displaced(aside: &Path, path: &Path, failure: SaveFailure) -> SaveFailure {
    match rename_file_without_replace(aside, path) {
        Ok(()) => failure,
        Err(restore_error) => SaveFailure::Io(io::Error::other(format!(
            "{failure}; restoring the original file failed ({restore_error}); \
             the previous version remains at {}",
            aside.display()
        ))),
    }
}

fn displaced_path(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{stem}.hane-displaced-{}-{sequence}.tmp",
        std::process::id()
    ))
}

const MAX_INSTALL_ATTEMPTS: usize = 3;

#[cfg(test)]
pub(crate) fn temporary_directory(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "hane-{name}-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MemoryFileService;

    #[test]
    fn atomic_document_save_preserves_markdown_bytes() {
        let root = temporary_directory("save");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("document.md");
        let source = "# 羽\n\n![画像](assets/羽.png)\n\n| A | B |\n|---|---|\n";
        OsFileService
            .save(&path, &RopeBuffer::from_text(source))
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creating_a_directory_in_an_existing_parent_accepts_only_real_directories() {
        let root = temporary_directory("create-dir-existing-parent");
        fs::create_dir_all(&root).unwrap();

        let fresh = root.join("fresh");
        OsFileService.create_dir_in_existing_parent(&fresh).unwrap();
        assert!(fresh.is_dir());
        OsFileService.create_dir_in_existing_parent(&fresh).unwrap();

        let file = root.join("file");
        fs::write(&file, "x").unwrap();
        let error = OsFileService
            .create_dir_in_existing_parent(&file)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        let missing_parent = root.join("missing").join("child");
        let error = OsFileService
            .create_dir_in_existing_parent(&missing_parent)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creating_a_directory_over_a_symlink_to_a_directory_fails() {
        let root = temporary_directory("create-dir-symlink");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let error = OsFileService
            .create_dir_in_existing_parent(&link)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_swapped_for_a_symlink_after_the_check_does_not_redirect_creation() {
        let root = temporary_directory("create-dir-parent-swap");
        let work = root.join("work");
        let parent = work.join("parent");
        let moved = work.join("moved");
        let outside = root.join("outside");
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir_all(&outside).unwrap();

        // The parent is replaced exactly between opening it and creating the
        // child, so the result does not depend on timing.
        create_dir_under_parent_handle(&parent.join("child"), || {
            fs::rename(&parent, &moved).unwrap();
            std::os::unix::fs::symlink(&outside, &parent).unwrap();
        })
        .unwrap();

        assert!(moved.join("child").is_dir());
        assert!(!outside.join("child").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_that_is_already_a_symlink_fails_without_creating_a_child() {
        let root = temporary_directory("create-dir-symlink-parent");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        assert!(
            OsFileService
                .create_dir_in_existing_parent(&link.join("child"))
                .is_err()
        );
        assert!(!outside.join("child").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_held_parent_cannot_be_replaced_while_the_child_is_created() {
        let root = temporary_directory("create-dir-parent-hold");
        let parent = root.join("parent");
        fs::create_dir_all(&parent).unwrap();

        create_dir_under_parent_handle(&parent.join("child"), || {
            // The open parent handle must block rename/replace of the parent.
            assert!(fs::rename(&parent, root.join("moved")).is_err());
        })
        .unwrap();

        assert!(parent.join("child").is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_reparse_point_parent_fails_without_creating_a_child() {
        let root = temporary_directory("create-dir-reparse-parent");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let link = root.join("link");
        // Creating a symlink needs a privilege that is not always granted.
        if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
            fs::remove_dir_all(root).unwrap();
            return;
        }

        assert!(
            OsFileService
                .create_dir_in_existing_parent(&link.join("child"))
                .is_err()
        );
        assert!(!outside.join("child").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn round_trip_through_the_real_filesystem_keeps_identity_and_stamp() {
        let root = temporary_directory("round-trip");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("notes.md");
        let saved = OsFileService
            .save(&path, &RopeBuffer::from_text("hello\n"))
            .unwrap();
        let loaded = OsFileService.load(&path).unwrap();
        assert!(saved.identity.is_same_file(&loaded.identity));
        assert_eq!(saved.stamp, loaded.stamp);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_streaming_reader_keeps_the_same_handle_for_both_stamps() {
        let root = temporary_directory("streaming-reader");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("search.md");
        fs::write(&path, "本文を少しずつ読む🙂\r\n").unwrap();

        let mut reader = OsFileService.open_reader(&path).unwrap();
        let before = reader.stamp_at_open().unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        let after = reader.current_stamp().unwrap().unwrap();

        assert_eq!(bytes, "本文を少しずつ読む🙂\r\n".as_bytes());
        assert_eq!(before, after);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_external_write_during_the_temporary_write_is_not_replaced() {
        let root = temporary_directory("save-race");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("race.md");
        let saved = OsFileService
            .save(&path, &RopeBuffer::from_text("one\n"))
            .unwrap();
        let guard = OverwriteGuard::ExpectContent {
            stamp: saved.stamp,
            baseline: saved.fingerprint,
        };
        let edited = RopeBuffer::from_text("my edit\n");

        // The pre-write check passes; the external save lands while the
        // temporary file is being written.
        check_overwrite_guard(&OsFileService, &path, guard).unwrap();
        let result = atomic_write_checked(
            &path,
            true,
            |writer| {
                fs::write(&path, "someone else, longer\n")?;
                edited.write_to(writer)
            },
            &|inspect| check_overwrite_guard(&OsFileService, inspect, guard),
        );

        assert!(matches!(result, Err(SaveFailure::ExternalChange)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "someone else, longer\n");
        let leftovers = fs::read_dir(&root).unwrap().count();
        assert_eq!(leftovers, 1, "the temporary file must be removed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_atomic_replace_after_the_last_check_is_not_overwritten() {
        let root = temporary_directory("save-race-after-check");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("race.md");
        let saved = OsFileService
            .save(&path, &RopeBuffer::from_text("one\n"))
            .unwrap();
        let guard = OverwriteGuard::ExpectContent {
            stamp: saved.stamp,
            baseline: saved.fingerprint,
        };
        let edited = RopeBuffer::from_text("my edit\n");

        // The pre-write check passes, then an external app atomically
        // replaces the file after the temporary file is complete and before
        // the save replaces the target.
        check_overwrite_guard(&OsFileService, &path, guard).unwrap();
        let external = root.join("external.tmp");
        let result = atomic_write_checked_with_hook(
            &path,
            true,
            |writer| edited.write_to(writer),
            &|inspect| check_overwrite_guard(&OsFileService, inspect, guard),
            &|| {
                fs::write(&external, "external, atomically placed\n").unwrap();
                fs::rename(&external, &path).unwrap();
            },
        );

        assert!(matches!(result, Err(SaveFailure::ExternalChange)));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "external, atomically placed\n"
        );
        let leftovers = fs::read_dir(&root).unwrap().count();
        assert_eq!(leftovers, 1, "no temporary or displaced file may remain");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_forced_save_replaces_a_version_placed_after_the_last_check() {
        let root = temporary_directory("save-race-after-check-force");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("race.md");
        fs::write(&path, "one\n").unwrap();
        let edited = RopeBuffer::from_text("my edit\n");

        atomic_write_checked_with_hook(
            &path,
            true,
            |writer| edited.write_to(writer),
            &|inspect| check_overwrite_guard(&OsFileService, inspect, OverwriteGuard::Force),
            &|| fs::write(&path, "someone else\n").unwrap(),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "my edit\n");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_save_onto_an_existing_directory_fails_and_leaves_it_untouched() {
        let root = temporary_directory("save-onto-directory");
        let target = root.join("folder");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("inner.md"), "inner\n").unwrap();
        let edited = RopeBuffer::from_text("my edit\n");

        let failure = atomic_write_checked(
            &target,
            true,
            |writer| edited.write_to(writer),
            &|inspect| check_overwrite_guard(&OsFileService, inspect, OverwriteGuard::Force),
        )
        .unwrap_err();

        assert!(matches!(failure, SaveFailure::Io(_)));
        assert!(target.is_dir());
        assert_eq!(
            fs::read_to_string(target.join("inner.md")).unwrap(),
            "inner\n"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_forced_save_still_replaces_an_external_write_during_the_temporary_write() {
        let root = temporary_directory("save-race-force");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("race.md");
        fs::write(&path, "one\n").unwrap();
        let edited = RopeBuffer::from_text("my edit\n");

        atomic_write_checked(
            &path,
            true,
            |writer| {
                fs::write(&path, "someone else\n")?;
                edited.write_to(writer)
            },
            &|inspect| check_overwrite_guard(&OsFileService, inspect, OverwriteGuard::Force),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "my edit\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_guarded_save_refuses_to_overwrite_a_file_that_moved_on() {
        let service = MemoryFileService::new();
        let path = Path::new("/notes/a.md");
        let first = service.save(path, &RopeBuffer::from_text("one\n")).unwrap();
        service.write_externally(path, "someone else\n");

        let failure = run_save_job(
            &service,
            path,
            &RopeBuffer::from_text("two\n"),
            OverwriteGuard::ExpectStamp(first.stamp),
        )
        .unwrap_err();
        assert!(matches!(failure, SaveFailure::ExternalChange));
        assert_eq!(service.contents(path).as_deref(), Some("someone else\n"));

        run_save_job(
            &service,
            path,
            &RopeBuffer::from_text("two\n"),
            OverwriteGuard::Force,
        )
        .unwrap();
        assert_eq!(service.contents(path).as_deref(), Some("two\n"));
    }

    #[test]
    fn a_rename_moves_the_file_on_the_real_filesystem() {
        let root = temporary_directory("rename");
        fs::create_dir_all(&root).unwrap();
        let from = root.join("a.md");
        let to = root.join("b.md");
        OsFileService
            .save(&from, &RopeBuffer::from_text("hello\n"))
            .unwrap();
        OsFileService.rename(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read_to_string(&to).unwrap(), "hello\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_case_only_file_rename_works_on_case_insensitive_filesystems() {
        let root = temporary_directory("rename-case-only-file");
        fs::create_dir_all(&root).unwrap();
        let from = root.join("Plain.md");
        let to = root.join("plain.md");
        fs::write(&from, "plain\n").unwrap();

        // A case-sensitive filesystem has two distinct names here, so this
        // platform does not exercise the case-alias path.
        if !to.exists() {
            fs::remove_dir_all(root).unwrap();
            return;
        }

        OsFileService.rename(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(&to).unwrap(), "plain\n");
        let names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name.to_string_lossy() == "plain.md")
        );
        assert!(
            !names
                .iter()
                .any(|name| name.to_string_lossy() == "Plain.md")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_rename_refuses_to_overwrite_an_existing_destination() {
        let root = temporary_directory("rename-collision");
        fs::create_dir_all(&root).unwrap();
        let from = root.join("a.md");
        let to = root.join("b.md");
        OsFileService
            .save(&from, &RopeBuffer::from_text("mine\n"))
            .unwrap();
        OsFileService
            .save(&to, &RopeBuffer::from_text("theirs\n"))
            .unwrap();
        assert!(OsFileService.rename(&from, &to).is_err());
        assert_eq!(fs::read_to_string(&from).unwrap(), "mine\n");
        assert_eq!(fs::read_to_string(&to).unwrap(), "theirs\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_case_only_file_rename_keeps_the_requested_spelling() {
        let root = temporary_directory("rename-case-only-file");
        fs::create_dir_all(&root).unwrap();
        let from = root.join("Note.md");
        let to = root.join("note.md");
        OsFileService
            .save(&from, &RopeBuffer::from_text("case only\n"))
            .unwrap();

        OsFileService.rename(&from, &to).unwrap();

        let names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["note.md"]);
        assert_eq!(fs::read_to_string(&to).unwrap(), "case only\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_non_ascii_case_only_file_rename_keeps_the_requested_spelling() {
        let root = temporary_directory("rename-non-ascii-case-only-file");
        fs::create_dir_all(&root).unwrap();
        let from = root.join("Ä.md");
        let to = root.join("ä.md");
        OsFileService
            .save(&from, &RopeBuffer::from_text("non-ascii case only\n"))
            .unwrap();

        OsFileService.rename(&from, &to).unwrap();

        let names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["ä.md"]);
        assert_eq!(fs::read_to_string(&to).unwrap(), "non-ascii case only\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_folder_rename_moves_descendants_without_replacing_an_existing_folder() {
        let root = temporary_directory("folder-rename");
        fs::create_dir_all(root.join("Project/Deep")).unwrap();
        fs::write(root.join("Project/Deep/Child.md"), "child\n").unwrap();
        fs::create_dir_all(root.join("ExistingFolder")).unwrap();
        fs::write(root.join("ExistingFolder/keep.md"), "keep\n").unwrap();

        let from = root.join("Project");
        let to = root.join("Renamed");
        OsFileService.rename_folder(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(
            fs::read_to_string(to.join("Deep/Child.md")).unwrap(),
            "child\n"
        );

        let collision = OsFileService.rename_folder(&to, &root.join("ExistingFolder"));
        assert!(collision.is_err());
        assert!(to.join("Deep/Child.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("ExistingFolder/keep.md")).unwrap(),
            "keep\n"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_case_only_folder_rename_keeps_the_requested_spelling() {
        let root = temporary_directory("rename-case-only-folder");
        fs::create_dir_all(root.join("Project/Deep")).unwrap();
        fs::write(root.join("Project/Deep/Child.md"), "child\n").unwrap();
        let from = root.join("Project");
        let to = root.join("project");

        OsFileService.rename_folder(&from, &to).unwrap();

        let names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["project"]);
        assert_eq!(
            fs::read_to_string(to.join("Deep/Child.md")).unwrap(),
            "child\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn japanese_filenames_and_paths_round_trip_on_the_real_filesystem() {
        let root = temporary_directory("日本語フォルダ");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("日本語タイトル.md");
        let source = "# 日本語タイトル\n\n日本語の文章をHaneで気持ちよく編集する\n";
        let saved = OsFileService
            .save(&path, &RopeBuffer::from_text(source))
            .unwrap();
        let loaded = OsFileService.load(&path).unwrap();
        assert!(saved.identity.is_same_file(&loaded.identity));
        assert_eq!(fs::read_to_string(&path).unwrap(), source);

        let renamed = root.join("改題した日本語タイトル.md");
        OsFileService.rename(&path, &renamed).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(&renamed).unwrap(), source);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_guarded_save_recreates_a_file_that_was_deleted() {
        let service = MemoryFileService::new();
        let path = Path::new("/notes/a.md");
        let first = service.save(path, &RopeBuffer::from_text("one\n")).unwrap();
        service.delete(path);
        run_save_job(
            &service,
            path,
            &RopeBuffer::from_text("one\n"),
            OverwriteGuard::ExpectStamp(first.stamp),
        )
        .unwrap();
        assert_eq!(service.contents(path).as_deref(), Some("one\n"));
    }
}
