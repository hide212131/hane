use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Identity of the file backing a session.
///
/// The canonical path answers "is this the same file?" across symlinks, `.` and
/// `..` segments, and relative spellings. The display name is what the UI shows
/// and never participates in identity, so renaming the label cannot make two
/// sessions look like different files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    path: PathBuf,
    canonical: PathBuf,
}

impl FileIdentity {
    /// Identity from a path plus the canonical path resolved by the I/O layer.
    pub fn new(path: impl Into<PathBuf>, canonical: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            canonical: canonical.into(),
        }
    }

    /// Identity resolved without touching the filesystem. Used for paths that do
    /// not exist yet (Save As targets) and by test doubles.
    pub fn lexical(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self::new(path, lexical_canonical(path))
    }

    /// The path to read and write. Kept as the user spelled it so error messages
    /// and the title bar do not suddenly show a resolved symlink target.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// Short name for tabs and lists; falls back to the whole path for exotic
    /// paths with no final component.
    pub fn display_name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    pub fn display_path(&self) -> String {
        self.path.display().to_string()
    }

    pub fn directory(&self) -> Option<&Path> {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
    }

    pub fn is_same_file(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }

    /// Same-file test against a path the caller has not resolved, e.g. a filer
    /// event. Lexical only: it cannot see through a symlink the OS would.
    pub fn matches_path(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.path == path || self.canonical == lexical_canonical(path)
    }

    /// Follow a rename or move. The document is untouched: only where it is
    /// written changes.
    #[must_use]
    pub fn moved_to(&self, path: impl AsRef<Path>) -> Self {
        Self::lexical(path)
    }
}

/// Cheap fingerprint of a file on disk, captured when it is read or written, so
/// a later mismatch can be reported as an external change without keeping the
/// previous contents around.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    pub fn new(len: u64, modified: Option<SystemTime>) -> Self {
        Self { len, modified }
    }
}

/// Result of comparing the stamp recorded at load/save time with what the disk
/// reports now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalChange {
    Unchanged,
    Modified,
    Deleted,
    /// Nothing to compare against: the session never touched the disk, or the
    /// platform did not report a usable stamp.
    Unknown,
}

/// Whether the backing file is still believed to exist.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FilePresence {
    #[default]
    Present,
    /// Deleted or moved away underneath us. The session keeps its identity so a
    /// save can recreate the file at the same path.
    Missing,
}

/// File-side state of a session: identity, what the disk looked like, and
/// whether the file is still there. An untitled session has no identity but is
/// still a valid file state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileState {
    identity: Option<FileIdentity>,
    untitled_label: String,
    stamp: Option<FileStamp>,
    presence: FilePresence,
}

impl FileState {
    pub fn untitled(label: impl Into<String>) -> Self {
        Self {
            identity: None,
            untitled_label: label.into(),
            stamp: None,
            presence: FilePresence::Present,
        }
    }

    pub fn tracked(identity: FileIdentity, stamp: Option<FileStamp>) -> Self {
        Self {
            identity: Some(identity),
            untitled_label: String::new(),
            stamp,
            presence: FilePresence::Present,
        }
    }

    pub fn identity(&self) -> Option<&FileIdentity> {
        self.identity.as_ref()
    }

    pub fn path(&self) -> Option<&Path> {
        self.identity.as_ref().map(FileIdentity::path)
    }

    pub fn directory(&self) -> Option<&Path> {
        self.identity.as_ref().and_then(FileIdentity::directory)
    }

    pub fn stamp(&self) -> Option<FileStamp> {
        self.stamp
    }

    pub fn presence(&self) -> FilePresence {
        self.presence
    }

    /// Label for the window title and tabs.
    pub fn label(&self) -> String {
        self.identity
            .as_ref()
            .map_or_else(|| self.untitled_label.clone(), FileIdentity::display_path)
    }

    pub fn short_label(&self) -> String {
        self.identity
            .as_ref()
            .map_or_else(|| self.untitled_label.clone(), FileIdentity::display_name)
    }

    pub(crate) fn set_identity(&mut self, identity: FileIdentity, stamp: Option<FileStamp>) {
        self.identity = Some(identity);
        self.stamp = stamp;
        self.presence = FilePresence::Present;
    }

    pub(crate) fn set_stamp(&mut self, stamp: Option<FileStamp>) {
        self.stamp = stamp;
        self.presence = FilePresence::Present;
    }

    pub(crate) fn mark_missing(&mut self) {
        self.presence = FilePresence::Missing;
    }

    /// Compare the recorded stamp with what the disk reports now. `None` means
    /// the file is gone.
    pub fn compare(&self, current: Option<FileStamp>) -> ExternalChange {
        match (self.identity.as_ref(), self.stamp, current) {
            (None, _, _) => ExternalChange::Unknown,
            (Some(_), _, None) => ExternalChange::Deleted,
            (Some(_), None, Some(_)) => ExternalChange::Unknown,
            (Some(_), Some(recorded), Some(current)) => {
                if recorded == current {
                    ExternalChange::Unchanged
                } else {
                    ExternalChange::Modified
                }
            }
        }
    }
}

/// Normalizes `.` and `..` without asking the filesystem, so two spellings of
/// the same path compare equal even when the file does not exist yet.
fn lexical_canonical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(Component::ParentDir);
                }
            }
            other => normalized.push(other),
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(Component::CurDir);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_of_one_path_share_an_identity() {
        let plain = FileIdentity::lexical("/notes/./drafts/../drafts/post.md");
        let direct = FileIdentity::lexical("/notes/drafts/post.md");
        assert!(plain.is_same_file(&direct));
        assert!(direct.matches_path("/notes/drafts/../drafts/post.md"));
        assert_eq!(direct.display_name(), "post.md");
        assert_eq!(direct.directory(), Some(Path::new("/notes/drafts")));
    }

    #[test]
    fn different_files_never_match() {
        let one = FileIdentity::lexical("/notes/a.md");
        let two = FileIdentity::lexical("/notes/b.md");
        assert!(!one.is_same_file(&two));
        assert!(!one.matches_path("/notes/b.md"));
    }

    #[test]
    fn a_move_keeps_the_document_and_changes_only_the_path() {
        let before = FileIdentity::lexical("/notes/a.md");
        let after = before.moved_to("/archive/a.md");
        assert!(!before.is_same_file(&after));
        assert_eq!(after.path(), Path::new("/archive/a.md"));
    }

    #[test]
    fn stamps_classify_external_changes() {
        let stamp = FileStamp::new(12, None);
        let state = FileState::tracked(FileIdentity::lexical("/notes/a.md"), Some(stamp));
        assert_eq!(state.compare(Some(stamp)), ExternalChange::Unchanged);
        assert_eq!(
            state.compare(Some(FileStamp::new(13, None))),
            ExternalChange::Modified
        );
        assert_eq!(state.compare(None), ExternalChange::Deleted);
        assert_eq!(
            FileState::untitled("Untitled").compare(None),
            ExternalChange::Unknown
        );
    }
}
