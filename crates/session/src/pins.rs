//! Sidebar pins: an ordered list of Markdown files and folders per work
//! folder, kept apart from settings and recent files.
//!
//! The list is a set of shortcuts. Only the path relative to the work folder
//! root and the kind are stored; names, modified times and expansion state
//! are always resolved from the current work folder. Paths are stored as
//! native bytes (hex encoded) so a name that is not valid UTF-8 never
//! collides with another one.

use crate::service::atomic_write_bytes;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const PINS_FILE: &str = "sidebar-pins.conf";
const PINS_HEADER_PREFIX: &str = "hane-pins ";
const PINS_VERSION: u32 = 1;

/// Serializes every pin update in this process, so concurrent windows apply
/// each change to the latest stored state instead of a stale snapshot.
static PIN_LOCK: Mutex<()> = Mutex::new(());
/// Grows with each load/update taken under `PIN_LOCK`, so a caller can tell
/// which of two results came later.
static PIN_REVISION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinKind {
    File,
    Folder,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedEntry {
    /// Path relative to the work folder root.
    pub relative_path: PathBuf,
    pub kind: PinKind,
}

impl PinnedEntry {
    pub fn new(relative_path: impl Into<PathBuf>, kind: PinKind) -> Self {
        Self {
            relative_path: relative_path.into(),
            kind,
        }
    }
}

/// One change to a work folder's pin list. Paths are root-relative.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinUpdate {
    /// Adds at the front; a path that is already pinned is left alone.
    Pin(PinnedEntry),
    Unpin(PathBuf),
    RenameFile {
        from: PathBuf,
        to: PathBuf,
    },
    /// Moves the folder pin itself and every pin below it.
    RenameFolder {
        from: PathBuf,
        to: PathBuf,
    },
    /// Drops a trashed file, or a trashed folder together with everything
    /// below it.
    Remove {
        path: PathBuf,
        folder: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinSnapshot {
    pub entries: Vec<PinnedEntry>,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinError {
    /// The path is empty, absolute, leaves the root or targets Hane's own
    /// state directory.
    InvalidPath(PathBuf),
    /// The stored data could not be read. It is left untouched on disk.
    Unreadable(String),
    /// The stored data is damaged. It is left untouched on disk.
    Corrupt(String),
    /// The stored data was written by a newer version.
    UnknownVersion(String),
    Write(String),
}

impl fmt::Display for PinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => {
                write!(formatter, "ピン留めできないパスです: {}", path.display())
            }
            Self::Unreadable(reason) => {
                write!(formatter, "ピン留めの保存データを読み込めません: {reason}")
            }
            Self::Corrupt(reason) => {
                write!(formatter, "ピン留めの保存データが壊れています: {reason}")
            }
            Self::UnknownVersion(version) => write!(
                formatter,
                "ピン留めの保存データは未対応の形式です（{version}）"
            ),
            Self::Write(reason) => write!(formatter, "ピン留めを保存できません: {reason}"),
        }
    }
}

impl std::error::Error for PinError {}

/// Persisted pins, keyed by work folder root.
pub trait PinRepository: Send + Sync + 'static {
    fn load(&self, root: &Path) -> Result<PinSnapshot, PinError>;
    /// Applies `updates` in order to the latest stored list of `root` and
    /// stores the result. Nothing is stored when any step fails.
    fn update(&self, root: &Path, updates: &[PinUpdate]) -> Result<PinSnapshot, PinError>;
}

/// Checks that `path` is a root-relative path made of plain names, outside
/// Hane's own `.hane` directory.
pub fn validate_pin_path(path: &Path) -> Result<(), PinError> {
    let mut count = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                if count == 0 && name == OsStr::new(".hane") {
                    return Err(PinError::InvalidPath(path.to_path_buf()));
                }
                count += 1;
            }
            _ => return Err(PinError::InvalidPath(path.to_path_buf())),
        }
    }
    if count == 0 {
        return Err(PinError::InvalidPath(path.to_path_buf()));
    }
    Ok(())
}

fn rebase(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(from).ok()?;
    Some(if relative.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(relative)
    })
}

fn dedupe(entries: &mut Vec<PinnedEntry>) {
    let mut seen: Vec<PathBuf> = Vec::new();
    entries.retain(|entry| {
        if seen.contains(&entry.relative_path) {
            false
        } else {
            seen.push(entry.relative_path.clone());
            true
        }
    });
}

/// Applies one update to `entries` in place. Returns whether anything
/// changed. On error `entries` is unchanged.
pub fn apply_pin_update(
    entries: &mut Vec<PinnedEntry>,
    update: &PinUpdate,
) -> Result<bool, PinError> {
    match update {
        PinUpdate::Pin(entry) => {
            validate_pin_path(&entry.relative_path)?;
            if entries
                .iter()
                .any(|existing| existing.relative_path == entry.relative_path)
            {
                return Ok(false);
            }
            entries.insert(0, entry.clone());
            Ok(true)
        }
        PinUpdate::Unpin(path) => {
            let before = entries.len();
            entries.retain(|entry| &entry.relative_path != path);
            Ok(entries.len() != before)
        }
        PinUpdate::RenameFile { from, to } => {
            validate_pin_path(to)?;
            let mut changed = false;
            for entry in entries.iter_mut() {
                if entry.kind == PinKind::File && &entry.relative_path == from {
                    entry.relative_path = to.clone();
                    changed = true;
                }
            }
            if changed {
                dedupe(entries);
            }
            Ok(changed)
        }
        PinUpdate::RenameFolder { from, to } => {
            validate_pin_path(to)?;
            let mut changed = false;
            for entry in entries.iter_mut() {
                if let Some(moved) = rebase(&entry.relative_path, from, to) {
                    entry.relative_path = moved;
                    changed = true;
                }
            }
            if changed {
                dedupe(entries);
            }
            Ok(changed)
        }
        PinUpdate::Remove { path, folder } => {
            let before = entries.len();
            if *folder {
                entries.retain(|entry| !entry.relative_path.starts_with(path));
            } else {
                entries.retain(|entry| {
                    !(entry.kind == PinKind::File && &entry.relative_path == path)
                });
            }
            Ok(entries.len() != before)
        }
    }
}

fn apply_all(entries: &mut Vec<PinnedEntry>, updates: &[PinUpdate]) -> Result<bool, PinError> {
    let mut working = entries.clone();
    let mut changed = false;
    for update in updates {
        changed |= apply_pin_update(&mut working, update)?;
    }
    *entries = working;
    Ok(changed)
}

fn next_revision() -> u64 {
    PIN_REVISION.fetch_add(1, Ordering::SeqCst) + 1
}

// --- In-memory repository -------------------------------------------------

/// Pins held in memory, for tests and for when no state directory exists.
#[derive(Debug, Default)]
pub struct MemoryPinStore {
    roots: Mutex<HashMap<PathBuf, Vec<PinnedEntry>>>,
}

impl PinRepository for MemoryPinStore {
    fn load(&self, root: &Path) -> Result<PinSnapshot, PinError> {
        let roots = self.roots.lock().expect("pins lock");
        Ok(PinSnapshot {
            entries: roots.get(root).cloned().unwrap_or_default(),
            revision: next_revision(),
        })
    }

    fn update(&self, root: &Path, updates: &[PinUpdate]) -> Result<PinSnapshot, PinError> {
        let mut roots = self.roots.lock().expect("pins lock");
        let mut entries = roots.get(root).cloned().unwrap_or_default();
        apply_all(&mut entries, updates)?;
        roots.insert(root.to_path_buf(), entries.clone());
        Ok(PinSnapshot {
            entries,
            revision: next_revision(),
        })
    }
}

// --- File repository ------------------------------------------------------

/// The file-backed repository: one small file under the state directory that
/// holds every work folder's list.
#[derive(Clone, Debug)]
pub struct FilePinStore {
    dir: PathBuf,
}

impl FilePinStore {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn file(&self) -> PathBuf {
        self.dir.join(PINS_FILE)
    }

    fn read_all(&self) -> Result<Vec<(OsString, Vec<PinnedEntry>)>, PinError> {
        match fs::read(self.file()) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes)
                    .map_err(|error| PinError::Corrupt(error.to_string()))?;
                parse_pins(&text)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(PinError::Unreadable(error.to_string())),
        }
    }

    fn write_all(&self, roots: &[(OsString, Vec<PinnedEntry>)]) -> Result<(), PinError> {
        fs::create_dir_all(&self.dir).map_err(|error| PinError::Write(error.to_string()))?;
        let text = render_pins(roots);
        atomic_write_bytes(&self.file(), text.as_bytes())
            .map_err(|error| PinError::Write(error.to_string()))
    }
}

/// The identity of a work folder in the store: its canonical path when the OS
/// can resolve one, otherwise the path as given.
fn root_key(root: &Path) -> OsString {
    fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .into_os_string()
}

impl PinRepository for FilePinStore {
    fn load(&self, root: &Path) -> Result<PinSnapshot, PinError> {
        let _guard = PIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = root_key(root);
        let roots = self.read_all()?;
        let entries = roots
            .into_iter()
            .find(|(stored, _)| *stored == key)
            .map(|(_, entries)| entries)
            .unwrap_or_default();
        Ok(PinSnapshot {
            entries,
            revision: next_revision(),
        })
    }

    fn update(&self, root: &Path, updates: &[PinUpdate]) -> Result<PinSnapshot, PinError> {
        let _guard = PIN_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = root_key(root);
        // A damaged or newer-format file fails here, before anything is
        // written, so it is never replaced by an empty list.
        let mut roots = self.read_all()?;
        let position = roots.iter().position(|(stored, _)| *stored == key);
        let mut entries = position
            .map(|index| roots[index].1.clone())
            .unwrap_or_default();
        let changed = apply_all(&mut entries, updates)?;
        if changed {
            match position {
                Some(index) => roots[index].1 = entries.clone(),
                None => roots.push((key, entries.clone())),
            }
            roots.retain(|(_, list)| !list.is_empty());
            self.write_all(&roots)?;
        }
        Ok(PinSnapshot {
            entries,
            revision: next_revision(),
        })
    }
}

fn render_pins(roots: &[(OsString, Vec<PinnedEntry>)]) -> String {
    let mut text = format!("{PINS_HEADER_PREFIX}{PINS_VERSION}\n");
    for (root, entries) in roots {
        text.push_str("root ");
        text.push_str(&encode_os(root));
        text.push('\n');
        for entry in entries {
            text.push_str(match entry.kind {
                PinKind::File => "file ",
                PinKind::Folder => "folder ",
            });
            text.push_str(&encode_os(entry.relative_path.as_os_str()));
            text.push('\n');
        }
    }
    text
}

fn parse_pins(text: &str) -> Result<Vec<(OsString, Vec<PinnedEntry>)>, PinError> {
    let mut lines = text.lines();
    let header = lines
        .next()
        .ok_or_else(|| PinError::Corrupt("empty file".to_owned()))?;
    let version = header
        .strip_prefix(PINS_HEADER_PREFIX)
        .ok_or_else(|| PinError::Corrupt("missing header".to_owned()))?;
    if version.trim() != PINS_VERSION.to_string() {
        return Err(PinError::UnknownVersion(version.trim().to_owned()));
    }
    let mut roots: Vec<(OsString, Vec<PinnedEntry>)> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let (tag, value) = line
            .split_once(' ')
            .ok_or_else(|| PinError::Corrupt(format!("unreadable line: {line}")))?;
        let decoded = decode_os(value)
            .ok_or_else(|| PinError::Corrupt(format!("unreadable value: {line}")))?;
        match tag {
            "root" => roots.push((decoded, Vec::new())),
            "file" | "folder" => {
                let kind = if tag == "file" {
                    PinKind::File
                } else {
                    PinKind::Folder
                };
                let relative = PathBuf::from(decoded);
                validate_pin_path(&relative)
                    .map_err(|_| PinError::Corrupt(format!("invalid path: {line}")))?;
                let Some((_, entries)) = roots.last_mut() else {
                    return Err(PinError::Corrupt("entry before root".to_owned()));
                };
                if entries
                    .iter()
                    .any(|existing| existing.relative_path == relative)
                {
                    return Err(PinError::Corrupt(format!("duplicate entry: {line}")));
                }
                entries.push(PinnedEntry {
                    relative_path: relative,
                    kind,
                });
            }
            _ => return Err(PinError::Corrupt(format!("unknown line: {line}"))),
        }
    }
    Ok(roots)
}

fn hex_digit(value: u8) -> char {
    char::from(b"0123456789abcdef"[usize::from(value & 0x0f)])
}

#[cfg(unix)]
fn os_units(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_units(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value
        .encode_wide()
        .flat_map(|unit| unit.to_be_bytes())
        .collect()
}

#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the Windows variant rejects an odd byte count"
)]
fn os_from_units(bytes: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(OsString::from_vec(bytes.to_vec()))
}

#[cfg(windows)]
fn os_from_units(bytes: &[u8]) -> Option<OsString> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() & 1 == 1 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    Some(OsString::from_wide(&units))
}

fn encode_os(value: &OsStr) -> String {
    let mut text = String::new();
    for byte in os_units(value) {
        text.push(hex_digit(byte >> 4));
        text.push(hex_digit(byte));
    }
    text
}

fn decode_os(hex: &str) -> Option<OsString> {
    let digits = hex.as_bytes();
    if digits.len() & 1 == 1 {
        return None;
    }
    let nibble = |digit: u8| match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    };
    let mut bytes = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks_exact(2) {
        let high = nibble(pair[0])?;
        let low = nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    os_from_units(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::temporary_directory;

    fn file(path: &str) -> PinnedEntry {
        PinnedEntry::new(path, PinKind::File)
    }

    fn folder(path: &str) -> PinnedEntry {
        PinnedEntry::new(path, PinKind::Folder)
    }

    fn paths(entries: &[PinnedEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.relative_path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn pinning_adds_to_the_front_and_ignores_duplicates() {
        let mut entries = Vec::new();
        assert!(apply_pin_update(&mut entries, &PinUpdate::Pin(file("a.md"))).unwrap());
        assert!(apply_pin_update(&mut entries, &PinUpdate::Pin(folder("dir"))).unwrap());
        assert!(!apply_pin_update(&mut entries, &PinUpdate::Pin(file("a.md"))).unwrap());
        assert_eq!(paths(&entries), ["dir", "a.md"]);
    }

    #[test]
    fn unpinning_then_pinning_again_goes_to_the_front() {
        let mut entries = vec![file("a.md"), file("b.md")];
        apply_pin_update(&mut entries, &PinUpdate::Unpin(PathBuf::from("a.md"))).unwrap();
        apply_pin_update(&mut entries, &PinUpdate::Pin(file("a.md"))).unwrap();
        assert_eq!(paths(&entries), ["a.md", "b.md"]);
    }

    #[test]
    fn invalid_paths_are_rejected() {
        let mut entries = Vec::new();
        for bad in ["", "../x.md", ".hane/drafts/a.md", "a/../../b.md"] {
            assert!(
                apply_pin_update(&mut entries, &PinUpdate::Pin(file(bad))).is_err(),
                "{bad:?} must be rejected"
            );
        }
        #[cfg(unix)]
        assert!(apply_pin_update(&mut entries, &PinUpdate::Pin(file("/abs/a.md"))).is_err());
        assert!(entries.is_empty());
    }

    #[test]
    fn same_name_under_different_parents_are_distinct() {
        let mut entries = Vec::new();
        apply_pin_update(&mut entries, &PinUpdate::Pin(file("設計/会議.md"))).unwrap();
        apply_pin_update(&mut entries, &PinUpdate::Pin(file("別/会議.md"))).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn file_rename_keeps_the_position() {
        let mut entries = vec![file("a.md"), file("b.md"), file("c.md")];
        let update = PinUpdate::RenameFile {
            from: PathBuf::from("b.md"),
            to: PathBuf::from("sub/b2.md"),
        };
        assert!(apply_pin_update(&mut entries, &update).unwrap());
        assert_eq!(paths(&entries), ["a.md", "sub/b2.md", "c.md"]);
    }

    #[test]
    fn folder_rename_moves_descendants_by_component_only() {
        let mut entries = vec![
            file("a/x.md"),
            folder("a"),
            file("ab/y.md"),
            file("a/deep/z.md"),
        ];
        let update = PinUpdate::RenameFolder {
            from: PathBuf::from("a"),
            to: PathBuf::from("c"),
        };
        assert!(apply_pin_update(&mut entries, &update).unwrap());
        assert_eq!(paths(&entries), ["c/x.md", "c", "ab/y.md", "c/deep/z.md"]);
    }

    #[test]
    fn a_rename_onto_an_existing_pin_collapses_into_one_in_original_order() {
        let mut entries = vec![file("b.md"), file("a.md")];
        let update = PinUpdate::RenameFile {
            from: PathBuf::from("a.md"),
            to: PathBuf::from("b.md"),
        };
        apply_pin_update(&mut entries, &update).unwrap();
        assert_eq!(paths(&entries), ["b.md"]);
    }

    #[test]
    fn removing_a_folder_drops_its_descendants_only() {
        let mut entries = vec![file("a/x.md"), folder("a"), file("ab/y.md")];
        let update = PinUpdate::Remove {
            path: PathBuf::from("a"),
            folder: true,
        };
        assert!(apply_pin_update(&mut entries, &update).unwrap());
        assert_eq!(paths(&entries), ["ab/y.md"]);
    }

    #[test]
    fn removing_a_file_keeps_everything_else() {
        let mut entries = vec![file("a.md"), file("b.md")];
        let update = PinUpdate::Remove {
            path: PathBuf::from("a.md"),
            folder: false,
        };
        apply_pin_update(&mut entries, &update).unwrap();
        assert_eq!(paths(&entries), ["b.md"]);
    }

    #[test]
    fn a_failing_step_leaves_the_batch_unapplied() {
        let mut entries = vec![file("a.md")];
        let updates = [
            PinUpdate::Pin(file("b.md")),
            PinUpdate::Pin(file("../escape.md")),
        ];
        assert!(apply_all(&mut entries, &updates).is_err());
        assert_eq!(paths(&entries), ["a.md"]);
    }

    #[test]
    fn memory_store_separates_roots_and_orders_updates() {
        let store = MemoryPinStore::default();
        let a = Path::new("/notes/a");
        let b = Path::new("/notes/b");
        store.update(a, &[PinUpdate::Pin(file("x.md"))]).unwrap();
        store.update(a, &[PinUpdate::Pin(file("y.md"))]).unwrap();
        store.update(b, &[PinUpdate::Pin(file("z.md"))]).unwrap();
        assert_eq!(paths(&store.load(a).unwrap().entries), ["y.md", "x.md"]);
        assert_eq!(paths(&store.load(b).unwrap().entries), ["z.md"]);
    }

    #[test]
    fn revisions_increase_with_each_call() {
        let store = MemoryPinStore::default();
        let root = Path::new("/notes");
        let first = store.update(root, &[PinUpdate::Pin(file("x.md"))]).unwrap();
        let second = store.update(root, &[PinUpdate::Pin(file("y.md"))]).unwrap();
        assert!(second.revision > first.revision);
    }

    #[test]
    fn file_store_round_trips_order_across_instances_and_roots() {
        let dir = temporary_directory("pins-round-trip");
        let root_a = temporary_directory("pins-root-a");
        let root_b = temporary_directory("pins-root-b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        let store = FilePinStore::at(&dir);
        store.update(&root_a, &[PinUpdate::Pin(file("x.md"))]).unwrap();
        store
            .update(&root_a, &[PinUpdate::Pin(folder("設計"))])
            .unwrap();
        store.update(&root_b, &[PinUpdate::Pin(file("z.md"))]).unwrap();
        let reopened = FilePinStore::at(&dir);
        assert_eq!(paths(&reopened.load(&root_a).unwrap().entries), ["設計", "x.md"]);
        assert_eq!(reopened.load(&root_a).unwrap().entries[0].kind, PinKind::Folder);
        assert_eq!(paths(&reopened.load(&root_b).unwrap().entries), ["z.md"]);
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(root_a).unwrap();
        fs::remove_dir_all(root_b).unwrap();
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        let dir = temporary_directory("pins-missing");
        let store = FilePinStore::at(&dir);
        assert!(store.load(Path::new("/nowhere")).unwrap().entries.is_empty());
    }

    #[test]
    fn a_damaged_file_is_reported_and_left_untouched() {
        let dir = temporary_directory("pins-damaged");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PINS_FILE), "not a pins file\n").unwrap();
        let store = FilePinStore::at(&dir);
        let root = Path::new("/nowhere");
        assert!(matches!(store.load(root), Err(PinError::Corrupt(_))));
        assert!(store.update(root, &[PinUpdate::Pin(file("a.md"))]).is_err());
        assert_eq!(
            fs::read_to_string(dir.join(PINS_FILE)).unwrap(),
            "not a pins file\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unknown_version_is_never_overwritten() {
        let dir = temporary_directory("pins-future");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(PINS_FILE), "hane-pins 99\nroot 00\n").unwrap();
        let store = FilePinStore::at(&dir);
        let root = Path::new("/nowhere");
        assert!(matches!(store.load(root), Err(PinError::UnknownVersion(_))));
        assert!(store.update(root, &[PinUpdate::Pin(file("a.md"))]).is_err());
        assert_eq!(
            fs::read_to_string(dir.join(PINS_FILE)).unwrap(),
            "hane-pins 99\nroot 00\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unwritable_state_directory_reports_the_failure_and_keeps_old_data() {
        let dir = temporary_directory("pins-write-fail");
        fs::create_dir_all(&dir).unwrap();
        // A directory where the file belongs makes the replace fail.
        fs::create_dir_all(dir.join(PINS_FILE)).unwrap();
        let store = FilePinStore::at(&dir);
        let root = Path::new("/nowhere");
        assert!(store.update(root, &[PinUpdate::Pin(file("a.md"))]).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn encoding_round_trips_non_ascii_names() {
        let original = OsString::from("設計/会議 メモ.md");
        assert_eq!(decode_os(&encode_os(&original)), Some(original));
        assert_eq!(decode_os("zz"), None);
        assert_eq!(decode_os("abc"), None);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_do_not_collide() {
        use std::os::unix::ffi::OsStringExt;
        let a = OsString::from_vec(vec![0x66, 0xff]);
        let b = OsString::from_vec(vec![0x66, 0xfe]);
        assert_ne!(encode_os(&a), encode_os(&b));
        assert_eq!(decode_os(&encode_os(&a)), Some(a));
    }
}
