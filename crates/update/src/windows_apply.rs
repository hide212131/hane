//! Windows owned-file apply path of the helper (Issue #510, U4 increment 2).
//!
//! Everything here is OS-neutral file work so it can be tested anywhere; the
//! helper reaches it only for a verified Windows x64 placement, after the
//! parent process is gone and the exclusive `runtime` lease is held.
//!
//! The helper accepts no path, command or environment. It reads only
//! `Coordination::transaction_dir(<its own transaction>)/candidate/` (the
//! application copies the Ready candidate there before it quits, see
//! [`stage_candidate`]) and derives everything else itself:
//!
//! 1. the current install and its signature-verified inventory,
//! 2. the candidate catalog and signature, verified again against the trust
//!    policy and bound to the requested candidate id,
//! 3. the candidate archive, hashed against the catalog, inspected and
//!    extracted into the transaction's own `stage/`,
//! 4. an [`ApplyPlan`] built from the two signed inventories, whose
//!    preconditions are checked against the disk.
//!
//! Only then is the journal created and the journal-first engine of
//! [`crate::apply`] run. The engine writes each intent durably before touching
//! a file and replaces `hane.exe` last.
//!
//! After the files are replaced, [`apply_and_settle`] starts the limited-health
//! child through a [`HealthLauncher`] (see [`crate::health`]) and reports an
//! applied update only once the journal is durably `Committed`; every other end
//! has restored the old version or stopped as `RecoveryRequired`.
//!
//! Not here: the macOS bundle swap, which lives in `macos_apply` and is never
//! reached from this path.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::apply::{self, ApplyOutcome, FsFileOps, HealthChild, HealthOutcome, ParentProcess};
use crate::archive::{ArchiveLimits, stage_archive};
use crate::catalog::{VerifyContext, verify_catalog};
use crate::clock::Clock;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{FileSeenStore, hash_regular_file, read_limited};
use crate::health::{HealthLauncher, LaunchFailure, LaunchRequest, Settled, conclude_with_child};
use crate::identity::{ApplyPlan, SignedInventory, TransactionId};
use crate::journal::{
    DurableJournal, FileJournalStore, FsDiskView, Phase, UpdateJournal, check_plan_preconditions,
};
use crate::limits::{
    JOURNAL_RESERVE_BYTES, MAX_ARCHIVE_BYTES, MAX_CATALOG_BYTES, MAX_INVENTORY_BYTES,
    MAX_SIGNATURE_BYTES,
};
use crate::model::{INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME, Target, UPDATER_PROTOCOL};
use crate::platform::Placement;
use crate::platform::lease::Coordination;
use crate::trust::{SeenCatalog, SeenStore, TrustPolicy};
use crate::util::is_sha256_hex;

/// Directory of the transaction that holds the copied candidate.
pub const CANDIDATE_DIR: &str = "candidate";
pub const CANDIDATE_CATALOG: &str = "catalog.json";
pub const CANDIDATE_SIGNATURE: &str = "catalog.sig";
pub const CANDIDATE_ARCHIVE: &str = "archive.zip";
pub const CANDIDATE_SEEN_SOURCE: &str = "seen-source";
const CHILD_EXE: &str = "hane.exe";
const STAGE_DIR: &str = "stage";
const BACKUP_DIR: &str = "backup";

/// A candidate id is the SHA-256 hex of the signed catalog and target.
pub fn valid_candidate_id(id: &str) -> bool {
    is_sha256_hex(id)
}

/// The three files of a Ready candidate, as the application stores them.
pub struct CandidateSource<'a> {
    pub catalog: &'a Path,
    pub signature: &'a Path,
    pub archive: &'a Path,
    /// The sequence floor the application persisted before it made the
    /// candidate Ready, kept outside the install's coordination directory (a
    /// protected install cannot be written before the user consents). When
    /// given, [`stage_candidate`] carries it into the coordination receipts,
    /// which is where the helper re-reads it.
    pub seen: Option<&'a Path>,
}

/// Carries the application's sequence floor into the coordination receipts.
/// The floor never goes down: a higher stored floor stays, and the same
/// sequence with different catalog bytes is a conflict. A missing or invalid
/// source is a refusal, never "no floor".
fn seed_seen_receipt(coordination: &Coordination, source: &Path) -> Result<()> {
    let unwritable = |what: &str, err: UpdateError| {
        UpdateError::new(
            ErrorCode::ReceiptUnwritable,
            format!("the catalog sequence receipt {what}: {err}"),
        )
    };
    let carried = FileSeenStore::new(source.to_path_buf())
        .load()
        .map_err(|err| unwritable("cannot be read", err))?
        .ok_or_else(|| {
            UpdateError::new(ErrorCode::NotFound, "the application's catalog sequence receipt is missing")
        })?;
    carried.validate()?;
    let path = coordination.seen_catalog_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| unwritable("cannot be created", UpdateError::from(err)))?;
    }
    let mut store = FileSeenStore::new(path);
    let _guard = store.lock().map_err(|err| unwritable("cannot be locked", err))?;
    if let Some(existing) = store.load().map_err(|err| unwritable("cannot be read", err))? {
        existing.validate()?;
        if existing.sequence > carried.sequence {
            return Ok(());
        }
        if existing.sequence == carried.sequence {
            if existing.catalog_sha256 != carried.catalog_sha256 {
                return Err(UpdateError::new(
                    ErrorCode::SequenceConflict,
                    "same catalog sequence with different bytes",
                ));
            }
            return Ok(());
        }
    }
    store
        .store(&SeenCatalog::new(carried.sequence, carried.catalog_sha256))
        .map_err(|err| unwritable("cannot be stored", err))
}

/// Records where the application keeps its own sequence receipt, so the helper
/// can re-read it under the runtime lease. The helper only ever reads it, and
/// only ever raises its own floor from it (a refusal), never lowers anything.
fn record_seen_source(candidate_dir: &Path, source: &Path) -> Result<()> {
    let text = source.to_str().ok_or_else(|| {
        UpdateError::new(ErrorCode::ReceiptUnwritable, "the sequence receipt path is not valid text")
    })?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(candidate_dir.join(CANDIDATE_SEEN_SOURCE))?;
    io::Write::write_all(&mut output, text.as_bytes())?;
    output.sync_all()?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Stands in for the OS-derived state root in tests.
    static TRUSTED_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Fixes the state root the helper trusts in a test.
#[cfg(test)]
fn set_trusted_state_root(root: Option<PathBuf>) {
    TRUSTED_ROOT.with(|slot| *slot.borrow_mut() = root);
}

/// The one state root this helper may derive the application's receipt from:
/// the default state directory of the helper's own account, taken from the OS
/// (known folder of the process token). Nothing the requesting side can write
/// (candidate directory, `state-root.binding`, environment) takes part. A
/// custom `HANE_STATE_DIR` therefore cannot be verified here and is refused
/// rather than replaced by another profile's directory.
fn trusted_state_root() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TRUSTED_ROOT.with(|slot| slot.borrow().clone()) {
        return Ok(root);
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::own_local_app_data().map(|dir| dir.join("Hane"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "the trusted state root of the account is not available on this platform",
        ))
    }
}

fn receipt_io(err: io::Error, what: &str) -> UpdateError {
    if err.kind() == io::ErrorKind::NotFound {
        UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"))
    } else {
        UpdateError::new(ErrorCode::ReceiptUnwritable, format!("{what} cannot be opened: {err}"))
    }
}

/// Opens without following a final link. Directories are opened with a share
/// mode that forbids rename and delete on Windows, so a pinned directory (and
/// with it every parent) cannot be replaced while the handle lives.
fn no_follow(options: &mut OpenOptions, directory: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut flags = libc::O_NOFOLLOW | libc::O_CLOEXEC;
        if directory {
            flags |= libc::O_DIRECTORY;
        }
        options.custom_flags(flags);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_SHARE_READ: u32 = 1;
        if directory {
            options
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .share_mode(FILE_SHARE_READ);
        } else {
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (options, directory);
}

/// The opened object must be the entry the path names now. Windows relies on
/// the pinned parents (no rename or replace while they are open); POSIX also
/// compares device and inode.
#[cfg(unix)]
fn same_object(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the application's receipt location changed while it was being checked",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn same_object(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

/// Opens a plain owned directory and keeps the handle: the object that was
/// checked is the one that stays in place.
fn pin_dir(path: &Path, what: &str) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options, true);
    let dir = options.open(path).map_err(|err| receipt_io(err, what))?;
    if !dir.metadata()?.is_dir() {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, format!("{what} is not a plain directory")));
    }
    crate::platform::acl::verify(path, true)?;
    same_object(&dir, path)?;
    Ok(dir)
}

/// Opens an existing plain owned file for the life of the handle; nothing is
/// created.
fn pin_file(path: &Path, what: &str, write: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    no_follow(&mut options, false);
    let file = options.open(path).map_err(|err| receipt_io(err, what))?;
    if !file.metadata()?.is_file() {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, format!("{what} is not a plain file")));
    }
    crate::platform::acl::verify(path, false)?;
    same_object(&file, path)?;
    Ok(file)
}

/// The application's receipt as the helper holds it: the directories down to
/// it stay pinned, its lock file is locked through the handle that was
/// checked, and the receipt is read through the handle that was checked. Drop
/// releases everything.
struct PinnedReceipt {
    receipt: File,
    _lock: File,
    _dirs: Vec<File>,
}

/// Helper side: the application's receipt location derived without trusting
/// the candidate directory or the binding: `<state root>/updates/<install
/// id>/receipts/` below the account's own trusted state root.
fn expected_user_receipt(coordination: &Coordination) -> Result<(PathBuf, PathBuf)> {
    let root = trusted_state_root()?;
    if !root.is_absolute() {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "the trusted state root is not absolute"));
    }
    let receipts = root.join("updates").join(coordination.install_id().as_str()).join("receipts");
    let receipt = receipts.join(crate::platform::lease::SEEN_CATALOG_FILE);
    Ok((root, receipt))
}

/// Pins the chain from the trusted root down to the receipt (every directory
/// must be plain, owned and closed to other writers; the lock file and the
/// receipt must already exist) and takes the lock. A parent or file replaced
/// by a link or another object is refused, never followed.
fn open_pinned_receipt(root: &Path, receipt: &Path) -> Result<PinnedReceipt> {
    let receipts = receipt
        .parent()
        .ok_or_else(|| UpdateError::new(ErrorCode::IdentityMismatch, "malformed receipt location"))?;
    let install = receipts.parent().ok_or_else(|| UpdateError::new(ErrorCode::IdentityMismatch, "malformed receipt location"))?;
    let updates = install.parent().ok_or_else(|| UpdateError::new(ErrorCode::IdentityMismatch, "malformed receipt location"))?;
    let mut dirs = Vec::with_capacity(4);
    for dir in [root, updates, install, receipts] {
        dirs.push(pin_dir(dir, "the application's receipt directory")?);
    }
    let lock = pin_file(&receipt.with_extension("lock"), "the catalog sequence receipt lock", true)?;
    lock.lock().map_err(|err| {
        UpdateError::new(
            ErrorCode::ReceiptUnwritable,
            format!("the catalog sequence receipt cannot be locked: {err}"),
        )
    })?;
    let file = pin_file(receipt, "the application's catalog sequence receipt", false)?;
    Ok(PinnedReceipt { receipt: file, _lock: lock, _dirs: dirs })
}

fn read_pinned_receipt(pinned: &PinnedReceipt) -> Result<SeenCatalog> {
    let mut data = Vec::new();
    (&pinned.receipt)
        .take(crate::limits::MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|err| {
            UpdateError::new(
                ErrorCode::ReceiptUnwritable,
                format!("the catalog sequence receipt cannot be read: {err}"),
            )
        })?;
    if data.len() as u64 > crate::limits::MAX_JOURNAL_BYTES {
        return Err(UpdateError::new(ErrorCode::LimitExceeded, "the catalog sequence receipt is too large"));
    }
    let seen: SeenCatalog = crate::util::parse_strict(&data)?;
    seen.validate()?;
    Ok(seen)
}

/// Helper side: re-reads the application's receipt now and returns the
/// effective floor, the higher of it and `coordination_floor`. The returned
/// guard holds the receipt's cross-process lock; the caller keeps it until the
/// first install mutation, so no service can record a higher sequence between
/// this check and the mutation. A candidate that declared a receipt but whose
/// receipt is missing, corrupt, unlockable or in conflict is refused.
fn reconfirm_user_floor(
    coordination: &Coordination,
    candidate_dir: &Path,
    coordination_floor: SeenCatalog,
) -> Result<(SeenCatalog, Option<Box<dyn Send>>)> {
    let Some(raw) = read_limited(&candidate_dir.join(CANDIDATE_SEEN_SOURCE), 4096)? else {
        return Ok((coordination_floor, None));
    };
    let declared = String::from_utf8(raw)
        .map(PathBuf::from)
        .map_err(|_| UpdateError::new(ErrorCode::ReceiptUnwritable, "the sequence receipt path is not valid text"))?;
    // The candidate directory is user-writable, so the declared path is only a
    // claim. The one location this (possibly elevated) helper opens or locks is
    // derived from the OS profile of its own account; anything else is refused
    // before any path is opened.
    let (root, path) = expected_user_receipt(coordination)?;
    if declared != path {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the declared sequence receipt is not the application's receipt location",
        ));
    }
    let pinned = open_pinned_receipt(&root, &path)?;
    let user = read_pinned_receipt(&pinned)?;
    let guard: Box<dyn Send> = Box::new(pinned);
    let effective = if user.sequence > coordination_floor.sequence {
        user
    } else if user.sequence == coordination_floor.sequence {
        if user.catalog_sha256 != coordination_floor.catalog_sha256 {
            return Err(UpdateError::new(
                ErrorCode::SequenceConflict,
                "same catalog sequence with different bytes",
            ));
        }
        coordination_floor
    } else {
        coordination_floor
    };
    Ok((effective, Some(guard)))
}

fn copy_regular(source: &Path, dest: &Path, limit: u64) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(UpdateError::new(
            ErrorCode::AssetMismatch,
            "a candidate file is not a regular file",
        ));
    }
    if meta.len() > limit {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "a candidate file exceeds its size limit",
        ));
    }
    let mut input = File::open(source)?.take(limit + 1);
    let mut output = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let copied = io::copy(&mut input, &mut output)?;
    if copied > limit {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "a candidate file exceeds its size limit",
        ));
    }
    output.sync_all()?;
    Ok(())
}

/// Application side: copies the Ready candidate into the helper's transaction
/// directory so the helper never needs a path from the application. The helper
/// verifies the copy itself; nothing here is trusted by it.
pub fn stage_candidate(
    coordination: &Coordination,
    transaction: &TransactionId,
    source: &CandidateSource<'_>,
) -> Result<PathBuf> {
    let dir = coordination.transaction_dir(transaction).join(CANDIDATE_DIR);
    fs::create_dir(&dir)?;
    let copied = copy_regular(source.catalog, &dir.join(CANDIDATE_CATALOG), MAX_CATALOG_BYTES)
        .and_then(|()| {
            copy_regular(source.signature, &dir.join(CANDIDATE_SIGNATURE), MAX_SIGNATURE_BYTES)
        })
        .and_then(|()| copy_regular(source.archive, &dir.join(CANDIDATE_ARCHIVE), MAX_ARCHIVE_BYTES))
        .and_then(|()| {
            source.seen.map_or(Ok(()), |seen| {
                seed_seen_receipt(coordination, seen).and_then(|()| record_seen_source(&dir, seen))
            })
        });
    if let Err(err) = copied {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(dir)
}

/// Free-space query of the volume holding a path. A seam so the preflight is
/// testable deterministically.
pub trait FreeSpace {
    /// Bytes that can still be allocated on the volume of `path`.
    fn available_bytes(&self, path: &Path) -> Result<u64>;
}

/// The real query: the Windows volume API. Any other OS is refused, never
/// assumed to have room.
pub struct SystemFreeSpace;

impl FreeSpace for SystemFreeSpace {
    #[cfg(target_os = "windows")]
    fn available_bytes(&self, path: &Path) -> Result<u64> {
        crate::platform::windows::available_bytes(path)
    }

    #[cfg(not(target_os = "windows"))]
    fn available_bytes(&self, _path: &Path) -> Result<u64> {
        Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "the free space of the install volume cannot be queried on this OS",
        ))
    }
}

/// Space the replacement still needs on the install volume, counted from the
/// moment of the check. The staged files already exist and are not counted
/// again, but the peak use of the install volume is: whole-file backups of
/// every replaced file, every new file that accumulates in the install (the
/// space of replaced old files is conservatively not assumed to be freed), the
/// largest single scratch copy (a replacement, or a restore during rollback)
/// and the journal reserve. Any overflow is a refusal.
fn required_free_bytes(plan: &ApplyPlan) -> Result<u64> {
    let overflow = || UpdateError::new(ErrorCode::LimitExceeded, "the required free space overflows");
    let mut backups = 0_u64;
    let mut installed = 0_u64;
    let mut largest = 0_u64;
    for step in &plan.steps {
        let old = step.old.as_ref().map_or(0, |id| id.size);
        let new = step.new.as_ref().map_or(0, |id| id.size);
        if step.backup_rel.is_some() {
            backups = backups.checked_add(old).ok_or_else(overflow)?;
        }
        installed = installed.checked_add(new).ok_or_else(overflow)?;
        largest = largest.max(old).max(new);
    }
    backups
        .checked_add(installed)
        .and_then(|sum| sum.checked_add(largest))
        .and_then(|sum| sum.checked_add(JOURNAL_RESERVE_BYTES))
        .ok_or_else(overflow)
}

/// Refuses before any mutation when the install volume cannot hold the backups,
/// the largest scratch copy and the journal reserve. The install root and the
/// coordination area share one volume (checked at placement); both paths are
/// queried and the smaller answer is used.
fn ensure_free_space(inputs: &ApplyInputs<'_>, plan: &ApplyPlan) -> Result<()> {
    let required = required_free_bytes(plan)?;
    let txn_dir = inputs.coordination.transaction_dir(inputs.transaction);
    let available = inputs
        .free_space
        .available_bytes(&inputs.placement.install_root)?
        .min(inputs.free_space.available_bytes(&txn_dir)?);
    if available < required {
        return Err(UpdateError::new(
            ErrorCode::InsufficientSpace,
            format!("the update needs {required} free bytes but only {available} are available"),
        ));
    }
    Ok(())
}

/// Everything the helper derives or holds when it applies.
pub struct ApplyInputs<'a> {
    pub free_space: &'a dyn FreeSpace,
    pub coordination: &'a Coordination,
    pub transaction: &'a TransactionId,
    pub candidate_id: &'a str,
    pub placement: &'a Placement,
    pub policy: &'a TrustPolicy,
    pub clock: &'a dyn Clock,
    /// True only while the helper holds the exclusive `runtime` lease, which
    /// is the OS-observed proof that the parent process is gone.
    pub runtime_held: bool,
}

struct RuntimeHeld(bool);

impl ParentProcess for RuntimeHeld {
    fn has_exited(&self) -> Result<bool> {
        Ok(self.0)
    }
}

/// Stands in while no limited-health child exists (before one is started, and
/// when a pending journal is recovered): holding the runtime lease exclusively
/// already proves that no Hane process is alive.
pub(crate) struct NoChild;

impl HealthChild for NoChild {
    fn stop_and_release_lease(&mut self) -> Result<()> {
        Ok(())
    }
}

fn read_installed_inventory(root: &Path, policy: &TrustPolicy) -> Result<SignedInventory> {
    let missing = |what: &str| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"));
    let raw = read_limited(&root.join(INVENTORY_FILE_NAME), MAX_INVENTORY_BYTES)?
        .ok_or_else(|| missing("the install inventory"))?;
    let signature = read_limited(&root.join(INVENTORY_SIGNATURE_FILE_NAME), MAX_SIGNATURE_BYTES)?
        .ok_or_else(|| missing("the inventory signature"))?;
    SignedInventory::verify(&raw, &signature, policy)
}

/// Independently re-verifies placement, install, candidate and archive, stages
/// the archive and builds the plan from the two signed inventories. Nothing in
/// the install is changed.
///
/// The returned guard (when the candidate declared the application's receipt)
/// keeps the sequence floor from advancing; the caller holds it until the first
/// install mutation.
fn verify_and_plan(inputs: &ApplyInputs<'_>) -> Result<(ApplyPlan, Option<Box<dyn Send>>)> {
    let coordination = inputs.coordination;
    let placement = inputs.placement;
    if placement.target != Target::WindowsX64 {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "only the Windows x64 file layout can be applied",
        ));
    }
    if &placement.install_id != coordination.install_id() {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the install is not the one this coordination directory belongs to",
        ));
    }
    if !valid_candidate_id(inputs.candidate_id) {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "malformed candidate id"));
    }

    let installed = read_installed_inventory(&placement.install_root, inputs.policy)?;
    let facts = installed.inventory();
    if facts.target != Target::WindowsX64.triple()
        || facts.layout_revision != Target::WindowsX64.layout_revision()
    {
        return Err(UpdateError::new(
            ErrorCode::UnsupportedInstall,
            "the installed layout is not the fixed Windows x64 layout",
        ));
    }
    if facts.updater_protocol != UPDATER_PROTOCOL {
        return Err(UpdateError::new(
            ErrorCode::ProtocolUnsupported,
            "the installed updater protocol is not supported",
        ));
    }

    let txn_dir = coordination.transaction_dir(inputs.transaction);
    let candidate_dir = txn_dir.join(CANDIDATE_DIR);
    let missing = |what: &str| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"));
    let catalog = read_limited(&candidate_dir.join(CANDIDATE_CATALOG), MAX_CATALOG_BYTES)?
        .ok_or_else(|| missing("the candidate catalog"))?;
    let signature = read_limited(&candidate_dir.join(CANDIDATE_SIGNATURE), MAX_SIGNATURE_BYTES)?
        .ok_or_else(|| missing("the candidate signature"))?;
    // The floor is re-read now, after the runtime lease is held and before any
    // install mutation: the application may have recorded a newer sequence
    // since it made this candidate Ready. A missing, corrupt or unreadable
    // receipt is a refusal, never "no floor".
    let seen = FileSeenStore::new(coordination.seen_catalog_path())
        .load()
        .map_err(|err| {
            UpdateError::new(
                ErrorCode::ReceiptUnwritable,
                format!("the catalog sequence receipt cannot be read: {err}"),
            )
        })?
        .ok_or_else(|| {
            UpdateError::new(ErrorCode::NotFound, "the catalog sequence receipt is missing")
        })?;
    // The application's own receipt is re-read last and its lock is kept: a
    // service that recorded a higher sequence after the candidate was staged
    // raises the effective floor here, and none can after this point.
    let (seen, floor_guard) = reconfirm_user_floor(coordination, &candidate_dir, seen)?;
    let candidate = verify_catalog(
        &catalog,
        &signature,
        &VerifyContext {
            policy: inputs.policy,
            clock: inputs.clock,
            current_version: &facts.version,
            target: Target::WindowsX64,
            seen: Some(&seen),
            expected_tag: None,
        },
    )?;
    if candidate.candidate_id().as_str() != inputs.candidate_id {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the staged candidate is not the requested one",
        ));
    }
    if !candidate.is_applicable() {
        return Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the candidate is not newer than the installed version",
        ));
    }

    let archive_path = candidate_dir.join(CANDIDATE_ARCHIVE);
    let entry = candidate.target_entry();
    let (size, sha256) = hash_regular_file(&archive_path)?.ok_or_else(|| missing("the candidate archive"))?;
    if size != entry.archive_size || sha256 != entry.archive_sha256 {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "the staged archive differs from the signed catalog",
        ));
    }
    let stage = txn_dir.join(STAGE_DIR);
    match fs::create_dir(&stage) {
        Ok(()) => {}
        // An existing stage is accepted only if it is empty; the extractor
        // refuses anything else.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    let mut archive = File::open(&archive_path)?;
    let inspected = stage_archive(&mut archive, &candidate, inputs.policy, &ArchiveLimits::default(), &stage)?;
    drop(archive);
    let new_inventory = inspected.inventory.ok_or_else(|| {
        UpdateError::new(ErrorCode::ArchiveRejected, "the archive carries no signed inventory")
    })?;

    let plan = ApplyPlan::for_inventories(
        coordination.install_id().clone(),
        inputs.transaction.clone(),
        &installed,
        &new_inventory,
        &candidate,
    )?;
    check_plan_preconditions(&plan, &FsDiskView::new(&placement.install_root))?;
    Ok((plan, floor_guard))
}

/// Files replaced and awaiting a health verdict.
pub struct AppliedFiles {
    journal: DurableJournal<FileJournalStore>,
    ops: FsFileOps,
}

impl AppliedFiles {
    pub fn journal(&self) -> &UpdateJournal {
        self.journal.journal()
    }
}

/// Verifies, plans, writes the journal and replaces the owned files. On
/// success the journal is `AwaitingHealth`. A failure after the first
/// mutation has already been rolled back (or stopped as `RecoveryRequired`).
pub fn apply_owned_files(inputs: &ApplyInputs<'_>) -> Result<AppliedFiles> {
    let parent = RuntimeHeld(inputs.runtime_held);
    if !parent.has_exited()? {
        return Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the application process is still running",
        ));
    }
    // `_floor_guard` lives until this function returns, which is after the
    // first install mutation.
    let (plan, _floor_guard) = verify_and_plan(inputs)?;
    // Before the receipts are retired, the backup directory exists or any
    // journal is written: a refusal here leaves the install untouched.
    ensure_free_space(inputs, &plan)?;
    // The previous terminal receipt must still describe this install before
    // anything is mutated; otherwise it could not be retired safely.
    crate::startup::ensure_prior_receipts_current(
        inputs.coordination,
        &FsDiskView::new(&inputs.placement.install_root),
    )?;
    // The previous receipt is retired (durably, or the apply is refused) before
    // the new journal exists, so the new receipt is the only terminal one and
    // no file time is needed to order them.
    crate::startup::retire_prior_receipts(inputs.coordination, inputs.transaction)?;
    let txn_dir = inputs.coordination.transaction_dir(inputs.transaction);
    let backup = txn_dir.join(BACKUP_DIR);
    fs::create_dir_all(&backup)?;

    let journal = UpdateJournal::begin(&plan, None)?;
    // A protected placement's recovery plan is sealed in the protected
    // coordination parent before the journal exists and before any mutation: an
    // elevated recovery trusts that seal, never the user-writable journal alone.
    crate::recovery_seal::seal_if_protected(inputs.coordination, &journal)?;
    let store = FileJournalStore::new(inputs.coordination.journal_path(inputs.transaction));
    let mut journal = DurableJournal::create(store, journal)?;
    let ops = FsFileOps::new(
        inputs.placement.install_root.clone(),
        backup,
        txn_dir.join(STAGE_DIR),
    )
    .with_unchanged(
        plan.unchanged
            .iter()
            .map(|file| (file.rel_path.clone(), file.identity.clone()))
            .collect(),
    );
    #[cfg(windows)]
    let ops = ops.with_scratch_ledger(inputs.coordination, inputs.transaction);
    let mut ops = ops;
    // Before the first install mutation: bind the original identities outside
    // the inventory, so an interrupted update or a moved install still reaches
    // this transaction's journal at startup. Without it nothing is mutated.
    if let Err(err) = crate::relocation::write(
        &inputs.placement.install_root,
        inputs.coordination,
        inputs.transaction,
    ) {
        let _ = apply::recover(&mut journal, &mut ops, &mut NoChild);
        seal_if_root_lost(&mut journal, &ops);
        return Err(err);
    }
    match apply::apply(&mut journal, &mut ops, &parent, &mut NoChild) {
        Ok(ApplyOutcome::AwaitingHealth) => Ok(AppliedFiles { journal, ops }),
        Ok(ApplyOutcome::RolledBack(err)) => Err(err),
        Ok(ApplyOutcome::RecoveryRequired(why)) => Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("recovery is required: {why}"),
        )),
        Err(err) => {
            // Refused before the first mutation: settle the prepared journal
            // so the next start finds nothing pending. A failure here leaves
            // it for the startup recovery, which reaches the same result.
            let _ = apply::recover(&mut journal, &mut ops, &mut NoChild);
            seal_if_root_lost(&mut journal, &ops);
            Err(err)
        }
    }
}

/// When the install root is no longer the directory the operation started on,
/// nothing can be written or rolled back safely: the journal is made
/// `RecoveryRequired` so the pending transaction keeps blocking normal startup.
fn seal_if_root_lost(journal: &mut DurableJournal<FileJournalStore>, ops: &FsFileOps) {
    if ops.verify_root().is_err() && !journal.journal().phase.is_terminal() {
        let _ = journal.advance_phase(Phase::RecoveryRequired);
    }
}

/// The executable the limited-health child is started from, re-verified
/// against the current signed inventory after the files were replaced.
struct ChildTarget {
    exe: PathBuf,
    version: String,
}

fn verified_child(placement: &Placement, policy: &TrustPolicy) -> Result<ChildTarget> {
    let installed = read_installed_inventory(&placement.install_root, policy)?;
    let inventory = installed.inventory();
    let owned = inventory
        .files
        .iter()
        .find(|file| file.path == CHILD_EXE)
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "the inventory does not own hane.exe"))?;
    let exe = placement.install_root.join(CHILD_EXE);
    let (size, sha256) = hash_regular_file(&exe)?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "hane.exe is missing"))?;
    if size != owned.size || sha256 != owned.sha256 {
        return Err(UpdateError::new(
            ErrorCode::HashMismatch,
            "hane.exe differs from the signed inventory",
        ));
    }
    Ok(ChildTarget {
        exe,
        version: inventory.version.clone(),
    })
}

/// The helper's whole apply step. After the files are replaced the limited
/// child is started and judged: only a verified `Healthy` from this
/// transaction's own child lets the journal reach a durable `Committed`
/// (`Ok`). Every other end has stopped the child and restored the old files,
/// or has stopped as `RecoveryRequired`; none of them is reported as applied.
pub fn apply_and_settle(inputs: &ApplyInputs<'_>, launcher: &mut dyn HealthLauncher) -> Result<()> {
    let mut applied = apply_owned_files(inputs)?;
    let mut child = match verified_child(inputs.placement, inputs.policy) {
        Ok(target) => launcher.launch(&LaunchRequest {
            exe: &target.exe,
            version: &target.version,
            candidate_id: inputs.candidate_id,
            coordination: inputs.coordination,
            transaction: inputs.transaction,
        }),
        Err(err) => launcher.failed(
            err,
            &LaunchFailure {
                candidate_id: inputs.candidate_id,
                coordination: inputs.coordination,
                transaction: inputs.transaction,
            },
        ),
    };
    let settled = conclude_with_child(
        &mut applied.journal,
        &mut applied.ops,
        &mut *child,
        launcher.health_timeout(),
    );
    seal_if_root_lost(&mut applied.journal, &applied.ops);
    settled_result(settled?)
}

/// `Ok` only for a durable `Committed`; every other verdict is an error that
/// says the old version was restored or recovery is required. Shared with the
/// macOS bundle path so both report success on exactly the same condition.
pub(crate) fn settled_result(settled: Settled) -> Result<()> {
    match settled.outcome {
        HealthOutcome::Committed => Ok(()),
        HealthOutcome::RolledBack => Err(settled.reason.unwrap_or_else(|| {
            UpdateError::new(
                ErrorCode::NotApplicable,
                "the new version did not report healthy; the files were restored",
            )
        })),
        HealthOutcome::RecoveryRequired(why) => Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("recovery is required: {why}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::time::Duration;

    use super::*;
    use crate::catalog::record_seen;
    use crate::health::LimitedChild;
    use crate::identity::{InstallFacts, InstallId};
    use crate::trust::SeenCatalog;
    use crate::journal::{JournalStore, Phase, StepState};
    use crate::testing::{
        FixedClock, TempDir, WindowsRelease, default_payload, test_now, test_policy, windows_release,
    };

    const OLD: &str = "0.25.0";
    const NEW: &str = "0.26.0";

    fn write_tree(root: &Path, files: &[(String, Vec<u8>)]) {
        for (rel, data) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                // The relocation record is bookkeeping, not install content.
                if path.file_name().is_some_and(|name| name == crate::relocation::RELOCATION_FILE) {
                    continue;
                }
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let rel = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    out.insert(rel, fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    struct Fixture {
        _dir: TempDir,
        coordination: Coordination,
        transaction: TransactionId,
        placement: Placement,
        candidate_id: String,
        policy: TrustPolicy,
        clock: FixedClock,
        before: BTreeMap<String, Vec<u8>>,
        space: FakeSpace,
    }

    /// Deterministic free-space answer; `None` is a failed query.
    struct FakeSpace(Cell<Option<u64>>);

    impl FreeSpace for FakeSpace {
        fn available_bytes(&self, _path: &Path) -> Result<u64> {
            self.0
                .get()
                .ok_or_else(|| UpdateError::new(ErrorCode::Io, "free space unavailable"))
        }
    }

    fn release_files(release: &WindowsRelease, version: &str) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = default_payload(version)
            .into_iter()
            .filter(|entry| !entry.is_dir())
            .map(|entry| (String::from_utf8(entry.name).unwrap(), entry.data))
            .collect();
        files.push((INVENTORY_FILE_NAME.to_owned(), release.inventory.clone()));
        files.push((INVENTORY_SIGNATURE_FILE_NAME.to_owned(), release.inventory_sig.clone()));
        files
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("windows-apply");
        let install_root = dir.path().join("Hane");
        let old = windows_release(OLD, 1, default_payload(OLD));
        write_tree(&install_root, &release_files(&old, OLD));
        // Not owned by the inventory: must survive every outcome.
        write_tree(
            &install_root,
            &[
                ("notes.md".to_owned(), b"user note".to_vec()),
                ("shell-integration/0.20.0/hane_shell_extension.dll".to_owned(), b"loaded".to_vec()),
            ],
        );

        let install_id = InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap();
        let coordination = Coordination::create(dir.path(), &install_id).unwrap();
        let transaction = TransactionId::generate().unwrap();
        coordination.ensure_helper_dir(&transaction).unwrap();

        let new = windows_release(NEW, 2, default_payload(NEW));
        let ready = dir.path().join("ready");
        fs::create_dir_all(&ready).unwrap();
        fs::write(ready.join("c"), &new.catalog).unwrap();
        fs::write(ready.join("s"), &new.catalog_sig).unwrap();
        fs::write(ready.join("a"), &new.zip).unwrap();
        stage_candidate(
            &coordination,
            &transaction,
            &CandidateSource {
                catalog: &ready.join("c"),
                signature: &ready.join("s"),
                archive: &ready.join("a"),
                seen: None,
            },
        )
        .unwrap();
        let verified = verify_catalog(
            &new.catalog,
            &new.catalog_sig,
            &VerifyContext {
                policy: &test_policy(),
                clock: &FixedClock::new(test_now()),
                current_version: OLD,
                target: Target::WindowsX64,
                seen: None,
                expected_tag: None,
            },
        )
        .unwrap();
        let candidate_id = verified.candidate_id().as_str().to_owned();
        // Ready is only reached once the application stored this floor.
        fs::create_dir_all(coordination.dir().join(crate::platform::lease::RECEIPTS_DIR)).unwrap();
        record_seen(&mut FileSeenStore::new(coordination.seen_catalog_path()), &verified).unwrap();

        let placement = Placement {
            target: Target::WindowsX64,
            install_root: install_root.clone(),
            coordination_parent: dir.path().to_path_buf(),
            install_id,
        };
        let before = snapshot(&install_root);
        Fixture {
            _dir: dir,
            coordination,
            transaction,
            placement,
            candidate_id,
            policy: test_policy(),
            clock: FixedClock::new(test_now()),
            before,
            space: FakeSpace(Cell::new(Some(u64::MAX))),
        }
    }

    impl Fixture {
        fn inputs(&self) -> ApplyInputs<'_> {
            ApplyInputs {
                free_space: &self.space,
                coordination: &self.coordination,
                transaction: &self.transaction,
                candidate_id: &self.candidate_id,
                placement: &self.placement,
                policy: &self.policy,
                clock: &self.clock,
                runtime_held: true,
            }
        }

        fn journal_path(&self) -> PathBuf {
            self.coordination.journal_path(&self.transaction)
        }

        fn candidate_file(&self, name: &str) -> PathBuf {
            self.coordination
                .transaction_dir(&self.transaction)
                .join(CANDIDATE_DIR)
                .join(name)
        }

        fn unchanged(&self) -> bool {
            snapshot(&self.placement.install_root) == self.before
        }
    }

    #[test]
    fn owned_files_are_replaced_with_the_exe_last_and_foreign_files_stay() {
        let fx = fixture();
        let applied = apply_owned_files(&fx.inputs()).unwrap();
        let journal = applied.journal();
        assert_eq!(journal.phase, Phase::AwaitingHealth);
        assert_eq!(journal.steps.last().unwrap().rel_path, "hane.exe");
        assert!(journal.steps.iter().all(|step| step.state == StepState::Applied));

        let after = snapshot(&fx.placement.install_root);
        assert_eq!(after["hane.exe"], format!("hane.exe {NEW}").into_bytes());
        assert_eq!(after["notes.md"], b"user note");
        // The retained shell DLLs of older versions are never touched.
        assert_eq!(after["shell-integration/0.20.0/hane_shell_extension.dll"], b"loaded");
        assert_eq!(
            after[&format!("shell-integration/{NEW}/hane_shell_extension.dll")],
            format!("dll {NEW}").into_bytes()
        );
        // Only inventory-owned paths changed; nothing else was added.
        for path in after.keys() {
            assert!(
                fx.before.contains_key(path)
                    || path.contains(NEW)
                    || path == "hane.exe"
                    || path == crate::relocation::RELOCATION_FILE,
                "unexpected file {path}"
            );
        }
    }

    #[test]
    fn too_little_free_space_refuses_before_any_mutation() {
        for available in [Some(0), Some(JOURNAL_RESERVE_BYTES), Some(JOURNAL_RESERVE_BYTES + 1), None] {
            // Each case gets its own fixture so a prior case's stage state
            // cannot make the apply fail before the free-space check.
            let fx = fixture();
            fx.space.0.set(available);
            let err = apply_owned_files(&fx.inputs()).err().unwrap();
            let expected = if available.is_some() { ErrorCode::InsufficientSpace } else { ErrorCode::Io };
            assert_eq!(err.code, expected);
            assert!(fx.unchanged(), "the install changed for {available:?}");
            assert!(!fx.journal_path().exists(), "a journal was written for {available:?}");
            assert!(
                !fx.coordination.transaction_dir(&fx.transaction).join(BACKUP_DIR).exists(),
                "a backup area was created for {available:?}"
            );
        }
        // Backups, the accumulated new files, the largest copy and the reserve suffice.
        let required = plan_requirement().required;
        let fx = fixture();
        fx.space.0.set(Some(required));
        let applied = apply_owned_files(&fx.inputs()).unwrap();
        assert_eq!(applied.journal().phase, Phase::AwaitingHealth);
    }

    struct PlanRequirement {
        required: u64,
        without_accumulated_new: u64,
    }

    /// The requirement of the fixture's plan, and what it would be if only the
    /// backups, the largest copy and the reserve were counted.
    fn plan_requirement() -> PlanRequirement {
        let fx = fixture();
        let (plan, _guard) = verify_and_plan(&fx.inputs()).unwrap();
        let mut backups = 0_u64;
        let mut largest = 0_u64;
        for step in &plan.steps {
            let old = step.old.as_ref().map_or(0, |id| id.size);
            let new = step.new.as_ref().map_or(0, |id| id.size);
            if step.backup_rel.is_some() {
                backups += old;
            }
            largest = largest.max(old).max(new);
        }
        PlanRequirement {
            required: required_free_bytes(&plan).unwrap(),
            without_accumulated_new: backups + largest + JOURNAL_RESERVE_BYTES,
        }
    }

    #[test]
    fn accumulated_new_files_are_reserved_before_any_mutation() {
        let need = plan_requirement();
        assert!(
            need.without_accumulated_new < need.required,
            "the fixture must replace more than one file for this case to bite"
        );
        // Enough for every backup and the largest file, but not for the new
        // files that pile up in the install: refused with the install untouched.
        let fx = fixture();
        fx.space.0.set(Some(need.without_accumulated_new));
        let err = apply_owned_files(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::InsufficientSpace);
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
        assert!(
            !fx.coordination.transaction_dir(&fx.transaction).join(BACKUP_DIR).exists(),
            "a backup area was created"
        );
        // One byte short of the full requirement is still refused.
        let fx = fixture();
        fx.space.0.set(Some(need.required - 1));
        assert_eq!(apply_owned_files(&fx.inputs()).err().unwrap().code, ErrorCode::InsufficientSpace);
        assert!(fx.unchanged());
    }

    /// What the fake limited child saw and was asked to do.
    #[derive(Default)]
    struct Probe {
        stops: Cell<usize>,
        commits: Cell<usize>,
        detaches: Cell<usize>,
        phase_at_detach: RefCell<Option<Phase>>,
        commits_at_detach: RefCell<Option<usize>>,
        phase_at_commit: RefCell<Option<Phase>>,
        launched: RefCell<Option<(PathBuf, String, String)>>,
    }

    struct FakeChild {
        health: Result<()>,
        stop: Result<()>,
        journal_path: PathBuf,
        /// Created while the child is judged, so the next journal write fails.
        sabotage: Option<PathBuf>,
        probe: Rc<Probe>,
    }

    impl LimitedChild for FakeChild {
        fn await_healthy(&mut self, _timeout: Duration) -> Result<()> {
            if let Some(path) = &self.sabotage {
                fs::create_dir(path).unwrap();
            }
            self.health.clone()
        }

        fn send_commit(&mut self) -> Result<()> {
            self.probe.commits.set(self.probe.commits.get() + 1);
            let stored = FileJournalStore::new(self.journal_path.clone()).load().unwrap().unwrap();
            *self.probe.phase_at_commit.borrow_mut() = Some(stored.phase);
            Ok(())
        }

        fn detach(&mut self) {
            self.probe.detaches.set(self.probe.detaches.get() + 1);
            *self.probe.phase_at_detach.borrow_mut() = Some(
                FileJournalStore::new(self.journal_path.clone())
                    .load()
                    .unwrap()
                    .unwrap()
                    .phase,
            );
            *self.probe.commits_at_detach.borrow_mut() = Some(self.probe.commits.get());
        }

        fn as_health_child(&mut self) -> &mut dyn HealthChild {
            self
        }
    }

    impl HealthChild for FakeChild {
        fn stop_and_release_lease(&mut self) -> Result<()> {
            self.probe.stops.set(self.probe.stops.get() + 1);
            self.stop.clone()
        }
    }

    struct FakeLauncher {
        health: Result<()>,
        stop: Result<()>,
        sabotage: Option<PathBuf>,
        journal_path: PathBuf,
        probe: Rc<Probe>,
    }

    impl FakeLauncher {
        fn new(fx: &Fixture, health: Result<()>) -> Self {
            Self {
                health,
                stop: Ok(()),
                sabotage: None,
                journal_path: fx.journal_path(),
                probe: Rc::new(Probe::default()),
            }
        }

        fn child(&self, health: Result<()>) -> Box<dyn LimitedChild> {
            Box::new(FakeChild {
                health,
                stop: self.stop.clone(),
                journal_path: self.journal_path.clone(),
                sabotage: self.sabotage.clone(),
                probe: Rc::clone(&self.probe),
            })
        }
    }

    impl HealthLauncher for FakeLauncher {
        fn launch(&mut self, request: &LaunchRequest<'_>) -> Box<dyn LimitedChild> {
            *self.probe.launched.borrow_mut() = Some((
                request.exe.to_path_buf(),
                request.version.to_owned(),
                request.candidate_id.to_owned(),
            ));
            self.child(self.health.clone())
        }

        fn failed(&mut self, error: UpdateError, _request: &LaunchFailure<'_>) -> Box<dyn LimitedChild> {
            self.child(Err(error))
        }

        fn health_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    fn stored_phase(fx: &Fixture) -> Phase {
        FileJournalStore::new(fx.journal_path()).load().unwrap().unwrap().phase
    }

    #[test]
    fn a_healthy_child_commits_durably_and_only_then_hears_commit() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        apply_and_settle(&fx.inputs(), &mut launcher).unwrap();

        assert_eq!(stored_phase(&fx), Phase::Committed);
        let probe = &launcher.probe;
        assert_eq!(probe.commits.get(), 1);
        assert_eq!(
            *probe.phase_at_commit.borrow(),
            Some(Phase::Committed),
            "Commit is sent only after the journal is durably Committed"
        );
        assert_eq!(probe.stops.get(), 0);
        // The child is released from the helper only after Committed is
        // durable and Commit was delivered.
        assert_eq!(probe.detaches.get(), 1);
        assert_eq!(*probe.phase_at_detach.borrow(), Some(Phase::Committed));
        assert_eq!(*probe.commits_at_detach.borrow(), Some(1));
        // The new version is kept, and the child came from the signed inventory.
        let after = snapshot(&fx.placement.install_root);
        assert_eq!(after["hane.exe"], format!("hane.exe {NEW}").into_bytes());
        let launched = probe.launched.borrow().clone().unwrap();
        assert_eq!(launched.0, fx.placement.install_root.join("hane.exe"));
        assert_eq!(launched.1, NEW);
        assert_eq!(launched.2, fx.candidate_id);
    }

    #[test]
    fn a_timeout_crash_or_rejected_health_stops_the_child_and_restores_the_old_version() {
        for code in [ErrorCode::Timeout, ErrorCode::Truncated, ErrorCode::IdentityMismatch] {
            let fx = fixture();
            let mut launcher = FakeLauncher::new(&fx, Err(UpdateError::new(code, "no health")));
            let err = apply_and_settle(&fx.inputs(), &mut launcher).unwrap_err();
            assert_eq!(err.code, code);
            assert!(fx.unchanged(), "{code:?}: the old files must be back byte for byte");
            assert_eq!(stored_phase(&fx), Phase::RolledBack, "{code:?}");
            assert_eq!(launcher.probe.stops.get(), 1, "{code:?}");
            assert_eq!(launcher.probe.commits.get(), 0, "{code:?}");
            assert_eq!(launcher.probe.detaches.get(), 0, "{code:?}: a rollback never detaches");
        }
    }

    #[test]
    fn a_child_that_cannot_be_stopped_or_leaves_the_lease_blocks_rollback() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Err(UpdateError::new(ErrorCode::Timeout, "no health")));
        launcher.stop = Err(UpdateError::new(ErrorCode::Timeout, "the runtime lease is still held"));
        let err = apply_and_settle(&fx.inputs(), &mut launcher).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
        assert_eq!(stored_phase(&fx), Phase::RecoveryRequired);
        assert_eq!(launcher.probe.stops.get(), 1);
        assert_eq!(launcher.probe.commits.get(), 0);
        // Nothing was written over the install: the new files are still there.
        let after = snapshot(&fx.placement.install_root);
        assert_eq!(after["hane.exe"], format!("hane.exe {NEW}").into_bytes());
        assert!(!fx.unchanged());
    }

    #[test]
    fn a_commit_that_cannot_be_made_durable_never_reaches_the_child() {
        let fx = fixture();
        let mut launcher = FakeLauncher::new(&fx, Ok(()));
        // A directory where the journal's scratch file goes: every journal
        // write fails from the moment the child is judged.
        launcher.sabotage = Some(
            fx.coordination
                .transaction_dir(&fx.transaction)
                .join(".journal.json.tmp"),
        );
        assert!(apply_and_settle(&fx.inputs(), &mut launcher).is_err());
        assert_eq!(launcher.probe.commits.get(), 0, "no Commit, so no normal startup");
        assert_eq!(launcher.probe.detaches.get(), 0, "the child stays owned and is stopped");
        assert_eq!(launcher.probe.stops.get(), 1, "the child is stopped, not released");
        assert_ne!(stored_phase(&fx), Phase::Committed);
    }

    #[test]
    fn an_unverifiable_child_executable_is_never_started() {
        let fx = fixture();
        drop(apply_owned_files(&fx.inputs()).unwrap());
        // The executable matches the signed inventory it was installed with.
        let target = verified_child(&fx.placement, &fx.policy).unwrap();
        assert_eq!(target.version, NEW);
        assert_eq!(target.exe, fx.placement.install_root.join("hane.exe"));
        // A policy that does not trust the inventory, or a swapped executable,
        // is refused before anything is started.
        assert!(verified_child(&fx.placement, &TrustPolicy::production()).is_err());
        fs::write(fx.placement.install_root.join("hane.exe"), b"swapped").unwrap();
        let err = verified_child(&fx.placement, &fx.policy).err().unwrap();
        assert_eq!(err.code, ErrorCode::HashMismatch);
    }

    #[test]
    fn a_living_parent_blocks_everything_before_any_write() {
        let fx = fixture();
        let mut inputs = fx.inputs();
        inputs.runtime_held = false;
        assert_eq!(apply_owned_files(&inputs).err().unwrap().code, ErrorCode::NotApplicable);
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
        assert!(!fx.coordination.transaction_dir(&fx.transaction).join(STAGE_DIR).exists());
    }

    #[test]
    fn a_tampered_or_swapped_candidate_changes_nothing() {
        let fx = fixture();
        let archive = fx.candidate_file(CANDIDATE_ARCHIVE);
        let mut bytes = fs::read(&archive).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::write(&archive, bytes).unwrap();
        assert_eq!(apply_owned_files(&fx.inputs()).err().unwrap().code, ErrorCode::HashMismatch);
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());

        let fx = fixture();
        let wrong = "0".repeat(64);
        let mut inputs = fx.inputs();
        inputs.candidate_id = &wrong;
        assert_eq!(apply_owned_files(&inputs).err().unwrap().code, ErrorCode::IdentityMismatch);
        let mut inputs = fx.inputs();
        inputs.candidate_id = "../escape";
        assert_eq!(apply_owned_files(&inputs).err().unwrap().code, ErrorCode::IdentityMismatch);
        assert!(fx.unchanged());
    }

    #[test]
    fn a_missing_or_forged_catalog_changes_nothing() {
        let fx = fixture();
        fs::remove_file(fx.candidate_file(CANDIDATE_SIGNATURE)).unwrap();
        assert_eq!(apply_owned_files(&fx.inputs()).err().unwrap().code, ErrorCode::NotFound);
        assert!(fx.unchanged());

        let fx = fixture();
        let untrusted = TrustPolicy::production();
        let mut inputs = fx.inputs();
        inputs.policy = &untrusted;
        assert!(apply_owned_files(&inputs).is_err());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
    }

    #[test]
    fn a_modified_install_is_never_overwritten() {
        let fx = fixture();
        fs::write(fx.placement.install_root.join("README.md"), b"edited").unwrap();
        let before = snapshot(&fx.placement.install_root);
        let err = apply_owned_files(&fx.inputs()).err().unwrap();
        assert_eq!(err.code, ErrorCode::InstallModified);
        assert_eq!(snapshot(&fx.placement.install_root), before);
        assert!(!fx.journal_path().exists());
    }

    #[test]
    fn only_the_windows_layout_and_this_install_are_accepted() {
        let fx = fixture();
        let mut placement = fx.placement.clone();
        placement.target = Target::MacosArm64;
        let mut inputs = fx.inputs();
        inputs.placement = &placement;
        assert_eq!(apply_owned_files(&inputs).err().unwrap().code, ErrorCode::UnsupportedInstall);

        let mut placement = fx.placement.clone();
        placement.install_id = InstallId::parse(&"a".repeat(32)).unwrap();
        let mut inputs = fx.inputs();
        inputs.placement = &placement;
        assert_eq!(apply_owned_files(&inputs).err().unwrap().code, ErrorCode::IdentityMismatch);
        assert!(fx.unchanged());
    }

    fn store_floor(fx: &Fixture, seen: &SeenCatalog) {
        FileSeenStore::new(fx.coordination.seen_catalog_path()).store(seen).unwrap();
    }

    #[test]
    fn a_floor_recorded_after_ready_rejects_the_stale_candidate() {
        let fx = fixture();
        store_floor(&fx, &SeenCatalog::new(3, "b".repeat(64)));
        assert_eq!(
            apply_owned_files(&fx.inputs()).err().unwrap().code,
            ErrorCode::SequenceRegression
        );
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
    }

    #[test]
    fn the_same_sequence_with_other_bytes_is_rejected_but_the_recorded_one_applies() {
        let fx = fixture();
        store_floor(&fx, &SeenCatalog::new(2, "b".repeat(64)));
        assert_eq!(
            apply_owned_files(&fx.inputs()).err().unwrap().code,
            ErrorCode::SequenceConflict
        );
        assert!(fx.unchanged());

        // The fixture's own receipt (same sequence, same bytes) is allowed.
        let fx = fixture();
        assert!(apply_owned_files(&fx.inputs()).is_ok());
    }

    /// Declares the application's own receipt for the fixture's staged
    /// candidate, as `stage_candidate` does when given a `seen` path.
    fn declare_user_receipt(fx: &Fixture) -> PathBuf {
        let root = fx.placement.coordination_parent.join("state");
        let receipts = root.join("updates").join(fx.placement.install_id.as_str()).join("receipts");
        fs::create_dir_all(&receipts).unwrap();
        let mut dir = receipts.clone();
        while dir != root.parent().unwrap() {
            crate::platform::acl::secure_new(&dir, true, 0o700).unwrap();
            dir.pop();
        }
        crate::state_root::publish_binding(&fx.coordination, &fx.transaction, &root).unwrap();
        set_trusted_state_root(Some(root.clone()));
        let path = receipts.join(crate::platform::lease::SEEN_CATALOG_FILE);
        let ready = FileSeenStore::new(fx.coordination.seen_catalog_path()).load().unwrap().unwrap();
        let mut store = FileSeenStore::new(path.clone());
        store.store(&ready).unwrap();
        // The application's own writer has created the lock file by now; the
        // helper never creates it.
        drop(store.lock().unwrap());
        let dir = fx.coordination.transaction_dir(&fx.transaction).join(CANDIDATE_DIR);
        record_seen_source(&dir, &path).unwrap();
        path
    }

    /// Free-space query that runs after the helper re-read and locked the
    /// application's receipt and before the first install mutation. On its
    /// first call it proves the lock is held and starts a competing writer that
    /// reports what the install looked like when it finally got the lock.
    struct CompetingWriterProbe {
        available: u64,
        user_receipt: PathBuf,
        install_root: PathBuf,
        before: BTreeMap<String, Vec<u8>>,
        journal_path: PathBuf,
        started: Cell<bool>,
        /// `(journal exists, install unchanged)` seen by the writer on locking.
        observed: RefCell<Option<std::sync::mpsc::Receiver<(bool, bool)>>>,
    }

    impl FreeSpace for CompetingWriterProbe {
        fn available_bytes(&self, _path: &Path) -> Result<u64> {
            if !self.started.replace(true) {
                let lock_path = self.user_receipt.with_extension("lock");
                let probe = OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
                assert!(
                    matches!(probe.try_lock(), Err(fs::TryLockError::WouldBlock)),
                    "the helper must hold the application receipt lock before the first mutation"
                );
                drop(probe);

                let (tx, rx) = std::sync::mpsc::channel();
                let (receipt, root, before, journal) = (
                    self.user_receipt.clone(),
                    self.install_root.clone(),
                    self.before.clone(),
                    self.journal_path.clone(),
                );
                std::thread::spawn(move || {
                    let store = FileSeenStore::new(receipt);
                    let _lock = store.lock().unwrap();
                    let _ = tx.send((journal.exists(), snapshot(&root) == before));
                });
                // The writer is waiting for the lock: it must not get in while
                // the helper is still before its first mutation.
                assert!(
                    matches!(
                        rx.recv_timeout(Duration::from_millis(300)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ),
                    "a competing writer got the sequence lock before the install was mutated"
                );
                *self.observed.borrow_mut() = Some(rx);
            }
            Ok(self.available)
        }
    }

    fn competing_writer_probe(fx: &Fixture, user_receipt: PathBuf, available: u64) -> CompetingWriterProbe {
        CompetingWriterProbe {
            available,
            user_receipt,
            install_root: fx.placement.install_root.clone(),
            before: fx.before.clone(),
            journal_path: fx.journal_path(),
            started: Cell::new(false),
            observed: RefCell::new(None),
        }
    }

    #[test]
    fn a_competing_writer_waits_for_the_lock_until_the_install_is_mutated() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        let probe = competing_writer_probe(&fx, user, u64::MAX);
        let mut inputs = fx.inputs();
        inputs.free_space = &probe;
        let applied = apply_owned_files(&inputs).unwrap();
        assert_eq!(applied.journal().phase, Phase::AwaitingHealth);
        drop(applied);

        let rx = probe.observed.borrow_mut().take().expect("the probe ran under the lock");
        let (journal_exists, install_unchanged) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(journal_exists, "the writer got the lock before the journal existed");
        assert!(!install_unchanged, "the writer got the lock before the install was mutated");
    }

    #[test]
    fn a_refused_apply_keeps_the_lock_and_leaves_install_and_journal_untouched() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        let probe = competing_writer_probe(&fx, user, 0);
        let mut inputs = fx.inputs();
        inputs.free_space = &probe;
        assert_eq!(apply_owned_files(&inputs).err().unwrap().code, ErrorCode::InsufficientSpace);
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());

        // The writer only proceeds once the refused apply released the lock,
        // and finds the install and journal as they were.
        let rx = probe.observed.borrow_mut().take().expect("the probe ran under the lock");
        let (journal_exists, install_unchanged) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!journal_exists);
        assert!(install_unchanged);
    }

    #[test]
    fn a_floor_recorded_in_the_user_receipt_after_staging_rejects_the_candidate() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        // Another service records a higher sequence after the candidate was
        // staged: only the application's receipt moves.
        FileSeenStore::new(user).store(&SeenCatalog::new(3, "b".repeat(64))).unwrap();
        assert_eq!(
            apply_owned_files(&fx.inputs()).err().unwrap().code,
            ErrorCode::SequenceRegression
        );
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
    }

    #[test]
    fn the_user_receipt_with_other_bytes_for_the_same_sequence_is_rejected() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        FileSeenStore::new(user).store(&SeenCatalog::new(2, "b".repeat(64))).unwrap();
        assert_eq!(
            apply_owned_files(&fx.inputs()).err().unwrap().code,
            ErrorCode::SequenceConflict
        );
        assert!(fx.unchanged());
    }

    #[test]
    fn a_declared_but_missing_or_corrupt_user_receipt_fails_closed() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        fs::remove_file(&user).unwrap();
        assert_eq!(apply_owned_files(&fx.inputs()).err().unwrap().code, ErrorCode::NotFound);
        assert!(fx.unchanged());

        let fx = fixture();
        let user = declare_user_receipt(&fx);
        fs::write(&user, b"{not json").unwrap();
        assert!(apply_owned_files(&fx.inputs()).is_err());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
    }

    #[test]
    fn a_tampered_receipt_path_is_refused_before_anything_is_opened_or_created() {
        let fx = fixture();
        let genuine = declare_user_receipt(&fx);
        let marker = fx.candidate_file(CANDIDATE_SEEN_SOURCE);

        // An absolute path elsewhere, with a valid-looking receipt, and one in a
        // directory that does not exist (the lock file would create it).
        let elsewhere = fx.placement.coordination_parent.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let forged = elsewhere.join("seen-catalog.json");
        let ready = FileSeenStore::new(genuine.clone()).load().unwrap().unwrap();
        FileSeenStore::new(forged.clone()).store(&ready).unwrap();
        let missing_parent = fx.placement.coordination_parent.join("nowhere").join("seen-catalog.json");
        let relative = PathBuf::from("seen-catalog.json");
        let dotted = genuine.parent().unwrap().join("..").join("receipts").join("seen-catalog.json");

        for bad in [forged.clone(), missing_parent.clone(), relative, dotted] {
            fs::write(&marker, bad.to_str().unwrap()).unwrap();
            assert_eq!(
                apply_owned_files(&fx.inputs()).err().unwrap().code,
                ErrorCode::IdentityMismatch,
                "{bad:?}"
            );
            assert!(fx.unchanged());
            assert!(!fx.journal_path().exists());
            assert!(!forged.with_extension("lock").exists());
            assert!(!missing_parent.parent().unwrap().exists());
        }

        // The genuine location still applies.
        fs::write(&marker, genuine.to_str().unwrap()).unwrap();
        assert!(apply_owned_files(&fx.inputs()).is_ok());
    }

    #[test]
    fn the_binding_and_the_candidate_claim_are_not_a_source_of_authority() {
        // Neither a removed nor a rewritten binding changes where the helper
        // looks: the receipt location comes from the account's own root.
        let fx = fixture();
        let genuine = declare_user_receipt(&fx);
        crate::state_root::discard_binding(&fx.coordination, &fx.transaction);
        assert!(apply_owned_files(&fx.inputs()).is_ok());

        let fx = fixture();
        let genuine_again = declare_user_receipt(&fx);
        let elsewhere = fx.placement.coordination_parent.join("elsewhere-state");
        let receipts = elsewhere
            .join("updates")
            .join(fx.placement.install_id.as_str())
            .join("receipts");
        fs::create_dir_all(&receipts).unwrap();
        let mut dir = receipts.clone();
        while dir != elsewhere.parent().unwrap() {
            crate::platform::acl::secure_new(&dir, true, 0o700).unwrap();
            dir.pop();
        }
        let forged = receipts.join(crate::platform::lease::SEEN_CATALOG_FILE);
        let ready = FileSeenStore::new(genuine_again.clone()).load().unwrap().unwrap();
        FileSeenStore::new(forged.clone()).store(&ready).unwrap();
        // Rebind to the other root and point the claim at its receipt.
        crate::state_root::publish_binding(&fx.coordination, &fx.transaction, &elsewhere).unwrap();
        fs::write(fx.candidate_file(CANDIDATE_SEEN_SOURCE), forged.to_str().unwrap()).unwrap();
        assert_eq!(
            apply_owned_files(&fx.inputs()).err().unwrap().code,
            ErrorCode::IdentityMismatch
        );
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
        assert!(!forged.with_extension("lock").exists());
        let _ = genuine;
    }

    #[test]
    fn an_untrusted_state_root_refuses_the_declared_receipt() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        set_trusted_state_root(None);
        assert!(apply_owned_files(&fx.inputs()).is_err());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
        let _ = user;
    }

    #[test]
    fn a_missing_lock_file_is_not_created_by_the_helper() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        fs::remove_file(user.with_extension("lock")).unwrap();
        assert_eq!(apply_owned_files(&fx.inputs()).err().unwrap().code, ErrorCode::NotFound);
        assert!(!user.with_extension("lock").exists());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_receipt_directory_replaced_by_a_link_is_not_followed() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        let receipts = user.parent().unwrap().to_path_buf();
        // A different directory with a complete, valid-looking receipt and lock.
        let other = fx.placement.coordination_parent.join("swapped");
        fs::create_dir_all(&other).unwrap();
        crate::platform::acl::secure_new(&other, true, 0o700).unwrap();
        let ready = FileSeenStore::new(user.clone()).load().unwrap().unwrap();
        let mut store = FileSeenStore::new(other.join(crate::platform::lease::SEEN_CATALOG_FILE));
        store.store(&ready).unwrap();
        drop(store.lock().unwrap());
        let moved = receipts.with_file_name("receipts-real");
        fs::rename(&receipts, &moved).unwrap();
        std::os::unix::fs::symlink(&other, &receipts).unwrap();
        assert!(apply_owned_files(&fx.inputs()).is_err());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
        // The lock of the swapped-in directory was never taken or created anew.
        assert!(other.join("seen-catalog.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_receipt_replaced_by_a_link_is_not_followed() {
        let fx = fixture();
        let user = declare_user_receipt(&fx);
        let target = fx.placement.coordination_parent.join("linked-receipt.json");
        let ready = FileSeenStore::new(user.clone()).load().unwrap().unwrap();
        FileSeenStore::new(target.clone()).store(&ready).unwrap();
        fs::remove_file(&user).unwrap();
        std::os::unix::fs::symlink(&target, &user).unwrap();
        assert!(apply_owned_files(&fx.inputs()).is_err());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());
    }

    #[test]
    fn an_unchanged_user_receipt_still_applies() {
        let fx = fixture();
        declare_user_receipt(&fx);
        assert!(apply_owned_files(&fx.inputs()).is_ok());
    }

    #[test]
    fn a_lower_floor_still_allows_the_candidate() {
        let fx = fixture();
        store_floor(&fx, &SeenCatalog::new(1, "b".repeat(64)));
        assert!(apply_owned_files(&fx.inputs()).is_ok());
    }

    #[test]
    fn a_missing_or_corrupt_receipt_fails_closed() {
        let fx = fixture();
        fs::remove_file(fx.coordination.seen_catalog_path()).unwrap();
        assert_eq!(apply_owned_files(&fx.inputs()).err().unwrap().code, ErrorCode::NotFound);
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());

        let fx = fixture();
        fs::write(fx.coordination.seen_catalog_path(), b"{not json").unwrap();
        assert!(apply_owned_files(&fx.inputs()).is_err());
        assert!(fx.unchanged());
        assert!(!fx.journal_path().exists());

        // An unreadable receipt (a directory in its place) is not "no floor".
        let fx = fixture();
        let path = fx.coordination.seen_catalog_path();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(apply_owned_files(&fx.inputs()).is_err());
        assert!(fx.unchanged());
    }

    #[test]
    fn staging_the_candidate_refuses_missing_files_and_leaves_nothing() {
        let fx = fixture();
        let other = TransactionId::generate().unwrap();
        fx.coordination.ensure_helper_dir(&other).unwrap();
        let nowhere = fx.placement.coordination_parent.join("missing");
        let err = stage_candidate(
            &fx.coordination,
            &other,
            &CandidateSource {
                catalog: &nowhere,
                signature: &nowhere,
                archive: &nowhere,
                seen: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(!fx.coordination.transaction_dir(&other).join(CANDIDATE_DIR).exists());
    }

    #[test]
    fn staging_carries_the_applications_floor_into_the_coordination_receipts() {
        let fx = fixture();
        let scratch = TempDir::new("windows-apply-seed");
        let ready = scratch.path();
        for name in ["c", "s", "a"] {
            fs::write(ready.join(name), b"x").unwrap();
        }
        let hash = "a".repeat(64);
        let user_floor = ready.join("seen-catalog.json");
        FileSeenStore::new(user_floor.clone())
            .store(&SeenCatalog::new(9, hash.clone()))
            .unwrap();
        // A protected install has no receipts directory until the helper's
        // bootstrap; the carried floor creates it.
        fs::remove_dir_all(fx.coordination.dir().join(crate::platform::lease::RECEIPTS_DIR)).unwrap();
        let stage = |txn: &TransactionId, seen: Option<&Path>| {
            fx.coordination.ensure_helper_dir(txn).unwrap();
            stage_candidate(
                &fx.coordination,
                txn,
                &CandidateSource {
                    catalog: &ready.join("c"),
                    signature: &ready.join("s"),
                    archive: &ready.join("a"),
                    seen,
                },
            )
        };

        let first = TransactionId::generate().unwrap();
        stage(&first, Some(&user_floor)).unwrap();
        let stored = FileSeenStore::new(fx.coordination.seen_catalog_path()).load().unwrap().unwrap();
        assert_eq!((stored.sequence, stored.catalog_sha256.as_str()), (9, hash.as_str()));

        // A lower carried floor never lowers the stored one.
        FileSeenStore::new(user_floor.clone())
            .store(&SeenCatalog::new(3, "b".repeat(64)))
            .unwrap();
        let second = TransactionId::generate().unwrap();
        stage(&second, Some(&user_floor)).unwrap();
        let stored = FileSeenStore::new(fx.coordination.seen_catalog_path()).load().unwrap().unwrap();
        assert_eq!(stored.sequence, 9);

        // The same sequence with other bytes is refused and leaves no candidate.
        FileSeenStore::new(user_floor.clone())
            .store(&SeenCatalog::new(9, "c".repeat(64)))
            .unwrap();
        let third = TransactionId::generate().unwrap();
        let err = stage(&third, Some(&user_floor)).unwrap_err();
        assert_eq!(err.code, ErrorCode::SequenceConflict);
        assert!(!fx.coordination.transaction_dir(&third).join(CANDIDATE_DIR).exists());

        // A missing carried floor is a refusal, not "no floor".
        let fourth = TransactionId::generate().unwrap();
        let err = stage(&fourth, Some(&ready.join("missing.json"))).unwrap_err();
        assert!(matches!(err.code, ErrorCode::NotFound | ErrorCode::ReceiptUnwritable));
        assert!(!fx.coordination.transaction_dir(&fourth).join(CANDIDATE_DIR).exists());
    }
}
