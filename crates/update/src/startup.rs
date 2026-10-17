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
use std::time::Duration;

use crate::error::{ErrorCode, Result, UpdateError};
use crate::helper::{HelloRequest, PREPARE_TIMEOUT, spawn_helper};
use crate::identity::TransactionId;
use crate::journal::{FileJournalStore, JournalStore, Phase};
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

/// Finds a transaction that still needs a decision. Terminal `Committed` and
/// `RolledBack` journals need none; unknown entries are ignored, never deleted.
fn scan_pending(coordination: &Coordination) -> io::Result<Option<Pending>> {
    let private = coordination.dir().join(PRIVATE_DIR);
    let entries = match fs::read_dir(&private) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut pending: Vec<TransactionId> = Vec::new();
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
            Phase::Committed | Phase::RolledBack => {}
            Phase::RecoveryRequired => {
                return Ok(Some(journal_problem(&name, "an earlier recovery did not complete")));
            }
            _ => pending.push(transaction),
        }
    }
    Ok(match pending.len() {
        0 => None,
        1 => pending.pop().map(Pending::Transaction),
        _ => Some(Pending::Stop("more than one transaction is pending".to_owned())),
    })
}

/// Runs the gate for a verified placement.
pub fn startup_gate(placement: &Placement, runner: &dyn RecoveryRunner, wait: Duration) -> GateOutcome {
    // An existing directory we cannot trust (link, foreign file) is not used
    // and cannot hold our journals; a directory that cannot be created means
    // updates are unavailable, not that editing is.
    let coordination = match placement.existing_coordination() {
        Ok(Some(coordination)) => coordination,
        Ok(None) => match placement.coordination() {
            Ok(coordination) => coordination,
            Err(_) => return GateOutcome::Unmanaged,
        },
        Err(_) => return GateOutcome::Unmanaged,
    };
    // One recovery attempt: the second look must find nothing pending.
    for attempt in 0..2 {
        let startup = match coordination.acquire_within(LeaseKind::Startup, &[], wait) {
            Ok(lease) => lease,
            Err(err) if err.code == ErrorCode::Timeout => return GateOutcome::UpdateInProgress,
            Err(err) => return stop(&err),
        };
        match scan_pending(&coordination) {
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

    fn placement(dir: &TempDir) -> Placement {
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
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::Committed);
        write_journal(&coord, &TransactionId::generate().unwrap(), Phase::RolledBack);
        let runner = Runner::new(false, Ok(RecoveryReport::Nothing));
        assert!(matches!(
            startup_gate(&placement, &runner, Duration::from_millis(50)),
            GateOutcome::Proceed(_)
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
    fn an_untrusted_coordination_entry_is_not_adopted() {
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
            GateOutcome::Unmanaged
        ));
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
