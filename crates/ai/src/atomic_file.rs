//! Same-filesystem atomic replace for the small JSON/text files this crate
//! persists (`AiSettings`, the credential operation journal): write to a
//! sibling temp file, `fsync` it, then `rename` over the destination. A
//! reader can therefore only ever observe the previous fully-valid file or
//! the new fully-valid file, never a torn write.
//!
//! `std::fs::rename` already replaces an existing destination atomically on
//! every platform this crate targets: on Unix it is a direct `rename(2)`,
//! and on Windows the standard library's implementation passes
//! `MOVEFILE_REPLACE_EXISTING` to `MoveFileExW`, so (unlike a naive
//! same-named-destination-must-not-exist assumption) an existing destination
//! file is replaced, not rejected. What `rename` alone does not guarantee is
//! that the *directory entry* update it performs is itself durable across a
//! crash: on Unix, that additionally requires `fsync`ing the parent
//! directory once the rename returns, which `fsync_parent_dir_after_rename`
//! below does. Windows has no equivalent safe-to-use-from-`std` operation
//! for that (opening a directory as a `File` is not portable), so this is
//! Unix-only; on Windows, `MoveFileExW`'s own metadata-flush behavior is
//! relied on instead.
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn fsync_parent_dir_after_rename(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn fsync_parent_dir_after_rename(_parent: &Path) -> io::Result<()> {
    Ok(())
}

/// The outcome of a failed [`atomic_write_bytes`]: callers must not treat
/// every failure the same way.
#[derive(Debug)]
pub(crate) enum AtomicWriteError {
    /// The write never took effect: `path` (if it existed) still has its
    /// previous contents. Safe to treat exactly like "this save did not
    /// happen".
    NotPersisted(io::Error),
    /// `rename` itself succeeded — any reader opening `path` now observes
    /// the new content — but syncing the parent directory entry for crash
    /// durability afterward failed. Only that crash-durability guarantee,
    /// not the write itself, is in question: callers must not treat this the
    /// same as [`Self::NotPersisted`] (e.g. by deleting a resource the new,
    /// already-visible content now references).
    RenameSucceededSyncFailed(io::Error),
}

impl std::fmt::Display for AtomicWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtomicWriteError::NotPersisted(e) => write!(f, "{e}"),
            AtomicWriteError::RenameSucceededSyncFailed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AtomicWriteError {}

#[cfg(test)]
pub(crate) mod fault_injection {
    //! Deterministic, per-path fault injection for
    //! [`super::AtomicWriteError::RenameSucceededSyncFailed`], used by this
    //! module's own tests and by other modules' tests (e.g.
    //! `crate::connect`'s credential-update tests) that need to exercise that
    //! ambiguous-durability path without a real disk fault. Scoped by exact
    //! destination path (not a single global flag) so concurrently running
    //! tests targeting their own unique temp paths can never interfere with
    //! each other.
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static FAIL_NEXT_SYNC_FOR: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(crate) fn fail_next_parent_dir_sync_for(path: &Path) {
        FAIL_NEXT_SYNC_FOR.lock().unwrap().push(path.to_path_buf());
    }

    pub(crate) fn take_should_fail(path: &Path) -> bool {
        let mut guard = FAIL_NEXT_SYNC_FOR.lock().unwrap();
        match guard.iter().position(|p| p == path) {
            Some(index) => {
                guard.remove(index);
                true
            }
            None => false,
        }
    }
}

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(AtomicWriteError::NotPersisted)?;
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("hane-ai");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{stem}.hane-ai-{}-{sequence}.tmp",
        std::process::id()
    ));
    let write_result: io::Result<()> = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, path)
    })();
    match write_result {
        Ok(()) => {
            // `rename` has already landed: any reader opening `path` now
            // observes `bytes`. Only the parent directory entry's own
            // crash-durability fsync remains, and its failure must be
            // reported distinctly from an outright "did not persist".
            #[cfg(test)]
            if fault_injection::take_should_fail(path) {
                return Err(AtomicWriteError::RenameSucceededSyncFailed(
                    io::Error::other("injected parent directory fsync failure for testing"),
                ));
            }
            fsync_parent_dir_after_rename(parent)
                .map_err(AtomicWriteError::RenameSucceededSyncFailed)
        }
        Err(e) => {
            let _ = fs::remove_file(&temporary);
            Err(AtomicWriteError::NotPersisted(e))
        }
    }
}

pub(crate) fn read_to_string_if_exists(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_then_read_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("hane-ai-atomic-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("value.json");

        assert_eq!(read_to_string_if_exists(&path).unwrap(), None);
        atomic_write_bytes(&path, b"{\"a\":1}").unwrap();
        assert_eq!(
            read_to_string_if_exists(&path).unwrap().as_deref(),
            Some("{\"a\":1}")
        );

        atomic_write_bytes(&path, b"{\"a\":2}").unwrap();
        assert_eq!(
            read_to_string_if_exists(&path).unwrap().as_deref(),
            Some("{\"a\":2}")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_an_existing_destination_leaves_no_stray_temp_files_behind() {
        // Regression coverage for "atomic replace of an existing
        // destination": repeatedly overwriting the same path must fully
        // replace its content each time (never merge/append) and must never
        // leave a `.<name>.hane-ai-*.tmp` sibling behind once a write
        // succeeds.
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-atomic-file-test-replace-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("value.json");

        atomic_write_bytes(&path, b"first").unwrap();
        atomic_write_bytes(&path, b"second-and-longer").unwrap();
        atomic_write_bytes(&path, b"third").unwrap();

        assert_eq!(
            read_to_string_if_exists(&path).unwrap().as_deref(),
            Some("third")
        );

        let leftover_temp_files: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".hane-ai-"))
            .collect();
        assert!(
            leftover_temp_files.is_empty(),
            "expected no leftover temp files after successful atomic replaces, found: {leftover_temp_files:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_succeeded_sync_failed_still_leaves_the_new_content_readable() {
        // Regression coverage for the "rename landed, but the parent
        // directory's own crash-durability fsync could not be confirmed"
        // case: the caller must be told this is not the same as "nothing was
        // written" (see `crate::connect::update_custom_credential`, which
        // must not delete a freshly written credential on this outcome).
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-atomic-file-test-ambiguous-durability-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("value.json");

        fault_injection::fail_next_parent_dir_sync_for(&path);
        let err = atomic_write_bytes(&path, b"{\"a\":1}").unwrap_err();
        assert!(
            matches!(err, AtomicWriteError::RenameSucceededSyncFailed(_)),
            "expected RenameSucceededSyncFailed, got {err:?}"
        );

        // The rename itself must have landed regardless of the reported
        // error: any reader opening `path` now sees the new content.
        assert_eq!(
            read_to_string_if_exists(&path).unwrap().as_deref(),
            Some("{\"a\":1}")
        );

        // The fault is consumed exactly once: the next write for the same
        // path is unaffected.
        atomic_write_bytes(&path, b"{\"a\":2}").unwrap();
        assert_eq!(
            read_to_string_if_exists(&path).unwrap().as_deref(),
            Some("{\"a\":2}")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
