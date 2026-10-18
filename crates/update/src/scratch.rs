//! Lifecycle of the Windows install scratch directories (Issue #510, U4).
//!
//! Every Windows install mutation displaces its old file into a
//! `.hane-update-*` scratch directory next to the install file (same volume,
//! write-through move). Removing what is left there is best effort: another
//! process (an antivirus scanner, an indexer) can hold the displaced file for a
//! while. A bounded retry covers a transient sharing violation. When the
//! cleanup still fails, the exact scratch directory and the files the updater
//! put there are recorded in a *ledger* record bound to the installation and
//! the transaction. The ledger lives in the coordination directory, outside the
//! transaction directory, so retiring the transaction does not discard it. A
//! later safe startup retries the recorded cleanup.
//!
//! The recovery never matches the `.hane-update-` prefix against what happens
//! to be on disk. It removes only the files the record lists, beneath a
//! directory chain that is pinned from the install root and free of reparse
//! points, and it leaves every other entry (and so the directory) alone.

use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, read_limited};
use crate::identity::TransactionId;
use crate::paths::{validate_component, validate_rel_path};
use crate::pinned::PinnedDir;
use crate::platform::acl;
use crate::platform::lease::Coordination;
use crate::util::parse_strict;

/// Directory of the ledger records inside the coordination directory.
pub(crate) const LEDGER_DIR: &str = "scratch-cleanup";
/// Every scratch directory the updater creates starts with this.
pub(crate) const SCRATCH_PREFIX: &str = ".hane-update-";
const SCHEMA: u32 = 1;
const RECORD_LIMIT: u64 = 4 * 1024 * 1024;
const ATTEMPTS: u32 = 4;
const RETRY_STEP: Duration = Duration::from_millis(25);
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// One scratch directory with the files the updater left in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Leftover {
    /// Forward-slash path of the directory relative to the install root.
    pub dir: String,
    /// File names (single components) the updater put there.
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    install_id: String,
    transaction_id: String,
    leftovers: Vec<Leftover>,
}

/// Test seam: makes the next removals fail like a sharing violation.
#[cfg(test)]
pub(crate) mod probe {
    use std::cell::Cell;

    thread_local! {
        pub(crate) static FAILURES: Cell<u32> = const { Cell::new(0) };
    }
}

fn injected() -> io::Result<()> {
    #[cfg(test)]
    {
        let left = probe::FAILURES.with(std::cell::Cell::get);
        if left > 0 {
            probe::FAILURES.with(|cell| cell.set(left - 1));
            // ERROR_SHARING_VIOLATION
            return Err(io::Error::from_raw_os_error(32));
        }
    }
    Ok(())
}

fn remove_file_checked(path: &Path) -> io::Result<()> {
    injected()?;
    fs::remove_file(path)
}

fn remove_dir_checked(path: &Path) -> io::Result<()> {
    injected()?;
    fs::remove_dir(path)
}

/// Runs `op` a bounded number of times; an already absent target is success.
fn retry(mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match op() {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                attempt += 1;
                if attempt >= ATTEMPTS {
                    return Err(err);
                }
                std::thread::sleep(RETRY_STEP * attempt);
            }
        }
    }
}

fn is_reparse(meta: &fs::Metadata) -> bool {
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Removes the listed files from `dir` and then `dir` itself. Returns whether
/// the cleanup is settled: everything listed is gone and the directory is gone
/// or still holds entries the updater does not own (those are never touched).
/// `false` means a removal failed and should be retried later.
pub(crate) fn remove_owned(dir: &Path, files: &[String]) -> bool {
    match fs::symlink_metadata(dir) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return false,
        // A link or a non-directory is not the directory the updater made.
        Ok(meta) if is_reparse(&meta) || !meta.is_dir() => return true,
        Ok(_) => {}
    }
    for name in files {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return false,
            Ok(meta) if meta.is_file() && !is_reparse(&meta) => {
                if retry(|| remove_file_checked(&path)).is_err() {
                    return false;
                }
            }
            // Not a plain file: not something the updater left.
            Ok(_) => {}
        }
    }
    if retry(|| remove_dir_checked(dir)).is_ok() {
        return true;
    }
    match fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_some(),
        Err(err) => err.kind() == io::ErrorKind::NotFound,
    }
}

/// `scratch` relative to `root` as a forward-slash path.
pub(crate) fn rel_dir(root: &Path, scratch: &Path) -> Option<String> {
    let rel = scratch.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|part| part.as_os_str().to_str()).collect();
    let rel = parts?.join("/");
    validate_rel_path(&rel).ok()?;
    Some(rel)
}

fn ledger_dir(coordination: &Coordination) -> PathBuf {
    coordination.dir().join(LEDGER_DIR)
}

fn record_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    ledger_dir(coordination).join(format!("{}.json", transaction.as_str()))
}

/// Creates the ledger directory owner-only, or accepts an existing one only
/// when it is a plain directory that is ours.
fn ensure_ledger_dir(dir: &Path) -> Result<()> {
    match fs::create_dir(dir) {
        Ok(()) => {
            let secured = acl::secure_new(dir, true, 0o700).and_then(|()| acl::verify(dir, true));
            if secured.is_err() {
                let _ = fs::remove_dir(dir);
            }
            secured
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(dir)?;
            if is_reparse(&meta) || !meta.is_dir() {
                return Err(UpdateError::new(
                    ErrorCode::UnsupportedInstall,
                    format!("{} is not a plain directory", dir.display()),
                ));
            }
            acl::verify(dir, true)
        }
        Err(err) => Err(err.into()),
    }
}

fn write_record(path: &Path, record: &Record) -> Result<()> {
    let bytes = serde_json::to_vec(record)
        .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
    durable_write(path, &bytes)?;
    acl::secure_new(path, false, 0o600).and_then(|()| acl::verify(path, false))
}

/// The cleanup-pending record of one transaction, held by the file operations
/// of that transaction.
pub(crate) struct ScratchLedger {
    dir: PathBuf,
    path: PathBuf,
    install_id: String,
    transaction_id: String,
    leftovers: Vec<Leftover>,
}

impl ScratchLedger {
    pub(crate) fn new(coordination: &Coordination, transaction: &TransactionId) -> Self {
        Self {
            dir: ledger_dir(coordination),
            path: record_path(coordination, transaction),
            install_id: coordination.install_id().as_str().to_owned(),
            transaction_id: transaction.as_str().to_owned(),
            leftovers: Vec::new(),
        }
    }

    /// Durably records `leftover` before the caller reports the mutation as
    /// done. A failure is an error and nothing is remembered.
    pub(crate) fn track(&mut self, leftover: Leftover) -> Result<()> {
        ensure_ledger_dir(&self.dir)?;
        self.leftovers.push(leftover);
        let record = Record {
            schema: SCHEMA,
            install_id: self.install_id.clone(),
            transaction_id: self.transaction_id.clone(),
            leftovers: self.leftovers.clone(),
        };
        if let Err(err) = write_record(&self.path, &record) {
            self.leftovers.pop();
            return Err(err);
        }
        Ok(())
    }
}

enum Chain {
    Held(PinnedDir),
    Gone,
    /// A link or a non-directory: never followed.
    Refused,
    /// Could not be examined now; try again later.
    Transient,
}

fn pin_dir(path: &Path) -> Chain {
    match PinnedDir::open(path) {
        Ok(Some(pin)) => Chain::Held(pin),
        Ok(None) => Chain::Gone,
        Err(err) if err.code == ErrorCode::InstallModified => Chain::Refused,
        Err(_) => Chain::Transient,
    }
}

/// Retries one recorded cleanup. Returns whether the entry is settled (done,
/// gone, or not something that may be touched) and can leave the record.
fn settle(root: &Path, leftover: &Leftover) -> bool {
    let valid = validate_rel_path(&leftover.dir).is_ok()
        && leftover
            .dir
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(SCRATCH_PREFIX))
        && leftover.files.iter().all(|name| validate_component(name, name).is_ok());
    if !valid {
        return true;
    }
    let components: Vec<&str> = leftover.dir.split('/').collect();
    let mut pins = Vec::new();
    let mut path = root.to_path_buf();
    match pin_dir(&path) {
        Chain::Held(pin) => pins.push(pin),
        Chain::Refused => return true,
        Chain::Gone | Chain::Transient => return false,
    }
    for component in &components[..components.len() - 1] {
        path.push(component);
        match pin_dir(&path) {
            Chain::Held(pin) => pins.push(pin),
            Chain::Gone | Chain::Refused => return true,
            Chain::Transient => return false,
        }
    }
    path.push(components[components.len() - 1]);
    // The pinned ancestors cannot be renamed or replaced while they are held.
    let settled = remove_owned(&path, &leftover.files);
    drop(pins);
    settled
}

/// Retries every recorded cleanup of this installation. The caller must hold
/// the exclusive leases, so no helper or Hane process is alive. A record that
/// belongs to another installation, transaction or schema is left untouched.
pub(crate) fn cleanup_pending(coordination: &Coordination, install_root: &Path) {
    let dir = ledger_dir(coordination);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if !is_reparse(&meta) && meta.is_dir() => {}
        _ => return,
    }
    if acl::verify(&dir, true).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(transaction) = TransactionId::parse(stem) else {
            continue;
        };
        let path = entry.path();
        settle_record(coordination, &transaction, &path, install_root);
    }
}

fn settle_record(
    coordination: &Coordination,
    transaction: &TransactionId,
    path: &Path,
    install_root: &Path,
) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if is_reparse(&meta) || !meta.is_file() || acl::verify(path, false).is_err() {
        return;
    }
    let Ok(Some(bytes)) = read_limited(path, RECORD_LIMIT) else {
        return;
    };
    let Ok(mut record) = parse_strict::<Record>(&bytes) else {
        return;
    };
    if record.schema != SCHEMA
        || record.install_id != coordination.install_id().as_str()
        || record.transaction_id != transaction.as_str()
    {
        return;
    }
    let before = record.leftovers.len();
    record
        .leftovers
        .retain(|leftover| !settle(install_root, leftover));
    if record.leftovers.is_empty() {
        let _ = fs::remove_file(path);
    } else if record.leftovers.len() != before {
        let _ = write_record(path, &record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::InstallId;
    use crate::testing::TempDir;

    struct Fixture {
        _dir: TempDir,
        root: PathBuf,
        coordination: Coordination,
        transaction: TransactionId,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("scratch-ledger");
        let root = dir.path().join("install");
        fs::create_dir_all(root.join("sub")).unwrap();
        let install_id = InstallId::parse(&"0".repeat(32)).unwrap();
        let coordination = Coordination::create(dir.path(), &install_id).unwrap();
        Fixture {
            _dir: dir,
            root,
            coordination,
            transaction: TransactionId::generate().unwrap(),
        }
    }

    fn scratch_with(fx: &Fixture, rel: &str, files: &[&str]) -> Leftover {
        let dir = fx.root.join(rel);
        fs::create_dir_all(&dir).unwrap();
        for name in files {
            fs::write(dir.join(name), b"old").unwrap();
        }
        Leftover {
            dir: rel.to_owned(),
            files: files.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn fail_removals(count: u32) {
        probe::FAILURES.with(|cell| cell.set(count));
    }

    #[test]
    fn a_transient_sharing_violation_is_retried_and_leaves_nothing() {
        let fx = fixture();
        let leftover = scratch_with(&fx, "sub/.hane-update-1-a-0", &["removed.done"]);
        fail_removals(2);
        assert!(remove_owned(&fx.root.join(&leftover.dir), &leftover.files));
        assert!(!fx.root.join(&leftover.dir).exists());
    }

    #[test]
    fn a_terminal_failure_is_reported_and_keeps_the_files() {
        let fx = fixture();
        let leftover = scratch_with(&fx, "sub/.hane-update-1-a-1", &["removed.done"]);
        fail_removals(u32::MAX);
        assert!(!remove_owned(&fx.root.join(&leftover.dir), &leftover.files));
        fail_removals(0);
        assert!(fx.root.join(&leftover.dir).join("removed.done").exists());
    }

    #[test]
    fn a_tracked_leftover_is_durable_and_cleaned_up_later() {
        let fx = fixture();
        let leftover = scratch_with(&fx, "sub/.hane-update-1-a-2", &["a.dll.aside"]);
        let mut ledger = ScratchLedger::new(&fx.coordination, &fx.transaction);
        ledger.track(leftover.clone()).unwrap();
        let path = record_path(&fx.coordination, &fx.transaction);
        assert!(path.exists());

        // The record is outside the transaction directory, so retiring the
        // transaction does not discard it.
        let txn_dir = fx.coordination.transaction_dir(&fx.transaction);
        assert!(!path.starts_with(&txn_dir));

        cleanup_pending(&fx.coordination, &fx.root);
        assert!(!fx.root.join(&leftover.dir).exists());
        assert!(!path.exists(), "a settled record is removed");
    }

    #[test]
    fn a_failing_later_cleanup_keeps_the_record_for_the_next_start() {
        let fx = fixture();
        let leftover = scratch_with(&fx, "sub/.hane-update-1-a-3", &["a.dll.aside"]);
        let mut ledger = ScratchLedger::new(&fx.coordination, &fx.transaction);
        ledger.track(leftover.clone()).unwrap();
        let path = record_path(&fx.coordination, &fx.transaction);

        fail_removals(u32::MAX);
        cleanup_pending(&fx.coordination, &fx.root);
        fail_removals(0);
        assert!(path.exists());
        assert!(fx.root.join(&leftover.dir).join("a.dll.aside").exists());

        cleanup_pending(&fx.coordination, &fx.root);
        assert!(!path.exists());
        assert!(!fx.root.join(&leftover.dir).exists());
    }

    #[test]
    fn unknown_neighbours_and_unlisted_prefixed_directories_are_preserved() {
        let fx = fixture();
        let leftover = scratch_with(&fx, "sub/.hane-update-1-a-4", &["a.dll.aside"]);
        fs::write(fx.root.join(&leftover.dir).join("user.txt"), b"keep").unwrap();
        let stranger = scratch_with(&fx, "sub/.hane-update-9-z-9", &["removed.done"]);
        let mut ledger = ScratchLedger::new(&fx.coordination, &fx.transaction);
        ledger.track(leftover.clone()).unwrap();

        cleanup_pending(&fx.coordination, &fx.root);
        let dir = fx.root.join(&leftover.dir);
        assert!(!dir.join("a.dll.aside").exists(), "the listed file is removed");
        assert_eq!(fs::read(dir.join("user.txt")).unwrap(), b"keep");
        assert!(
            fx.root.join(&stranger.dir).join("removed.done").exists(),
            "a prefixed directory that no record lists is never swept"
        );
        assert!(
            !record_path(&fx.coordination, &fx.transaction).exists(),
            "the entry is settled once only unknown content remains"
        );
    }

    #[test]
    fn a_record_of_another_installation_or_transaction_is_ignored() {
        let fx = fixture();
        let leftover = scratch_with(&fx, "sub/.hane-update-1-a-5", &["a.dll.aside"]);
        let mut ledger = ScratchLedger::new(&fx.coordination, &fx.transaction);
        ledger.track(leftover.clone()).unwrap();
        let path = record_path(&fx.coordination, &fx.transaction);

        // The file name claims this transaction, the content another one.
        let other = TransactionId::generate().unwrap();
        let record = Record {
            schema: SCHEMA,
            install_id: fx.coordination.install_id().as_str().to_owned(),
            transaction_id: other.as_str().to_owned(),
            leftovers: vec![leftover.clone()],
        };
        write_record(&path, &record).unwrap();
        cleanup_pending(&fx.coordination, &fx.root);
        assert!(fx.root.join(&leftover.dir).join("a.dll.aside").exists());

        let record = Record {
            schema: SCHEMA,
            install_id: "f".repeat(32),
            transaction_id: fx.transaction.as_str().to_owned(),
            leftovers: vec![leftover.clone()],
        };
        write_record(&path, &record).unwrap();
        cleanup_pending(&fx.coordination, &fx.root);
        assert!(fx.root.join(&leftover.dir).join("a.dll.aside").exists());
    }

    #[test]
    fn a_recorded_path_that_escapes_or_is_not_a_scratch_directory_is_never_deleted() {
        let fx = fixture();
        fs::write(fx.root.join("hane.exe"), b"exe").unwrap();
        fs::write(fx.root.join("sub").join("user.txt"), b"keep").unwrap();
        let record = Record {
            schema: SCHEMA,
            install_id: fx.coordination.install_id().as_str().to_owned(),
            transaction_id: fx.transaction.as_str().to_owned(),
            leftovers: vec![
                Leftover { dir: "sub".to_owned(), files: vec!["user.txt".to_owned()] },
                Leftover { dir: "../install".to_owned(), files: vec!["hane.exe".to_owned()] },
                Leftover {
                    dir: "sub/.hane-update-1-a-6".to_owned(),
                    files: vec!["..\\..\\hane.exe".to_owned()],
                },
            ],
        };
        ensure_ledger_dir(&ledger_dir(&fx.coordination)).unwrap();
        write_record(&record_path(&fx.coordination, &fx.transaction), &record).unwrap();
        fs::create_dir_all(fx.root.join("sub/.hane-update-1-a-6")).unwrap();

        cleanup_pending(&fx.coordination, &fx.root);
        assert!(fx.root.join("hane.exe").exists());
        assert!(fx.root.join("sub").join("user.txt").exists());
    }

    #[test]
    fn a_junction_in_the_recorded_chain_is_not_followed() {
        let fx = fixture();
        let outside = fx.root.parent().unwrap().join("outside");
        fs::create_dir_all(outside.join(".hane-update-1-a-7")).unwrap();
        fs::write(outside.join(".hane-update-1-a-7").join("a.dll.aside"), b"keep").unwrap();
        let junction = fx.root.join("linked");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        assert!(status, "the junction could not be created");

        let mut ledger = ScratchLedger::new(&fx.coordination, &fx.transaction);
        ledger
            .track(Leftover {
                dir: "linked/.hane-update-1-a-7".to_owned(),
                files: vec!["a.dll.aside".to_owned()],
            })
            .unwrap();
        cleanup_pending(&fx.coordination, &fx.root);
        assert!(outside.join(".hane-update-1-a-7").join("a.dll.aside").exists());
        let _ = fs::remove_dir(&junction);
    }
}
