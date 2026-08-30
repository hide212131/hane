//! Work folder model: the Markdown index for a directory the user opens as a
//! "notebook", kept separate from the `DocumentSession`s actually loaded into
//! memory.
//!
//! Listing a work folder answers "which notes exist and what do we call them",
//! nothing more. It never reads note contents and never creates a
//! `DocumentSession`; callers open a `WorkFolderEntry::path()` through
//! `FileService::load` only once a note is actually selected.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One Markdown file discovered under a work folder, not yet loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkFolderEntry {
    path: PathBuf,
    name: String,
}

impl WorkFolderEntry {
    pub(crate) fn new(path: PathBuf) -> Self {
        let name = path.file_stem().map_or_else(
            || path.display().to_string(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        Self { path, name }
    }

    /// The path to open, relative to the process only insofar as the work
    /// folder root itself was relative.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Display name for a sidebar entry: the filename without its `.md`
    /// extension, so a note reads like a title rather than a filesystem path.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The Markdown index of one work folder: every `.md` file discovered under
/// its root, in display order. Deliberately holds no document content and no
/// `DocumentSession`, so scanning a folder with many notes stays cheap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkFolder {
    root: PathBuf,
    entries: Vec<WorkFolderEntry>,
}

impl WorkFolder {
    pub(crate) fn new(root: PathBuf, mut entries: Vec<WorkFolderEntry>) -> Self {
        Self::sort(&mut entries);
        Self { root, entries }
    }

    fn sort(entries: &mut [WorkFolderEntry]) {
        entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    }

    /// Adds a note this app itself just created to the index, so it appears
    /// in the sidebar without waiting for the next full rescan. A path
    /// already present (a race with a rescan that beat this call) is left
    /// alone rather than duplicated.
    pub fn insert(&mut self, path: PathBuf) {
        if self.entries.iter().any(|entry| entry.path == path) {
            return;
        }
        self.entries.push(WorkFolderEntry::new(path));
        Self::sort(&mut self.entries);
    }

    /// Follows a rename this app itself just performed, keeping the index in
    /// sync instead of leaving a stale entry at a path that no longer exists.
    /// A `from` the folder was not scanned with (already renamed, or never
    /// present) is a no-op.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == from) {
            *entry = WorkFolderEntry::new(to.to_path_buf());
            Self::sort(&mut self.entries);
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> &[WorkFolderEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry backed by `path`, if the folder was scanned with one.
    pub fn entry_for_path(&self, path: &Path) -> Option<&WorkFolderEntry> {
        self.entries.iter().find(|entry| entry.path == path)
    }
}

/// The filesystem boundary for opening a work folder.
///
/// Kept separate from `FileService`: that trait reads and writes one file
/// whose path the caller already knows, while a scan walks an entire
/// directory tree to discover paths the caller does not know yet. Mixing the
/// two would force every `FileService` implementation (including test
/// doubles that only ever stand in for a handful of named files) to also
/// model a directory tree. Both boundaries share the same rule, though: every
/// method blocks and every caller runs it off the input path.
pub trait WorkFolderScanner: Send + Sync + 'static {
    /// Lists the `.md` files under `root`, including subdirectories. An empty
    /// or newly created directory scans to an empty `WorkFolder`, not an
    /// error; a `root` that is missing or not a directory is an error.
    fn scan(&self, root: &Path) -> io::Result<WorkFolder>;
}

/// `WorkFolderScanner` backed by the real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsWorkFolderScanner;

impl WorkFolderScanner for OsWorkFolderScanner {
    fn scan(&self, root: &Path) -> io::Result<WorkFolder> {
        if !fs::metadata(root)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "work folder root is not a directory",
            ));
        }
        let mut entries = Vec::new();
        walk(root, root, &mut entries)?;
        Ok(WorkFolder::new(root.to_path_buf(), entries))
    }
}

fn walk(root: &Path, dir: &Path, entries: &mut Vec<WorkFolderEntry>) -> io::Result<()> {
    for item in fs::read_dir(dir)? {
        let item = item?;
        let path = item.path();
        let file_type = item.file_type()?;
        if file_type.is_dir() {
            // `.hane` directly under the work folder root is where Hane
            // keeps its own state for this work folder (the unnamed-note
            // recovery journal, for instance): never a directory of the
            // user's own notes. Only that one directory is excluded, so
            // dotfile directories the user actually keeps notes in (`.notes`,
            // `.github`, and the like) are still scanned, the same as before
            // the recovery journal existed.
            if is_root_hane_directory(root, &path) {
                continue;
            }
            walk(root, &path, entries)?;
        } else if file_type.is_file() && is_markdown(&path) {
            entries.push(WorkFolderEntry::new(path));
        }
    }
    Ok(())
}

fn is_root_hane_directory(root: &Path, path: &Path) -> bool {
    path.parent() == Some(root)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".hane")
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::temporary_directory;

    #[test]
    fn inserting_a_new_note_adds_it_in_sorted_position_without_a_rescan() {
        let mut folder = WorkFolder::new(
            PathBuf::from("/notes"),
            vec![
                WorkFolderEntry::new(PathBuf::from("/notes/Alpha.md")),
                WorkFolderEntry::new(PathBuf::from("/notes/Zeta.md")),
            ],
        );
        folder.insert(PathBuf::from("/notes/Mid.md"));
        let names: Vec<&str> = folder.entries().iter().map(WorkFolderEntry::name).collect();
        assert_eq!(names, ["Alpha", "Mid", "Zeta"]);
    }

    #[test]
    fn inserting_an_already_present_path_does_not_duplicate_it() {
        let mut folder = WorkFolder::new(
            PathBuf::from("/notes"),
            vec![WorkFolderEntry::new(PathBuf::from("/notes/Alpha.md"))],
        );
        folder.insert(PathBuf::from("/notes/Alpha.md"));
        assert_eq!(folder.len(), 1);
    }

    #[test]
    fn renaming_an_entry_follows_the_note_to_its_new_path_and_resorts() {
        let mut folder = WorkFolder::new(
            PathBuf::from("/notes"),
            vec![
                WorkFolderEntry::new(PathBuf::from("/notes/Alpha.md")),
                WorkFolderEntry::new(PathBuf::from("/notes/Zeta.md")),
            ],
        );
        folder.rename(Path::new("/notes/Alpha.md"), Path::new("/notes/Omega.md"));
        let names: Vec<&str> = folder.entries().iter().map(WorkFolderEntry::name).collect();
        assert_eq!(names, ["Omega", "Zeta"]);
        assert!(
            folder
                .entry_for_path(Path::new("/notes/Alpha.md"))
                .is_none()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/Omega.md"))
                .is_some()
        );
    }

    #[test]
    fn renaming_a_path_the_folder_was_not_scanned_with_is_a_no_op() {
        let mut folder = WorkFolder::new(
            PathBuf::from("/notes"),
            vec![WorkFolderEntry::new(PathBuf::from("/notes/Alpha.md"))],
        );
        folder.rename(
            Path::new("/notes/Missing.md"),
            Path::new("/notes/Renamed.md"),
        );
        assert_eq!(folder.len(), 1);
        assert!(
            folder
                .entry_for_path(Path::new("/notes/Alpha.md"))
                .is_some()
        );
    }

    #[test]
    fn an_empty_directory_scans_to_an_empty_work_folder() {
        let root = temporary_directory("workfolder-empty");
        fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        assert!(work_folder.is_empty());
        assert_eq!(work_folder.root(), root);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn markdown_files_are_discovered_and_non_markdown_files_are_ignored() {
        let root = temporary_directory("workfolder-scan");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("LangChain4j.md"), "# LangChain4j\n").unwrap();
        fs::write(root.join("Meeting.MD"), "# Meeting\n").unwrap();
        fs::write(root.join("notes.txt"), "not markdown\n").unwrap();
        fs::write(root.join("nested/TODO.md"), "# TODO\n").unwrap();

        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let names: Vec<&str> = work_folder
            .entries()
            .iter()
            .map(WorkFolderEntry::name)
            .collect();
        assert_eq!(names, ["LangChain4j", "Meeting", "TODO"]);
        assert!(
            work_folder
                .entry_for_path(&root.join("nested/TODO.md"))
                .is_some()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_root_recovery_journal_directory_is_not_scanned() {
        let root = temporary_directory("workfolder-hidden");
        fs::create_dir_all(root.join(".hane/drafts")).unwrap();
        fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();
        fs::write(root.join(".hane/drafts/0000000000000001.md"), "draft\n").unwrap();

        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let names: Vec<&str> = work_folder
            .entries()
            .iter()
            .map(WorkFolderEntry::name)
            .collect();
        assert_eq!(names, ["Meeting"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dotfile_directories_other_than_the_root_recovery_journal_are_still_scanned() {
        // Only `.hane` directly under the work folder root is Hane's own
        // state; a user keeping notes under a dotfile directory of their own
        // (`.notes`, `.github`, and so on) must not have them disappear from
        // the work folder just because the recovery journal also lives under
        // a dot-prefixed name.
        let root = temporary_directory("workfolder-dotfile-notes");
        fs::create_dir_all(root.join(".notes")).unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::create_dir_all(root.join("nested/.hane")).unwrap();
        fs::write(root.join(".notes/foo.md"), "# foo\n").unwrap();
        fs::write(root.join(".github/ISSUE_TEMPLATE.md"), "# template\n").unwrap();
        fs::write(root.join("nested/.hane/not-a-draft.md"), "# nope\n").unwrap();

        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let mut names: Vec<&str> = work_folder
            .entries()
            .iter()
            .map(WorkFolderEntry::name)
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["ISSUE_TEMPLATE", "foo", "not-a-draft"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_root_is_reported_as_an_error() {
        let root = temporary_directory("workfolder-missing");
        assert!(OsWorkFolderScanner.scan(&root).is_err());
    }

    #[test]
    fn a_file_root_is_reported_as_an_error() {
        let root = temporary_directory("workfolder-file");
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(&root, "not a directory").unwrap();
        assert!(OsWorkFolderScanner.scan(&root).is_err());
        fs::remove_file(root).unwrap();
    }
}
