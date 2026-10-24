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

    /// The sidecar that records the folder the note will be created in.
    fn target_file_name(self) -> String {
        format!("{:016x}.target", self.0)
    }

    fn from_file_name(name: &str) -> Option<Self> {
        u64::from_str_radix(name.strip_suffix(".md")?, 16)
            .ok()
            .map(Self)
    }
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
/// A target outside the root, or not valid UTF-8, falls back to the root.
fn encode_target(root: &Path, target_directory: &Path) -> String {
    target_directory
        .strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .unwrap_or("")
        .to_owned()
}

/// Inverse of `encode_target`. Anything that is not a plain relative path
/// below the root (absolute, `..`) is ignored in favour of the root.
fn decode_target(root: &Path, encoded: &str) -> PathBuf {
    let relative = Path::new(encoded.trim());
    let plain = relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    if plain {
        root.join(relative)
    } else {
        root.to_path_buf()
    }
}

fn write_atomically(dir: &Path, name: &str, contents: &str) -> io::Result<()> {
    let target = dir.join(name);
    let temporary = dir.join(format!("{name}.tmp"));
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(contents.as_bytes())?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }
    fs::rename(&temporary, &target)
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
        let dir = drafts_dir(root);
        fs::create_dir_all(&dir)?;
        // The target goes first: a crash between the two writes leaves a
        // draft whose text is merely one write old, never one without its
        // destination.
        write_atomically(
            &dir,
            &id.target_file_name(),
            &encode_target(root, target_directory),
        )?;
        write_atomically(&dir, &id.file_name(), text)
    }

    fn remove(&self, root: &Path, id: DraftId) -> io::Result<()> {
        let dir = drafts_dir(root);
        let ignore_missing = |result: io::Result<()>| match result {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
        ignore_missing(fs::remove_file(dir.join(id.file_name())))?;
        ignore_missing(fs::remove_file(dir.join(id.target_file_name())))
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
            let Ok(text) = fs::read_to_string(&path) else {
                failed += 1;
                continue;
            };
            // A missing or unreadable sidecar (a journal written before
            // targets were recorded) recovers to the work folder root.
            let target_directory = fs::read_to_string(dir.join(id.target_file_name()))
                .map_or_else(
                    |_| root.to_path_buf(),
                    |encoded| decode_target(root, &encoded),
                );
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
