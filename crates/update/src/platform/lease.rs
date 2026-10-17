//! Process-lifetime leases in the coordination directory beside the install.
//!
//! A lease is an advisory OS file lock (`LockFileEx` on Windows, `flock` on
//! macOS) held by an open file handle. The OS releases it when the owning
//! process ends, however it ends, so "the lock is free" is evidence that no
//! process holds the handle any more. No PID file is consulted. The lock files
//! are opened without handle inheritance (the `std` default), so a spawned
//! child never keeps a parent's lease alive by accident.
//!
//! Order is fixed: `transaction` -> `startup` -> `runtime`. A request that
//! would go backwards while a later lease is held is refused instead of waiting.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::identity::{InstallId, TransactionId};

pub const TRANSACTION_LOCK: &str = "transaction.lock";
pub const STARTUP_LOCK: &str = "startup.lock";
pub const RUNTIME_LOCK: &str = "runtime.lock";
pub const PRIVATE_DIR: &str = "private";
pub const JOURNAL_FILE: &str = "journal.json";
pub const HELPER_DIR: &str = "helper";

const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeaseKind {
    Transaction,
    Startup,
    RuntimeShared,
    RuntimeExclusive,
}

impl LeaseKind {
    fn rank(self) -> u8 {
        match self {
            LeaseKind::Transaction => 0,
            LeaseKind::Startup => 1,
            LeaseKind::RuntimeShared | LeaseKind::RuntimeExclusive => 2,
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            LeaseKind::Transaction => TRANSACTION_LOCK,
            LeaseKind::Startup => STARTUP_LOCK,
            LeaseKind::RuntimeShared | LeaseKind::RuntimeExclusive => RUNTIME_LOCK,
        }
    }

    fn shared(self) -> bool {
        self == LeaseKind::RuntimeShared
    }
}

/// A held lease. Dropping it (or ending the process) releases the lock.
#[derive(Debug)]
pub struct Lease {
    kind: LeaseKind,
    file: File,
}

impl Lease {
    pub fn kind(&self) -> LeaseKind {
        self.kind
    }

    /// Releases the lease now instead of at drop.
    pub fn release(self) {
        let _ = self.file.unlock();
    }
}

/// The `.hane-update-<InstallId>` directory of one installation.
#[derive(Debug, Clone)]
pub struct Coordination {
    dir: PathBuf,
    install_id: InstallId,
}

fn refuse(detail: impl Into<String>) -> UpdateError {
    UpdateError::new(ErrorCode::UnsupportedInstall, detail)
}

/// Accepts only a real directory: a link or a file with the name is never adopted.
fn require_plain_dir(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(refuse(format!("{} is not a plain directory", path.display())));
    }
    Ok(())
}

#[cfg(unix)]
fn create_dir(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(mode).create(path)
}

#[cfg(not(unix))]
fn create_dir(path: &Path, _mode: u32) -> io::Result<()> {
    fs::create_dir(path)
}

fn ensure_dir(path: &Path, mode: u32) -> Result<()> {
    match create_dir(path, mode) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => require_plain_dir(path),
        Err(err) => Err(err.into()),
    }
}

impl Coordination {
    /// Opens (creating when needed) the coordination directory under `parent`.
    pub fn create(parent: &Path, install_id: &InstallId) -> Result<Self> {
        let dir = parent.join(install_id.coordination_dir_name());
        // Readable by other users' launches; only the owner can write.
        ensure_dir(&dir, 0o755)?;
        Ok(Self {
            dir,
            install_id: install_id.clone(),
        })
    }

    /// Opens an existing coordination directory; `None` when there is none.
    pub fn open_existing(parent: &Path, install_id: &InstallId) -> Result<Option<Self>> {
        let dir = parent.join(install_id.coordination_dir_name());
        match fs::symlink_metadata(&dir) {
            Ok(_) => {
                require_plain_dir(&dir)?;
                Ok(Some(Self {
                    dir,
                    install_id: install_id.clone(),
                }))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Derives the coordination directory from a helper copy's own location
    /// (`<coordination>/private/<TransactionId>/helper/<exe>`). The helper
    /// takes its expected identity from where it lives, not from arguments.
    pub fn from_helper_exe(exe: &Path) -> Result<(Self, TransactionId)> {
        let bad = || refuse("the helper is not running from its transaction directory");
        let helper_dir = exe.parent().ok_or_else(bad)?;
        if helper_dir.file_name().and_then(|n| n.to_str()) != Some(HELPER_DIR) {
            return Err(bad());
        }
        let txn_dir = helper_dir.parent().ok_or_else(bad)?;
        let txn_name = txn_dir.file_name().and_then(|n| n.to_str()).ok_or_else(bad)?;
        let transaction = TransactionId::parse(txn_name).map_err(|_| bad())?;
        let private = txn_dir.parent().ok_or_else(bad)?;
        if private.file_name().and_then(|n| n.to_str()) != Some(PRIVATE_DIR) {
            return Err(bad());
        }
        let dir = private.parent().ok_or_else(bad)?;
        let name = dir.file_name().and_then(|n| n.to_str()).ok_or_else(bad)?;
        let id = name.strip_prefix(".hane-update-").ok_or_else(bad)?;
        let install_id = InstallId::parse(id).map_err(|_| bad())?;
        require_plain_dir(dir)?;
        Ok((
            Self {
                dir: dir.to_path_buf(),
                install_id,
            },
            transaction,
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn install_id(&self) -> &InstallId {
        &self.install_id
    }

    /// `private/` exists with owner-only permission; created on first use.
    pub fn private_dir(&self) -> Result<PathBuf> {
        let private = self.dir.join(PRIVATE_DIR);
        ensure_dir(&private, 0o700)?;
        Ok(private)
    }

    pub fn transaction_dir(&self, transaction: &TransactionId) -> PathBuf {
        self.dir.join(PRIVATE_DIR).join(transaction.as_str())
    }

    /// `private/<TransactionId>/helper/`, owner-only, created on demand.
    pub fn ensure_helper_dir(&self, transaction: &TransactionId) -> Result<PathBuf> {
        self.private_dir()?;
        let txn_dir = self.transaction_dir(transaction);
        ensure_dir(&txn_dir, 0o700)?;
        let helper = txn_dir.join(HELPER_DIR);
        ensure_dir(&helper, 0o700)?;
        Ok(helper)
    }

    pub fn journal_path(&self, transaction: &TransactionId) -> PathBuf {
        self.transaction_dir(transaction).join(JOURNAL_FILE)
    }

    fn open_lock(&self, kind: LeaseKind) -> Result<File> {
        let path = self.dir.join(kind.file_name());
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                return Err(refuse(format!("{} is not a regular lock file", path.display())));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        match options.open(&path) {
            Ok(file) => Ok(file),
            // Another user's ordinary launch can read the lock file but not
            // write it; a read handle is enough to take the lock.
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                Ok(OpenOptions::new().read(true).open(&path)?)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn check_order(kind: LeaseKind, held: &[&Lease]) -> Result<()> {
        if held.iter().any(|lease| lease.kind.rank() >= kind.rank()) {
            return Err(UpdateError::new(
                ErrorCode::TransitionInvalid,
                format!("{kind:?} cannot be taken in this lease order"),
            ));
        }
        Ok(())
    }

    /// One non-blocking attempt. `Ok(None)` means another holder has it.
    pub fn try_acquire(&self, kind: LeaseKind, held: &[&Lease]) -> Result<Option<Lease>> {
        Self::check_order(kind, held)?;
        let file = self.open_lock(kind)?;
        let attempt = if kind.shared() {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match attempt {
            Ok(()) => Ok(Some(Lease { kind, file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err.into()),
        }
    }

    /// Polls until the lease is free or `timeout` has passed.
    pub fn acquire_within(
        &self,
        kind: LeaseKind,
        held: &[&Lease],
        timeout: Duration,
    ) -> Result<Lease> {
        let started = Instant::now();
        loop {
            if let Some(lease) = self.try_acquire(kind, held)? {
                return Ok(lease);
            }
            if started.elapsed() >= timeout {
                return Err(UpdateError::new(
                    ErrorCode::Timeout,
                    format!("{kind:?} lease is held by another process"),
                ));
            }
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};

    use super::*;
    use crate::identity::InstallFacts;
    use crate::testing::TempDir;

    const CHILD_ENV: &str = "HANE_UPDATE_LEASE_TEST_PARENT";
    const CHILD_TEST: &str = "platform::lease::tests::lease_holder_child";

    fn install_id() -> InstallId {
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap()
    }

    fn coordination(dir: &TempDir) -> Coordination {
        Coordination::create(dir.path(), &install_id()).unwrap()
    }

    #[test]
    fn exclusive_leases_do_not_split_across_opens_of_the_same_install() {
        let dir = TempDir::new("lease-excl");
        let first = coordination(&dir);
        let second = Coordination::create(dir.path(), &install_id()).unwrap();
        let held = first
            .try_acquire(LeaseKind::Transaction, &[])
            .unwrap()
            .expect("first holder");
        assert!(second.try_acquire(LeaseKind::Transaction, &[]).unwrap().is_none());
        held.release();
        assert!(second.try_acquire(LeaseKind::Transaction, &[]).unwrap().is_some());
    }

    #[test]
    fn shared_runtime_leases_coexist_but_block_the_exclusive_one() {
        let dir = TempDir::new("lease-shared");
        let coord = coordination(&dir);
        let a = coord.try_acquire(LeaseKind::RuntimeShared, &[]).unwrap().unwrap();
        let b = coord.try_acquire(LeaseKind::RuntimeShared, &[]).unwrap().unwrap();
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_none()
        );
        drop(a);
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_none()
        );
        drop(b);
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_second_startup_gate_waits_and_times_out() {
        let dir = TempDir::new("lease-startup");
        let coord = coordination(&dir);
        let _gate = coord.try_acquire(LeaseKind::Startup, &[]).unwrap().unwrap();
        let err = coord
            .acquire_within(LeaseKind::Startup, &[], Duration::from_millis(60))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Timeout);
    }

    #[test]
    fn the_lease_order_cannot_go_backwards() {
        let dir = TempDir::new("lease-order");
        let coord = coordination(&dir);
        let runtime = coord.try_acquire(LeaseKind::RuntimeShared, &[]).unwrap().unwrap();
        for kind in [LeaseKind::Transaction, LeaseKind::Startup] {
            let err = coord.try_acquire(kind, &[&runtime]).unwrap_err();
            assert_eq!(err.code, ErrorCode::TransitionInvalid);
        }
        let txn = coord.try_acquire(LeaseKind::Transaction, &[]).unwrap().unwrap();
        let startup = coord.try_acquire(LeaseKind::Startup, &[&txn]).unwrap().unwrap();
        assert!(coord.try_acquire(LeaseKind::Startup, &[&startup]).is_err());
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[&txn, &startup])
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_linked_or_foreign_coordination_entry_is_not_adopted() {
        let dir = TempDir::new("lease-foreign");
        let id = install_id();
        std::fs::write(dir.path().join(id.coordination_dir_name()), b"file").unwrap();
        let err = Coordination::create(dir.path(), &id).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedInstall);
        assert!(Coordination::open_existing(dir.path(), &id).is_err());

        let other = TempDir::new("lease-foreign-lock");
        let coord = coordination(&other);
        std::fs::create_dir(coord.dir().join(RUNTIME_LOCK)).unwrap();
        let err = coord.try_acquire(LeaseKind::RuntimeShared, &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedInstall);
    }

    #[test]
    fn a_helper_takes_its_identity_from_its_own_location() {
        let dir = TempDir::new("lease-helper-path");
        let coord = coordination(&dir);
        let txn = TransactionId::generate().unwrap();
        let exe = coord
            .transaction_dir(&txn)
            .join(HELPER_DIR)
            .join("hane-updater");
        let (derived, derived_txn) = Coordination::from_helper_exe(&exe).unwrap();
        assert_eq!(derived.install_id(), &install_id());
        assert_eq!(derived_txn, txn);

        let elsewhere = dir.path().join("hane-updater");
        assert!(Coordination::from_helper_exe(&elsewhere).is_err());
        let wrong_dir = coord.dir().join(PRIVATE_DIR).join("not-a-txn").join(HELPER_DIR).join("x");
        assert!(Coordination::from_helper_exe(&wrong_dir).is_err());
    }

    /// Runs only as the re-executed child of the real-process test below.
    #[test]
    fn lease_holder_child() {
        let Some(parent) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        let coord = Coordination::create(Path::new(&parent), &install_id()).unwrap();
        let _lease = coord
            .try_acquire(LeaseKind::RuntimeShared, &[])
            .unwrap()
            .expect("child lease");
        println!("HELD");
        let _ = io::stdout().flush();
        let mut sink = Vec::new();
        let _ = io::stdin().read_to_end(&mut sink);
    }

    #[test]
    fn a_real_process_holds_the_lease_until_it_ends() {
        let dir = TempDir::new("lease-process");
        let coord = coordination(&dir);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        loop {
            let line = lines.next().expect("child output").unwrap();
            if line.trim() == "HELD" {
                break;
            }
        }
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_none(),
            "a live process must keep its lease"
        );
        drop(child.stdin.take());
        child.wait().unwrap();
        coord
            .acquire_within(LeaseKind::RuntimeExclusive, &[], Duration::from_secs(5))
            .expect("the OS releases the lease when the process ends");
    }
}
