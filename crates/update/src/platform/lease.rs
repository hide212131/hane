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
use crate::platform::acl;

pub const TRANSACTION_LOCK: &str = "transaction.lock";
pub const STARTUP_LOCK: &str = "startup.lock";
pub const RUNTIME_LOCK: &str = "runtime.lock";
pub const PRIVATE_DIR: &str = "private";
pub const JOURNAL_FILE: &str = "journal.json";
pub const HELPER_DIR: &str = "helper";
pub const RECEIPTS_DIR: &str = "receipts";
pub const SEEN_CATALOG_FILE: &str = "seen-catalog.json";
/// Per-install home of the external bundle inventories (macOS layout 2), one
/// `<version>-<source_sha>/` generation per installed or staged release.
pub const INVENTORIES_DIR: &str = "inventories";

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

/// Accepts only a real directory owned by the current principal and not
/// writable by anyone else: a link, a file, or a directory another user
/// created first is never adopted.
fn require_plain_dir(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(refuse(format!("{} is not a plain directory", path.display())));
    }
    acl::verify(path, true)
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
        Ok(()) => {
            // Restrict first, then confirm; a directory that cannot be made
            // private is removed so it is not left for a later adoption.
            let secured = acl::secure_new(path, true, mode).and_then(|()| acl::verify(path, true));
            if secured.is_err() {
                let _ = fs::remove_dir(path);
            }
            secured
        }
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
        // The whole chain from the root to the running copy must be ours and
        // closed to other writers before the helper trusts its own location.
        for checked in [dir, private, txn_dir, helper_dir] {
            require_plain_dir(checked)?;
        }
        acl::verify(exe, false)?;
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

    /// Receipt directory of an install under `parent`. The application and the
    /// helper both derive the sequence floor's location from this.
    pub fn receipts_dir_for(parent: &Path, install_id: &InstallId) -> PathBuf {
        parent
            .join(install_id.coordination_dir_name())
            .join(RECEIPTS_DIR)
    }

    /// Where the sidecar inventories of an install under `parent` live. Both the
    /// application and the helper derive it from the placement alone; the
    /// directory may not exist yet.
    pub fn inventories_dir_for(parent: &Path, install_id: &InstallId) -> PathBuf {
        parent
            .join(install_id.coordination_dir_name())
            .join(INVENTORIES_DIR)
    }

    pub fn inventories_dir(&self) -> PathBuf {
        self.dir.join(INVENTORIES_DIR)
    }

    /// `inventories/` below the verified coordination directory, created on
    /// first use. Readable by every launch, writable only by the owner.
    pub fn ensure_inventories_dir(&self) -> Result<PathBuf> {
        require_plain_dir(&self.dir)?;
        let dir = self.inventories_dir();
        ensure_dir(&dir, 0o755)?;
        Ok(dir)
    }

    /// The durable highest-seen catalog receipt of this placement.
    pub fn seen_catalog_path(&self) -> PathBuf {
        self.dir.join(RECEIPTS_DIR).join(SEEN_CATALOG_FILE)
    }

    pub fn install_id(&self) -> &InstallId {
        &self.install_id
    }

    /// `private/` exists with owner-only permission; created on first use.
    pub fn private_dir(&self) -> Result<PathBuf> {
        require_plain_dir(&self.dir)?;
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

    /// Removes the transaction directory of `transaction` when it is certain
    /// that no journal was ever written for it. The chain from the root to the
    /// directory must be plain, owned and closed to other writers; anything
    /// that cannot be verified, or any trace of a journal (even an unreadable
    /// one), leaves the directory untouched. Returns whether it was removed.
    pub fn remove_unjournaled_transaction(&self, transaction: &TransactionId) -> Result<bool> {
        let private = self.dir.join(PRIVATE_DIR);
        let txn_dir = self.transaction_dir(transaction);
        for checked in [&self.dir, &private, &txn_dir] {
            require_plain_dir(checked)?;
        }
        match fs::symlink_metadata(self.journal_path(transaction)) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            _ => return Ok(false),
        }
        // `remove_dir_all` unlinks links inside instead of following them.
        fs::remove_dir_all(&txn_dir)?;
        Ok(true)
    }

    /// Removes the retirement tombstone `.<TransactionId>.retired` of a terminal
    /// receipt whose cleanup failed after the durable rename (Windows). Only a
    /// plain directory of that exact name qualifies, below a verified root and
    /// private directory; links, files and other names stay. Returns whether
    /// it was removed.
    pub fn remove_retired_receipt(&self, entry_name: &str) -> Result<bool> {
        let Some(id) = entry_name
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(".retired"))
        else {
            return Ok(false);
        };
        if TransactionId::parse(id).is_err() {
            return Ok(false);
        }
        let private = self.dir.join(PRIVATE_DIR);
        for checked in [&self.dir, &private] {
            require_plain_dir(checked)?;
        }
        let tombstone = private.join(entry_name);
        let meta = match fs::symlink_metadata(&tombstone) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Ok(false);
        }
        require_plain_dir(&tombstone)?;
        // `remove_dir_all` unlinks links inside instead of following them.
        fs::remove_dir_all(&tombstone)?;
        Ok(true)
    }

    fn open_lock(&self, kind: LeaseKind) -> Result<File> {
        let path = self.dir.join(kind.file_name());
        // The root is re-checked so a directory swapped after `create` is not trusted.
        require_plain_dir(&self.dir)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        match options.open(&path) {
            Ok(file) => {
                match acl::secure_new(&path, false, 0o644).and_then(|()| acl::verify(&path, false)) {
                    Ok(()) => Ok(file),
                    Err(err) => {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        Err(err)
                    }
                }
            }
            // An existing lock file is used only when it is a plain file of ours
            // that nobody else can write. There is no read-only fallback: a
            // lock this principal cannot write is not shared with.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                acl::verify(&path, false)?;
                Ok(OpenOptions::new().read(true).write(true).open(&path)?)
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

    fn staged(coord: &Coordination) -> TransactionId {
        let transaction = TransactionId::generate().unwrap();
        let helper = coord.ensure_helper_dir(&transaction).unwrap();
        fs::write(helper.join("staged.bin"), b"staged data").unwrap();
        fs::write(coord.transaction_dir(&transaction).join("candidate.bin"), b"candidate").unwrap();
        transaction
    }

    #[test]
    fn an_unjournaled_transaction_is_removed_with_its_staged_data_only() {
        let dir = TempDir::new("lease-unjournaled");
        let coord = coordination(&dir);
        let aborted = staged(&coord);
        let other = staged(&coord);
        assert!(coord.remove_unjournaled_transaction(&aborted).unwrap());
        assert!(!coord.transaction_dir(&aborted).exists());
        assert!(coord.transaction_dir(&other).join(HELPER_DIR).join("staged.bin").exists());
        assert!(coord.private_dir().unwrap().exists());
    }

    #[test]
    fn a_transaction_with_any_journal_trace_is_kept() {
        let dir = TempDir::new("lease-journaled");
        let coord = coordination(&dir);
        let journaled = staged(&coord);
        fs::write(coord.journal_path(&journaled), b"{ not a journal").unwrap();
        assert!(!coord.remove_unjournaled_transaction(&journaled).unwrap());
        assert!(coord.journal_path(&journaled).exists());
        assert!(coord.transaction_dir(&journaled).join(HELPER_DIR).join("staged.bin").exists());
        // A journal path that is not a plain file is ambiguous and kept too.
        let odd = staged(&coord);
        fs::create_dir(coord.journal_path(&odd)).unwrap();
        assert!(!coord.remove_unjournaled_transaction(&odd).unwrap());
        assert!(coord.transaction_dir(&odd).exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_unverifiable_transaction_directory_is_never_followed() {
        let dir = TempDir::new("lease-symlink");
        let coord = coordination(&dir);
        let target = TempDir::new("lease-symlink-target");
        fs::write(target.path().join("keep.txt"), b"keep").unwrap();
        let transaction = TransactionId::generate().unwrap();
        coord.private_dir().unwrap();
        std::os::unix::fs::symlink(target.path(), coord.transaction_dir(&transaction)).unwrap();
        assert!(coord.remove_unjournaled_transaction(&transaction).is_err());
        assert!(target.path().join("keep.txt").exists());
        // A transaction that was never staged is simply not there to remove.
        let missing = TransactionId::generate().unwrap();
        assert!(coord.remove_unjournaled_transaction(&missing).is_err());
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
        drop(runtime);
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
        // The error class for creating a directory over a regular file differs by OS
        // (`UnsupportedInstall` or `Io`), so only the refusal and non-mutation are pinned.
        let entry = dir.path().join(id.coordination_dir_name());
        assert!(Coordination::create(dir.path(), &id).is_err());
        assert!(Coordination::open_existing(dir.path(), &id).is_err());
        assert!(entry.is_file());
        assert_eq!(std::fs::read(&entry).unwrap(), b"file");

        let other = TempDir::new("lease-foreign-lock");
        let coord = coordination(&other);
        let foreign_lock = coord.dir().join(RUNTIME_LOCK);
        std::fs::create_dir(&foreign_lock).unwrap();
        // Windows reports the refused `create_new` as `Io`, so the error class is not pinned.
        assert!(coord.try_acquire(LeaseKind::RuntimeShared, &[]).is_err());
        assert!(foreign_lock.is_dir());
        assert_eq!(std::fs::read_dir(&foreign_lock).unwrap().count(), 0);
    }

    #[test]
    fn a_helper_takes_its_identity_from_its_own_location() {
        let dir = TempDir::new("lease-helper-path");
        let coord = coordination(&dir);
        let txn = TransactionId::generate().unwrap();
        let exe = coord.ensure_helper_dir(&txn).unwrap().join("hane-updater");
        std::fs::write(&exe, b"helper").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let (derived, derived_txn) = Coordination::from_helper_exe(&exe).unwrap();
        assert_eq!(derived.install_id(), &install_id());
        assert_eq!(derived_txn, txn);

        let elsewhere = dir.path().join("hane-updater");
        assert!(Coordination::from_helper_exe(&elsewhere).is_err());
        let wrong_dir = coord.dir().join(PRIVATE_DIR).join("not-a-txn").join(HELPER_DIR).join("x");
        assert!(Coordination::from_helper_exe(&wrong_dir).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_entry_open_to_other_writers_is_not_adopted() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let dir = TempDir::new("lease-open-root");
        let id = install_id();
        let root = dir.path().join(id.coordination_dir_name());
        std::fs::create_dir(&root).unwrap();
        mode(&root, 0o777);
        assert_eq!(
            Coordination::create(dir.path(), &id).unwrap_err().code,
            ErrorCode::UnsupportedInstall
        );
        assert!(Coordination::open_existing(dir.path(), &id).is_err());

        let other = TempDir::new("lease-open-private");
        let coord = coordination(&other);
        let private = coord.private_dir().unwrap();
        mode(&private, 0o770);
        assert!(coord.private_dir().is_err());
        assert!(coord.ensure_helper_dir(&TransactionId::generate().unwrap()).is_err());
        mode(&private, 0o700);

        let lock = coord.dir().join(RUNTIME_LOCK);
        std::fs::write(&lock, b"").unwrap();
        mode(&lock, 0o666);
        let err = coord.try_acquire(LeaseKind::RuntimeShared, &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedInstall);
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
        // The test harness may have written its own prefix on this line.
        println!("\nHELD");
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
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.trim() == "HELD" {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        if rx.recv_timeout(Duration::from_secs(30)).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the child never reported that it holds the lease");
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
