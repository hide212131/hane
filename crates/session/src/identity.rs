use hane_document::RopeBuffer;
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

    /// Same-file test for a Save As target. Beyond the lexical match this asks
    /// the filesystem, so a symlink (both ends resolved) or, on Unix, a hard link
    /// to the session's own file is recognised as the same file and keeps the
    /// ordinary external-change protection. A target that does not exist yet or
    /// cannot be inspected is a different file. On Windows the volume serial
    /// number and file index of the opened file identify hard links.
    pub fn is_same_file_on_disk(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        if self.matches_path(path) {
            return true;
        }
        if let (Ok(own), Ok(target)) = (
            std::fs::canonicalize(&self.path),
            std::fs::canonicalize(path),
        ) && own == target
        {
            return true;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let (Ok(own), Ok(target)) = (
                std::fs::metadata(&self.path),
                std::fs::metadata(path),
            ) {
                return own.dev() == target.dev() && own.ino() == target.ino();
            }
        }
        #[cfg(windows)]
        {
            if let (Some(own), Some(target)) = (
                windows_file_id(&self.path),
                windows_file_id(path),
            ) {
                return own == target;
            }
        }
        false
    }

    /// Follow a rename or move. The document is untouched: only where it is
    /// written changes.
    #[must_use]
    pub fn moved_to(&self, path: impl AsRef<Path>) -> Self {
        Self::lexical(path)
    }
}

/// Mirrors `BY_HANDLE_FILE_INFORMATION` from `<fileapi.h>`; it is passed by
/// pointer straight to `kernel32.dll`, so the layout must match exactly.
#[cfg(windows)]
#[repr(C)]
struct ByHandleFileInformation {
    file_attributes: u32,
    creation_time: [u32; 2],
    last_access_time: [u32; 2],
    last_write_time: [u32; 2],
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetFileInformationByHandle"]
    fn get_file_information_by_handle(
        handle: *mut std::ffi::c_void,
        info: *mut ByHandleFileInformation,
    ) -> i32;
}

/// Volume serial number and file index of the file at `path`, which are the
/// same for every hard link to one physical file. `None` when the file cannot
/// be opened or queried.
#[cfg(windows)]
fn windows_file_id(path: &Path) -> Option<(u32, u64)> {
    let file = std::fs::File::open(path).ok()?;
    windows_handle_id(&file)
}

/// Same as `windows_file_id`, for a handle that is already open.
#[cfg(windows)]
fn windows_handle_id(file: &std::fs::File) -> Option<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;

    // SAFETY: `file` owns a valid handle for the duration of the call, and
    // `info` is a plain-old-data struct matching `BY_HANDLE_FILE_INFORMATION`
    // that the API fully initializes when it returns non-zero.
    let info = unsafe {
        let mut info: ByHandleFileInformation = std::mem::zeroed();
        if get_file_information_by_handle(file.as_raw_handle().cast(), &mut info) == 0 {
            return None;
        }
        info
    };
    Some((
        info.volume_serial_number,
        (u64::from(info.file_index_high) << 32) | u64::from(info.file_index_low),
    ))
}

/// Whether `path` still names the very file `file` has open. A path that is
/// gone or now names another file (an atomic replacement) is `false`, so a
/// body read through the old handle is never attributed to the new entry. On
/// platforms without an OS file id the caller's stamp comparison is the only
/// evidence, so this reports `true`.
#[cfg(unix)]
pub(crate) fn handle_is_path(file: &std::fs::File, path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let own = file.metadata()?;
    match std::fs::metadata(path) {
        Ok(target) => Ok(own.dev() == target.dev() && own.ino() == target.ino()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
pub(crate) fn handle_is_path(file: &std::fs::File, path: &Path) -> std::io::Result<bool> {
    Ok(match (windows_handle_id(file), windows_file_id(path)) {
        (Some(own), Some(target)) => own == target,
        _ => false,
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn handle_is_path(_file: &std::fs::File, _path: &Path) -> std::io::Result<bool> {
    Ok(true)
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

/// Fingerprint of the exact bytes a document is written as. The stamp alone
/// cannot tell a same-size, same-mtime rewrite or an atomic replacement apart
/// from the file the session last read, so every comparison that decides
/// "is this still my version?" also uses this. It is computed from the bytes
/// the save format emits (`RopeBuffer::write_to`), which is what a later read
/// of the same file yields, so BOM / newline handling stays the document's.
///
/// Two independent 64-bit lanes plus the length guard against accidental
/// collisions; it is a change detector, not a security primitive.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ContentFingerprint {
    len: u64,
    lane_a: u64,
    lane_b: u64,
}

impl ContentFingerprint {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut fingerprinter = Fingerprinter::default();
        fingerprinter.update(bytes);
        fingerprinter.finish()
    }

    /// Fingerprint of the bytes `document` is saved as. Walks the rope chunks
    /// without materializing one contiguous string.
    pub fn of_document(document: &RopeBuffer) -> Self {
        let mut fingerprinter = Fingerprinter::default();
        // Writing into the fingerprinter cannot fail.
        let _ = document.write_to(&mut fingerprinter);
        fingerprinter.finish()
    }

    /// Byte length of the fingerprinted content.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Incremental builder for [`ContentFingerprint`]; also an `io::Write` sink so
/// a save or read path can fingerprint exactly what it streams.
#[derive(Clone, Copy, Debug)]
pub struct Fingerprinter {
    len: u64,
    lane_a: u64,
    lane_b: u64,
}

impl Default for Fingerprinter {
    fn default() -> Self {
        Self {
            len: 0,
            lane_a: 0xcbf2_9ce4_8422_2325,
            lane_b: 0x9e37_79b9_7f4a_7c15,
        }
    }
}

impl Fingerprinter {
    pub fn update(&mut self, bytes: &[u8]) {
        let (mut lane_a, mut lane_b) = (self.lane_a, self.lane_b);
        for &byte in bytes {
            lane_a ^= u64::from(byte);
            lane_a = lane_a.wrapping_mul(0x0000_0100_0000_01b3);
            lane_b = (lane_b.rotate_left(5) ^ u64::from(byte)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        }
        self.lane_a = lane_a;
        self.lane_b = lane_b;
        self.len = self.len.wrapping_add(bytes.len() as u64);
    }

    pub fn finish(&self) -> ContentFingerprint {
        ContentFingerprint {
            len: self.len,
            lane_a: self.lane_a,
            lane_b: self.lane_b,
        }
    }
}

impl std::io::Write for Fingerprinter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
    /// Content of the last version this session read or wrote. `None` means
    /// not established yet (a freshly opened file is fingerprinted by the
    /// first background check, never on the UI thread).
    baseline: Option<ContentFingerprint>,
    presence: FilePresence,
}

impl FileState {
    pub fn untitled(label: impl Into<String>) -> Self {
        Self {
            identity: None,
            untitled_label: label.into(),
            stamp: None,
            baseline: None,
            presence: FilePresence::Present,
        }
    }

    pub fn tracked(identity: FileIdentity, stamp: Option<FileStamp>) -> Self {
        Self {
            identity: Some(identity),
            untitled_label: String::new(),
            stamp,
            baseline: None,
            presence: FilePresence::Present,
        }
    }

    pub fn baseline(&self) -> Option<ContentFingerprint> {
        self.baseline
    }

    pub(crate) fn set_baseline(&mut self, baseline: Option<ContentFingerprint>) {
        self.baseline = baseline;
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

    /// The file exists again (a different version than the recorded one, so
    /// the stamp is left alone).
    pub(crate) fn mark_present(&mut self) {
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
pub fn lexical_canonical(path: &Path) -> PathBuf {
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

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hane-identity-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_as_target_on_disk_matches_only_the_same_file() {
        let dir = scratch_dir("plain");
        let own = dir.join("a.md");
        let other = dir.join("b.md");
        std::fs::write(&own, "a").unwrap();
        std::fs::write(&other, "b").unwrap();
        let identity = FileIdentity::new(&own, std::fs::canonicalize(&own).unwrap());
        assert!(identity.is_same_file_on_disk(&own));
        assert!(!identity.is_same_file_on_disk(&other));
        assert!(!identity.is_same_file_on_disk(dir.join("missing.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn save_as_target_through_a_symlink_is_the_same_file() {
        let dir = scratch_dir("symlink");
        let own = dir.join("a.md");
        let link = dir.join("link.md");
        std::fs::write(&own, "a").unwrap();
        std::os::unix::fs::symlink(&own, &link).unwrap();
        let identity = FileIdentity::new(&own, std::fs::canonicalize(&own).unwrap());
        assert!(!identity.matches_path(&link));
        assert!(identity.is_same_file_on_disk(&link));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn save_as_target_through_a_hard_link_is_the_same_file() {
        let dir = scratch_dir("hardlink");
        let own = dir.join("a.md");
        let hard = dir.join("hard.md");
        std::fs::write(&own, "a").unwrap();
        std::fs::hard_link(&own, &hard).unwrap();
        let identity = FileIdentity::new(&own, std::fs::canonicalize(&own).unwrap());
        assert!(!identity.matches_path(&hard));
        assert!(identity.is_same_file_on_disk(&hard));
        let _ = std::fs::remove_dir_all(&dir);
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
