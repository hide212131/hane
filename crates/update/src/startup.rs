//! The startup gate: the first thing a launched application does.
//!
//! Before the AI service, any stateful editor object or an ordinary open
//! request exists, the process takes the `startup` lease, looks for a pending
//! transaction journal and, if there is one, hands it to a verified helper copy
//! of the *current* version. Only when nothing is pending (or recovery settled
//! it) does the process take the shared `runtime` lease, keep it until it ends,
//! and let the application start. A transaction that cannot be settled is an
//! explicit [`GateOutcome::RecoveryRequired`]; the application never falls
//! through to normal editing in that case.
//!
//! The gate is deliberately unaware of how files are replaced; it only reads
//! journals and delegates to a [`RecoveryRunner`].

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::helper::{HelloRequest, PREPARE_TIMEOUT, spawn_helper};
use crate::identity::TransactionId;
use crate::journal::{
    DiskView, FileJournalStore, FsDiskView, JournalStore, Phase, RecoveryDecision, UpdateJournal,
    decide_recovery,
};
use crate::platform::lease::{Coordination, Lease, LeaseKind, PRIVATE_DIR};
use crate::platform::{self, Placement};
use crate::protocol::{Mode, RecoveryReport, bounded_reason};
use crate::trust::TrustPolicy;

/// How long a normal launch waits for a helper that holds the startup gate.
pub const STARTUP_WAIT: Duration = Duration::from_secs(30);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(120);

/// Held for the rest of the process: the shared `runtime` lease.
#[derive(Debug)]
pub struct RuntimeGuard {
    _lease: Lease,
}

#[derive(Debug)]
pub enum GateOutcome {
    /// Start normally. The guard must be kept alive until the process ends.
    Proceed(RuntimeGuard),
    /// This placement cannot be updated and has no pending transaction
    /// (unsupported placement, or the coordination directory cannot be used).
    Unmanaged,
    /// A helper held the gate for the whole wait. Show "updating" and exit.
    UpdateInProgress,
    /// A transaction could not be settled. Never start normal editing.
    RecoveryRequired(String),
}

/// Delegates a pending transaction to a helper of the current version.
pub trait RecoveryRunner {
    fn recover(
        &self,
        placement: &Placement,
        coordination: &Coordination,
        transaction: &TransactionId,
    ) -> Result<RecoveryReport>;
}

/// Spawns the verified helper copy in recovery mode and waits for its verdict.
pub struct HelperRecoveryRunner {
    pub policy: TrustPolicy,
}

impl RecoveryRunner for HelperRecoveryRunner {
    fn recover(
        &self,
        placement: &Placement,
        coordination: &Coordination,
        transaction: &TransactionId,
    ) -> Result<RecoveryReport> {
        // The live install may be half replaced after a power loss, so its
        // helper is neither re-verified nor run; the copy staged before the
        // first mutation is.
        recover_with_staged_helper(coordination, transaction, |copy| {
            let mut spawned = spawn_helper(
                copy,
                &HelloRequest {
                    mode: Mode::Recover,
                    install_id: placement.install_id.clone(),
                    transaction_id: transaction.clone(),
                    candidate_id: None,
                },
                PREPARE_TIMEOUT,
            )?;
            // A helper that is slow is left alone: it may be mid-rollback, and
            // the next start settles whatever it leaves from the journal.
            let report = spawned.link.await_recovered(RECOVERY_TIMEOUT)?;
            let _ = spawned.child.wait();
            Ok(report)
        })
    }
}

/// Runs `run` with the transaction's staged helper copy, only after it is bound
/// to this install, transaction and the pending journal. Any missing, altered
/// or foreign piece is an error: recovery never falls back to the live install.
fn recover_with_staged_helper(
    coordination: &Coordination,
    transaction: &TransactionId,
    run: impl FnOnce(&Path) -> Result<RecoveryReport>,
) -> Result<RecoveryReport> {
    let journal = FileJournalStore::new(coordination.journal_path(transaction))
        .load()?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "the pending journal is missing"))?;
    if &journal.transaction_id != transaction || &journal.install_id != coordination.install_id() {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the journal belongs to another transaction or install",
        ));
    }
    let staged = platform::staged_helper(coordination, transaction)?;
    run(&staged)
}

enum Pending {
    Transaction(TransactionId),
    Stop(String),
}

fn journal_problem(name: &str, why: impl std::fmt::Display) -> Pending {
    Pending::Stop(bounded_reason(&format!("journal {name}: {why}")))
}

/// Finds a transaction that still needs a decision. Every unfinished journal is
/// inspected. Of the terminal journals only the newest is compared with the
/// disk: a `Committed` one must still see every step at its new identity, a
/// `RolledBack` one at its old identity (the rollback renames and the journal
/// rename may not have been flushed together, so a mixed or unknown install
/// stops instead of starting). Older terminal receipts were superseded by the
/// newer transaction. Unknown entries are ignored, never deleted.
fn scan_pending(coordination: &Coordination, disk: &dyn DiskView) -> io::Result<Option<Pending>> {
    let private = coordination.dir().join(PRIVATE_DIR);
    let entries = match fs::read_dir(&private) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut pending: Vec<TransactionId> = Vec::new();
    let mut terminal: Vec<Terminal> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(transaction) = TransactionId::parse(&name) else {
            continue;
        };
        let dir_meta = fs::symlink_metadata(entry.path())?;
        if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
            continue;
        }
        let store = FileJournalStore::new(coordination.journal_path(&transaction));
        let journal = match store.load() {
            Ok(Some(journal)) => journal,
            Ok(None) => continue,
            Err(err) => return Ok(Some(journal_problem(&name, err))),
        };
        if journal.transaction_id != transaction || &journal.install_id != coordination.install_id()
        {
            return Ok(Some(journal_problem(&name, "belongs to another transaction or install")));
        }
        match journal.phase {
            Phase::Committed | Phase::RolledBack => {
                let modified = fs::symlink_metadata(coordination.journal_path(&transaction))
                    .and_then(|meta| meta.modified())?;
                terminal.push(Terminal {
                    name,
                    journal,
                    modified,
                });
            }
            Phase::RecoveryRequired => {
                return Ok(Some(journal_problem(&name, "an earlier recovery did not complete")));
            }
            _ => pending.push(transaction),
        }
    }
    // Pending work is settled first, against its own journal. Terminal
    // receipts only matter once nothing is pending, and then only the newest.
    match pending.len() {
        0 => {}
        1 => return Ok(pending.pop().map(Pending::Transaction)),
        _ => return Ok(Some(Pending::Stop("more than one transaction is pending".to_owned()))),
    }
    Ok(check_latest_terminal(coordination, terminal, disk))
}

/// A `Committed` / `RolledBack` journal and when it last changed.
struct Terminal {
    name: String,
    journal: UpdateJournal,
    modified: SystemTime,
}

/// Only the newest terminal receipt describes the current install; older ones
/// were superseded by it and are not compared with the disk. Two receipts that
/// cannot be ordered stop instead of guessing.
fn check_latest_terminal(
    coordination: &Coordination,
    mut terminal: Vec<Terminal>,
    disk: &dyn DiskView,
) -> Option<Pending> {
    let newest = terminal.iter().map(|entry| entry.modified).max()?;
    terminal.retain(|entry| entry.modified == newest);
    if terminal.len() > 1 {
        return Some(Pending::Stop(
            "the latest terminal journal cannot be determined".to_owned(),
        ));
    }
    let latest = terminal.pop()?;
    check_terminal(coordination, &latest.name, &latest.journal, disk)
}

fn check_terminal(
    coordination: &Coordination,
    name: &str,
    journal: &UpdateJournal,
    disk: &dyn DiskView,
) -> Option<Pending> {
    let decision = decide_recovery(Some(journal), coordination.install_id(), disk);
    match (journal.phase, decision) {
        (Phase::Committed, Ok(RecoveryDecision::KeepNew))
        | (Phase::RolledBack, Ok(RecoveryDecision::KeepOld)) => None,
        (_, Ok(RecoveryDecision::RecoveryRequired(why))) => Some(journal_problem(name, why)),
        (Phase::Committed, Ok(other)) => Some(journal_problem(
            name,
            format!("committed but the install is not at the new version: {other:?}"),
        )),
        (_, Ok(other)) => Some(journal_problem(
            name,
            format!("rolled back but the install is not at the old version: {other:?}"),
        )),
        (_, Err(err)) => Some(journal_problem(name, err)),
    }
}

/// Before a new mutation: nothing may be pending and the newest terminal
/// receipt must still match the live install.
pub(crate) fn ensure_prior_receipts_current(
    coordination: &Coordination,
    disk: &dyn DiskView,
) -> Result<()> {
    match scan_pending(coordination, disk).map_err(UpdateError::from)? {
        None => Ok(()),
        Some(Pending::Stop(why)) => Err(UpdateError::new(ErrorCode::InstallModified, why)),
        Some(Pending::Transaction(_)) => Err(UpdateError::new(
            ErrorCode::InstallModified,
            "another transaction is still pending",
        )),
    }
}

/// Retires the terminal receipts of earlier transactions. Call only after the
/// new transaction's journal is durable: an interruption then leaves a pending
/// journal, which the gate recovers before it looks at any receipt. Removal is
/// best effort; a leftover receipt is ignored because the newest one wins.
pub(crate) fn prune_superseded_receipts(coordination: &Coordination, current: &TransactionId) {
    let private = coordination.dir().join(PRIVATE_DIR);
    let Ok(entries) = fs::read_dir(&private) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(transaction) = TransactionId::parse(&name) else {
            continue;
        };
        if &transaction == current {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let terminal = matches!(
            FileJournalStore::new(coordination.journal_path(&transaction)).load(),
            Ok(Some(journal))
                if journal.transaction_id == transaction
                    && &journal.install_id == coordination.install_id()
                    && matches!(journal.phase, Phase::Committed | Phase::RolledBack)
        );
        if terminal {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Runs the gate for a verified placement.
pub fn startup_gate(placement: &Placement, runner: &dyn RecoveryRunner, wait: Duration) -> GateOutcome {
    // Only a definite absence may start unmanaged. An existing entry that
    // cannot be verified (ACL, owner, link, I/O) may hold a journal or a live
    // helper, so it stops instead of skipping the gate.
    let coordination = match placement.existing_coordination() {
        Ok(Some(coordination)) => coordination,
        Ok(None) => match placement.coordination() {
            Ok(coordination) => coordination,
            // Creation failed: unmanaged only while the entry is still
            // definitely absent; anything that appeared meanwhile is unverified.
            Err(err) => {
                return match placement.existing_coordination() {
                    Ok(None) => GateOutcome::Unmanaged,
                    _ => stop(&err),
                };
            }
        },
        Err(err) => return stop(&err),
    };
    let disk = FsDiskView::new(&placement.install_root);
    // One recovery attempt: the second look must find nothing pending.
    for attempt in 0..2 {
        let startup = match coordination.acquire_within(LeaseKind::Startup, &[], wait) {
            Ok(lease) => lease,
            Err(err) if err.code == ErrorCode::Timeout => return GateOutcome::UpdateInProgress,
            Err(err) => return stop(&err),
        };
        match scan_pending(&coordination, &disk) {
            Err(err) => return stop(&UpdateError::from(err)),
            Ok(Some(Pending::Stop(why))) => return GateOutcome::RecoveryRequired(why),
            Ok(None) => {
                return match coordination.try_acquire(LeaseKind::RuntimeShared, &[&startup]) {
                    Ok(Some(lease)) => {
                        startup.release();
                        GateOutcome::Proceed(RuntimeGuard { _lease: lease })
                    }
                    Ok(None) => GateOutcome::UpdateInProgress,
                    Err(err) => stop(&err),
                };
            }
            Ok(Some(Pending::Transaction(transaction))) => {
                if attempt > 0 {
                    return GateOutcome::RecoveryRequired(
                        "recovery did not settle the pending transaction".to_owned(),
                    );
                }
                // The helper takes transaction -> startup -> runtime itself, so
                // this process must not hold the gate while it works.
                startup.release();
                match runner.recover(placement, &coordination, &transaction) {
                    Ok(RecoveryReport::Blocked(why) | RecoveryReport::RecoveryRequired(why)) => {
                        return GateOutcome::RecoveryRequired(bounded_reason(&why));
                    }
                    Ok(_) => {}
                    Err(err) => return stop(&err),
                }
            }
        }
    }
    GateOutcome::RecoveryRequired("recovery did not settle the pending transaction".to_owned())
}

fn stop(err: &UpdateError) -> GateOutcome {
    GateOutcome::RecoveryRequired(bounded_reason(&err.to_string()))
}

/// Convenience for the application: locate the running executable's placement,
/// then run the gate. Only a placement that is clearly unsupported is
/// `Unmanaged`; any other failure to resolve it (I/O errors, a partly missing
/// or inconsistent managed install) stops with `RecoveryRequired`.
pub fn startup_gate_for_exe(exe: &Path, runner: &dyn RecoveryRunner) -> GateOutcome {
    gate_for_located(platform::locate(exe), runner)
}

fn gate_for_located(located: Result<Placement>, runner: &dyn RecoveryRunner) -> GateOutcome {
    match located {
        Ok(placement) => startup_gate(&placement, runner, STARTUP_WAIT),
        Err(err) if matches!(err.code, ErrorCode::UnsupportedInstall | ErrorCode::TargetUnsupported) => {
            GateOutcome::Unmanaged
        }
        Err(err) => stop(&err),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::identity::{
        ApplyPlan, FileIdentity, InstallFacts, InstallId, PlanStep, ReleaseIdentity,
    };    use crate::journal::{DurableJournal, UpdateJournal};
    use crate::model::Target;
    use crate::testing::TempDir;

    fn install_id() -> InstallId {
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap()
    }

    /// The live install every test journal describes: `a.dll` at its old content.
    fn placement(dir: &TempDir) -> Placement {
        let root = dir.path().join("Hane");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.dll"), b"old").unwrap();
        Placement {
            target: Target::WindowsX64,
            install_root: dir.path().join("Hane"),
            coordination_parent: dir.path().to_path_buf(),
            install_id: install_id(),
        }
    }

    fn release(version: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            target: "x86_64-pc-windows-msvc".to_owned(),
            version: version.to_owned(),
            catalog_sha256: None,
            inventory_sha256: None,
            bundle_id: None,
            team_id: None,
        }
    }

    /// Writes a journal for `transaction` and advances it to `phase`.
    fn write_journal(coord: &Coordination, transaction: &TransactionId, phase: Phase) {
        let plan = ApplyPlan {
            install_id: install_id(),
            transaction_id: transaction.clone(),
            source: release("0.1.0"),
            destination: release("0.2.0"),
            steps: vec![PlanStep {
                rel_path: "a.dll".to_owned(),
                old: Some(FileIdentity::of(b"old")),
                new: Some(FileIdentity::of(b"new")),
                backup_rel: Some("backup/a.dll".to_owned()),
            }],
            unchanged: Vec::new(),
        };
        coord.ensure_helper_dir(transaction).unwrap();
        let journal = UpdateJournal::begin(&plan, None).unwrap();
        let mut durable = DurableJournal::create(
            FileJournalStore::new(coord.journal_path(transaction)),
            journal,
        )
        .unwrap();
        let path: &[Phase] = match phase {
            Phase::Prepared => &[],
            Phase::ExitAuthorized => &[Phase::ExitAuthorized],
            Phase::Applying => &[Phase::ExitAuthorized, Phase::Applying],
            Phase::RolledBack => &[Phase::RolledBack],
            Phase::Committed => &[
                Phase::ExitAuthorized,
                Phase::Applying,
                Phase::AwaitingHealth,
                Phase::Committed,
            ],
            Phase::RecoveryRequired => &[Phase::RecoveryRequired],
            other => panic!("unused test phase {other:?}"),
        };
        for next in path {
            durable.advance_phase(*next).unwrap();
        }
    }

    /// Records its calls; optionally settles the journal like a real helper.
    struct Runner {
        calls: Cell<usize>,
        startup_was_free: Cell<bool>,
        settle: bool,
        report: RefCell<Result<RecoveryReport>>,
    }

    impl Runner {
        fn new(settle: bool, report: Result<RecoveryReport>) -> Self {
            Self {
                calls: Cell::new(0),
                startup_was_free: Cell::new(false),
                settle,
                report: RefCell::new(report),
            }
        }
    }

    impl RecoveryRunner for Runner {
        fn recover(
            &self,
            _placement: &Placement,
            coordination: &Coordination,
            transaction: &TransactionId,
        ) -> Result<RecoveryReport> {
            self.calls.set(self.calls.get() + 1);
            // The helper must be able to take the gate: we may not hold it.
            let gate = coordination.try_acquire(LeaseKind::Startup, &[]).unwrap();
            self.startup_was_free.set(gate.is_some());
            drop(gate);
            if self.settle {
                let mut journal =
                    DurableJournal::open(FileJournalStore::new(coordination.journal_path(transaction)))
                        .unwrap()
                        .unwrap();
                // Rolling back passes through `RollingBack` once mutation began.
                let _ = journal.advance_phase(Phase::RollingBack);
                journal.advance_phase(Phase::RolledBack).unwrap();
            }
            self.report.borrow().clone()
        }
    }

    #[test]
    fn a_clean_install_proceeds_and_keeps_the_runtime_lease_for_its_lifetime() {
        let dir = TempDir::new("gate-clean");
        let placement = placement(&dir);
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        let GateOutcome::Proceed(guard) = startup_gate(&placement, &runner, Duration::from_millis(50))
        else {
            panic!("a clean install must proceed");
        };
        assert_eq!(runner.calls.get(), 0);
        let coord = placement.coordination().unwrap();
        assert!(
            coord.try_acquire(LeaseKind::Startup, &[]).unwrap().is_some(),
            "the gate is released once the runtime lease is held"
        );
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_none(),
            "a live process keeps the runtime lease"
        );
        // A second launch of the same install shares the runtime lease.
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::Proceed(_)
        ));
        drop(guard);
    }

    #[test]
    fn a_held_gate_makes_a_launch_wait_and_report_update_in_progress() {
        let dir = TempDir::new("gate-held");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        let _helper = coord.try_acquire(LeaseKind::Startup, &[]).unwrap().unwrap();
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(60)),
            GateOutcome::UpdateInProgress
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn terminal_journals_need_no_recovery() {
        let dir = TempDir::new("gate-terminal");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::RolledBack);
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::Proceed(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    /// Journals `a.dll` from `from` to `to`, advanced to `phase`, with its file
    /// time set `age_secs` after a fixed origin so receipts are ordered.
    fn write_versioned(
        coord: &Coordination,
        transaction: &TransactionId,
        (from, to): (&str, &str),
        phase: Phase,
        age_secs: u64,
    ) {
        let plan = ApplyPlan {
            install_id: install_id(),
            transaction_id: transaction.clone(),
            source: release(from),
            destination: release(to),
            steps: vec![PlanStep {
                rel_path: "a.dll".to_owned(),
                old: Some(FileIdentity::of(from.as_bytes())),
                new: Some(FileIdentity::of(to.as_bytes())),
                backup_rel: Some("backup/a.dll".to_owned()),
            }],
            unchanged: Vec::new(),
        };
        coord.ensure_helper_dir(transaction).unwrap();
        let mut durable = DurableJournal::create(
            FileJournalStore::new(coord.journal_path(transaction)),
            UpdateJournal::begin(&plan, None).unwrap(),
        )
        .unwrap();
        let path: &[Phase] = match phase {
            Phase::Applying => &[Phase::ExitAuthorized, Phase::Applying],
            Phase::Committed => &[
                Phase::ExitAuthorized,
                Phase::Applying,
                Phase::AwaitingHealth,
                Phase::Committed,
            ],
            other => panic!("unused test phase {other:?}"),
        };
        for next in path {
            durable.advance_phase(*next).unwrap();
        }
        set_age(coord, transaction, age_secs);
    }

    fn set_age(coord: &Coordination, transaction: &TransactionId, age_secs: u64) {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(coord.journal_path(transaction))
            .unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + age_secs))
            .unwrap();
    }

    #[test]
    fn a_later_update_is_not_blocked_by_an_older_committed_receipt() {
        let dir = TempDir::new("gate-multi-version");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        // v1 -> v2 committed, then v2 -> v3 committed; the disk is at v3.
        write_versioned(&coord, &TransactionId::generate().unwrap(), ("v1", "v2"), Phase::Committed, 0);
        let newest = TransactionId::generate().unwrap();
        write_versioned(&coord, &newest, ("v2", "v3"), Phase::Committed, 10);
        fs::write(placement.install_root.join("a.dll"), b"v3").unwrap();
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::Proceed(_)
        ));
        assert_eq!(runner.calls.get(), 0);

        // The newest receipt still gates: a disk that is not at v3 stops.
        fs::write(placement.install_root.join("a.dll"), b"v2").unwrap();
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn receipts_that_cannot_be_ordered_stop() {
        let dir = TempDir::new("gate-unordered");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        write_versioned(&coord, &TransactionId::generate().unwrap(), ("v1", "v2"), Phase::Committed, 5);
        write_versioned(&coord, &TransactionId::generate().unwrap(), ("v2", "v3"), Phase::Committed, 5);
        fs::write(placement.install_root.join("a.dll"), b"v3").unwrap();
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
    }

    #[test]
    fn a_pending_transaction_beside_an_older_receipt_is_recovered_first() {
        // v1 -> v2 committed (disk at v2), then v2 -> v3 was cut short while
        // applying: the older receipt must not hide or pre-empt the recovery.
        for settle in [true, false] {
            let dir = TempDir::new("gate-pending-after-commit");
            let placement = placement(&dir);
            let coord = placement.coordination().unwrap();
            write_versioned(&coord, &TransactionId::generate().unwrap(), ("v1", "v2"), Phase::Committed, 0);
            let pending = TransactionId::generate().unwrap();
            write_versioned(&coord, &pending, ("v2", "v3"), Phase::Applying, 10);
            // Mid-apply the disk matches neither receipt's committed state.
            fs::write(placement.install_root.join("a.dll"), b"v3").unwrap();
            let runner = Runner::new(settle, Ok(RecoveryReport::RolledBack));
            if settle {
                // The rollback restores v2, the older receipt's version.
                fs::write(placement.install_root.join("a.dll"), b"v2").unwrap();
            }
            let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
            assert_eq!(runner.calls.get(), 1, "settle={settle}: pending journal reached recovery");
            if settle {
                assert!(matches!(outcome, GateOutcome::Proceed(_)), "{outcome:?}");
            } else {
                assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)), "{outcome:?}");
            }
        }
    }

    #[test]
    fn pruning_retires_only_earlier_terminal_receipts() {
        let dir = TempDir::new("gate-prune");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        let old_committed = TransactionId::generate().unwrap();
        write_versioned(&coord, &old_committed, ("v1", "v2"), Phase::Committed, 0);
        let other_pending = TransactionId::generate().unwrap();
        write_versioned(&coord, &other_pending, ("v2", "v3"), Phase::Applying, 5);
        let current = TransactionId::generate().unwrap();
        write_versioned(&coord, &current, ("v2", "v3"), Phase::Applying, 10);

        prune_superseded_receipts(&coord, &current);
        assert!(!coord.transaction_dir(&old_committed).exists());
        assert!(coord.journal_path(&other_pending).exists(), "unfinished work is never pruned");
        assert!(coord.journal_path(&current).exists());
    }

    #[test]
    fn a_committed_journal_needs_every_file_at_its_new_identity() {
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        type Disk = fn(&std::path::Path);
        let cases: [(&str, Disk, bool); 5] = [
            ("new content", |root| fs::write(root.join("a.dll"), b"new").unwrap(), true),
            ("old content", |_| {}, false),
            ("unknown content", |root| fs::write(root.join("a.dll"), b"???").unwrap(), false),
            ("missing file", |root| fs::remove_file(root.join("a.dll")).unwrap(), false),
            (
                "unreadable (directory)",
                |root| {
                    fs::remove_file(root.join("a.dll")).unwrap();
                    fs::create_dir(root.join("a.dll")).unwrap();
                },
                false,
            ),
        ];
        for (name, prepare, proceeds) in cases {
            let dir = TempDir::new("gate-committed");
            let placement = placement(&dir);
            let coord = placement.coordination().unwrap();
            write_journal(&coord, &TransactionId::generate().unwrap(), Phase::Committed);
            prepare(&placement.install_root);
            let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
            if proceeds {
                assert!(matches!(outcome, GateOutcome::Proceed(_)), "{name}: {outcome:?}");
            } else {
                assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)), "{name}: {outcome:?}");
                assert!(
                    coord
                        .try_acquire(LeaseKind::RuntimeExclusive, &[])
                        .unwrap()
                        .is_some(),
                    "{name}: normal editing never started"
                );
            }
        }
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn a_rolled_back_journal_needs_every_file_at_its_old_identity() {
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        type Damage = fn(&std::path::Path);
        let cases: [(&str, Damage); 4] = [
            ("new content", |root| fs::write(root.join("a.dll"), b"new").unwrap()),
            ("unknown content", |root| fs::write(root.join("a.dll"), b"???").unwrap()),
            ("missing file", |root| fs::remove_file(root.join("a.dll")).unwrap()),
            ("unreadable (directory)", |root| {
                fs::remove_file(root.join("a.dll")).unwrap();
                fs::create_dir(root.join("a.dll")).unwrap();
            }),
        ];
        for (name, damage) in cases {
            let dir = TempDir::new("gate-rolled-back-mixed");
            let placement = placement(&dir);
            let coord = placement.coordination().unwrap();
            write_journal(&coord, &TransactionId::generate().unwrap(), Phase::RolledBack);
            damage(&placement.install_root);
            let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
            assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)), "{name}: {outcome:?}");
            assert!(
                coord
                    .try_acquire(LeaseKind::RuntimeExclusive, &[])
                    .unwrap()
                    .is_some(),
                "{name}: normal editing never started"
            );
        }
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn a_rolled_back_journal_with_an_added_file_present_stops() {
        // A step that only adds a file: the old version had no such path.
        let dir = TempDir::new("gate-rolled-back-added");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        let transaction = TransactionId::generate().unwrap();
        let plan = ApplyPlan {
            install_id: install_id(),
            transaction_id: transaction.clone(),
            source: release("0.1.0"),
            destination: release("0.2.0"),
            steps: vec![PlanStep {
                rel_path: "added.dll".to_owned(),
                old: None,
                new: Some(FileIdentity::of(b"new")),
                backup_rel: None,
            }],
            unchanged: Vec::new(),
        };
        coord.ensure_helper_dir(&transaction).unwrap();
        let journal = UpdateJournal::begin(&plan, None).unwrap();
        let mut durable = DurableJournal::create(
            FileJournalStore::new(coord.journal_path(&transaction)),
            journal,
        )
        .unwrap();
        durable.advance_phase(Phase::RolledBack).unwrap();
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));

        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::Proceed(_)
        ));
        fs::write(placement.install_root.join("added.dll"), b"new").unwrap();
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn a_pending_journal_is_recovered_before_the_application_starts() {
        for phase in [Phase::Prepared, Phase::ExitAuthorized, Phase::Applying] {
            let dir = TempDir::new("gate-pending");
            let placement = placement(&dir);
            let coord = placement.coordination().unwrap();
            let transaction = TransactionId::generate().unwrap();
            write_journal(&coord, &transaction, phase);
            let runner = Runner::new(true, Ok(RecoveryReport::RolledBack));
            let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
            assert!(matches!(outcome, GateOutcome::Proceed(_)), "{phase:?}: {outcome:?}");
            assert_eq!(runner.calls.get(), 1, "{phase:?}");
            assert!(runner.startup_was_free.get(), "{phase:?}: the gate must be released first");
        }
    }

    #[test]
    fn an_unsettled_or_failed_recovery_never_starts_normal_editing() {
        let dir = TempDir::new("gate-unsettled");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::Applying);

        // The helper claims success but the journal is still pending.
        let claims = Runner::new(false, Ok(RecoveryReport::RolledBack));
        assert!(matches!(
            startup_gate(&placement, &claims, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        assert_eq!(claims.calls.get(), 1);

        let blocked = Runner::new(false, Ok(RecoveryReport::Blocked("other Hane".to_owned())));
        assert!(matches!(
            startup_gate(&placement, &blocked, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));

        let required = Runner::new(false, Ok(RecoveryReport::RecoveryRequired("bad".to_owned())));
        assert!(matches!(
            startup_gate(&placement, &required, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));

        let failing = Runner::new(
            false,
            Err(UpdateError::new(ErrorCode::Io, "helper failed to start")),
        );
        assert!(matches!(
            startup_gate(&placement, &failing, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        // After every stop the runtime lease is free: no editor ran.
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn recovery_required_corrupt_and_foreign_journals_stop_without_calling_the_helper() {
        let runner = Runner::new(true, Ok(RecoveryReport::RolledBack));

        let dir = TempDir::new("gate-required");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::RecoveryRequired);
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));

        let dir = TempDir::new("gate-corrupt");
        let placement = placement_of(&dir);
        let coord = placement.coordination().unwrap();
        let transaction = TransactionId::generate().unwrap();
        coord.ensure_helper_dir(&transaction).unwrap();
        fs::write(coord.journal_path(&transaction), b"{ not a journal").unwrap();
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));

        // A journal filed under another transaction's directory is not trusted.
        let dir = TempDir::new("gate-foreign");
        let placement = placement_of(&dir);
        let coord = placement.coordination().unwrap();
        let real = TransactionId::generate().unwrap();
        write_journal(&coord, &real, Phase::Prepared);
        let other = TransactionId::generate().unwrap();
        coord.ensure_helper_dir(&other).unwrap();
        fs::copy(coord.journal_path(&real), coord.journal_path(&other)).unwrap();
        fs::remove_dir_all(coord.transaction_dir(&real)).unwrap();
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    fn placement_of(dir: &TempDir) -> Placement {
        placement(dir)
    }

    #[test]
    fn two_pending_transactions_stop_instead_of_guessing() {
        let dir = TempDir::new("gate-two");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::Prepared);
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::Prepared);
        let runner = Runner::new(true, Ok(RecoveryReport::RolledBack));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn an_untrusted_coordination_entry_is_not_adopted_and_stops() {
        let dir = TempDir::new("gate-foreign-dir");
        let placement = placement(&dir);
        fs::write(
            dir.path().join(install_id().coordination_dir_name()),
            b"not a directory",
        )
        .unwrap();
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::RecoveryRequired(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn an_absent_coordination_directory_is_created_and_proceeds() {
        let dir = TempDir::new("gate-absent");
        let placement = placement(&dir);
        assert!(placement.existing_coordination().unwrap().is_none());
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::Proceed(_)
        ));
        assert_eq!(runner.calls.get(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_coordination_directory_that_fails_verification_with_a_pending_journal_stops() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("gate-unverifiable");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::Applying);
        // Group/other-writable: the owner-only verification must reject it.
        fs::set_permissions(coord.dir(), fs::Permissions::from_mode(0o777)).unwrap();

        let runner = Runner::new(true, Ok(RecoveryReport::RolledBack));
        let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
        assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)), "{outcome:?}");
        assert_eq!(runner.calls.get(), 0, "no helper may run on an unverified directory");

        // No runtime lease was taken, so no editing session exists.
        fs::set_permissions(coord.dir(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            coord
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_some(),
            "normal editing never started"
        );
    }

    #[test]
    fn recovery_of_a_transaction_without_a_journal_reports_nothing() {
        let dir = TempDir::new("gate-nojournal");
        let placement = placement(&dir);
        let coord = placement.coordination().unwrap();
        let report =
            crate::helper::recover_pending(&coord, &TransactionId::generate().unwrap()).unwrap();
        assert_eq!(report, RecoveryReport::Nothing);
    }

    /// Recovers like `HelperRecoveryRunner`, with the process spawn replaced by
    /// a rollback that records which helper bytes it was given.
    struct StagedRunner {
        ran_with: RefCell<Option<Vec<u8>>>,
    }

    impl RecoveryRunner for StagedRunner {
        fn recover(
            &self,
            _placement: &Placement,
            coordination: &Coordination,
            transaction: &TransactionId,
        ) -> Result<RecoveryReport> {
            recover_with_staged_helper(coordination, transaction, |copy| {
                *self.ran_with.borrow_mut() = Some(fs::read(copy).unwrap());
                let mut journal =
                    DurableJournal::open(FileJournalStore::new(coordination.journal_path(transaction)))
                        .unwrap()
                        .unwrap();
                let _ = journal.advance_phase(Phase::RollingBack);
                journal.advance_phase(Phase::RolledBack).unwrap();
                Ok(RecoveryReport::RolledBack)
            })
        }
    }

    /// An interrupted update: the helper was staged and verified against the
    /// old version, the journal is mid-apply, and then the live helper was
    /// replaced (or lost) by the update that the power loss cut short.
    fn interrupted(dir: &TempDir, phase: Phase) -> (Placement, Coordination, TransactionId) {
        let placement = placement(dir);
        let coord = placement.coordination().unwrap();
        let transaction = TransactionId::generate().unwrap();
        let live = dir.path().join(format!("live-helper-{}", transaction.as_str()));
        fs::write(&live, b"old-helper").unwrap();
        let source = crate::platform::HelperSource {
            path: live.clone(),
            identity: FileIdentity::of(b"old-helper"),
        };
        crate::platform::stage_helper_copy(&coord, &transaction, &source).unwrap();
        write_journal(&coord, &transaction, phase);
        fs::write(&live, b"half-written-new-helper").unwrap();
        (placement, coord, transaction)
    }

    fn staged_path(coord: &Coordination, transaction: &TransactionId) -> std::path::PathBuf {
        coord
            .transaction_dir(transaction)
            .join(crate::platform::lease::HELPER_DIR)
            .join(crate::platform::HELPER_FILE_NAME)
    }

    #[test]
    fn a_mixed_live_install_is_rolled_back_by_the_staged_helper() {
        for phase in [Phase::ExitAuthorized, Phase::Applying] {
            let dir = TempDir::new("gate-staged");
            let (placement, coord, _transaction) = interrupted(&dir, phase);
            let runner = StagedRunner {
                ran_with: RefCell::new(None),
            };
            let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
            assert!(matches!(outcome, GateOutcome::Proceed(_)), "{phase:?}: {outcome:?}");
            assert_eq!(
                runner.ran_with.borrow().as_deref(),
                Some(&b"old-helper"[..]),
                "{phase:?}: the verified pre-mutation copy ran, not the live file"
            );
            drop(outcome);
            drop(coord);
        }
    }

    #[test]
    fn a_missing_altered_or_foreign_staged_helper_is_recovery_required() {
        type Tamper = fn(&Coordination, &TransactionId);
        let cases: [(&str, Tamper); 6] = [
            ("helper missing", |coord, txn| {
                fs::remove_file(staged_path(coord, txn)).unwrap();
            }),
            ("helper altered", |coord, txn| {
                fs::write(staged_path(coord, txn), b"evil-helper").unwrap();
            }),
            ("record missing", |coord, txn| {
                fs::remove_file(coord.transaction_dir(txn).join("helper.identity")).unwrap();
            }),
            ("record garbled", |coord, txn| {
                fs::write(coord.transaction_dir(txn).join("helper.identity"), b"garbage").unwrap();
            }),
            ("record for another transaction", |coord, txn| {
                let other = TransactionId::generate().unwrap();
                let text = format!(
                    "{}\n{}\n{}\n{}\n",
                    coord.install_id().as_str(),
                    other.as_str(),
                    FileIdentity::of(b"old-helper").size,
                    FileIdentity::of(b"old-helper").sha256
                );
                fs::write(coord.transaction_dir(txn).join("helper.identity"), text).unwrap();
            }),
            ("record for another helper", |coord, txn| {
                let text = format!(
                    "{}\n{}\n{}\n{}\n",
                    coord.install_id().as_str(),
                    txn.as_str(),
                    FileIdentity::of(b"evil-helper").size,
                    FileIdentity::of(b"evil-helper").sha256
                );
                fs::write(coord.transaction_dir(txn).join("helper.identity"), text).unwrap();
            }),
        ];
        for (name, tamper) in cases {
            let dir = TempDir::new("gate-staged-bad");
            let (placement, coord, transaction) = interrupted(&dir, Phase::Applying);
            tamper(&coord, &transaction);
            let runner = StagedRunner {
                ran_with: RefCell::new(None),
            };
            let outcome = startup_gate(&placement, &runner, Duration::from_millis(50));
            assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)), "{name}: {outcome:?}");
            assert!(runner.ran_with.borrow().is_none(), "{name}: no helper may run");
            assert!(
                coord
                    .try_acquire(LeaseKind::RuntimeExclusive, &[])
                    .unwrap()
                    .is_some(),
                "{name}: normal editing never started"
            );
        }
    }

    #[test]
    fn a_staged_helper_is_not_used_without_its_journal() {
        let dir = TempDir::new("gate-staged-journal");
        let (_placement, coord, transaction) = interrupted(&dir, Phase::Applying);
        // The journal is gone: nothing ties the staged copy to a transaction.
        fs::remove_file(coord.journal_path(&transaction)).unwrap();
        let err = recover_with_staged_helper(&coord, &transaction, |_| {
            panic!("no helper may run without its journal")
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn only_a_clearly_unsupported_placement_is_unmanaged() {
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        for code in [ErrorCode::UnsupportedInstall, ErrorCode::TargetUnsupported] {
            assert!(
                matches!(
                    gate_for_located(Err(UpdateError::new(code, "no")), &runner),
                    GateOutcome::Unmanaged
                ),
                "{code:?}"
            );
        }
        for code in [ErrorCode::Io, ErrorCode::InstallModified, ErrorCode::IdentityMismatch] {
            assert!(
                matches!(
                    gate_for_located(Err(UpdateError::new(code, "partial")), &runner),
                    GateOutcome::RecoveryRequired(_)
                ),
                "{code:?}"
            );
        }
        // Normal editing is never started: no helper ran and no lease is held.
        assert_eq!(runner.calls.get(), 0);
    }

    #[test]
    fn a_failed_or_empty_principal_fails_closed_not_unmanaged() {
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        let failures = [
            platform::owner_from(Err(UpdateError::new(
                ErrorCode::UnsupportedInstall,
                "token query failed",
            ))),
            platform::owner_from(Ok(String::new())),
        ];
        for failure in failures {
            let err = failure.unwrap_err();
            assert!(
                matches!(
                    gate_for_located(Err(err), &runner),
                    GateOutcome::RecoveryRequired(_)
                ),
                "principal failure must not start normal editing"
            );
        }
        assert_eq!(runner.calls.get(), 0);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn an_unsupported_placement_starts_unmanaged() {
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate_for_exe(Path::new("/opt/hane/hane"), &runner),
            GateOutcome::Unmanaged
        ));
    }
}
