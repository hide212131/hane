//! Work folder model: the Markdown/folder index for a directory the user opens
//! as a "notebook", kept separate from the `DocumentSession`s actually loaded
//! into memory.
//!
//! Listing a work folder answers "which notes and folders exist and how do
//! they nest", nothing more. It never reads note contents and never creates a
//! `DocumentSession`; callers open a `WorkFolderEntry::path()` through
//! `FileService::load` only once a note is actually selected.

use crate::service::FileService;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::SystemTime;

/// Process-wide monotonic counter stamped onto a node's modified time at the
/// moment that time is actually read off the filesystem — not when the read
/// later reaches the UI thread through a background task's completion.
/// `WorkFolder::touch` and `WorkFolder::rename` compare these sequence
/// numbers, never the modified time values themselves, to tell a read that
/// lands on the UI thread out of order with another read of the same path
/// apart from one that is genuinely newer; see `set_modified` for why the
/// time value alone cannot do this safely.
static NEXT_MODIFIED_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Allocates the next value from `NEXT_MODIFIED_SEQUENCE`. Callers acquire
/// this at the same point they read a node's real modified time, pairing the
/// two before either crosses back onto the UI thread through `WorkFolder::touch`.
pub fn next_modified_sequence() -> u64 {
    NEXT_MODIFIED_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed)
}

/// One lock per path ever stamped through `modified_with_sequence`, kept for
/// the life of the process. Entries are never removed: the set of distinct
/// paths a running process stamps is bounded by the folders and files the
/// user's work folder actually has, the same bound `WorkFolder`'s own tree
/// already keeps.
static PATH_STAMP_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn path_stamp_lock(path: &Path) -> Arc<Mutex<()>> {
    let mut locks = PATH_STAMP_LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    locks
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Reads a path's current modified time and allocates its paired sequence
/// under the same path-specific lock used by saves and renames. `read_modified`
/// preserves the caller's metadata source (for example, `DirEntry::metadata`
/// in the scanner) while keeping the read and sequence allocation indivisible
/// with respect to other observations of the same path.
///
/// Pairing the read and the sequence this way — not merely reading, then
/// allocating a sequence afterward — is what keeps two calls for the *same*
/// path in real read order: without the lock, one call's read can land first
/// while a second, independent call for the same path (most commonly two
/// sibling saves whose `ancestor_folder_paths` both name the same parent
/// folder) reads afterward yet reaches `fetch_add` first, handing the
/// genuinely later read the *smaller* sequence number. `WorkFolder::touch`
/// and `rename` only have the sequence to tell the two apart (see
/// `set_modified`), so once that numbering itself is wrong, no comparison on
/// the receiving side can recover the real order.
fn modified_with_sequence(
    path: &Path,
    read_modified: impl FnOnce() -> Option<SystemTime>,
) -> (Option<SystemTime>, u64) {
    let lock = path_stamp_lock(path);
    let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let modified = read_modified();
    let sequence = next_modified_sequence();
    (modified, sequence)
}

/// Reads `path`'s current modified time through `FileService::stamp` and
/// allocates its paired sequence as one step, held behind the path-specific
/// lock shared with scanner reads.
pub fn stamp_with_sequence(files: &dyn FileService, path: &Path) -> (Option<SystemTime>, u64) {
    modified_with_sequence(path, || files.stamp(path).and_then(|stamp| stamp.modified))
}

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
    /// entry sorts last under `WorkFolderSortOrder::UpdatedNewest` or
    /// `WorkFolderSortOrder::UpdatedOldest`.
    modified: Option<SystemTime>,
    /// The `next_modified_sequence` value paired with `modified` when it was
    /// last set through `set_modified`, or `None` if `modified` has never
    /// been set that way (built fresh, or only ever carried over by
    /// `WorkFolder::rename` from an entry that itself had none). See
    /// `set_modified` for how this, not `modified` itself, decides whether a
    /// later call may overwrite what is retained here.
    modified_seq: Option<u64>,
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
            modified_seq: None,
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
    /// See `WorkFolderEntry::modified_seq`'s field doc comment.
    modified_seq: Option<u64>,
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
            modified_seq: None,
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
/// `UpdatedNewest` as the default for a new settings file, one missing the
/// key, or one holding a value this version does not recognize.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkFolderSortOrder {
    /// Most recently modified first, using each node's retained
    /// `modified()`; a node with no retained time sorts last regardless of
    /// direction.
    #[default]
    UpdatedNewest,
    /// Least recently modified first; a node with no retained time still
    /// sorts last, the same as under `UpdatedNewest`.
    UpdatedOldest,
    /// Alphabetical by display name (`WorkFolderNode::sort_key`), ascending,
    /// falling back to the path (also ascending) to break a tie.
    NameAscending,
    /// Alphabetical by display name, descending, falling back to the path
    /// (also descending) to break a tie.
    NameDescending,
}

impl WorkFolderSortOrder {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpdatedNewest => "updated_newest",
            Self::UpdatedOldest => "updated_oldest",
            Self::NameAscending => "name_ascending",
            Self::NameDescending => "name_descending",
        }
    }

    /// Parses a persisted value, falling back to the default
    /// (`UpdatedNewest`) for anything this version does not recognize —
    /// including a settings file written before this setting existed, one
    /// simply missing the key, or a value written by a newer version.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "updated_oldest" => Self::UpdatedOldest,
            "name_ascending" => Self::NameAscending,
            "name_descending" => Self::NameDescending,
            _ => Self::UpdatedNewest,
        }
    }

    /// The label shown next to this order wherever the user picks among all
    /// four: the sidebar toolbar's "並べ替え" menu and the settings screen.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::UpdatedNewest => "更新日時（新しい順）",
            Self::UpdatedOldest => "更新日時（古い順）",
            Self::NameAscending => "名前（昇順）",
            Self::NameDescending => "名前（降順）",
        }
    }

    /// Every order, in the fixed display order used by both the sidebar
    /// toolbar's menu and the settings screen.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [
            Self::UpdatedNewest,
            Self::UpdatedOldest,
            Self::NameAscending,
            Self::NameDescending,
        ]
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
    /// of a real filesystem walk that also knows about empty folders. Its own
    /// (top) level is always sorted by `WorkFolderSortOrder::default()` on
    /// construction, matching the order `WorkFolderScanner::scan`'s walk
    /// already sorted every nested level by, so the whole tree starts out
    /// consistent without a recursive re-sort here; a caller applying a
    /// different persisted `WorkFolderSortOrder` does so afterward through
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
    ///
    /// `sequence` must be the `next_modified_sequence` value the caller
    /// acquired alongside `modified` itself, off the UI thread, at the point
    /// it actually read the filesystem; see `set_modified` for why this,
    /// not `modified`, is what decides whether this call may overwrite what
    /// is already retained.
    pub fn touch(&mut self, path: &Path, modified: Option<SystemTime>, sequence: u64) {
        set_modified(&mut self.children, path, modified, Some(sequence), self.sort_order);
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
        // of letting it fall back to "oldest" under an `Updated` sort. The
        // sequence travels with it unchanged too, rather than minting a new
        // one here: nothing was actually re-read from disk, so the fresh
        // entry `insert` just built (itself not yet sequenced) only needs
        // whatever freshness `entry` already carried.
        set_modified(&mut self.children, to, entry.modified, entry.modified_seq, self.sort_order);
    }

    /// Follows a file this app itself just moved to the Trash, dropping its
    /// entry from the index. A `path` the folder was not scanned with is a
    /// no-op.
    pub fn remove(&mut self, path: &Path) {
        let _ = remove_file(&mut self.children, path);
    }

    /// Follows a folder this app itself just moved to the Trash, dropping its
    /// node and every descendant from the index. The root is never removed, and
    /// a `path` the folder was not scanned with is a no-op.
    pub fn remove_folder(&mut self, path: &Path) {
        if path == self.root {
            return;
        }
        let _ = remove_folder(&mut self.children, path);
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

    /// `path`'s own containing folder, if any, as a single-element list (or
    /// empty when `path` sits directly under this work folder's root, which
    /// is never a tracked `WorkFolderNode`): the one folder whose own real
    /// modified time changes alongside `path`'s whenever a save, create, or
    /// rename actually writes to the filesystem. An atomic save replaces the
    /// target via a temp-file rename, and a plain rename changes a directory
    /// entry outright; either way only the directory that entry lives in is
    /// touched on disk — a grandparent's own entry (naming that directory
    /// itself) is untouched, so it is deliberately left out rather than
    /// walked up as a chain. Pure path arithmetic — it does not touch the
    /// filesystem or search the tree, so callers acquire the parent's real
    /// modified time off the UI thread (the same `FileService::stamp`
    /// boundary a save already uses) before feeding it back through
    /// `touch`.
    pub fn ancestor_folder_paths(&self, path: &Path) -> Vec<PathBuf> {
        match path.parent() {
            Some(parent) if parent != self.root => vec![parent.to_path_buf()],
            _ => Vec::new(),
        }
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
    let modified_seq = folder.modified_seq;
    let children = folder
        .children
        .into_iter()
        .map(|child| rebase_node(child, from, to))
        .collect();
    let mut rebased = WorkFolderFolder::new(path, children, modified);
    rebased.modified_seq = modified_seq;
    rebased
}

fn rebase_node(node: WorkFolderNode, from: &Path, to: &Path) -> WorkFolderNode {
    match node {
        WorkFolderNode::File(entry) => {
            let mut rebased = WorkFolderEntry::new(rebase_path(entry.path(), from, to));
            rebased.modified = entry.modified;
            rebased.modified_seq = entry.modified_seq;
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

/// The ordering for two siblings at one hierarchy level: `NameAscending`/
/// `NameDescending` compare the display name in the chosen direction and
/// fall back to the path in that same direction so two nodes never tie;
/// `UpdatedNewest`/`UpdatedOldest` sort by retained modified time in the
/// chosen direction (an unknown time always sorting last, regardless of
/// direction) and fall back to the name-ascending/path-ascending rule
/// whenever both retained modified times tie, including two unknown times.
/// Applying this one rule at every level, independently per folder, is the
/// "same-rank rule per hierarchy level" every sort order needs; the filter
/// view inherits it for free since it only ever reads already-sorted
/// `children()`, never re-sorting its own matches.
fn compare_nodes(a: &WorkFolderNode, b: &WorkFolderNode, order: WorkFolderSortOrder) -> Ordering {
    let name_ascending_then_path = |a: &WorkFolderNode, b: &WorkFolderNode| {
        a.sort_key().cmp(b.sort_key()).then_with(|| a.path().cmp(b.path()))
    };
    match order {
        WorkFolderSortOrder::NameAscending => name_ascending_then_path(a, b),
        WorkFolderSortOrder::NameDescending => name_ascending_then_path(b, a),
        WorkFolderSortOrder::UpdatedNewest => {
            compare_modified(a, b, true).then_with(|| name_ascending_then_path(a, b))
        }
        WorkFolderSortOrder::UpdatedOldest => {
            compare_modified(a, b, false).then_with(|| name_ascending_then_path(a, b))
        }
    }
}

/// Compares two nodes' retained modified times for `UpdatedNewest`
/// (`newest_first`) or `UpdatedOldest`: whichever of the two has no retained
/// time always sorts last, independent of `newest_first`; between two known
/// times, the more recent one sorts first under `newest_first` and last
/// otherwise. Two unknown times, or two equal known times, compare equal, so
/// the caller's name/path tie-break decides.
fn compare_modified(a: &WorkFolderNode, b: &WorkFolderNode, newest_first: bool) -> Ordering {
    match (a.modified(), b.modified()) {
        (Some(a_time), Some(b_time)) => {
            if newest_first {
                b_time.cmp(&a_time)
            } else {
                a_time.cmp(&b_time)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
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
///
/// An out-of-order call is dropped instead of applied, decided by `sequence`
/// — a `next_modified_sequence` value, not `modified` itself. `touch`'s
/// callers acquire `modified` off the UI thread and apply it once their
/// background work completes, so two completions for the same node — most
/// commonly two sibling saves whose `ancestor_folder_paths` both name the
/// same parent folder — can reach the UI thread in an order that does not
/// match the order their reads actually happened in. Comparing `modified`
/// values themselves cannot tell these apart: a freshly re-read real
/// directory entry can legitimately report an *older* time than what is
/// retained (a folder whose mtime was briefly set into the future, say, by
/// another process, catching back up on its next real change), which a
/// time-based guard would wrongly reject forever, and a stale `None` can
/// otherwise always slip through and erase an already-applied newer `Some`,
/// since a value-based guard cannot compare against "nothing" at all.
/// `sequence`, captured synchronously at the point of the read itself rather
/// than at the point its result reaches the UI thread, is monotonic in real
/// read order regardless of what either read actually observed, including
/// a `None` (acquisition failed, or the node never had one) — which still
/// applies whenever its `sequence` is newer, the same degrade-on-failure
/// contract `stamp_ancestor_folders` already documents. A node with no
/// retained `sequence` yet (built fresh, or only ever carried over by
/// `WorkFolder::rename`) accepts any call unconditionally, the same as
/// before this guard existed.
fn set_modified(
    nodes: &mut [WorkFolderNode],
    path: &Path,
    modified: Option<SystemTime>,
    sequence: Option<u64>,
    order: WorkFolderSortOrder,
) -> bool {
    if let Some(index) = nodes.iter().position(|node| node.path() == path) {
        let retained_seq = match &nodes[index] {
            WorkFolderNode::File(entry) => entry.modified_seq,
            WorkFolderNode::Folder(folder) => folder.modified_seq,
        };
        if let (Some(retained_seq), Some(incoming_seq)) = (retained_seq, sequence)
            && incoming_seq <= retained_seq
        {
            return true;
        }
        match &mut nodes[index] {
            WorkFolderNode::File(entry) => {
                entry.modified = modified;
                entry.modified_seq = sequence;
            }
            WorkFolderNode::Folder(folder) => {
                folder.modified = modified;
                folder.modified_seq = sequence;
            }
        }
        sort_nodes(nodes, order);
        return true;
    }
    for node in nodes.iter_mut() {
        if let WorkFolderNode::Folder(folder) = node
            && set_modified(&mut folder.children, path, modified, sequence, order)
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
        let (modified, sequence) = modified_with_sequence(&path, || {
            item.metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
        });
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
            let mut folder = WorkFolderFolder::new(path, children, modified);
            folder.modified_seq = Some(sequence);
            nodes.push(WorkFolderNode::Folder(folder));
        } else if file_type.is_file() && is_markdown(&path) {
            let mut entry = WorkFolderEntry::new(path);
            entry.modified = modified;
            entry.modified_seq = Some(sequence);
            nodes.push(WorkFolderNode::File(entry));
        }
    }
    // Always the default order: a scan builds a fresh tree from scratch, and
    // `OsWorkFolderScanner::scan` hands it to `WorkFolder::from_tree`, which
    // sorts its own (top) level the same way, so every level starts out
    // consistent with `WorkFolder::sort_order()`'s initial value without a
    // recursive re-sort; a caller wanting a different persisted
    // `WorkFolderSortOrder` applies it afterward through `set_sort_order`,
    // which does resort every level.
    sort_nodes(&mut nodes, WorkFolderSortOrder::default());
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
    fn removing_a_folder_drops_it_with_descendants_and_keeps_siblings_and_root() {
        let mut folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        folder.insert_folder(PathBuf::from("/notes/dev/archive"));
        folder.insert(PathBuf::from("/notes/dev/GPUI.md"));
        folder.insert(PathBuf::from("/notes/dev/archive/Old.md"));
        folder.insert_folder(PathBuf::from("/notes/devtools"));
        folder.insert(PathBuf::from("/notes/devtools/Keep.md"));
        folder.insert(PathBuf::from("/notes/Zeta.md"));

        folder.remove_folder(Path::new("/notes/dev"));

        assert!(folder.children_at(Path::new("/notes/dev")).is_none());
        assert!(
            folder
                .children_at(Path::new("/notes/dev/archive"))
                .is_none()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/dev/GPUI.md"))
                .is_none()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/dev/archive/Old.md"))
                .is_none()
        );
        assert!(folder.children_at(Path::new("/notes/devtools")).is_some());
        assert!(
            folder
                .entry_for_path(Path::new("/notes/devtools/Keep.md"))
                .is_some()
        );
        assert!(folder.entry_for_path(Path::new("/notes/Zeta.md")).is_some());
        assert_eq!(folder.root(), Path::new("/notes"));
    }

    #[test]
    fn removing_a_nested_folder_keeps_its_parent_and_the_parents_files() {
        let mut folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        folder.insert_folder(PathBuf::from("/notes/dev/archive"));
        folder.insert(PathBuf::from("/notes/dev/GPUI.md"));
        folder.insert(PathBuf::from("/notes/dev/archive/Old.md"));

        folder.remove_folder(Path::new("/notes/dev/archive"));

        assert!(folder.children_at(Path::new("/notes/dev")).is_some());
        assert!(
            folder
                .children_at(Path::new("/notes/dev/archive"))
                .is_none()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/dev/GPUI.md"))
                .is_some()
        );
        assert!(
            folder
                .entry_for_path(Path::new("/notes/dev/archive/Old.md"))
                .is_none()
        );
    }

    #[test]
    fn removing_the_root_or_an_unknown_folder_changes_nothing() {
        let mut folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        folder.insert_folder(PathBuf::from("/notes/dev"));
        folder.insert(PathBuf::from("/notes/dev/GPUI.md"));

        folder.remove_folder(Path::new("/notes"));
        folder.remove_folder(Path::new("/notes/missing"));

        assert!(folder.children_at(Path::new("/notes/dev")).is_some());
        assert!(
            folder
                .entry_for_path(Path::new("/notes/dev/GPUI.md"))
                .is_some()
        );
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

        let mut names: Vec<String> = work_folder
            .entries()
            .into_iter()
            .map(|entry| entry.name().to_owned())
            .collect();
        names.sort_unstable();
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
        let mut paths: Vec<_> = folder
            .entries()
            .into_iter()
            .map(|entry| entry.path().to_path_buf())
            .collect();
        paths.sort_unstable();

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
    fn work_folder_sort_order_defaults_to_updated_newest_and_round_trips_through_as_str() {
        assert_eq!(
            WorkFolderSortOrder::default(),
            WorkFolderSortOrder::UpdatedNewest
        );
        for order in WorkFolderSortOrder::all() {
            assert_eq!(WorkFolderSortOrder::parse(order.as_str()), order);
        }
        assert_eq!(
            WorkFolderSortOrder::parse("unrecognized"),
            WorkFolderSortOrder::UpdatedNewest,
            "an unrecognized or missing value must fall back to the default order"
        );
        // Pre-existing values from before the four-way split (`"name"`,
        // `"updated"`) are also unrecognized now, and must fall back the
        // same way rather than erroring.
        assert_eq!(
            WorkFolderSortOrder::parse("name"),
            WorkFolderSortOrder::UpdatedNewest
        );
        assert_eq!(
            WorkFolderSortOrder::parse("updated"),
            WorkFolderSortOrder::UpdatedNewest
        );
    }

    #[test]
    fn updated_newest_lists_the_most_recently_modified_node_first_with_a_name_tie_break() {
        let children = vec![
            file_node("/notes/Older.md", Some(system_time_at(100))),
            file_node("/notes/Newer.md", Some(system_time_at(300))),
            file_node("/notes/SameTime_B.md", Some(system_time_at(200))),
            file_node("/notes/SameTime_A.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::UpdatedNewest);

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
    fn updated_oldest_lists_the_least_recently_modified_node_first_with_a_name_tie_break() {
        let children = vec![
            file_node("/notes/Older.md", Some(system_time_at(100))),
            file_node("/notes/Newer.md", Some(system_time_at(300))),
            file_node("/notes/SameTime_B.md", Some(system_time_at(200))),
            file_node("/notes/SameTime_A.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::UpdatedOldest);

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
            ["Older.md", "SameTime_A.md", "SameTime_B.md", "Newer.md"],
            "oldest first, with same-time siblings still broken by name ascending"
        );
    }

    #[test]
    fn an_unknown_modified_time_sorts_last_under_either_updated_direction() {
        let children = vec![
            file_node("/notes/Known.md", Some(system_time_at(100))),
            file_node("/notes/Unknown.md", None),
        ];
        for order in [
            WorkFolderSortOrder::UpdatedNewest,
            WorkFolderSortOrder::UpdatedOldest,
        ] {
            let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children.clone());
            folder.set_sort_order(order);

            let names: Vec<&str> = folder
                .children()
                .iter()
                .map(|node| node.path().file_name().unwrap().to_str().unwrap())
                .collect();
            assert_eq!(names, ["Known.md", "Unknown.md"], "order {order:?}");
        }
    }

    #[test]
    fn name_descending_sorts_both_name_and_path_in_the_same_direction() {
        let children = vec![
            file_node("/notes/Alpha.md", None),
            file_node("/notes/Mid.md", None),
            file_node("/notes/Zeta.md", None),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::NameDescending);

        let names: Vec<&str> = folder
            .children()
            .iter()
            .map(|node| node.path().file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["Zeta.md", "Mid.md", "Alpha.md"]);
    }

    #[test]
    fn set_sort_order_resorts_every_level_of_the_tree() {
        let nested = vec![
            file_node("/notes/dev/Newer.md", Some(system_time_at(20))),
            file_node("/notes/dev/Older.md", Some(system_time_at(10))),
        ];
        let children = vec![WorkFolderNode::Folder(WorkFolderFolder::new(
            PathBuf::from("/notes/dev"),
            nested,
            None,
        ))];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        let nested_names = |folder: &WorkFolder| {
            let WorkFolderNode::Folder(dev) = &folder.children()[0] else {
                panic!("dev folder expected");
            };
            dev.children()
                .iter()
                .map(|node| match node {
                    WorkFolderNode::File(entry) => entry.file_name().to_owned(),
                    WorkFolderNode::Folder(_) => {
                        unreachable!("only files were nested under dev")
                    }
                })
                .collect::<Vec<_>>()
        };

        folder.set_sort_order(WorkFolderSortOrder::NameDescending);
        assert_eq!(nested_names(&folder), ["Older.md", "Newer.md"]);
        folder.set_sort_order(WorkFolderSortOrder::UpdatedNewest);

        assert_eq!(
            nested_names(&folder),
            ["Newer.md", "Older.md"],
            "a sort-order switch must resort a nested folder's own children too, not just the root"
        );
    }

    #[test]
    fn each_folder_sorts_independently_of_its_siblings_under_name_descending() {
        // "階層は各親直下で独立": a folder's own children compare only among
        // themselves, never against another folder's children, regardless
        // of which order is active.
        let dev_children = vec![file_node("/notes/dev/Zeta.md", None)];
        let docs_children = vec![file_node("/notes/docs/Alpha.md", None)];
        let children = vec![
            WorkFolderNode::Folder(WorkFolderFolder::new(
                PathBuf::from("/notes/dev"),
                dev_children,
                None,
            )),
            WorkFolderNode::Folder(WorkFolderFolder::new(
                PathBuf::from("/notes/docs"),
                docs_children,
                None,
            )),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::NameDescending);

        let folder_names: Vec<&str> = folder
            .children()
            .iter()
            .map(|node| match node {
                WorkFolderNode::Folder(folder) => folder.name(),
                WorkFolderNode::File(_) => unreachable!("only folders at the root"),
            })
            .collect();
        assert_eq!(
            folder_names,
            ["docs", "dev"],
            "the root's own two folders still sort by name descending"
        );
        for node in folder.children() {
            let WorkFolderNode::Folder(folder) = node else {
                unreachable!("only folders at the root");
            };
            assert_eq!(
                folder.children().len(),
                1,
                "each folder keeps exactly its own single child regardless of sibling folders"
            );
        }
    }

    #[test]
    fn touching_an_entry_moves_it_without_a_rescan_under_updated_newest_order() {
        let children = vec![
            file_node("/notes/A.md", Some(system_time_at(100))),
            file_node("/notes/B.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::UpdatedNewest);
        assert_eq!(folder.children()[0].path(), Path::new("/notes/B.md"));

        // Simulates the modified time a save or autosave just produced,
        // without touching the filesystem from this test.
        folder.touch(
            Path::new("/notes/A.md"),
            Some(system_time_at(300)),
            next_modified_sequence(),
        );

        assert_eq!(folder.children()[0].path(), Path::new("/notes/A.md"));
    }

    #[test]
    fn a_reverse_delivered_touch_does_not_regress_a_newer_retained_modified_time() {
        // Two sibling saves under the same parent folder each acquire that
        // folder's own modified time off the UI thread once their write
        // lands, then feed it back through `touch` once their completion
        // reaches the UI thread — see `WorkFolder::ancestor_folder_paths`.
        // The read that actually happened later (`later_seq`, reporting 400)
        // is simulated as reaching the UI thread first, and the read that
        // happened earlier (`earlier_seq`, reporting 200) reaches it after —
        // the reversed delivery order `set_modified` must guard against,
        // using the acquisition-order `sequence`, not the two `SystemTime`
        // values, to tell which is actually newer.
        let earlier_seq = next_modified_sequence();
        let later_seq = next_modified_sequence();
        let children = vec![WorkFolderNode::Folder(WorkFolderFolder::new(
            PathBuf::from("/notes/dev"),
            vec![file_node("/notes/dev/Child.md", Some(system_time_at(100)))],
            Some(system_time_at(100)),
        ))];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);

        folder.touch(Path::new("/notes/dev"), Some(system_time_at(400)), later_seq);
        folder.touch(
            Path::new("/notes/dev"),
            Some(system_time_at(200)),
            earlier_seq,
        );

        let WorkFolderNode::Folder(dev) = &folder.children()[0] else {
            panic!("dev folder expected");
        };
        assert_eq!(
            dev.modified(),
            Some(system_time_at(400)),
            "the later-arriving but earlier-acquired touch must not overwrite the newer one"
        );
    }

    #[test]
    fn a_stale_none_arriving_after_a_newer_some_does_not_erase_it() {
        // The ancestor-fetch read that failed (reporting `None`, e.g. a
        // transient race) happened before the read that succeeded, but its
        // completion reaches the UI thread after the successful one's. A
        // guard comparing `modified` values directly cannot reject this —
        // there is nothing to compare a `None` against — so only comparing
        // `sequence` protects the already-applied `Some` here.
        let earlier_seq = next_modified_sequence();
        let later_seq = next_modified_sequence();
        let children = vec![file_node("/notes/A.md", None)];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);

        folder.touch(Path::new("/notes/A.md"), Some(system_time_at(400)), later_seq);
        folder.touch(Path::new("/notes/A.md"), None, earlier_seq);

        let WorkFolderNode::File(entry) = &folder.children()[0] else {
            panic!("file entry expected");
        };
        assert_eq!(
            entry.modified(),
            Some(system_time_at(400)),
            "a stale None must not erase an already-applied newer Some"
        );
    }

    #[test]
    fn a_newer_read_still_applies_a_none_per_the_degrade_on_failure_contract() {
        // The opposite of the previous case: the newer read is the one that
        // failed. `sequence` alone decides this, so the `None` still
        // overwrites the older `Some`, exactly like a value-based guard
        // would already do for two `Some`s in the correct order.
        let earlier_seq = next_modified_sequence();
        let later_seq = next_modified_sequence();
        let children = vec![file_node("/notes/A.md", None)];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);

        folder.touch(
            Path::new("/notes/A.md"),
            Some(system_time_at(100)),
            earlier_seq,
        );
        folder.touch(Path::new("/notes/A.md"), None, later_seq);

        let WorkFolderNode::File(entry) = &folder.children()[0] else {
            panic!("file entry expected");
        };
        assert_eq!(
            entry.modified(),
            None,
            "a genuinely newer read must apply even when it degrades to None"
        );
    }

    #[test]
    fn a_newer_read_applies_even_when_its_system_time_is_smaller() {
        // A real directory entry can legitimately report an earlier
        // `SystemTime` than what is already retained — e.g. a folder whose
        // mtime briefly ran ahead into the future catching back up on its
        // next real change. Comparing `SystemTime` values would wrongly
        // reject this forever; only `sequence` decides freshness here.
        let earlier_seq = next_modified_sequence();
        let later_seq = next_modified_sequence();
        let children = vec![file_node("/notes/A.md", None)];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);

        folder.touch(
            Path::new("/notes/A.md"),
            Some(system_time_at(900)),
            earlier_seq,
        );
        folder.touch(
            Path::new("/notes/A.md"),
            Some(system_time_at(100)),
            later_seq,
        );

        let WorkFolderNode::File(entry) = &folder.children()[0] else {
            panic!("file entry expected");
        };
        assert_eq!(
            entry.modified(),
            Some(system_time_at(100)),
            "a genuinely newer read must apply even though its own SystemTime is smaller"
        );
    }

    #[test]
    fn stamp_with_sequence_serializes_concurrent_reads_of_the_same_path() {
        // Reproduces the race `stamp_ancestor_folders` used to be exposed to
        // before it was rewritten on top of `stamp_with_sequence`: two
        // sibling saves whose `ancestor_folder_paths` both name the same
        // parent folder, each acquiring that folder's modified time off the
        // UI thread. Without per-path serialization, a first call's read can
        // land before a second, independent call's read for the same path,
        // yet still receive a *later* `next_modified_sequence` value if a
        // thread switch lands in the gap between "read the real modified
        // time" and "allocate the sequence" — exactly the gap
        // `stamp_with_sequence` closes by holding that path's lock across
        // both steps. A barrier inside this `FileService` double holds the
        // first call inside `stamp` so the test can assert, from the
        // outside, that a second call for the same path cannot even start
        // its own read (let alone finish and allocate a sequence) until the
        // first call has released the lock.
        use crate::identity::FileStamp;
        use crate::service::{LoadedFile, ReadFile, SavedFile};
        use hane_document::RopeBuffer;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        struct GatedFileService {
            entered_tx: Mutex<Option<mpsc::Sender<()>>>,
            release_rx: Mutex<Option<mpsc::Receiver<()>>>,
            modified: SystemTime,
        }

        impl FileService for GatedFileService {
            fn load(&self, _path: &Path) -> io::Result<LoadedFile> {
                unreachable!("not exercised by this test")
            }

            fn open_reader(&self, _path: &Path) -> io::Result<ReadFile> {
                unreachable!("not exercised by this test")
            }

            fn save(&self, _path: &Path, _document: &RopeBuffer) -> io::Result<SavedFile> {
                unreachable!("not exercised by this test")
            }

            fn stamp(&self, _path: &Path) -> Option<FileStamp> {
                if let Some(entered) = self.entered_tx.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                if let Some(release) = self.release_rx.lock().unwrap().take() {
                    release.recv().expect("test releases the first call");
                }
                Some(FileStamp::new(0, Some(self.modified)))
            }

            fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
                unreachable!("not exercised by this test")
            }

            fn rename_folder(&self, _from: &Path, _to: &Path) -> io::Result<()> {
                unreachable!("not exercised by this test")
            }

            fn create_dir(&self, _path: &Path) -> io::Result<()> {
                unreachable!("not exercised by this test")
            }
        }

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let files = Arc::new(GatedFileService {
            entered_tx: Mutex::new(Some(entered_tx)),
            release_rx: Mutex::new(Some(release_rx)),
            modified: system_time_at(100),
        });
        let path = PathBuf::from("/notes/dev");

        let first = {
            let files = Arc::clone(&files);
            let path = path.clone();
            thread::spawn(move || stamp_with_sequence(files.as_ref(), &path))
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the first call enters FileService::stamp while holding the path lock");

        let (second_started_tx, second_started_rx) = mpsc::channel();
        let second = {
            let files = Arc::clone(&files);
            let path = path.clone();
            thread::spawn(move || {
                let _ = second_started_tx.send(());
                stamp_with_sequence(files.as_ref(), &path)
            })
        };
        second_started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the second call's own thread starts running");

        // The second call must still be blocked acquiring the first call's
        // path lock: with no serialization it would reach `stamp` (which
        // this double only gates once) and return almost immediately.
        thread::sleep(Duration::from_millis(200));
        assert!(
            !second.is_finished(),
            "a second stamp_with_sequence call for the same path must not complete \
             while the first call still holds that path's lock"
        );

        release_tx
            .send(())
            .expect("first call is still waiting on the release channel");
        let (first_modified, first_sequence) = first.join().expect("first call completes");
        let (second_modified, second_sequence) = second.join().expect("second call completes");

        assert_eq!(first_modified, Some(system_time_at(100)));
        assert_eq!(second_modified, Some(system_time_at(100)));
        assert!(
            first_sequence < second_sequence,
            "the call whose read completed first must also receive the smaller sequence, \
             since serialization never lets the second call's read start earlier"
        );
    }

    #[test]
    fn renaming_a_file_preserves_its_retained_modified_time_for_updated_newest_order() {
        let children = vec![
            file_node("/notes/Alpha.md", Some(system_time_at(100))),
            file_node("/notes/Zeta.md", Some(system_time_at(200))),
        ];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::UpdatedNewest);

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
    fn renaming_a_folder_preserves_descendant_modified_times_for_updated_newest_order() {
        let nested = vec![file_node("/notes/dev/Child.md", Some(system_time_at(50)))];
        let children = vec![WorkFolderNode::Folder(WorkFolderFolder::new(
            PathBuf::from("/notes/dev"),
            nested,
            Some(system_time_at(60)),
        ))];
        let mut folder = WorkFolder::from_tree(PathBuf::from("/notes"), children);
        folder.set_sort_order(WorkFolderSortOrder::UpdatedNewest);

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
    fn ancestor_folder_paths_is_only_the_immediate_parent_not_the_whole_chain_to_root() {
        // Only the directory a write's directory entry actually lives in
        // changes on disk; `Archive` (the grandparent here) keeps its own
        // real modified time untouched, so it must not appear.
        let folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        assert_eq!(
            folder.ancestor_folder_paths(Path::new("/notes/Archive/Sub/Nested.md")),
            [PathBuf::from("/notes/Archive/Sub")]
        );
    }

    #[test]
    fn ancestor_folder_paths_is_empty_for_a_top_level_path() {
        let folder = WorkFolder::new(PathBuf::from("/notes"), Vec::new());
        assert!(
            folder
                .ancestor_folder_paths(Path::new("/notes/Meeting.md"))
                .is_empty()
        );
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
            "a real scan must acquire a modified time for later UpdatedNewest/UpdatedOldest sorting"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_scan_read_and_sequence_stay_ordered_with_a_newer_save() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let path = PathBuf::from("/notes/External.md");
        let (scan_read_tx, scan_read_rx) = mpsc::channel();
        let (release_scan_tx, release_scan_rx) = mpsc::channel();
        let scan_path = path.clone();
        let scan = thread::spawn(move || {
            modified_with_sequence(&scan_path, || {
                let stale = Some(system_time_at(100));
                scan_read_tx
                    .send(())
                    .expect("the test observes the stale scan read");
                release_scan_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the test releases the scan before sequence allocation");
                stale
            })
        });
        scan_read_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the scan has read its stale mtime and is paused before sequence allocation");

        let scan_holds_path_lock = matches!(
            path_stamp_lock(&path).try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        );

        let (save_lock_result_tx, save_lock_result_rx) = mpsc::channel();
        let save_path = path.clone();
        let save = thread::spawn(move || {
            let lock = path_stamp_lock(&save_path);
            let blocked_by_scan =
                matches!(lock.try_lock(), Err(std::sync::TryLockError::WouldBlock));
            save_lock_result_tx
                .send(blocked_by_scan)
                .expect("the test observes the save thread's path-lock probe");
            modified_with_sequence(&save_path, || Some(system_time_at(300)))
        });
        let save_was_blocked_by_scan = save_lock_result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the save probes the same path lock while the scan is paused");

        release_scan_tx
            .send(())
            .expect("the scan is paused until the test releases it");
        let (scan_modified, scan_sequence) = scan.join().expect("the scan completes");
        let (save_modified, save_sequence) = save.join().expect("the save completes");

        assert!(
            scan_holds_path_lock,
            "the scan must keep the path lock from its mtime read through sequence allocation"
        );
        assert!(
            save_was_blocked_by_scan,
            "the save's try_lock must observe the scan holding the path lock after its mtime read"
        );
        assert_eq!(scan_modified, Some(system_time_at(100)));
        assert_eq!(save_modified, Some(system_time_at(300)));
        assert!(scan_sequence < save_sequence);

        let other_path = PathBuf::from("/notes/Other.md");
        let mut folder = WorkFolder::from_tree(
            PathBuf::from("/notes"),
            vec![
                file_node(path.to_str().unwrap(), Some(system_time_at(50))),
                file_node(other_path.to_str().unwrap(), Some(system_time_at(200))),
            ],
        );
        folder.set_sort_order(WorkFolderSortOrder::UpdatedNewest);
        // Deliver the newer save first, then the scan that read the old mtime.
        folder.touch(&path, save_modified, save_sequence);
        folder.touch(&path, scan_modified, scan_sequence);

        let sorted_paths: Vec<&Path> = folder.children().iter().map(WorkFolderNode::path).collect();
        assert_eq!(
            sorted_paths,
            [path.as_path(), other_path.as_path()],
            "the newer saved mtime remains first under the actual UpdatedNewest ordering"
        );
        assert_eq!(
            folder
                .entry_for_path(&path)
                .and_then(WorkFolderEntry::modified),
            Some(system_time_at(300))
        );
    }

    #[test]
    fn a_save_read_taken_before_a_scan_does_not_overwrite_the_scanned_time_when_it_lands_later() {
        // Save side: its mtime read (and sequence) happens first, off the UI
        // thread. An external edit follows, the rescan observes it and is
        // applied; only then does the save completion arrive.
        let save_modified = Some(system_time_at(100));
        let save_seq = next_modified_sequence();

        let root = temporary_directory("workfolder-save-rescan-race");
        fs::create_dir_all(&root).unwrap();
        let external = root.join("External.md");
        let other = root.join("Other.md");
        fs::write(&external, "# External\n").unwrap();
        fs::write(&other, "# Other\n").unwrap();

        let mut folder = OsWorkFolderScanner.scan(&root).unwrap();
        let scanned = folder
            .entries()
            .into_iter()
            .find(|entry| entry.path() == external)
            .and_then(|entry| entry.modified())
            .expect("the scan observed the external file's modified time");
        assert!(scanned > system_time_at(100));

        // The stale save completion arrives after the scan result.
        folder.touch(&external, save_modified, save_seq);

        let after = folder
            .entries()
            .into_iter()
            .find(|entry| entry.path() == external)
            .and_then(|entry| entry.modified());
        assert_eq!(
            after,
            Some(scanned),
            "an older save read must not replace the scan's observation"
        );

        // A save whose read happens after the scan still applies.
        let later_seq = next_modified_sequence();
        folder.touch(&external, Some(system_time_at(9_999_999_999)), later_seq);
        let updated = folder
            .entries()
            .into_iter()
            .find(|entry| entry.path() == external)
            .and_then(|entry| entry.modified());
        assert_eq!(updated, Some(system_time_at(9_999_999_999)));
        fs::remove_dir_all(root).unwrap();
    }
}
