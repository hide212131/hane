use crate::identity::{FileIdentity, FileStamp};
use hane_document::RopeBuffer;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// A document read from the I/O boundary, with everything the session needs to
/// track the file afterwards.
pub struct LoadedFile {
    pub document: RopeBuffer,
    pub identity: FileIdentity,
    pub stamp: Option<FileStamp>,
}

/// The result of one successful write.
#[derive(Clone, Debug)]
pub struct SavedFile {
    pub identity: FileIdentity,
    pub stamp: Option<FileStamp>,
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

    /// Writes `document` to `path` atomically: a complete temporary file is
    /// renamed over the target, so a crash never leaves a half-written document.
    fn save(&self, path: &Path, document: &RopeBuffer) -> io::Result<SavedFile>;

    /// Current stamp, or `None` when the file does not exist.
    fn stamp(&self, path: &Path) -> Option<FileStamp>;
}

/// Runs one save job against a service, enforcing the external-change rule the
/// job carries. This is the only place a document is allowed to overwrite a file.
pub fn run_save_job(
    service: &dyn FileService,
    path: &Path,
    document: &RopeBuffer,
    guard: OverwriteGuard,
) -> Result<SavedFile, SaveFailure> {
    if let OverwriteGuard::ExpectStamp(expected) = guard {
        let current = service.stamp(path);
        let unchanged = match (expected, current) {
            (Some(expected), Some(current)) => expected == current,
            // The file we expected to update is gone: recreating it is the
            // session's own content, not an overwrite of someone else's.
            (Some(_), None) | (None, None) => true,
            // We believed the path was free and it is not.
            (None, Some(_)) => false,
        };
        if !unchanged {
            return Err(SaveFailure::ExternalChange);
        }
    }
    service.save(path, document).map_err(SaveFailure::Io)
}

/// Whether a write must first prove the file has not changed underneath it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverwriteGuard {
    /// Write unconditionally: the user picked this target explicitly, or already
    /// answered the conflict prompt.
    Force,
    /// Write only if the disk still matches the stamp the session recorded.
    ExpectStamp(Option<FileStamp>),
}

/// `FileService` backed by the real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsFileService;

impl FileService for OsFileService {
    fn load(&self, path: &Path) -> io::Result<LoadedFile> {
        let file = fs::File::open(path)?;
        let stamp = stamp_from_metadata(file.metadata().ok());
        let document = RopeBuffer::from_reader(io::BufReader::new(file))?;
        Ok(LoadedFile {
            document,
            identity: identity_for(path),
            stamp,
        })
    }

    fn save(&self, path: &Path, document: &RopeBuffer) -> io::Result<SavedFile> {
        atomic_write(path, |writer| document.write_to(writer))?;
        Ok(SavedFile {
            identity: identity_for(path),
            stamp: self.stamp(path),
        })
    }

    fn stamp(&self, path: &Path) -> Option<FileStamp> {
        stamp_from_metadata(fs::metadata(path).ok())
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

pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, |writer| writer.write_all(bytes))
}

fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<fs::File>) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
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
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        write(&mut writer)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

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
