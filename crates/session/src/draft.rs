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

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

    fn from_file_name(name: &str) -> Option<Self> {
        u64::from_str_radix(name.strip_suffix(".md")?, 16)
            .ok()
            .map(Self)
    }
}

/// One draft recovered from a previous run: its id, so it can keep being
/// journalled and removed under the same name, and the text it last held.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredDraft {
    pub id: DraftId,
    pub text: String,
}

/// The filesystem boundary for the unnamed-note recovery journal.
///
/// Every method blocks, and every caller runs them off the input path, the
/// same rule `FileService` and `WorkFolderScanner` follow.
pub trait DraftStore: Send + Sync + 'static {
    /// Writes (or overwrites) one draft's full text, atomically. Called on
    /// the same debounce cadence as autosave, so it must stay cheap.
    fn write(&self, root: &Path, id: DraftId, text: &str) -> io::Result<()>;

    /// Deletes one draft, once the note it recovered either got a real
    /// filename or its session closed clean. Already-missing is not an error.
    fn remove(&self, root: &Path, id: DraftId) -> io::Result<()>;

    /// Every draft left over from a previous run, in no particular order. A
    /// work folder with no recovery directory yet recovers to nothing, not
    /// an error.
    fn recover(&self, root: &Path) -> io::Result<Vec<RecoveredDraft>>;
}

fn drafts_dir(root: &Path) -> PathBuf {
    root.join(".hane").join("drafts")
}

/// `DraftStore` backed by the real filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsDraftStore;

impl DraftStore for OsDraftStore {
    fn write(&self, root: &Path, id: DraftId, text: &str) -> io::Result<()> {
        let dir = drafts_dir(root);
        fs::create_dir_all(&dir)?;
        let target = dir.join(id.file_name());
        let temporary = dir.join(format!("{}.tmp", id.file_name()));
        {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(text.as_bytes())?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
        }
        fs::rename(&temporary, &target)
    }

    fn remove(&self, root: &Path, id: DraftId) -> io::Result<()> {
        match fs::remove_file(drafts_dir(root).join(id.file_name())) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn recover(&self, root: &Path) -> io::Result<Vec<RecoveredDraft>> {
        let dir = drafts_dir(root);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut drafts = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(id) = DraftId::from_file_name(name) else {
                // Stray `.tmp` from an interrupted write, or something the
                // user dropped in here by hand: not a draft, skip it.
                continue;
            };
            let text = fs::read_to_string(&path)?;
            drafts.push(RecoveredDraft { id, text });
        }
        Ok(drafts)
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
        assert!(OsDraftStore.recover(&root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_written_draft_round_trips_through_recover() {
        let root = temporary_directory("draft-roundtrip");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        OsDraftStore
            .write(&root, id, "today I thought about this design")
            .unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, id);
        assert_eq!(recovered[0].text, "today I thought about this design");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rewriting_a_draft_keeps_one_file_with_the_latest_text() {
        let root = temporary_directory("draft-rewrite");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        OsDraftStore.write(&root, id, "first").unwrap();
        OsDraftStore.write(&root, id, "first, then more").unwrap();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].text, "first, then more");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_a_draft_drops_it_from_recovery() {
        let root = temporary_directory("draft-remove");
        fs::create_dir_all(&root).unwrap();
        let id = DraftId::generate();
        OsDraftStore.write(&root, id, "gone once named").unwrap();
        OsDraftStore.remove(&root, id).unwrap();

        assert!(OsDraftStore.recover(&root).unwrap().is_empty());
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

        assert!(OsDraftStore.recover(&root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generated_ids_are_distinct_even_back_to_back() {
        let a = DraftId::generate();
        let b = DraftId::generate();
        assert_ne!(a, b);
    }
}
