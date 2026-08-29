//! In-memory doubles so the session rules can be tested without a filesystem.

use crate::identity::{FileIdentity, FileStamp};
use crate::service::{FileService, LoadedFile, SavedFile};
use crate::workfolder::{WorkFolder, WorkFolderScanner};
use hane_document::RopeBuffer;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A filesystem that lives in a map. Writes bump a synthetic modification
/// counter, so external-change detection can be exercised deterministically.
#[derive(Debug, Default)]
pub struct MemoryFileService {
    files: Mutex<HashMap<PathBuf, (String, u64)>>,
    clock: AtomicU64,
}

impl MemoryFileService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a file without going through the session, as if it were already on
    /// disk when the app started.
    pub fn write_externally(&self, path: impl AsRef<Path>, contents: &str) {
        let version = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        self.files
            .lock()
            .expect("files lock")
            .insert(canonical(path.as_ref()), (contents.to_owned(), version));
    }

    pub fn delete(&self, path: impl AsRef<Path>) {
        self.files
            .lock()
            .expect("files lock")
            .remove(&canonical(path.as_ref()));
    }

    pub fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) {
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
            .map(|(contents, _)| contents.clone())
    }
}

impl FileService for MemoryFileService {
    fn load(&self, path: &Path) -> io::Result<LoadedFile> {
        let files = self.files.lock().expect("files lock");
        let (contents, version) = files
            .get(&canonical(path))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))?;
        Ok(LoadedFile {
            document: RopeBuffer::from_text(contents),
            identity: FileIdentity::lexical(path),
            stamp: Some(FileStamp::new(contents.len() as u64, None)).map(|stamp| FileStamp {
                len: stamp.len ^ (*version << 32),
                modified: None,
            }),
        })
    }

    fn save(&self, path: &Path, document: &RopeBuffer) -> io::Result<SavedFile> {
        let mut contents = Vec::new();
        document.write_to(&mut contents)?;
        let contents = String::from_utf8(contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let version = self.clock.fetch_add(1, Ordering::Relaxed) + 1;
        let stamp = FileStamp::new((contents.len() as u64) ^ (version << 32), None);
        self.files
            .lock()
            .expect("files lock")
            .insert(canonical(path), (contents, version));
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
}

fn canonical(path: &Path) -> PathBuf {
    FileIdentity::lexical(path).canonical_path().to_path_buf()
}

/// A work folder that lives in a map, keyed by root. Lets tests exercise
/// sidebar/discovery logic without touching the real filesystem.
#[derive(Debug, Default)]
pub struct MemoryWorkFolderScanner {
    roots: Mutex<HashMap<PathBuf, Vec<PathBuf>>>,
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
            .insert(root.into(), paths.into_iter().collect());
    }
}

impl WorkFolderScanner for MemoryWorkFolderScanner {
    fn scan(&self, root: &Path) -> io::Result<WorkFolder> {
        let roots = self.roots.lock().expect("roots lock");
        let paths = roots
            .get(root)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such work folder"))?;
        Ok(WorkFolder::new(
            root.to_path_buf(),
            paths
                .iter()
                .cloned()
                .map(crate::workfolder::WorkFolderEntry::new)
                .collect(),
        ))
    }
}
