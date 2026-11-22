//! The descriptor file a client reads to find a running instance: where its
//! pipe is, who it is and the token to present. Published atomically into a
//! per-user directory and removed on a normal exit; the leftovers of a crash
//! are recognised by a process whose creation time no longer matches.

use crate::auth::Credentials;
use crate::message::{PROTOCOL_MAJOR, PROTOCOL_MINOR};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The file schema, not the protocol version.
const SCHEMA: u32 = 1;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub schema: u32,
    pub pid: u32,
    /// The process creation time as a decimal string (on Windows the FILETIME).
    pub process_created: String,
    /// The pipe's name without the `\\.\pipe\` prefix.
    pub pipe_name: String,
    pub instance_id: String,
    pub token: String,
    pub protocol: ProtocolRange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolRange {
    pub major: u32,
    pub minor: u32,
}

/// The pipe name for a process and instance: `Hane.Whisparrot.<pid>.<instanceId>`.
pub fn pipe_name(pid: u32, instance_id: &str) -> String {
    format!("Hane.Whisparrot.{pid}.{instance_id}")
}

impl Descriptor {
    pub fn new(pid: u32, process_created: u64, credentials: &Credentials) -> Self {
        Self {
            schema: SCHEMA,
            pid,
            process_created: process_created.to_string(),
            pipe_name: pipe_name(pid, credentials.instance_id()),
            instance_id: credentials.instance_id().to_owned(),
            token: credentials.token().to_owned(),
            protocol: ProtocolRange {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
            },
        }
    }

    pub fn file_name(pid: u32) -> String {
        format!("{pid}.json")
    }
}

/// The token never reaches a log through `{:?}`.
impl std::fmt::Debug for Descriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Descriptor")
            .field("pid", &self.pid)
            .field("pipe_name", &self.pipe_name)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// What the platform knows about a process, to tell a live instance's
/// descriptor from a crashed one's.
pub trait ProcessProbe {
    /// The creation time of `pid`, or `None` when no such process exists.
    fn creation_time(&self, pid: u32) -> Option<u64>;
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Creates the directories `components` under `base`, private to the current
/// user from the moment they exist, refusing any that is a link or reparse
/// point. Returns the innermost directory.
///
/// Only directories this call creates, and the innermost one (which holds the
/// token), get the private permissions. An outer directory that already exists
/// (such as the application's own folder) is shared with other parts of the
/// application and is left as it is.
pub fn ensure_private_dir(base: &Path, components: &[&str]) -> io::Result<PathBuf> {
    let mut path = base.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if is_reparse(&metadata) || !metadata.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "descriptor directory is not a plain directory",
                    ));
                }
                if index + 1 == components.len() {
                    crate::private_fs::restrict_dir(&path)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                crate::private_fs::create_private_dir(&path)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(path)
}

/// Publishes `descriptor` as `<dir>/<pid>.json`: a private temporary file,
/// flushed to disk, then renamed over the final name.
pub fn publish(dir: &Path, descriptor: &Descriptor) -> io::Result<PathBuf> {
    let final_path = dir.join(Descriptor::file_name(descriptor.pid));
    let temp_path = dir.join(format!("{}.tmp", Descriptor::file_name(descriptor.pid)));
    // A leftover of an earlier run of this very pid number.
    match fs::symlink_metadata(&temp_path) {
        Ok(metadata) if is_reparse(&metadata) || !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor temporary path is not a plain file",
            ));
        }
        Ok(_) => fs::remove_file(&temp_path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bytes = serde_json::to_vec(descriptor).map_err(io::Error::other)?;
    let mut file = crate::private_fs::create_private_file(&temp_path)?;
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| fs::rename(&temp_path, &final_path)) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(final_path)
}

/// Removes a descriptor this process published.
pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Deletes descriptors (and temporary files) of processes that are gone, or
/// whose pid now belongs to a different process. Never touches `own_pid`.
pub fn sweep_stale(dir: &Path, own_pid: u32, probe: &dyn ProcessProbe) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let stem = name.strip_suffix(".tmp").unwrap_or(name);
        let Some(pid) = stem
            .strip_suffix(".json")
            .and_then(|digits| digits.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if is_reparse(&metadata) || !metadata.is_file() {
            continue;
        }
        let live = match fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Descriptor>(&bytes).ok())
        {
            Some(descriptor) if name.ends_with(".json") => probe
                .creation_time(pid)
                .is_some_and(|created| created.to_string() == descriptor.process_created),
            // A temporary or unreadable file of a dead process is garbage; of a
            // live one it is being written right now.
            _ => probe.creation_time(pid).is_some(),
        };
        if !live {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Probe(HashMap<u32, u64>);

    impl ProcessProbe for Probe {
        fn creation_time(&self, pid: u32) -> Option<u64> {
            self.0.get(&pid).copied()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hane-link-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn credentials() -> Credentials {
        Credentials::from_parts("0123456789abcdef0123456789abcdef", "tok")
    }

    #[test]
    fn publishes_a_descriptor_that_round_trips_and_removes_it() {
        let base = scratch("publish");
        let dir = ensure_private_dir(&base, &["Hane", "integration", "whisparrot"]).unwrap();
        let descriptor = Descriptor::new(4321, 99, &credentials());
        let path = publish(&dir, &descriptor).unwrap();
        assert_eq!(path.file_name().unwrap(), "4321.json");
        let read: Descriptor = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read, descriptor);
        assert_eq!(
            read.pipe_name,
            "Hane.Whisparrot.4321.0123456789abcdef0123456789abcdef"
        );
        assert_eq!(read.process_created, "99");
        // No temporary file is left behind.
        assert!(!dir.join("4321.json.tmp").exists());
        // Publishing again replaces it (a re-enable with a fresh token).
        let fresh = Descriptor::new(4321, 99, &Credentials::from_parts("other", "tok2"));
        publish(&dir, &fresh).unwrap();
        let read: Descriptor = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read.token, "tok2");
        remove(&path);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_and_file_are_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("modes");
        let dir = ensure_private_dir(&base, &["a", "b"]).unwrap();
        let path = publish(&dir, &Descriptor::new(1, 1, &credentials())).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(dir.parent().unwrap()), 0o700);
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_directory_chain_is_refused() {
        let base = scratch("links");
        let elsewhere = scratch("links-target");
        std::os::unix::fs::symlink(&elsewhere, base.join("Hane")).unwrap();
        assert!(ensure_private_dir(&base, &["Hane", "integration"]).is_err());
        assert!(!elsewhere.join("integration").exists());
        // A link in place of the temporary file is refused as well.
        let dir = ensure_private_dir(&base, &["real"]).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("victim"), dir.join("7.json.tmp")).unwrap();
        assert!(publish(&dir, &Descriptor::new(7, 1, &credentials())).is_err());
        assert!(!elsewhere.join("victim").exists());
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn sweep_removes_dead_and_reused_pids_and_keeps_live_and_own() {
        let base = scratch("sweep");
        let live = Descriptor::new(10, 111, &credentials());
        let reused = Descriptor::new(11, 222, &credentials());
        let dead = Descriptor::new(12, 333, &credentials());
        let own = Descriptor::new(13, 444, &credentials());
        for descriptor in [&live, &reused, &dead, &own] {
            publish(&base, descriptor).unwrap();
        }
        fs::write(base.join("14.json.tmp"), b"half").unwrap();
        fs::write(base.join("notes.txt"), b"keep").unwrap();
        // pid 11 is alive but is a different process than the descriptor's.
        let probe = Probe(HashMap::from([(10, 111), (11, 999), (13, 1)]));
        sweep_stale(&base, 13, &probe);
        assert!(base.join("10.json").exists());
        assert!(!base.join("11.json").exists());
        assert!(!base.join("12.json").exists());
        assert!(base.join("13.json").exists());
        assert!(!base.join("14.json.tmp").exists());
        assert!(base.join("notes.txt").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn debug_output_hides_the_token() {
        let descriptor = Descriptor::new(1, 1, &Credentials::from_parts("i", "supersecret"));
        assert!(!format!("{descriptor:?}").contains("supersecret"));
    }
}
