//! Work folder model: the Markdown/folder index for a directory the user opens
//! as a "notebook", kept separate from the `DocumentSession`s actually loaded
//! into memory.
//!
//! Listing a work folder answers "which notes and folders exist and how do
//! they nest", nothing more. It never reads note contents and never creates a
//! `DocumentSession`; callers open a `WorkFolderEntry::path()` through
//! `FileService::load` only once a note is actually selected.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One Markdown file discovered under a work folder, not yet loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkFolderEntry {
    path: PathBuf,
    name: String,
    file_name: String,
    /// The file's last-modified time, retained from whichever of a scan, a
    /// save, or a rename most recently acquired it, rather than re-read from
    /// disk every time the sidebar sorts. `None` until something has
    /// acquired one (a synthetic entry built without touching the real
    /// filesystem, or a platform that does not report `modified`); such an
    /// entry sorts as the oldest under `WorkFolderSortOrder::Updated`.
    modified: Option<SystemTime>,
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
            modified: None,
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

    /// The retained last-modified time; see the field doc comment.
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
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

    /// The retained last-modified time of whichever node this is; see
    /// `WorkFolderEntry`'s field doc comment.
    pub fn modified(&self) -> Option<SystemTime> {
        match self {
            Self::File(entry) => entry.modified(),
            Self::Folder(folder) => folder.modified(),
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
    /// See `WorkFolderEntry::modified`'s field doc comment; a folder's own
    /// last-modified time (not any child's), the same filesystem entry a
    /// directory listing would report it for.
    modified: Option<SystemTime>,
}

impl WorkFolderFolder {
    fn new(path: PathBuf, children: Vec<WorkFolderNode>, modified: Option<SystemTime>) -> Self {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Self {
            path,
            name,
            children,
            modified,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The retained last-modified time; see the field doc comment.
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// This folder's direct children, folders and files mixed and sorted
    /// according to the work folder's current `WorkFolderSortOrder`.
    pub fn children(&self) -> &[WorkFolderNode] {
        &self.children
    }
}

/// How the sidebar orders a work folder's children, applied independently at
/// every level of the hierarchy (a folder's children never compare against
/// another folder's). Persisted as part of `Settings` (`sidebar_sort`), with
/// `Name` as the default so a settings file written before this existed, or
/// one simply missing the key, keeps the original alphabetical order
/// unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkFolderSortOrder {
    /// Alphabetical by display name (`WorkFolderNode::sort_key`), the
    /// original and only order before this setting existed.
    #[default]
    Name,
    /// Most recently modified first, using each node's retained
    /// `modified()`; a node with no retained time sorts as the oldest.
    Updated,
}

impl WorkFolderSortOrder {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Updated => "updated",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "updated" => Self::Updated,
            _ => Self::Name,
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Updated,
            Self::Updated => Self::Name,
        }
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
    sort_order: WorkFolderSortOrder,
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
            sort_order: WorkFolderSortOrder::default(),
        };
        for entry in entries {
            folder.insert(entry.path().to_path_buf());
        }
        folder
    }

    /// Builds a work folder from an already-assembled tree, e.g. the result
    /// of a real filesystem walk that also knows about empty folders. Always
    /// sorted by `Name` on construction; a caller applying a different
    /// persisted `WorkFolderSortOrder` does so afterward through
    /// `set_sort_order`, so a scan's own sort order does not need threading
    /// through `WorkFolderScanner`.
    pub(crate) fn from_tree(root: PathBuf, mut children: Vec<WorkFolderNode>) -> Self {
        let sort_order = WorkFolderSortOrder::default();
        sort_nodes(&mut children, sort_order);
        Self {
            root,
            children,
            sort_order,
        }
    }

    /// Re-sorts every level of the tree in place for a newly chosen
    /// `WorkFolderSortOrder`, and remembers it for subsequent `insert`,
    /// `insert_folder`, `rename`, and `rename_folder` calls. A no-op when
    /// `order` already matches, so switching the setting back and forth does
    /// not needlessly re-sort an unchanged tree.
    pub fn set_sort_order(&mut self, order: WorkFolderSortOrder) {
        if self.sort_order == order {
            return;
        }
        self.sort_order = order;
        resort_tree(&mut self.children, order);
    }

    pub fn sort_order(&self) -> WorkFolderSortOrder {
        self.sort_order
    }

    /// Updates the retained modified time for a node this app's own write
    /// just produced — a save, an autosave, or a newly created file or
    /// folder — without a rescan, and re-sorts the level it lives at so an
    /// `Updated` sort order reflects the change immediately. A `path` the
    /// tree was not scanned, inserted, or created with is a no-op.
    pub fn touch(&mut self, path: &Path, modified: Option<SystemTime>) {
        set_modified(&mut self.children, path, modified, self.sort_order);
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
        insert_file(
            &mut self.children,
            &self.root,
            &components,
            path,
            self.sort_order,
        );
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
        insert_folder_at(&mut self.children, &self.root, &components, self.sort_order);
    }

    /// Follows a rename this app itself just performed, keeping the index in
    /// sync instead of leaving a stale entry at a path that no longer exists.
    /// A `from` the folder was not scanned with (already renamed, or never
    /// present) is a no-op.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        let Some(entry) = remove_file(&mut self.children, from) else {
            return;
        };
        self.insert(to.to_path_buf());
        // `insert` always builds the fresh entry with no retained modified
        // time, since a plain rename (unlike a save) never changes a file's
        // content or its mtime; carry over what was already known instead
        // of letting it fall back to "oldest" under an `Updated` sort.
        set_modified(&mut self.children, to, entry.modified, self.sort_order);
    }

    /// Follows a folder rename this app itself just performed: `from` moves to
    /// `to`, and every descendant path (files and subfolders alike) moves with
    /// it, keeping the same structure relative to the new parent so the tree
    /// resorts under the folder's new name and position. A `from` the folder
    /// was not scanned with (already renamed, or never present) is a no-op.
    pub fn rename_folder(&mut self, from: &Path, to: &Path) {
        let Some(folder) = remove_folder(&mut self.children, from) else {
            return;
        };
        let renamed = rebase_folder(folder, from, to);
        let Some(parent) = to.parent() else {
            return;
        };
        if parent == self.root {
            self.children.push(WorkFolderNode::Folder(renamed));
            sort_nodes(&mut self.children, self.sort_order);
        } else if let Some(parent_folder) = find_folder_mut(&mut self.children, parent) {
            parent_folder.children.push(WorkFolderNode::Folder(renamed));
            sort_nodes(&mut parent_folder.children, self.sort_order);
        }
        // A `to` whose parent is not itself in the tree drops the folder
        // rather than reinserting it at the wrong place; a real rename never
        // moves a folder under a parent the tree does not know about.
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root's direct children, folders and files mixed and sorted
    /// according to the work folder's current `WorkFolderSortOrder`.
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

fn find_folder_mut<'a>(
    nodes: &'a mut [WorkFolderNode],
    path: &Path,
) -> Option<&'a mut WorkFolderFolder> {
    for node in nodes {
        let WorkFolderNode::Folder(folder) = node else {
            continue;
        };
        if folder.path() == path {
            return Some(folder);
        }
        if let Some(found) = find_folder_mut(&mut folder.children, path) {
            return Some(found);
        }
    }
    None
}

/// Removes and returns the folder node at `path`, searching the whole tree
/// (not just `nodes` itself), the folder-shaped counterpart to `remove_file`.
fn remove_folder(nodes: &mut Vec<WorkFolderNode>, path: &Path) -> Option<WorkFolderFolder> {
    if let Some(index) = nodes
        .iter()
        .position(|node| matches!(node, WorkFolderNode::Folder(folder) if folder.path() == path))
        && let WorkFolderNode::Folder(folder) = nodes.remove(index)
    {
        return Some(folder);
    }
    for node in nodes.iter_mut() {
        if let WorkFolderNode::Folder(folder) = node
            && let Some(found) = remove_folder(&mut folder.children, path)
        {
            return Some(found);
        }
    }
    None
}

/// Rebuilds `folder` and every node beneath it with paths moved from under
/// `from` to under `to`, preserving each node's position relative to the
/// folder root that moved, and each node's own retained modified time (a
/// rename changes neither a file's content nor a folder's own identity, so
/// neither should fall back to "oldest" under an `Updated` sort just because
/// it moved).
fn rebase_folder(folder: WorkFolderFolder, from: &Path, to: &Path) -> WorkFolderFolder {
    let path = rebase_path(folder.path(), from, to);
    let modified = folder.modified;
    let children = folder
        .children
        .into_iter()
        .map(|child| rebase_node(child, from, to))
        .collect();
    WorkFolderFolder::new(path, children, modified)
}

fn rebase_node(node: WorkFolderNode, from: &Path, to: &Path) -> WorkFolderNode {
    match node {
        WorkFolderNode::File(entry) => {
            let mut rebased = WorkFolderEntry::new(rebase_path(entry.path(), from, to));
            rebased.modified = entry.modified;
            WorkFolderNode::File(rebased)
        }
        WorkFolderNode::Folder(folder) => WorkFolderNode::Folder(rebase_folder(folder, from, to)),
    }
}

/// `path` with its `from` prefix replaced by `to`, or `path` unchanged when it
/// is not under `from`.
fn rebase_path(path: &Path, from: &Path, to: &Path) -> PathBuf {
    let Ok(relative) = path.strip_prefix(from) else {
        return path.to_path_buf();
    };
    if relative.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(relative)
    }
}

fn sort_nodes(nodes: &mut [WorkFolderNode], order: WorkFolderSortOrder) {
    nodes.sort_by(|a, b| compare_nodes(a, b, order));
}

/// The ordering for two siblings at one hierarchy level: `Name` compares the
/// display name and falls back to the path so two nodes never tie; `Updated`
/// sorts the more recently modified node first and falls back to the exact
/// same name/path rule whenever both retained modified times tie (including
/// two unknown times). Applying this one rule at every level, independently
/// per folder, is the "same-rank rule per hierarchy level" this sort order
/// needs; the filter view inherits it for free since it only ever reads
/// already-sorted `children()`, never re-sorting its own matches.
fn compare_nodes(a: &WorkFolderNode, b: &WorkFolderNode, order: WorkFolderSortOrder) -> Ordering {
    let name_then_path = |a: &WorkFolderNode, b: &WorkFolderNode| {
        a.sort_key().cmp(b.sort_key()).then_with(|| a.path().cmp(b.path()))
    };
    match order {
        WorkFolderSortOrder::Name => name_then_path(a, b),
        WorkFolderSortOrder::Updated => b
            .modified()
            .cmp(&a.modified())
            .then_with(|| name_then_path(a, b)),
    }
}

/// Re-sorts `nodes` and, recursively, every folder's own children beneath
/// it, for a `WorkFolderSortOrder` switch applied to an already-built tree.
fn resort_tree(nodes: &mut [WorkFolderNode], order: WorkFolderSortOrder) {
    sort_nodes(nodes, order);
    for node in nodes {
        if let WorkFolderNode::Folder(folder) = node {
            resort_tree(&mut folder.children, order);
        }
    }
}

/// Sets the retained modified time of the node at `path`, wherever in the
/// tree it lives, and re-sorts the level it lives at. Returns whether a node
/// was found; a `path` the tree was never scanned, inserted, or created with
/// is a no-op (`WorkFolder::touch` and `WorkFolder::rename` both rely on
/// this, the latter after the node has already been reinserted at its new
/// path).
fn set_modified(
    nodes: &mut Vec<WorkFolderNode>,
    path: &Path,
    modified: Option<SystemTime>,
    order: WorkFolderSortOrder,
) -> bool {
    if let Some(index) = nodes.iter().position(|node| node.path() == path) {
        match &mut nodes[index] {
            WorkFolderNode::File(entry) => entry.modified = modified,
            WorkFolderNode::Folder(folder) => folder.modified = modified,
        }
        sort_nodes(nodes, order);
        return true;
    }
    for node in nodes.iter_mut() {
        if let WorkFolderNode::Folder(folder) = node
            && set_modified(&mut folder.children, path, modified, order)
        {
            return true;
        }
    }
    false
}

fn insert_file(
    nodes: &mut Vec<WorkFolderNode>,
    current_dir: &Path,
    components: &[&OsStr],
    file_path: PathBuf,
    order: WorkFolderSortOrder,
) {
    if components.len() == 1 {
        nodes.push(WorkFolderNode::File(WorkFolderEntry::new(file_path)));
        sort_nodes(nodes, order);
        return;
    }
    let index = folder_index(nodes, current_dir, components[0], order);
    if let WorkFolderNode::Folder(folder) = &mut nodes[index] {
        let folder_path = folder.path().to_path_buf();
        insert_file(
            &mut folder.children,
            &folder_path,
            &components[1..],
            file_path,
            order,
        );
    }
}

fn insert_folder_at(
    nodes: &mut Vec<WorkFolderNode>,
    current_dir: &Path,
    components: &[&OsStr],
    order: WorkFolderSortOrder,
) {
    let index = folder_index(nodes, current_dir, components[0], order);
    if components.len() == 1 {
        return;
    }
    if let WorkFolderNode::Folder(folder) = &mut nodes[index] {
        let folder_path = folder.path().to_path_buf();
        insert_folder_at(&mut folder.children, &folder_path, &components[1..], order);
    }
}

/// The index in `nodes` of the folder named `name` directly under
/// `current_dir`, creating it (with no children yet) if it is not already
/// there.
fn folder_index(
    nodes: &mut Vec<WorkFolderNode>,
    current_dir: &Path,
    name: &OsStr,
    order: WorkFolderSortOrder,
) -> usize {
    let folder_path = current_dir.join(name);
    if let Some(index) = nodes.iter().position(
        |node| matches!(node, WorkFolderNode::Folder(folder) if folder.path() == folder_path),
    ) {
        return index;
    }
    nodes.push(WorkFolderNode::Folder(WorkFolderFolder::new(
        folder_path.clone(),
        Vec::new(),
        None,
    )));
    sort_nodes(nodes, order);
    nodes
        .iter()
        .position(
            |node| matches!(node, WorkFolderNode::Folder(folder) if folder.path() == folder_path),
        )
        .expect("folder just inserted")
}

/// Removes and returns the file node at `path`, searching the whole tree,
/// the file-shaped counterpart to `remove_folder`. Returning the removed
/// entry (rather than just whether one was found) is what lets `rename`
/// carry its retained modified time over to the reinserted node.
fn remove_file(nodes: &mut Vec<WorkFolderNode>, path: &Path) -> Option<WorkFolderEntry> {
    if let Some(index) = nodes
        .iter()
        .position(|node| matches!(node, WorkFolderNode::File(entry) if entry.path() == path))
        && let WorkFolderNode::File(entry) = nodes.remove(index)
    {
        return Some(entry);
    }
    for node in nodes.iter_mut() {
        if let WorkFolderNode::Folder(folder) = node
            && let Some(entry) = remove_file(&mut folder.children, path)
        {
            return Some(entry);
        }
    }
    None
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
        // Acquired here, once per scan, and retained on the node from then
        // on (see `WorkFolderEntry::modified`'s doc comment) rather than
        // re-read from disk every time the sidebar sorts or a persisted
        // `WorkFolderSortOrder` is switched to `Updated`. A transient
        // failure (permission, or an entry removed mid-walk) degrades to
        // `None` instead of failing the whole scan, since this is purely an
        // enrichment of the listing `file_type` above already succeeded at.
        let modified = item
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok());
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
                path, children, modified,
            )));
        } else if file_type.is_file() && is_markdown(&path) {
            let mut entry = WorkFolderEntry::new(path);
            entry.modified = modified;
            nodes.push(WorkFolderNode::File(entry));
        }
    }
    // Always `Name`: a scan builds a fresh tree from scratch, and
    // `OsWorkFolderScanner::scan` hands it to `WorkFolder::from_tree`, which
    // sorts it the same way; a caller wanting a different persisted
    // `WorkFolderSortOrder` applies it afterward through `set_sort_order`.
    sort_nodes(&mut nodes, WorkFolderSortOrder::Name);
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
    fn renaming_a_folder_moves_its_descendant_paths_and_resorts_the_tree() {
        let mut folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        folder.insert_folder(PathBuf::from("/notes/dev/archive"));
        folder.insert(PathBuf::from("/notes/dev/GPUI.md"));
        folder.insert(PathBuf::from("/notes/dev/archive/Old.md"));
        folder.insert(PathBuf::from("/notes/Zeta.md"));

        folder.rename_folder(Path::new("/notes/dev"), Path::new("/notes/Projects"));

        assert!(
            folder
                .entry_for_path(Path::new("/notes/dev/GPUI.md"))
                .is_none()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/Projects/GPUI.md"))
                .is_some()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/Projects/archive/Old.md"))
                .is_some()
        );
        let names: Vec<String> = folder
            .children()
            .iter()
            .map(|node| match node {
                WorkFolderNode::File(entry) => entry.name().to_owned(),
                WorkFolderNode::Folder(folder) => folder.name().to_owned(),
            })
            .collect();
        assert_eq!(names, ["Projects", "Zeta"]);
    }

    #[test]
    fn renaming_a_folder_the_tree_was_not_scanned_with_is_a_no_op() {
        let mut folder = WorkFolder::new(
            PathBuf::from("/notes"),
            vec![WorkFolderEntry::new(PathBuf::from("/notes/Alpha.md"))],
        );
        folder.rename_folder(Path::new("/notes/missing"), Path::new("/notes/renamed"));
        assert_eq!(folder.len(), 1);
        assert!(
            folder
                .entry_for_path(Path::new("/notes/Alpha.md"))
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
    fn scanner_keeps_gitignored_and_hidden_notes_but_skips_root_state() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;

        let root = temporary_directory("workfolder-search-targets");
        #[cfg(unix)]
        let external = root.with_extension("external");
        fs::create_dir_all(root.join(".notes")).unwrap();
        fs::create_dir_all(root.join(".hane/drafts")).unwrap();
        fs::create_dir_all(root.join("nested/.hane")).unwrap();
        #[cfg(unix)]
        fs::create_dir_all(&external).unwrap();
        fs::write(root.join(".gitignore"), "Ignored.md\n").unwrap();
        fs::write(root.join("Ignored.md"), "gitignored but indexed\n").unwrap();
        fs::write(root.join(".notes/private.MD"), "hidden note\n").unwrap();
        fs::write(root.join(".hane/drafts/draft.md"), "Hane state\n").unwrap();
        fs::write(
            root.join("nested/.hane/user.md"),
            "nested Hane-looking notes\n",
        )
        .unwrap();
        #[cfg(unix)]
        fs::write(external.join("Outside.md"), "outside target\n").unwrap();
        #[cfg(unix)]
        symlink(&external, root.join("linked")).unwrap();

        let folder = OsWorkFolderScanner.scan(&root).unwrap();
        let paths: Vec<_> = folder
            .entries()
            .into_iter()
            .map(|entry| entry.path().to_path_buf())
            .collect();

        assert_eq!(
            paths,
            [
                root.join(".notes/private.MD"),
                root.join("Ignored.md"),
                root.join("nested/.hane/user.md"),
            ]
        );
        fs::remove_dir_all(root).unwrap();
        #[cfg(unix)]
        fs::remove_dir_all(external).unwrap();
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

    fn system_time_at(seconds: u64) -> SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
    }

    fn file_node(path: &str, modified: Option<SystemTime>) -> WorkFolderNode {
        let mut entry = WorkFolderEntry::new(PathBuf::from(path));
        entry.modified = modified;
        WorkFolderNode::File(entry)
    }

    #[test]
    fn work_folder_sort_order_defaults_to_name_and_round_trips_through_as_str() {
        assert_eq!(WorkFolderSortOrder::default(), WorkFolderSortOrder::Name);
        assert_eq!(
            WorkFolderSortOrder::parse("updated"),
            WorkFolderSortOrder::Updated
        );
        assert_eq!(
            WorkFolderSortOrder::parse("name"),
            WorkFolderSortOrder::Name
        );
        assert_eq!(
            WorkFolderSortOrder::parse("unrecognized"),
            WorkFolderSortOrder::Name,
            "an unrecognized or missing value must fall back to the pre-existing order"
        );
        assert_eq!(WorkFolderSortOrder::Name.as_str(), "name");
        assert_eq!(WorkFolderSortOrder::Updated.as_str(), "updated");
        assert_eq!(WorkFolderSortOrder::Name.next(), WorkFolderSortOrder::Updated);
        assert_eq!(WorkFolderSortOrder::Updated.next(), WorkFolderSortOrder::Name);
    }

    #[test]
    fn updated_sort_order_lists_the_most_recently_modified_node_first_with_a_name_tie_break() {
        let children = vec![
            file_node("/notes/Older.md", Some(system_time_at(100))),
            file_node("/notes/Newer.md", Some(system_time_at(300))),
            file_node("/notes/SameTime_B.md", Some(system_time_at(200))),
            file_node("/notes/SameTime_A.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::Updated);

        let names: Vec<String> = folder
            .children()
            .iter()
            .map(|node| match node {
                WorkFolderNode::File(entry) => entry.file_name().to_owned(),
                WorkFolderNode::Folder(_) => unreachable!("only files were inserted"),
            })
            .collect();
        assert_eq!(
            names,
            ["Newer.md", "SameTime_A.md", "SameTime_B.md", "Older.md"],
            "newest first, with same-time siblings broken by name ascending"
        );
    }

    #[test]
    fn an_unknown_modified_time_sorts_last_under_updated_order() {
        let children = vec![
            file_node("/notes/Known.md", Some(system_time_at(100))),
            file_node("/notes/Unknown.md", None),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::Updated);

        let names: Vec<&str> = folder
            .children()
            .iter()
            .map(|node| node.path().file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["Known.md", "Unknown.md"]);
    }

    #[test]
    fn set_sort_order_resorts_every_level_of_the_tree() {
        let nested = vec![
            file_node("/notes/dev/Older.md", Some(system_time_at(10))),
            file_node("/notes/dev/Newer.md", Some(system_time_at(20))),
        ];
        let children = vec![WorkFolderNode::Folder(WorkFolderFolder::new(
            PathBuf::from("/notes/dev"),
            nested,
            None,
        ))];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::Updated);

        let WorkFolderNode::Folder(dev) = &folder.children()[0] else {
            panic!("dev folder expected");
        };
        let names: Vec<&str> = dev
            .children()
            .iter()
            .map(|node| match node {
                WorkFolderNode::File(entry) => entry.file_name(),
                WorkFolderNode::Folder(_) => unreachable!("only files were nested under dev"),
            })
            .collect();
        assert_eq!(
            names,
            ["Newer.md", "Older.md"],
            "a sort-order switch must resort a nested folder's own children too, not just the root"
        );
    }

    #[test]
    fn touching_an_entry_moves_it_without_a_rescan_under_updated_order() {
        let children = vec![
            file_node("/notes/A.md", Some(system_time_at(100))),
            file_node("/notes/B.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::Updated);
        assert_eq!(folder.children()[0].path(), Path::new("/notes/B.md"));

        // Simulates the modified time a save or autosave just produced,
        // without touching the filesystem from this test.
        folder.touch(Path::new("/notes/A.md"), Some(system_time_at(300)));

        assert_eq!(folder.children()[0].path(), Path::new("/notes/A.md"));
    }

    #[test]
    fn renaming_a_file_preserves_its_retained_modified_time_for_updated_order() {
        let children = vec![
            file_node("/notes/Alpha.md", Some(system_time_at(100))),
            file_node("/notes/Zeta.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::Updated);

        folder.rename(Path::new("/notes/Alpha.md"), Path::new("/notes/Omega.md"));

        let omega = folder
            .children()
            .iter()
            .find_map(|node| match node {
                WorkFolderNode::File(entry) if entry.file_name() == "Omega.md" => Some(entry),
                _ => None,
            })
            .expect("renamed entry still indexed");
        assert_eq!(
            omega.modified(),
            Some(system_time_at(100)),
            "a rename must not reset the retained modified time to unknown"
        );
        assert_eq!(
            folder.children()[0].path(),
            Path::new("/notes/Zeta.md"),
            "Zeta.md (modified at 200) must still sort before the renamed Omega.md (100)"
        );
    }

    #[test]
    fn renaming_a_folder_preserves_descendant_modified_times_for_updated_order() {
        let nested = vec![file_node("/notes/dev/Child.md", Some(system_time_at(50)))];
        let children = vec![WorkFolderNode::Folder(WorkFolderFolder::new(
            PathBuf::from("/notes/dev"),
            nested,
            Some(system_time_at(60)),
        ))];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::Updated);

        folder.rename_folder(Path::new("/notes/dev"), Path::new("/notes/Projects"));

        let WorkFolderNode::Folder(renamed) = &folder.children()[0] else {
            panic!("renamed folder expected");
        };
        assert_eq!(renamed.modified(), Some(system_time_at(60)));
        let child = renamed
            .children()
            .first()
            .expect("child moved with the renamed folder");
        assert_eq!(child.modified(), Some(system_time_at(50)));
    }

    #[test]
    fn the_real_filesystem_scanner_retains_each_entrys_modified_time() {
        let root = temporary_directory("workfolder-modified-time");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();

        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
        let entry = work_folder
            .entries()
            .into_iter()
            .next()
            .expect("the written note is scanned");
        assert!(
            entry.modified().is_some(),
            "a real scan must acquire a modified time for later Updated sorting"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
