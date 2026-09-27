//! Same-filesystem atomic replace for the small JSON/text files this crate
//! persists (`AiSettings`, the credential operation journal): write to a
//! sibling temp file, `fsync` it, then `rename` over the destination. A
//! reader can therefore only ever observe the previous fully-valid file or
//! the new fully-valid file, never a torn write.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        fs::rename(&temporary, path)
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
}
