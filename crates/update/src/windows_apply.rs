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
//! Not here: the macOS bundle swap, the limited-health child and the durable
//! commit. Without them no update may be reported as applied, so
//! [`apply_and_settle`] returns the install to the old version before it
//! reports back.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::apply::{self, ApplyOutcome, FsFileOps, HealthChild, HealthOutcome, ParentProcess};
use crate::archive::{ArchiveLimits, stage_archive};
use crate::catalog::{VerifyContext, verify_catalog};
use crate::clock::Clock;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{FileSeenStore, hash_regular_file, read_limited};
use crate::identity::{ApplyPlan, SignedInventory, TransactionId};
use crate::journal::{
    DurableJournal, FileJournalStore, FsDiskView, UpdateJournal, check_plan_preconditions,
};
use crate::limits::{MAX_ARCHIVE_BYTES, MAX_CATALOG_BYTES, MAX_INVENTORY_BYTES, MAX_SIGNATURE_BYTES};
use crate::model::{INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME, Target, UPDATER_PROTOCOL};
use crate::platform::Placement;
use crate::platform::lease::Coordination;
use crate::trust::{SeenStore, TrustPolicy};
use crate::util::is_sha256_hex;

/// Directory of the transaction that holds the copied candidate.
pub const CANDIDATE_DIR: &str = "candidate";
pub const CANDIDATE_CATALOG: &str = "catalog.json";
pub const CANDIDATE_SIGNATURE: &str = "catalog.sig";
pub const CANDIDATE_ARCHIVE: &str = "archive.zip";
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
        .and_then(|()| copy_regular(source.archive, &dir.join(CANDIDATE_ARCHIVE), MAX_ARCHIVE_BYTES));
    if let Err(err) = copied {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(dir)
}

/// Everything the helper derives or holds when it applies.
pub struct ApplyInputs<'a> {
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

/// No limited-health child exists in this increment. Holding the runtime
/// lease exclusively already proves that no Hane process is alive.
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
fn verify_and_plan(inputs: &ApplyInputs<'_>) -> Result<ApplyPlan> {
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
    Ok(plan)
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
    let plan = verify_and_plan(inputs)?;
    let txn_dir = inputs.coordination.transaction_dir(inputs.transaction);
    let backup = txn_dir.join(BACKUP_DIR);
    fs::create_dir_all(&backup)?;

    let journal = UpdateJournal::begin(&plan, None)?;
    let store = FileJournalStore::new(inputs.coordination.journal_path(inputs.transaction));
    let mut journal = DurableJournal::create(store, journal)?;
    let mut ops = FsFileOps::new(
        inputs.placement.install_root.clone(),
        backup,
        txn_dir.join(STAGE_DIR),
    );
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
            Err(err)
        }
    }
}

/// The helper's whole apply step for this increment. There is no limited-health
/// child or durable commit yet, so the replaced files are rolled back at once
/// and the call never reports an applied update.
pub fn apply_and_settle(inputs: &ApplyInputs<'_>) -> Result<()> {
    let mut applied = apply_owned_files(inputs)?;
    let outcome = apply::conclude_health(&mut applied.journal, &mut applied.ops, &mut NoChild, false)?;
    Err(match outcome {
        HealthOutcome::RolledBack => UpdateError::new(
            ErrorCode::NotApplicable,
            "limited health and durable commit are not connected yet; the files were restored",
        ),
        HealthOutcome::RecoveryRequired(why) => UpdateError::new(
            ErrorCode::InstallModified,
            format!("recovery is required: {why}"),
        ),
        HealthOutcome::Committed => UpdateError::new(
            ErrorCode::TransitionInvalid,
            "an unhealthy conclusion cannot commit",
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::catalog::record_seen;
    use crate::identity::{InstallFacts, InstallId};
    use crate::trust::SeenCatalog;
    use crate::journal::{Phase, StepState};
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
        }
    }

    impl Fixture {
        fn inputs(&self) -> ApplyInputs<'_> {
            ApplyInputs {
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
                fx.before.contains_key(path) || path.contains(NEW) || path == "hane.exe",
                "unexpected file {path}"
            );
        }
    }

    #[test]
    fn applying_without_health_and_commit_restores_the_old_version() {
        let fx = fixture();
        let err = apply_and_settle(&fx.inputs()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotApplicable);
        assert!(fx.unchanged(), "the old files must be back byte for byte");
        let journal = FileJournalStore::new(fx.journal_path());
        let stored = crate::journal::JournalStore::load(&journal).unwrap().unwrap();
        assert_eq!(stored.phase, Phase::RolledBack);
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
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(!fx.coordination.transaction_dir(&other).join(CANDIDATE_DIR).exists());
    }
}
