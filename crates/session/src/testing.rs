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
    directories: Mutex<std::collections::HashSet<PathBuf>>,
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
            .map(|(contents, _)| contents.clone())
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
