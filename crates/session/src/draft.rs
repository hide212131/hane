//! Crash-safe recovery for unnamed notes created in a work folder.
//!
//! An unnamed note (issue #5) is not written to the work folder as a `.md`
//! file until it has a real name, but "not saved until named" cannot mean
//! "lost on a crash". Instead every unnamed note is journalled into a hidden
//! `.hane/drafts` directory under the work folder root, one file per note,
//! keyed by a `DraftId` rather than a filename the user would have to manage.
//! A draft is removed once the note it recovers either gets a real filename
//! or the session it belongs to is closed cleanly.
//!
//! Kept separate from `FileService`: a draft is addressed by an id under a
//! directory the caller never names, not by a path it already knows, and a
//! `WorkFolderScanner` deliberately never looks inside `.hane`.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

static DRAFT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Identifies one draft file, stable for as long as the note it recovers
/// stays unnamed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DraftId(u64);

impl DraftId {
    /// A new id, unique within this process and, barring a clock rolled
    /// backwards across restarts, across runs too: the high bits are
    /// wall-clock nanoseconds, folded with a per-process sequence so two
    /// notes created in the same tick still differ.
    #[must_use]
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64);
        let sequence = DRAFT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(nanos ^ sequence)
    }

    fn file_name(self) -> String {
        format!("{:016x}.md", self.0)
    }

    /// The sidecar that records the folder the note will be created in.
    fn target_file_name(self) -> String {
        format!("{:016x}.target", self.0)
    }

    /// The single-file journal entry: target and text in one atomic rename,
    /// so recovery can never pair a text with another generation's target.
    fn entry_file_name(self) -> String {
        format!("{:016x}.draft", self.0)
    }

    /// Accepts both the current `.draft` entry and the legacy `.md` text file.
    fn from_file_name(name: &str) -> Option<Self> {
        let stem = name
            .strip_suffix(".draft")
            .or_else(|| name.strip_suffix(".md"))?;
        u64::from_str_radix(stem, 16).ok().map(Self)
    }
}

/// Orders draft snapshots across threads. Taken when a snapshot's text and
/// target are read, so a snapshot read later always carries a larger number.
static SNAPSHOT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// A fresh snapshot sequence number for `DraftStore::write_snapshot`.
#[must_use]
pub fn next_draft_snapshot_sequence() -> u64 {
    SNAPSHOT_SEQUENCE.fetch_add(1, Ordering::SeqCst)
}

const ENTRY_MAGIC: &[u8] = b"hane-draft-v1\n";

/// `ENTRY_MAGIC`, the target length as 8 little-endian bytes, the encoded
/// target, then the text.
fn encode_entry(target: &[u8], text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ENTRY_MAGIC.len() + 8 + target.len() + text.len());
    bytes.extend_from_slice(ENTRY_MAGIC);
    bytes.extend_from_slice(&(target.len() as u64).to_le_bytes());
    bytes.extend_from_slice(target);
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// Inverse of `encode_entry`: the encoded target and the text.
fn decode_entry(bytes: &[u8]) -> Option<(&[u8], String)> {
    let rest = bytes.strip_prefix(ENTRY_MAGIC)?;
    let (length, rest) = rest.split_first_chunk::<8>()?;
    let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
    if rest.len() < length {
        return None;
    }
    let (target, text) = rest.split_at(length);
    Some((target, String::from_utf8(text.to_vec()).ok()?))
}

/// One draft recovered from a previous run: its id, so it can keep being
/// journalled and removed under the same name, the text it last held, and the
/// folder the note will be created in once it is named. Entries journalled
/// without a target (older versions) recover to the work folder root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredDraft {
    pub id: DraftId,
    pub text: String,
    pub target_directory: PathBuf,
}

/// The outcome of a recovery pass: every draft that could be read, plus how
/// many entries in the recovery directory looked like a draft (or might have
/// been one) but could not be read. `failed` being nonzero must be visible to
/// the user — an unreadable draft is unsaved content that recovery could not
/// bring back, not a draft that never existed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveredDrafts {
    pub drafts: Vec<RecoveredDraft>,
    pub failed: usize,
}

/// The filesystem boundary for the unnamed-note recovery journal.
///
/// Every method blocks, and every caller runs them off the input path, the
/// same rule `FileService` and `WorkFolderScanner` follow.
pub trait DraftStore: Send + Sync + 'static {
    /// Writes (or overwrites) one draft's full text, atomically. Called on
    /// the same debounce cadence as autosave, so it must stay cheap.
    /// `target_directory` is the folder the note will be created in; it is
    /// journalled alongside the text so recovery can restore it.
    fn write(
        &self,
        root: &Path,
        id: DraftId,
        target_directory: &Path,
        text: &str,
    ) -> io::Result<()>;

    /// Like `write`, for a snapshot read when `sequence` was taken from
    /// `next_draft_snapshot_sequence`. A store that can order snapshots skips
    /// one older than what it already holds, so a late debounce write cannot
    /// roll back a newer flush. The default ignores the order.
    fn write_snapshot(
        &self,
        root: &Path,
        id: DraftId,
        target_directory: &Path,
        text: &str,
        _sequence: u64,
    ) -> io::Result<()> {
        self.write(root, id, target_directory, text)
    }

    /// Deletes one draft, once the note it recovered either got a real
    /// filename or its session closed clean. Already-missing is not an error.
    fn remove(&self, root: &Path, id: DraftId) -> io::Result<()>;

    /// Every draft left over from a previous run, in no particular order,
    /// plus a count of entries that could not be read. A work folder with no
    /// recovery directory yet recovers to nothing, not an error; a directory
    /// that cannot be listed at all (a permission error on `.hane/drafts`
    /// itself) still is.
    fn recover(&self, root: &Path) -> io::Result<RecoveredDrafts>;
}

fn drafts_dir(root: &Path) -> PathBuf {
    root.join(".hane").join("drafts")
}

/// The target directory as a root-relative string (empty for the root
/// itself). Relative so the journal survives the work folder being moved.
/// A target outside the root falls back to the root. On Unix the raw path
/// bytes are kept, so a valid non-UTF-8 folder name is not lost. On Windows a
/// UTF-8 path is stored as plain UTF-8 (the original sidecar format); any other
/// path is stored as its UTF-16 code units behind `WIDE_PREFIX`. Elsewhere a
/// path that is not valid UTF-8 falls back to the root.
fn encode_target(root: &Path, target_directory: &Path) -> Vec<u8> {
    let Ok(relative) = target_directory.strip_prefix(root) else {
        return Vec::new();
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        relative.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        match relative.to_str() {
            Some(text) => text.as_bytes().to_vec(),
            None => encode_wide_units(&relative.as_os_str().encode_wide().collect::<Vec<_>>()),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        relative.to_str().unwrap_or("").as_bytes().to_vec()
    }
}

/// Marks a sidecar holding UTF-16 code units. It starts with a NUL, which no
/// path can contain, so it never collides with a plain UTF-8 target.
#[cfg(any(windows, test))]
const WIDE_PREFIX: &[u8] = b"\0wide:";

/// UTF-16 code units as `WIDE_PREFIX` followed by four lowercase hex digits
/// per unit.
#[cfg(any(windows, test))]
fn encode_wide_units(units: &[u16]) -> Vec<u8> {
    let mut encoded = WIDE_PREFIX.to_vec();
    for unit in units {
        encoded.extend_from_slice(format!("{unit:04x}").as_bytes());
    }
    encoded
}

/// Inverse of `encode_wide_units`; `None` when `encoded` is not that format.
#[cfg(any(windows, test))]
fn decode_wide_units(encoded: &[u8]) -> Option<Vec<u16>> {
    let digits = encoded.strip_prefix(WIDE_PREFIX)?;
    if digits.len() % 4 != 0 {
        return None;
    }
    digits
        .chunks(4)
        .map(|chunk| u16::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok())
        .collect()
}

/// Inverse of `encode_target`. Anything that is not a plain relative path
/// below the root (absolute, `..`) is ignored in favour of the root.
fn decode_target(root: &Path, encoded: &[u8]) -> PathBuf {
    // Not trimmed: a folder name may validly begin or end with whitespace.
    #[cfg(unix)]
    let relative = {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(encoded))
    };
    #[cfg(windows)]
    let relative = if encoded.starts_with(WIDE_PREFIX) {
        use std::os::windows::ffi::OsStringExt;
        match decode_wide_units(encoded) {
            Some(units) => PathBuf::from(std::ffi::OsString::from_wide(&units)),
            None => return root.to_path_buf(),
        }
    } else {
        match std::str::from_utf8(encoded) {
            Ok(text) => PathBuf::from(text),
            Err(_) => return root.to_path_buf(),
        }
    };
    #[cfg(not(any(unix, windows)))]
    let relative = match std::str::from_utf8(encoded) {
        Ok(text) => PathBuf::from(text),
        Err(_) => return root.to_path_buf(),
    };
    let plain = relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    if plain {
        root.join(&relative)
    } else {
        root.to_path_buf()
    }
}

fn write_atomically(dir: &Path, name: &str, contents: &[u8]) -> io::Result<()> {
    let target = dir.join(name);
    let temporary = dir.join(format!("{name}.tmp"));
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(contents)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }
    fs::rename(&temporary, &target)
}

/// Guards one draft; the value is the newest snapshot sequence accepted (or
/// the sequence of its removal) in this process.
type DraftLocks = Mutex<HashMap<(PathBuf, DraftId), Arc<Mutex<u64>>>>;

static DRAFT_LOCKS: OnceLock<DraftLocks> = OnceLock::new();

/// The lock serializing every write/remove of one draft in one work folder.
fn draft_lock(root: &Path, id: DraftId) -> Arc<Mutex<u64>> {
    let mut locks = DRAFT_LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    Arc::clone(locks.entry((root.to_path_buf(), id)).or_default())
}

/// `DraftStore` backed by the real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsDraftStore;

impl DraftStore for OsDraftStore {
    fn write(
        &self,
        root: &Path,
        id: DraftId,
        target_directory: &Path,
        text: &str,
    ) -> io::Result<()> {
        self.write_snapshot(
            root,
            id,
            target_directory,
            text,
            next_draft_snapshot_sequence(),
        )
    }

    fn write_snapshot(
        &self,
        root: &Path,
        id: DraftId,
        target_directory: &Path,
        text: &str,
        sequence: u64,
    ) -> io::Result<()> {
        // The debounced save and the quit/switch flush can write the same
        // draft from different threads, so the transaction is serialized per
        // draft, and a snapshot older than one already accepted (or than the
        // draft's removal) is dropped instead of rolling it back.
        let lock = draft_lock(root, id);
        let mut newest = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if sequence < *newest {
            return Ok(());
        }
        let dir = drafts_dir(root);
        fs::create_dir_all(&dir)?;
        // Target and text land in one file by one atomic rename, so a crash
        // leaves either the previous generation or this one, never a mix.
        write_atomically(
            &dir,
            &id.entry_file_name(),
            &encode_entry(&encode_target(root, target_directory), text),
        )?;
        *newest = sequence;
        // A legacy pair from an older version is superseded. The new entry
        // wins on recovery anyway, so a failed cleanup is harmless.
        let _ = fs::remove_file(dir.join(id.file_name()));
        let _ = fs::remove_file(dir.join(id.target_file_name()));
        Ok(())
    }

    fn remove(&self, root: &Path, id: DraftId) -> io::Result<()> {
        let lock = draft_lock(root, id);
        let mut newest = lock.lock().unwrap_or_else(PoisonError::into_inner);
        // Snapshots read before this removal must not resurrect the draft.
        *newest = (*newest).max(next_draft_snapshot_sequence());
        let dir = drafts_dir(root);
        let ignore_missing = |result: io::Result<()>| match result {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
        // Legacy files first: if a crash lands in between, the surviving
        // entry is a consistent one rather than an older legacy generation.
        ignore_missing(fs::remove_file(dir.join(id.file_name())))?;
        ignore_missing(fs::remove_file(dir.join(id.target_file_name())))?;
        ignore_missing(fs::remove_file(dir.join(id.entry_file_name())))
    }

    fn recover(&self, root: &Path) -> io::Result<RecoveredDrafts> {
        let dir = drafts_dir(root);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RecoveredDrafts::default());
            }
            Err(error) => return Err(error),
        };
        let mut drafts = Vec::new();
        let mut failed = 0;
        let mut seen = std::collections::HashSet::new();
        for entry in entries {
            // One unreadable entry (a permission error on a single file, a
            // race with another process removing it) must not sacrifice
            // every other draft in the directory: skip it and keep going,
            // rather than letting `?` here abort the whole recovery and lose
            // drafts that were perfectly readable. It is still lost content,
            // though, so it counts toward `failed` rather than disappearing
            // silently: the caller surfaces that count to the user.
            let Ok(entry) = entry else {
                failed += 1;
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                failed += 1;
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(id) = DraftId::from_file_name(name) else {
                // Stray `.tmp` from an interrupted write, or something the
                // user dropped in here by hand: not a draft, so its absence
                // from the result is not a failure.
                continue;
            };
            // Both `NNN.draft` and legacy `NNN.md` name the same id.
            if !seen.insert(id) {
                continue;
            }
            match fs::read(dir.join(id.entry_file_name())) {
                Ok(bytes) => {
                    match decode_entry(&bytes) {
                        Some((encoded, text)) => drafts.push(RecoveredDraft {
                            id,
                            text,
                            target_directory: decode_target(root, encoded),
                        }),
                        None => failed += 1,
                    }
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    failed += 1;
                    continue;
                }
            }
            // Legacy two-file journal.
            let Ok(text) = fs::read_to_string(dir.join(id.file_name())) else {
                failed += 1;
                continue;
            };
            // A missing sidecar (a journal written before targets were
            // recorded) recovers to the work folder root. Any other read
            // failure must not silently move the note to the root: the
            // draft is left on disk and reported as failed instead.
            let target_directory = match fs::read(dir.join(id.target_file_name())) {
                Ok(encoded) => decode_target(root, &encoded),
                Err(error) if error.kind() == io::ErrorKind::NotFound => root.to_path_buf(),
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            drafts.push(RecoveredDraft {
                id,
                text,
                target_directory,
            });
        }
        Ok(RecoveredDrafts { drafts, failed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::temporary_directory;

    #[test]
    fn a_folder_with_no_recovery_directory_recovers_to_nothing() {
        let root = temporary_directory("draft-none");
        fs::create_dir_all(&root).unwrap();
        let recovered = OsDraftStore.recover(&root).unwrap();
        assert!(recovered.drafts.is_empty());
        assert_eq!(recovered.failed, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_written_draft_round_trips_through_recover() {
        let root = temporary_directory("draft-roundtrip");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        OsDraftStore
            .write(&root, id, &root, "today I thought about this design")
            .unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].id, id);
        assert_eq!(recovered.drafts[0].target_directory, root);
        assert_eq!(
            recovered.drafts[0].text,
            "today I thought about this design"
        );
        assert_eq!(recovered.failed, 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rewriting_a_draft_keeps_one_file_with_the_latest_text() {
        let root = temporary_directory("draft-rewrite");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        OsDraftStore.write(&root, id, &root, "first").unwrap();
        OsDraftStore
            .write(&root, id, &root, "first, then more")
            .unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].text, "first, then more");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_writes_to_one_draft_recover_one_consistent_snapshot() {
        use std::sync::Barrier;

        let root = temporary_directory("draft-concurrent-write");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let threads = 8;
        for round in 0..20 {
            let barrier = Barrier::new(threads);
            std::thread::scope(|scope| {
                for n in 0..threads {
                    let (root, barrier) = (&root, &barrier);
                    scope.spawn(move || {
                        let target = root.join(format!("folder-{n}"));
                        let text = format!("snapshot-{n}");
                        barrier.wait();
                        OsDraftStore
                            .write(root, id, &target, &text)
                            .unwrap_or_else(|error| {
                                panic!("round {round} writer {n} failed: {error}")
                            });
                    });
                }
            });

            let recovered = OsDraftStore.recover(&root).unwrap();
            assert_eq!(recovered.failed, 0);
            assert_eq!(recovered.drafts.len(), 1);
            let draft = &recovered.drafts[0];
            let n = draft
                .text
                .strip_prefix("snapshot-")
                .expect("body comes from a written snapshot");
            assert_eq!(draft.target_directory, root.join(format!("folder-{n}")));
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_interrupted_update_recovers_the_previous_generation_whole() {
        let root = temporary_directory("draft-interrupted-update");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let old_target = root.join("old-folder");
        OsDraftStore.write(&root, id, &old_target, "old text").unwrap();

        // A crash before the rename leaves only a partial temporary file of
        // the next generation; it must not leak into the recovered draft.
        let dir = drafts_dir(&root);
        let partial = encode_entry(&encode_target(&root, &root.join("new-folder")), "new text");
        fs::write(
            dir.join(format!("{}.tmp", id.entry_file_name())),
            &partial[..partial.len() / 2],
        )
        .unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.failed, 0);
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].text, "old text");
        assert_eq!(recovered.drafts[0].target_directory, old_target);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_new_write_supersedes_a_legacy_text_and_target_pair() {
        let root = temporary_directory("draft-supersedes-legacy");
        let dir = drafts_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        let id = DraftId::generate();
        fs::write(dir.join(id.file_name()), "legacy text").unwrap();
        fs::write(dir.join(id.target_file_name()), "legacy-folder").unwrap();

        let target = root.join("new-folder");
        OsDraftStore.write(&root, id, &target, "new text").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.failed, 0);
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].text, "new text");
        assert_eq!(recovered.drafts[0].target_directory, target);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_stale_snapshot_arriving_after_a_newer_flush_does_not_roll_it_back() {
        let root = temporary_directory("draft-stale-snapshot");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        // The timer snapshot is read first, the flush snapshot after it, but
        // the flush's write completes before the timer's write starts.
        let timer_sequence = next_draft_snapshot_sequence();
        let flush_sequence = next_draft_snapshot_sequence();
        let new_target = root.join("new-folder");
        OsDraftStore
            .write_snapshot(&root, id, &new_target, "newer", flush_sequence)
            .unwrap();
        OsDraftStore
            .write_snapshot(&root, id, &root.join("old-folder"), "older", timer_sequence)
            .unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].text, "newer");
        assert_eq!(recovered.drafts[0].target_directory, new_target);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_snapshot_read_before_a_removal_does_not_resurrect_the_draft() {
        let root = temporary_directory("draft-stale-after-remove");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let sequence = next_draft_snapshot_sequence();
        OsDraftStore.remove(&root, id).unwrap();
        OsDraftStore
            .write_snapshot(&root, id, &root, "late", sequence)
            .unwrap();

        assert!(OsDraftStore.recover(&root).unwrap().drafts.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_a_draft_drops_it_from_recovery() {
        let root = temporary_directory("draft-remove");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let child = root.join("child");
        OsDraftStore
            .write(&root, id, &child, "gone once named")
            .unwrap();
        OsDraftStore.remove(&root, id).unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert!(recovered.drafts.is_empty());
        assert_eq!(recovered.failed, 0);
        assert!(!drafts_dir(&root).join(id.target_file_name()).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_child_folder_target_round_trips_through_recover() {
        let root = temporary_directory("draft-target-roundtrip");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let child = root.join("projects").join("notes");
        OsDraftStore.write(&root, id, &child, "# Title").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].target_directory, child);
        assert_eq!(recovered.failed, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_target_with_edge_whitespace_round_trips_without_being_renamed() {
        let root = temporary_directory("draft-target-whitespace");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let child = root.join(" lead").join("notes ");
        OsDraftStore.write(&root, id, &child, "# Title").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].target_directory, child);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_target_round_trips_losslessly_and_stays_creatable() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let root = temporary_directory("draft-target-non-utf8");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let child = root.join(OsStr::from_bytes(b"notes-\xff\xfe"));
        // The sidecar round-trip does not need the folder to exist, so it
        // runs on every Unix filesystem, including ones that reject
        // non-UTF-8 names (e.g. APFS on macOS).
        OsDraftStore.write(&root, id, &child, "# Title").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.failed, 0);
        let target = &recovered.drafts[0].target_directory;
        assert_eq!(target, &child);
        assert_ne!(target, &root);
        assert_eq!(target.file_name(), child.file_name());

        // Creating the real folder only works where the filesystem accepts
        // invalid UTF-8 names. Only that specific rejection is tolerated;
        // permission and other I/O errors fail the test.
        match fs::create_dir_all(&child) {
            Ok(()) => {
                fs::write(target.join("Title.md"), "# Title").unwrap();
                assert!(child.join("Title.md").is_file());
                assert!(!root.join("Title.md").exists());
            }
            Err(error) if rejects_non_utf8_names(&error) => {}
            Err(error) => panic!("unexpected error creating {child:?}: {error}"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// Whether `error` is a filesystem refusing a name's encoding (illegal
    /// byte sequence / invalid argument), as opposed to a real I/O failure.
    #[cfg(unix)]
    fn rejects_non_utf8_names(error: &io::Error) -> bool {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        const EILSEQ: i32 = 92;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        const EILSEQ: i32 = 84;
        const EINVAL: i32 = 22;
        matches!(error.raw_os_error(), Some(EILSEQ | EINVAL))
    }

    #[test]
    fn wide_units_round_trip_losslessly_on_every_platform() {
        // An unpaired surrogate is not valid UTF-16, hence not UTF-8 either.
        let units = [0x006e, 0xd800, 0x006f, 0x3042];
        let encoded = encode_wide_units(&units);
        assert!(std::str::from_utf8(&encoded).is_ok());
        assert_eq!(decode_wide_units(&encoded).unwrap(), units);
        assert_eq!(decode_wide_units(b"plain/relative"), None);
        assert_eq!(decode_wide_units(b"\0wide:00z0"), None);
        assert_eq!(decode_wide_units(b"\0wide:006"), None);
    }

    #[cfg(windows)]
    #[test]
    fn a_non_utf8_windows_target_round_trips_through_wide_units() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let root = temporary_directory("draft-target-wide");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        let name = OsString::from_wide(&[0x006e, 0xd800, 0x006f]);
        assert!(name.to_str().is_none());
        let child = root.join(&name);
        OsDraftStore.write(&root, id, &child, "# Title").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.failed, 0);
        assert_eq!(recovered.drafts[0].target_directory, child);
        assert_ne!(recovered.drafts[0].target_directory, root);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_sidecar_that_cannot_be_read_keeps_the_draft_out_of_the_root() {
        let root = temporary_directory("draft-sidecar-read-error");
        let dir = drafts_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        let id = DraftId::generate();
        fs::write(dir.join(id.file_name()), "text").unwrap();
        // A directory in the sidecar's place fails to read with an error
        // other than NotFound, whatever user the test runs as.
        fs::create_dir(dir.join(id.target_file_name())).unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert!(recovered.drafts.is_empty());
        assert_eq!(recovered.failed, 1);
        assert!(dir.join(id.file_name()).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_legacy_draft_without_a_target_recovers_to_the_root() {
        let root = temporary_directory("draft-legacy-target");
        let dir = drafts_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        let id = DraftId::generate();
        fs::write(dir.join(id.file_name()), "legacy").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].target_directory, root);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_escaping_target_recovers_to_the_root() {
        let root = temporary_directory("draft-escaping-target");
        let dir = drafts_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        let id = DraftId::generate();
        fs::write(dir.join(id.file_name()), "text").unwrap();
        fs::write(dir.join(id.target_file_name()), "../outside").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts[0].target_directory, root);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_a_draft_that_was_never_written_is_not_an_error() {
        let root = temporary_directory("draft-remove-missing");
        fs::create_dir_all(&root).unwrap();
        OsDraftStore.remove(&root, DraftId::generate()).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stray_non_draft_files_are_ignored_on_recovery() {
        let root = temporary_directory("draft-stray");
        let dir = drafts_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("not-a-draft.txt"), "ignore me").unwrap();
        fs::write(dir.join("0000000000000001.tmp"), "interrupted write").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert!(recovered.drafts.is_empty());
        // Neither file is even shaped like a draft, so their absence is not
        // reported as a failure: there was nothing here to lose.
        assert_eq!(recovered.failed, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generated_ids_are_distinct_even_back_to_back() {
        let a = DraftId::generate();
        let b = DraftId::generate();
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn a_draft_that_fails_to_read_is_counted_as_failed_without_losing_the_others() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_directory("draft-partial-failure");
        fs::create_dir_all(&root).unwrap();
        let readable = DraftId::generate();
        OsDraftStore
            .write(&root, readable, &root, "keep me")
            .unwrap();
        let unreadable = DraftId(readable.0.wrapping_add(1));
        let unreadable_path = drafts_dir(&root).join(unreadable.file_name());
        fs::write(&unreadable_path, "lost to a permission error").unwrap();
        fs::set_permissions(&unreadable_path, fs::Permissions::from_mode(0o000)).unwrap();

        // Running as root ignores permission bits entirely; skip the
        // assertion in that case rather than asserting on a premise that
        // does not hold, but still exercise `recover` so the test is not a
        // silent no-op under a normal, non-root run.
        let permission_actually_blocks_reads = fs::read_to_string(&unreadable_path).is_err();
        let recovered = OsDraftStore.recover(&root);

        // Restore permissions before any assertion can fail the test early,
        // so cleanup always succeeds.
        fs::set_permissions(&unreadable_path, fs::Permissions::from_mode(0o644)).unwrap();

        if permission_actually_blocks_reads {
            let recovered = recovered.unwrap();
            assert_eq!(recovered.drafts.len(), 1);
            assert_eq!(recovered.drafts[0].id, readable);
            assert_eq!(recovered.drafts[0].text, "keep me");
            // The unreadable draft must not vanish without a trace: it is
            // reported as a failure so the caller can tell the user one
            // draft could not be recovered.
            assert_eq!(recovered.failed, 1);
        }

        fs::remove_dir_all(root).unwrap();
    }
}
