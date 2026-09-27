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

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let stem = path.file_name().and_then(|name| name.to_str()).unwrap_or("hane-ai");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{stem}.hane-ai-{}-{sequence}.tmp", std::process::id()));
    let result = (|| {
        let file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&temporary, path)?;
        fsync_parent_dir_after_rename(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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
        let dir = std::env::temp_dir().join(format!("hane-ai-atomic-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("value.json");

        assert_eq!(read_to_string_if_exists(&path).unwrap(), None);
        atomic_write_bytes(&path, b"{\"a\":1}").unwrap();
        assert_eq!(read_to_string_if_exists(&path).unwrap().as_deref(), Some("{\"a\":1}"));

        atomic_write_bytes(&path, b"{\"a\":2}").unwrap();
        assert_eq!(read_to_string_if_exists(&path).unwrap().as_deref(), Some("{\"a\":2}"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_an_existing_destination_leaves_no_stray_temp_files_behind() {
        // Regression coverage for "atomic replace of an existing
        // destination": repeatedly overwriting the same path must fully
        // replace its content each time (never merge/append) and must never
        // leave a `.<name>.hane-ai-*.tmp` sibling behind once a write
        // succeeds.
        let dir = std::env::temp_dir().join(format!("hane-ai-atomic-file-test-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("value.json");

        atomic_write_bytes(&path, b"first").unwrap();
        atomic_write_bytes(&path, b"second-and-longer").unwrap();
        atomic_write_bytes(&path, b"third").unwrap();

        assert_eq!(read_to_string_if_exists(&path).unwrap().as_deref(), Some("third"));

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
}
