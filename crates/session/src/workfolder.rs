//! Work folder model: the Markdown/folder index for a directory the user opens
//! as a "notebook", kept separate from the `DocumentSession`s actually loaded
//! into memory.
//!
//! Listing a work folder answers "which notes and folders exist and how do
//! they nest", nothing more. It never reads note contents and never creates a
//! `DocumentSession`; callers open a `WorkFolderEntry::path()` through
//! `FileService::load` only once a note is actually selected.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One Markdown file discovered under a work folder, not yet loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkFolderEntry {
    path: PathBuf,
    name: String,
    file_name: String,
}

impl WorkFolderEntry {
    pub(crate) fn new(path: PathBuf) -> Self {
        let name = path.file_stem().map_or_else(
            || path.display().to_string(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        let file_name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Self {
            path,
            name,
            file_name,
        }
    }

    /// The path to open, relative to the process only insofar as the work
    /// folder root itself was relative.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Display name for a flat sidebar entry: the filename without its `.md`
    /// extension, so a note reads like a title rather than a filesystem path.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Display name for a tree entry, including the `.md` extension.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

/// One node of a work folder's hierarchy: either a Markdown file or a folder
/// holding more nodes (possibly none, for an empty folder).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkFolderNode {
    File(WorkFolderEntry),
    Folder(WorkFolderFolder),
}

impl WorkFolderNode {
    pub fn path(&self) -> &Path {
        match self {
            Self::File(entry) => entry.path(),
            Self::Folder(folder) => folder.path(),
        }
    }

    fn sort_key(&self) -> &str {
        match self {
            Self::File(entry) => entry.file_name(),
            Self::Folder(folder) => folder.name(),
        }
    }
}

/// A folder under a work folder, holding its own children (files and, in
/// turn, subfolders). Kept even when `children` is empty, so an empty folder
/// still shows up in the sidebar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkFolderFolder {
    path: PathBuf,
    name: String,
    children: Vec<WorkFolderNode>,
}

impl WorkFolderFolder {
    fn new(path: PathBuf, children: Vec<WorkFolderNode>) -> Self {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Self {
            path,
            name,
            children,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// This folder's direct children, folders and files mixed and sorted by
    /// display name.
    pub fn children(&self) -> &[WorkFolderNode] {
        &self.children
    }
}

/// The hierarchical index of one work folder: every folder and `.md` file
/// discovered under its root, nested the same way they are on disk.
/// Deliberately holds no document content and no `DocumentSession`, so
/// scanning a folder with many notes stays cheap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkFolder {
    root: PathBuf,
    children: Vec<WorkFolderNode>,
}

impl WorkFolder {
    /// Builds a work folder from a flat list of Markdown paths, inferring the
    /// folder structure between `root` and each path. Used by tests and by
    /// `MemoryWorkFolderScanner`, which model file discovery without also
    /// modeling directories explicitly.
    pub(crate) fn new(root: PathBuf, entries: Vec<WorkFolderEntry>) -> Self {
        let mut folder = Self {
            root,
            children: Vec::new(),
        };
        for entry in entries {
            folder.insert(entry.path().to_path_buf());
        }
        folder
    }

    /// Builds a work folder from an already-assembled tree, e.g. the result
    /// of a real filesystem walk that also knows about empty folders.
    pub(crate) fn from_tree(root: PathBuf, mut children: Vec<WorkFolderNode>) -> Self {
        sort_nodes(&mut children);
        Self { root, children }
    }

    /// Adds a note this app itself just created to the index, so it appears
    /// in the sidebar without waiting for the next full rescan. A path
    /// already present (a race with a rescan that beat this call) is left
    /// alone rather than duplicated. Any folder between `root` and `path`
    /// that is not yet in the tree is created along the way.
    pub fn insert(&mut self, path: PathBuf) {
        if self.entry_for_path(&path).is_some() {
            return;
        }
        let Ok(relative) = path.strip_prefix(&self.root).map(Path::to_path_buf) else {
            return;
        };
        let components: Vec<&OsStr> = relative.components().map(|c| c.as_os_str()).collect();
        if components.is_empty() {
            return;
        }
        insert_file(&mut self.children, &self.root, &components, path);
    }

    /// Adds a folder this app itself just created to the index, so it
    /// appears in the sidebar without waiting for the next full rescan. Any
    /// folder between `root` and `path` that is not yet in the tree is
    /// created along the way.
    pub fn insert_folder(&mut self, path: PathBuf) {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        let components: Vec<&OsStr> = relative.components().map(|c| c.as_os_str()).collect();
        if components.is_empty() {
            return;
        }
        insert_folder_at(&mut self.children, &self.root, &components);
    }

    /// Follows a rename this app itself just performed, keeping the index in
    /// sync instead of leaving a stale entry at a path that no longer exists.
    /// A `from` the folder was not scanned with (already renamed, or never
    /// present) is a no-op.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        if remove_file(&mut self.children, from) {
            self.insert(to.to_path_buf());
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root's direct children, folders and files mixed and sorted by
    /// display name.
    pub fn children(&self) -> &[WorkFolderNode] {
        &self.children
    }

    /// Every Markdown file in the tree, depth-first, folders visited in
    /// display order. Kept for callers (and tests) that only care about the
    /// flat file list, not the hierarchy.
    pub fn entries(&self) -> Vec<WorkFolderEntry> {
        let mut out = Vec::new();
        collect_files(&self.children, &mut out);
        out
    }

    pub fn len(&self) -> usize {
        count_files(&self.children)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The entry backed by `path`, if the folder was scanned with one.
    pub fn entry_for_path(&self, path: &Path) -> Option<&WorkFolderEntry> {
        find_file(&self.children, path)
    }

    /// The children of the folder at `path`, used to pick a collision-free
    /// name for a new file or folder created inside it. `path` equal to
    /// `root` returns the top-level children; any other `path` must name a
    /// folder already in the tree, or `None` is returned.
    pub fn children_at(&self, path: &Path) -> Option<&[WorkFolderNode]> {
        if path == self.root {
            return Some(&self.children);
        }
        find_folder(&self.children, path).map(WorkFolderFolder::children)
    }
}

fn find_folder<'a>(nodes: &'a [WorkFolderNode], path: &Path) -> Option<&'a WorkFolderFolder> {
    for node in nodes {
        match node {
            WorkFolderNode::Folder(folder) if folder.path() == path => return Some(folder),
            WorkFolderNode::Folder(folder) => {
                if let Some(found) = find_folder(&folder.children, path) {
                    return Some(found);
                }
            }
            WorkFolderNode::File(_) => {}
        }
    }
    None
}

fn sort_nodes(nodes: &mut [WorkFolderNode]) {
    nodes.sort_by(|a, b| {
        a.sort_key()
            .cmp(b.sort_key())
            .then_with(|| a.path().cmp(b.path()))
    });
}

fn insert_file(
    nodes: &mut Vec<WorkFolderNode>,
    current_dir: &Path,
    components: &[&OsStr],
    file_path: PathBuf,
) {
    if components.len() == 1 {
        nodes.push(WorkFolderNode::File(WorkFolderEntry::new(file_path)));
        sort_nodes(nodes);
        return;
    }
    let index = folder_index(nodes, current_dir, components[0]);
    if let WorkFolderNode::Folder(folder) = &mut nodes[index] {
        let folder_path = folder.path().to_path_buf();
        insert_file(
            &mut folder.children,
            &folder_path,
            &components[1..],
            file_path,
        );
    }
}

fn insert_folder_at(nodes: &mut Vec<WorkFolderNode>, current_dir: &Path, components: &[&OsStr]) {
    let index = folder_index(nodes, current_dir, components[0]);
    if components.len() == 1 {
        return;
    }
    if let WorkFolderNode::Folder(folder) = &mut nodes[index] {
        let folder_path = folder.path().to_path_buf();
        insert_folder_at(&mut folder.children, &folder_path, &components[1..]);
    }
}

/// The index in `nodes` of the folder named `name` directly under
/// `current_dir`, creating it (with no children yet) if it is not already
/// there.
fn folder_index(nodes: &mut Vec<WorkFolderNode>, current_dir: &Path, name: &OsStr) -> usize {
    let folder_path = current_dir.join(name);
    if let Some(index) = nodes.iter().position(
        |node| matches!(node, WorkFolderNode::Folder(folder) if folder.path() == folder_path),
    ) {
        return index;
    }
    nodes.push(WorkFolderNode::Folder(WorkFolderFolder::new(
        folder_path.clone(),
        Vec::new(),
    )));
    sort_nodes(nodes);
    nodes
        .iter()
        .position(
            |node| matches!(node, WorkFolderNode::Folder(folder) if folder.path() == folder_path),
        )
        .expect("folder just inserted")
}

fn remove_file(nodes: &mut Vec<WorkFolderNode>, path: &Path) -> bool {
    if let Some(index) = nodes
        .iter()
        .position(|node| matches!(node, WorkFolderNode::File(entry) if entry.path() == path))
    {
        nodes.remove(index);
        return true;
    }
    for node in nodes.iter_mut() {
        if let WorkFolderNode::Folder(folder) = node
            && remove_file(&mut folder.children, path)
        {
            return true;
        }
    }
    false
}

fn find_file<'a>(nodes: &'a [WorkFolderNode], path: &Path) -> Option<&'a WorkFolderEntry> {
    for node in nodes {
        match node {
            WorkFolderNode::File(entry) if entry.path() == path => return Some(entry),
            WorkFolderNode::Folder(folder) => {
                if let Some(found) = find_file(&folder.children, path) {
                    return Some(found);
                }
            }
            WorkFolderNode::File(_) => {}
        }
    }
    None
}

fn collect_files(nodes: &[WorkFolderNode], out: &mut Vec<WorkFolderEntry>) {
    for node in nodes {
        match node {
            WorkFolderNode::File(entry) => out.push(entry.clone()),
            WorkFolderNode::Folder(folder) => collect_files(&folder.children, out),
        }
    }
}

fn count_files(nodes: &[WorkFolderNode]) -> usize {
    nodes
        .iter()
        .map(|node| match node {
            WorkFolderNode::File(_) => 1,
            WorkFolderNode::Folder(folder) => count_files(&folder.children),
        })
        .sum()
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
    /// Lists the folders and `.md` files under `root`, including empty
    /// subdirectories. An empty or newly created directory scans to an empty
    /// `WorkFolder`, not an error; a `root` that is missing or not a
    /// directory is an error.
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
        let children = walk(root, root)?;
        Ok(WorkFolder::from_tree(root.to_path_buf(), children))
    }
}

fn walk(root: &Path, dir: &Path) -> io::Result<Vec<WorkFolderNode>> {
    let mut nodes = Vec::new();
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
            let children = walk(root, &path)?;
            nodes.push(WorkFolderNode::Folder(WorkFolderFolder::new(
                path, children,
            )));
        } else if file_type.is_file() && is_markdown(&path) {
            nodes.push(WorkFolderNode::File(WorkFolderEntry::new(path)));
        }
    }
    sort_nodes(&mut nodes);
    Ok(nodes)
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
        let names: Vec<String> = folder
            .entries()
            .into_iter()
            .map(|entry| entry.name().to_owned())
            .collect();
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
        let names: Vec<String> = folder
            .entries()
            .into_iter()
            .map(|entry| entry.name().to_owned())
            .collect();
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

        let names: Vec<String> = work_folder
            .entries()
            .into_iter()
            .map(|entry| entry.name().to_owned())
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
        let names: Vec<String> = work_folder
            .entries()
            .into_iter()
            .map(|entry| entry.name().to_owned())
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
        let mut names: Vec<String> = work_folder
            .entries()
            .into_iter()
            .map(|entry| entry.name().to_owned())
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

    #[test]
    fn empty_folders_are_discovered_and_kept_in_the_tree() {
        let root = temporary_directory("workfolder-empty-folder");
        fs::create_dir_all(root.join("Archive")).unwrap();
        fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();

        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let folder = work_folder
            .children()
            .iter()
            .find_map(|node| match node {
                WorkFolderNode::Folder(folder) if folder.name() == "Archive" => Some(folder),
                _ => None,
            })
            .expect("empty Archive folder must still appear in the tree");
        assert!(folder.children().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_files_appear_under_their_folder_node() {
        let root = temporary_directory("workfolder-nested-node");
        fs::create_dir_all(root.join("dev")).unwrap();
        fs::write(root.join("dev/GPUI.md"), "# GPUI\n").unwrap();

        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let folder = work_folder
            .children()
            .iter()
            .find_map(|node| match node {
                WorkFolderNode::Folder(folder) if folder.name() == "dev" => Some(folder),
                _ => None,
            })
            .expect("dev folder must appear in the tree");
        assert_eq!(folder.children().len(), 1);
        assert!(matches!(
            &folder.children()[0],
            WorkFolderNode::File(entry) if entry.file_name() == "GPUI.md"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inserting_a_folder_creates_missing_intermediate_folders() {
        let mut folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        folder.insert_folder(PathBuf::from("/notes/dev/archive"));
        let dev = folder
            .children()
            .iter()
            .find_map(|node| match node {
                WorkFolderNode::Folder(folder) if folder.name() == "dev" => Some(folder),
                _ => None,
            })
            .expect("dev folder created");
        assert!(
            dev.children()
                .iter()
                .any(|node| matches!(node, WorkFolderNode::Folder(f) if f.name() == "archive"))
        );
    }
}
