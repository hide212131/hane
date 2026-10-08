//! In-memory doubles so the session rules can be tested without a filesystem.

use crate::identity::{FileIdentity, FileStamp};
use crate::service::{FileService, LoadedFile, ReadFile, SavedFile, StampedRead};
use crate::workfolder::{WorkFolder, WorkFolderScanner};
use hane_document::RopeBuffer;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type MemoryFile = (Arc<[u8]>, u64);

/// A filesystem that lives in a map. Writes bump a synthetic modification
/// counter, so external-change detection can be exercised deterministically.
#[derive(Debug, Default)]
pub struct MemoryFileService {
    files: Arc<Mutex<HashMap<PathBuf, MemoryFile>>>,
    directories: Mutex<std::collections::HashSet<PathBuf>>,
    reader_behaviors: Mutex<HashMap<PathBuf, MemoryReadBehavior>>,
    load_calls: AtomicU64,
    save_calls: AtomicU64,
    clock: AtomicU64,
}

/// Deterministic reader faults for tests of streaming search behavior.
#[derive(Clone, Debug, Default)]
pub struct MemoryReadBehavior {
    /// Largest byte count returned by one `read` call.
    pub max_chunk_bytes: Option<usize>,
    /// Return an I/O error after this many bytes have been read.
    pub fail_after_bytes: Option<usize>,
    /// Delay each non-empty read by this duration.
    pub delay_per_read: Option<Duration>,
    /// Report a changed stamp after this many bytes have been read.
    pub change_stamp_after_bytes: Option<usize>,
}

struct MemoryStampedRead {
    contents: Arc<[u8]>,
    position: usize,
    stamp: FileStamp,
    behavior: MemoryReadBehavior,
    files: Arc<Mutex<HashMap<PathBuf, MemoryFile>>>,
    path: PathBuf,
}

impl io::Read for MemoryStampedRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Some(delay) = self.behavior.delay_per_read {
            std::thread::sleep(delay);
        }
        if self
            .behavior
            .fail_after_bytes
            .is_some_and(|limit| self.position >= limit)
        {
            return Err(io::Error::other("injected reader failure"));
        }
        let bytes = self.contents.as_ref();
        if self.position == bytes.len() {
            return Ok(0);
        }
        let mut count = buffer.len().min(bytes.len() - self.position);
        if let Some(chunk_limit) = self.behavior.max_chunk_bytes {
            count = count.min(chunk_limit.max(1));
        }
        if let Some(fail_after) = self.behavior.fail_after_bytes {
            count = count.min(fail_after.saturating_sub(self.position));
            if count == 0 {
                return Err(io::Error::other("injected reader failure"));
            }
        }
        buffer[..count].copy_from_slice(&bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl StampedRead for MemoryStampedRead {
    fn current_stamp(&self) -> io::Result<Option<FileStamp>> {
        Ok(Some(
            if self
                .behavior
                .change_stamp_after_bytes
                .is_some_and(|limit| self.position >= limit)
            {
                FileStamp::new(self.stamp.len.wrapping_add(1), self.stamp.modified)
            } else {
                self.stamp
            },
        ))
    }

    /// Looks the stamp up by path in the shared map, independent of the
    /// snapshot this handle was opened with. This is what lets tests model an
    /// atomic rename that replaces `path` while a reader opened before the
    /// replacement is still mid-read.
    fn path_stamp(&self) -> io::Result<Option<FileStamp>> {
        Ok(self
            .files
            .lock()
            .expect("files lock")
            .get(&self.path)
            .map(|(contents, version)| memory_stamp(contents, *version)))
    }
}

fn memory_stamp(contents: &Arc<[u8]>, version: u64) -> FileStamp {
    FileStamp::new((contents.len() as u64) ^ (version << 32), None)
}

impl MemoryFileService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a file without going through the session, as if it were already on
    /// disk when the app started.
    pub fn write_externally(&self, path: impl AsRef<Path>, contents: &str) {
        self.write_bytes_externally(path, contents.as_bytes());
    }

    /// Seeds arbitrary bytes, including invalid UTF-8, for reader tests.
    pub fn write_bytes_externally(&self, path: impl AsRef<Path>, contents: &[u8]) {
        let version = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        self.files
            .lock()
            .expect("files lock")
            .insert(canonical(path.as_ref()), (Arc::from(contents), version));
    }

    /// Configures deterministic behavior for readers opened on `path`.
    pub fn set_read_behavior(&self, path: impl AsRef<Path>, behavior: MemoryReadBehavior) {
        self.reader_behaviors
            .lock()
            .expect("reader behaviors lock")
            .insert(canonical(path.as_ref()), behavior);
    }

    /// Number of times `load` has been called, for asserting search does not
    /// create document sessions by loading files.
    pub fn load_calls(&self) -> u64 {
        self.load_calls.load(Ordering::Relaxed)
    }

    /// Number of times `save` has been called.
    pub fn save_calls(&self) -> u64 {
        self.save_calls.load(Ordering::Relaxed)
    }

    pub fn delete(&self, path: impl AsRef<Path>) {
        self.files
            .lock()
            .expect("files lock")
            .remove(&canonical(path.as_ref()));
    }

    /// Simulates a rename done by something other than this session, e.g. a
    /// filer or an external editor — as opposed to `FileService::rename`,
    /// which is the session's own boundary for renaming a file it owns.
    pub fn rename_externally(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
        let mut files = self.files.lock().expect("files lock");
        if let Some(entry) = files.remove(&canonical(from.as_ref())) {
            files.insert(canonical(to.as_ref()), entry);
        }
    }

    pub fn contents(&self, path: impl AsRef<Path>) -> Option<String> {
        self.files
            .lock()
            .expect("files lock")
            .get(&canonical(path.as_ref()))
            .and_then(|(contents, _)| String::from_utf8(contents.to_vec()).ok())
    }

    /// Whether `create_dir` has been called for `path` (or an ancestor of it
    /// implied a directory that has since been created directly).
    pub fn directory_exists(&self, path: impl AsRef<Path>) -> bool {
        self.directories
            .lock()
            .expect("directories lock")
            .contains(&canonical(path.as_ref()))
    }
}

impl FileService for MemoryFileService {
    fn open_reader(&self, path: &Path) -> io::Result<ReadFile> {
        let key = canonical(path);
        let (contents, version) = self
            .files
            .lock()
            .expect("files lock")
            .get(&key)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))?;
        let behavior = self
            .reader_behaviors
            .lock()
            .expect("reader behaviors lock")
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let stamp = memory_stamp(&contents, version);
        ReadFile::from_reader(
            MemoryStampedRead {
                contents,
                position: 0,
                stamp,
                behavior,
                files: Arc::clone(&self.files),
                path: key,
            },
            FileIdentity::lexical(path),
        )
    }

    fn load(&self, path: &Path) -> io::Result<LoadedFile> {
        self.load_calls.fetch_add(1, Ordering::Relaxed);
        let files = self.files.lock().expect("files lock");
        let (contents, version) = files
            .get(&canonical(path))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))?;
        Ok(LoadedFile {
            document: RopeBuffer::from_text(
                std::str::from_utf8(contents)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            ),
            identity: FileIdentity::lexical(path),
            stamp: Some(FileStamp::new(contents.len() as u64, None)).map(|stamp| FileStamp {
                len: stamp.len ^ (*version << 32),
                modified: None,
            }),
        })
    }

    fn save(&self, path: &Path, document: &RopeBuffer) -> io::Result<SavedFile> {
        self.save_calls.fetch_add(1, Ordering::Relaxed);
        let mut contents = Vec::new();
        document.write_to(&mut contents)?;
        let contents = String::from_utf8(contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let version = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let stamp = FileStamp::new((contents.len() as u64) ^ (version << 32), None);
        self.files
            .lock()
            .expect("files lock")
            .insert(canonical(path), (Arc::from(contents.into_bytes()), version));
        Ok(SavedFile {
            identity: FileIdentity::lexical(path),
            stamp: Some(stamp),
        })
    }

    fn stamp(&self, path: &Path) -> Option<FileStamp> {
        self.files
            .lock()
            .expect("files lock")
            .get(&canonical(path))
            .map(|(contents, version)| {
                FileStamp::new((contents.len() as u64) ^ (version << 32), None)
            })
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut files = self.files.lock().expect("files lock");
        if files.contains_key(&canonical(to)) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "rename target already exists",
            ));
        }
        let Some(entry) = files.remove(&canonical(from)) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such file"));
        };
        files.insert(canonical(to), entry);
        Ok(())
    }

    fn rename_folder(&self, from: &Path, to: &Path) -> io::Result<()> {
        let from = canonical(from);
        let to = canonical(to);
        let mut files = self.files.lock().expect("files lock");
        let mut directories = self.directories.lock().expect("directories lock");
        if files.contains_key(&to)
            || directories.contains(&to)
            || files.keys().any(|path| path.starts_with(&to))
            || directories.iter().any(|path| path.starts_with(&to))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "rename target already exists",
            ));
        }
        let has_source =
            directories.contains(&from) || files.keys().any(|path| path.starts_with(&from));
        if !has_source {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such directory"));
        }

        let moved_files: Vec<_> = files
            .keys()
            .filter_map(|path| {
                rebase_memory_path(path, &from, &to).map(|moved| (path.clone(), moved))
            })
            .collect();
        for (old, new) in moved_files {
            if let Some(value) = files.remove(&old) {
                files.insert(new, value);
            }
        }
        let moved_directories: Vec<_> = directories
            .iter()
            .filter_map(|path| {
                rebase_memory_path(path, &from, &to).map(|moved| (path.clone(), moved))
            })
            .collect();
        for (old, new) in moved_directories {
            directories.remove(&old);
            directories.insert(new);
        }
        Ok(())
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.directories
            .lock()
            .expect("directories lock")
            .insert(canonical(path));
        Ok(())
    }
}

fn canonical(path: &Path) -> PathBuf {
    FileIdentity::lexical(path).canonical_path().to_path_buf()
}

fn rebase_memory_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(from).ok()?;
    Some(if relative.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(relative)
    })
}

/// One seeded work folder's Markdown files and empty folders.
type SeededWorkFolder = (Vec<PathBuf>, Vec<PathBuf>);

/// A work folder that lives in a map, keyed by root. Lets tests exercise
/// sidebar/discovery logic without touching the real filesystem.
#[derive(Debug, Default)]
pub struct MemoryWorkFolderScanner {
    roots: Mutex<HashMap<PathBuf, SeededWorkFolder>>,
}

impl MemoryWorkFolderScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a work folder with the given Markdown paths, as if they already
    /// existed on disk when the folder was opened. Registering a root with no
    /// paths models an empty directory.
    pub fn seed(&self, root: impl Into<PathBuf>, paths: impl IntoIterator<Item = PathBuf>) {
        self.roots
            .lock()
            .expect("roots lock")
            .entry(root.into())
            .or_default()
            .0 = paths.into_iter().collect();
    }

    /// Seeds a work folder with the given empty folders, as if they already
    /// existed on disk with nothing in them when the folder was opened.
    pub fn seed_folders(
        &self,
        root: impl Into<PathBuf>,
        folders: impl IntoIterator<Item = PathBuf>,
    ) {
        self.roots
            .lock()
            .expect("roots lock")
            .entry(root.into())
            .or_default()
            .1 = folders.into_iter().collect();
    }
}

impl WorkFolderScanner for MemoryWorkFolderScanner {
    fn scan(&self, root: &Path) -> io::Result<WorkFolder> {
        let roots = self.roots.lock().expect("roots lock");
        let (files, folders) = roots
            .get(root)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such work folder"))?;
        let mut work_folder = WorkFolder::new(
            root.to_path_buf(),
            files
                .iter()
                .cloned()
                .map(crate::workfolder::WorkFolderEntry::new)
                .collect(),
        );
        for folder in folders {
            work_folder.insert_folder(folder.clone());
        }
        Ok(work_folder)
    }
}
