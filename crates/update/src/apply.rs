//! Journaled apply, health conclusion and rollback for an owned file set.
//!
//! This is the OS-neutral engine the platform helper drives (Issue #510, U4).
//! It never decides *what* to change: every path and identity comes from the
//! journal built from a signed [`crate::identity::ApplyPlan`]. It only changes
//! files through the [`FileOps`] seam, writes the journal intent durably before
//! each mutation, and uses the disk (not the journal) as the evidence when it
//! rolls back. Anything that is neither the recorded old nor new content stops
//! the process with `RecoveryRequired`; nothing is overwritten on a guess.
//!
//! Not provided here: the private IPC, the real process lease / parent handle,
//! the Windows and macOS adapters, and any claim about real power-loss
//! durability. Those need a real OS and are verified separately.

use std::path::{Path, PathBuf};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, hash_regular_file_under, read_limited};
use crate::identity::FileIdentity;
use crate::journal::{
    DiskView, DurableJournal, JournalStore, Observed, Phase, RecoveryDecision, RollbackAction,
    StepState, decide_recovery, identity_matches,
};
use crate::limits::MAX_ENTRY_BYTES;
use crate::paths::validate_rel_path;

/// Backups are addressed as `backup/<path>`; no owned file may live there.
pub const BACKUP_PREFIX: &str = "backup/";

/// File mutation seam. Each call replaces or removes exactly one named file and
/// must be atomic: afterwards the file has the old or the new content, never a
/// mixture.
pub trait FileOps: DiskView {
    /// Copies the installed file to `backup_rel`.
    fn backup(&mut self, rel_path: &str, backup_rel: &str) -> Result<()>;
    /// Places the staged file whose identity is `expected` at `rel_path`.
    fn install_new(&mut self, rel_path: &str, expected: &FileIdentity) -> Result<()>;
    fn remove(&mut self, rel_path: &str) -> Result<()>;
    /// Puts the backup (whose identity is `expected`) back at `rel_path`.
    fn restore(&mut self, rel_path: &str, backup_rel: &str, expected: &FileIdentity) -> Result<()>;
}

/// The old application process, observed through the OS process lifetime.
pub trait ParentProcess {
    fn has_exited(&self) -> Result<bool>;
}

/// The limited-health child and its runtime lease. `stop_and_release_lease`
/// must be idempotent and may only return `Ok` once the child is gone and the
/// lease is released.
pub trait HealthChild {
    fn stop_and_release_lease(&mut self) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Every file is replaced; the limited-health child may now be started.
    AwaitingHealth,
    /// A step failed and the old version was restored.
    RolledBack(UpdateError),
    RecoveryRequired(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthOutcome {
    /// `Committed` is durable; normal startup and the success display may follow.
    Committed,
    RolledBack,
    RecoveryRequired(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    Nothing,
    KeptOld,
    KeptNew,
    RolledBack,
    RecoveryRequired(String),
}

/// Replaces the owned files in journal order. Nothing is changed while the
/// parent process is alive or any file differs from its recorded old state.
pub fn apply<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
    parent: &dyn ParentProcess,
    child: &mut dyn HealthChild,
) -> Result<ApplyOutcome> {
    if !parent.has_exited()? {
        return Err(UpdateError::new(
            ErrorCode::NotApplicable,
            "the application process is still running",
        ));
    }
    if journal.journal().phase != Phase::Prepared {
        return Err(UpdateError::new(
            ErrorCode::TransitionInvalid,
            "apply requires a freshly prepared journal",
        ));
    }
    let steps = journal.journal().steps.clone();
    for step in &steps {
        if step.rel_path.starts_with(BACKUP_PREFIX) || step.state != StepState::Planned {
            return Err(UpdateError::new(
                ErrorCode::JournalInvalid,
                format!("{} cannot be applied", step.rel_path),
            ));
        }
        if !identity_matches(&ops.observe(&step.rel_path)?, &step.old) {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not in its installed state", step.rel_path),
            ));
        }
    }
    journal.advance_phase(Phase::ExitAuthorized)?;
    journal.advance_phase(Phase::Applying)?;
    match run_steps(journal, ops) {
        Ok(()) => Ok(ApplyOutcome::AwaitingHealth),
        Err(err) => Ok(match recover(journal, ops, child)? {
            RecoveryOutcome::RecoveryRequired(why) => ApplyOutcome::RecoveryRequired(why),
            RecoveryOutcome::RolledBack | RecoveryOutcome::KeptOld => ApplyOutcome::RolledBack(err),
            other => ApplyOutcome::RecoveryRequired(format!("unexpected recovery result {other:?}")),
        }),
    }
}

fn run_steps<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
) -> Result<()> {
    let steps = journal.journal().steps.clone();
    for (index, step) in steps.iter().enumerate() {
        // The intent is durable before the file is touched.
        journal.advance_step(index, StepState::Intent)?;
        if let (Some(_), Some(backup_rel)) = (&step.old, &step.backup_rel) {
            ops.backup(&step.rel_path, backup_rel)?;
        }
        match &step.new {
            Some(identity) => ops.install_new(&step.rel_path, identity)?,
            None => ops.remove(&step.rel_path)?,
        }
        journal.advance_step(index, StepState::Applied)?;
    }
    for step in &steps {
        if !identity_matches(&ops.observe(&step.rel_path)?, &step.new) {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("{} does not have the new content after replacement", step.rel_path),
            ));
        }
    }
    journal.advance_phase(Phase::AwaitingHealth)
}

/// Concludes the limited-health run. Only a durable `Committed` returns
/// [`HealthOutcome::Committed`]; any other path stops the child and rolls back.
pub fn conclude_health<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
    child: &mut dyn HealthChild,
    healthy: bool,
) -> Result<HealthOutcome> {
    if journal.journal().phase != Phase::AwaitingHealth {
        return Err(UpdateError::new(
            ErrorCode::TransitionInvalid,
            "health can only be concluded while awaiting health",
        ));
    }
    if healthy && journal.advance_phase(Phase::Committed).is_ok() {
        return Ok(HealthOutcome::Committed);
    }
    Ok(match recover(journal, ops, child)? {
        RecoveryOutcome::RecoveryRequired(why) => HealthOutcome::RecoveryRequired(why),
        RecoveryOutcome::RolledBack | RecoveryOutcome::KeptOld => HealthOutcome::RolledBack,
        other => HealthOutcome::RecoveryRequired(format!("unexpected recovery result {other:?}")),
    })
}

/// Decides from the journal and the actual disk, then acts. Used both after a
/// failed step and at startup before anything else is initialized.
pub fn recover<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
    child: &mut dyn HealthChild,
) -> Result<RecoveryOutcome> {
    let current = journal.journal().clone();
    let decision = decide_recovery(Some(&current), &current.install_id, &*ops)?;
    match decision {
        RecoveryDecision::NoJournal => Ok(RecoveryOutcome::Nothing),
        RecoveryDecision::KeepNew => Ok(RecoveryOutcome::KeptNew),
        RecoveryDecision::KeepOld => {
            settle_old(journal)?;
            Ok(RecoveryOutcome::KeptOld)
        }
        RecoveryDecision::RecoveryRequired(why) => Ok(require(journal, why)),
        RecoveryDecision::RollBack {
            actions,
            require_child_exit,
        } => {
            if require_child_exit && let Err(err) = child.stop_and_release_lease() {
                return Ok(require(
                    journal,
                    format!("the health child could not be stopped: {err}"),
                ));
            }
            let phase = journal.journal().phase;
            match phase {
                Phase::Applying | Phase::AwaitingHealth => {
                    journal.advance_phase(Phase::RollingBack)?;
                }
                Phase::RollingBack => {}
                other => {
                    return Ok(require(journal, format!("cannot roll back from {other:?}")));
                }
            }
            for action in actions.iter().rev() {
                if let Some(why) = perform(journal, ops, action)? {
                    return Ok(require(journal, why));
                }
            }
            let after = journal.journal().clone();
            match decide_recovery(Some(&after), &after.install_id, &*ops)? {
                RecoveryDecision::KeepOld => {}
                other => {
                    return Ok(require(
                        journal,
                        format!("the disk is not at the old version after rollback: {other:?}"),
                    ));
                }
            }
            journal.advance_phase(Phase::RolledBack)?;
            Ok(RecoveryOutcome::RolledBack)
        }
    }
}

fn require<S: JournalStore>(journal: &mut DurableJournal<S>, why: String) -> RecoveryOutcome {
    // Terminal phases refuse the move; the explicit state is then already set.
    let _ = journal.advance_phase(Phase::RecoveryRequired);
    RecoveryOutcome::RecoveryRequired(why)
}

fn settle_old<S: JournalStore>(journal: &mut DurableJournal<S>) -> Result<()> {
    let phase = journal.journal().phase;
    match phase {
        Phase::Prepared | Phase::ExitAuthorized => journal.advance_phase(Phase::RolledBack),
        Phase::Applying | Phase::AwaitingHealth => {
            journal.advance_phase(Phase::RollingBack)?;
            journal.advance_phase(Phase::RolledBack)
        }
        Phase::RollingBack => journal.advance_phase(Phase::RolledBack),
        Phase::RolledBack | Phase::Committed | Phase::RecoveryRequired => Ok(()),
    }
}

/// Re-checks the file right before acting; `Ok(Some(why))` means stop.
fn perform<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
    action: &RollbackAction,
) -> Result<Option<String>> {
    let rel_path = match action {
        RollbackAction::RestoreFromBackup {
            rel_path,
            backup_rel,
            restore,
            expected_current,
        } => {
            if !identity_matches(&ops.observe(rel_path)?, expected_current) {
                return Ok(Some(format!("{rel_path} changed during recovery")));
            }
            if ops.observe(backup_rel)? != Observed::File(restore.clone()) {
                return Ok(Some(format!("backup of {rel_path} is missing or damaged")));
            }
            ops.restore(rel_path, backup_rel, restore)?;
            rel_path
        }
        RollbackAction::RemoveAdded { rel_path, expected } => {
            if ops.observe(rel_path)? != Observed::File(expected.clone()) {
                return Ok(Some(format!("{rel_path} changed during recovery")));
            }
            ops.remove(rel_path)?;
            rel_path
        }
    };
    let Some(index) = journal
        .journal()
        .steps
        .iter()
        .position(|step| &step.rel_path == rel_path)
    else {
        return Ok(Some(format!("{rel_path} is not part of the journal")));
    };
    let state = journal.journal().steps[index].state;
    match state {
        StepState::Intent | StepState::Applied => {
            journal.advance_step(index, StepState::Restored)?;
        }
        StepState::Restored => {}
        StepState::Planned => {
            return Ok(Some(format!("{rel_path} was changed without a recorded intent")));
        }
    }
    Ok(None)
}

/// Real-filesystem [`FileOps`]. `root` is the install root, `backup_root` holds
/// `backup/<path>` copies and `stage_root` holds the verified new files.
pub struct FsFileOps {
    root: PathBuf,
    backup_root: PathBuf,
    stage_root: PathBuf,
}

impl FsFileOps {
    pub fn new(root: PathBuf, backup_root: PathBuf, stage_root: PathBuf) -> Self {
        Self {
            root,
            backup_root,
            stage_root,
        }
    }

    fn locate<'a>(&'a self, rel_path: &'a str) -> Result<(&'a Path, &'a str)> {
        validate_rel_path(rel_path)?;
        Ok(match rel_path.strip_prefix(BACKUP_PREFIX) {
            Some(rest) => (self.backup_root.as_path(), rest),
            None => (self.root.as_path(), rel_path),
        })
    }

    fn read_regular(&self, rel_path: &str) -> Result<Vec<u8>> {
        let (base, rest) = self.locate(rel_path)?;
        let components: Vec<&str> = rest.split('/').collect();
        if hash_regular_file_under(base, &components)?.is_none() {
            return Err(UpdateError::new(
                ErrorCode::NotFound,
                format!("{rel_path} does not exist"),
            ));
        }
        read_limited(&base.join(rest), MAX_ENTRY_BYTES)?.ok_or_else(|| {
            UpdateError::new(ErrorCode::NotFound, format!("{rel_path} does not exist"))
        })
    }

    fn write_atomic(&self, rel_path: &str, bytes: &[u8]) -> Result<()> {
        let (base, rest) = self.locate(rel_path)?;
        let path = base.join(rest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        durable_write(&path, bytes).map_err(UpdateError::from)
    }
}

impl DiskView for FsFileOps {
    fn observe(&self, rel_path: &str) -> Result<Observed> {
        let (base, rest) = self.locate(rel_path)?;
        let components: Vec<&str> = rest.split('/').collect();
        Ok(match hash_regular_file_under(base, &components)? {
            None => Observed::Missing,
            Some((size, sha256)) => Observed::File(FileIdentity { size, sha256 }),
        })
    }
}

impl FileOps for FsFileOps {
    fn backup(&mut self, rel_path: &str, backup_rel: &str) -> Result<()> {
        let bytes = self.read_regular(rel_path)?;
        self.write_atomic(backup_rel, &bytes)
    }

    fn install_new(&mut self, rel_path: &str, expected: &FileIdentity) -> Result<()> {
        validate_rel_path(rel_path)?;
        let staged = self.stage_root.join(rel_path);
        let bytes = read_limited(&staged, MAX_ENTRY_BYTES)?.ok_or_else(|| {
            UpdateError::new(ErrorCode::NotFound, format!("{rel_path} is not staged"))
        })?;
        if &FileIdentity::of(&bytes) != expected {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("staged {rel_path} differs from the plan"),
            ));
        }
        self.write_atomic(rel_path, &bytes)
    }

    fn remove(&mut self, rel_path: &str) -> Result<()> {
        let (base, rest) = self.locate(rel_path)?;
        std::fs::remove_file(base.join(rest))?;
        Ok(())
    }

    fn restore(&mut self, rel_path: &str, backup_rel: &str, expected: &FileIdentity) -> Result<()> {
        let bytes = self.read_regular(backup_rel)?;
        if &FileIdentity::of(&bytes) != expected {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("backup of {rel_path} is damaged"),
            ));
        }
        self.write_atomic(rel_path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::fs;
    use std::rc::Rc;

    use super::*;
    use crate::identity::{ApplyPlan, InstallId, PlanStep, ReleaseIdentity, TransactionId};
    use crate::journal::{SharedJournalStore, UpdateJournal};
    use crate::testing::TempDir;

    const OLD_DLL: &[u8] = b"dll-old";
    const NEW_DLL: &[u8] = b"dll-new";
    const OLD_EXE: &[u8] = b"exe-old";
    const NEW_EXE: &[u8] = b"exe-new";
    const OLD_GONE: &[u8] = b"gone";
    const NEW_ADDED: &[u8] = b"added";

    fn release(version: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            target: "x86_64-pc-windows-msvc".to_string(),
            version: version.to_string(),
            catalog_sha256: None,
            inventory_sha256: None,
            bundle_id: None,
            team_id: None,
        }
    }

    fn step(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> PlanStep {
        PlanStep {
            rel_path: path.to_string(),
            old: old.map(FileIdentity::of),
            new: new.map(FileIdentity::of),
            backup_rel: old.map(|_| format!("backup/{path}")),
        }
    }

    fn plan() -> ApplyPlan {
        ApplyPlan {
            install_id: InstallId::parse(&"0".repeat(32)).unwrap(),
            transaction_id: TransactionId::parse(&"1".repeat(32)).unwrap(),
            source: release("0.1.0"),
            destination: release("0.2.0"),
            steps: vec![
                step("a.dll", Some(OLD_DLL), Some(NEW_DLL)),
                step("added.txt", None, Some(NEW_ADDED)),
                step("gone.txt", Some(OLD_GONE), None),
                step("hane.exe", Some(OLD_EXE), Some(NEW_EXE)),
            ],
            unchanged: Vec::new(),
        }
    }

    fn original() -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        files.insert("a.dll".to_string(), OLD_DLL.to_vec());
        files.insert("gone.txt".to_string(), OLD_GONE.to_vec());
        files.insert("hane.exe".to_string(), OLD_EXE.to_vec());
        files.insert("user.md".to_string(), b"user data".to_vec());
        files
    }

    fn io_error() -> UpdateError {
        UpdateError::new(ErrorCode::Io, "injected")
    }

    #[derive(Default)]
    struct FakeOps {
        files: BTreeMap<String, Vec<u8>>,
        staged: BTreeMap<String, Vec<u8>>,
        mutations: usize,
        /// 1-based mutation that breaks the device; later ones fail as well.
        break_at: Option<usize>,
        broken: bool,
        changed: Vec<String>,
    }

    impl FakeOps {
        fn new() -> Self {
            let mut staged = BTreeMap::new();
            staged.insert("a.dll".to_string(), NEW_DLL.to_vec());
            staged.insert("added.txt".to_string(), NEW_ADDED.to_vec());
            staged.insert("hane.exe".to_string(), NEW_EXE.to_vec());
            Self {
                files: original(),
                staged,
                ..Self::default()
            }
        }

        fn tick(&mut self) -> Result<()> {
            self.mutations += 1;
            if self.break_at == Some(self.mutations) {
                self.broken = true;
            }
            if self.broken {
                return Err(io_error());
            }
            Ok(())
        }

        fn repair(&mut self) {
            self.break_at = None;
            self.broken = false;
        }

        fn non_backup(&self) -> BTreeMap<String, Vec<u8>> {
            self.files
                .iter()
                .filter(|(path, _)| !path.starts_with(BACKUP_PREFIX))
                .map(|(path, data)| (path.clone(), data.clone()))
                .collect()
        }
    }

    impl DiskView for FakeOps {
        fn observe(&self, rel_path: &str) -> Result<Observed> {
            Ok(match self.files.get(rel_path) {
                Some(bytes) => Observed::File(FileIdentity::of(bytes)),
                None => Observed::Missing,
            })
        }
    }

    impl FileOps for FakeOps {
        fn backup(&mut self, rel_path: &str, backup_rel: &str) -> Result<()> {
            self.tick()?;
            let bytes = self.files.get(rel_path).cloned().ok_or_else(io_error)?;
            self.files.insert(backup_rel.to_string(), bytes);
            Ok(())
        }

        fn install_new(&mut self, rel_path: &str, expected: &FileIdentity) -> Result<()> {
            self.tick()?;
            let bytes = self.staged.get(rel_path).cloned().ok_or_else(io_error)?;
            assert_eq!(&FileIdentity::of(&bytes), expected);
            self.files.insert(rel_path.to_string(), bytes);
            self.changed.push(rel_path.to_string());
            Ok(())
        }

        fn remove(&mut self, rel_path: &str) -> Result<()> {
            self.tick()?;
            self.files.remove(rel_path);
            self.changed.push(rel_path.to_string());
            Ok(())
        }

        fn restore(
            &mut self,
            rel_path: &str,
            backup_rel: &str,
            expected: &FileIdentity,
        ) -> Result<()> {
            self.tick()?;
            let bytes = self.files.get(backup_rel).cloned().ok_or_else(io_error)?;
            assert_eq!(&FileIdentity::of(&bytes), expected);
            self.files.insert(rel_path.to_string(), bytes);
            Ok(())
        }
    }

    struct Parent(bool);

    impl ParentProcess for Parent {
        fn has_exited(&self) -> Result<bool> {
            Ok(self.0)
        }
    }

    #[derive(Default)]
    struct Child {
        stops: usize,
        fail: bool,
    }

    impl HealthChild for Child {
        fn stop_and_release_lease(&mut self) -> Result<()> {
            self.stops += 1;
            if self.fail { Err(io_error()) } else { Ok(()) }
        }
    }

    /// Fails every store once `budget` successful stores have been made.
    #[derive(Clone)]
    struct FlakyStore {
        inner: SharedJournalStore,
        budget: Rc<Cell<Option<usize>>>,
    }

    impl JournalStore for FlakyStore {
        fn load(&self) -> Result<Option<UpdateJournal>> {
            self.inner.load()
        }

        fn store(&mut self, journal: &UpdateJournal) -> Result<()> {
            if let Some(left) = self.budget.get() {
                if left == 0 {
                    return Err(io_error());
                }
                self.budget.set(Some(left - 1));
            }
            self.inner.store(journal)
        }
    }

    fn flaky() -> FlakyStore {
        FlakyStore {
            inner: SharedJournalStore::default(),
            budget: Rc::new(Cell::new(None)),
        }
    }

    fn new_journal(store: FlakyStore) -> DurableJournal<FlakyStore> {
        let journal = UpdateJournal::begin(&plan(), None).unwrap();
        DurableJournal::create(store, journal).unwrap()
    }

    #[test]
    fn happy_path_commits_and_replaces_the_exe_last() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        let mut child = Child::default();
        let outcome = apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        assert_eq!(outcome, ApplyOutcome::AwaitingHealth);
        assert_eq!(ops.changed.last().map(String::as_str), Some("hane.exe"));
        assert_eq!(ops.files["user.md"], b"user data");
        let health = conclude_health(&mut journal, &mut ops, &mut child, true).unwrap();
        assert_eq!(health, HealthOutcome::Committed);
        assert_eq!(journal.journal().phase, Phase::Committed);
        assert_eq!(ops.files["hane.exe"], NEW_EXE);
        assert!(!ops.files.contains_key("gone.txt"));
        assert_eq!(child.stops, 0);
    }

    #[test]
    fn a_running_parent_blocks_every_mutation() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        let err = apply(&mut journal, &mut ops, &Parent(false), &mut Child::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotApplicable);
        assert_eq!(ops.mutations, 0);
        assert_eq!(journal.journal().phase, Phase::Prepared);
    }

    #[test]
    fn a_modified_install_blocks_every_mutation() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        ops.files.insert("a.dll".to_string(), b"edited".to_vec());
        let err = apply(&mut journal, &mut ops, &Parent(true), &mut Child::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InstallModified);
        assert_eq!(ops.mutations, 0);
    }

    #[test]
    fn every_replacement_fault_point_returns_to_the_old_version() {
        // 3 backups + 4 installs/removals in one clean run.
        for fault in 1..=7 {
            let store = flaky();
            let mut journal = new_journal(store.clone());
            let mut ops = FakeOps::new();
            ops.break_at = Some(fault);
            let mut child = Child::default();
            let _ = apply(&mut journal, &mut ops, &Parent(true), &mut child);

            ops.repair();
            let mut reopened = DurableJournal::open(store.clone()).unwrap().unwrap();
            let outcome = recover(&mut reopened, &mut ops, &mut child).unwrap();
            assert!(
                matches!(outcome, RecoveryOutcome::RolledBack | RecoveryOutcome::KeptOld),
                "fault {fault}: {outcome:?}"
            );
            assert_eq!(ops.non_backup(), original(), "fault {fault}");
            assert_eq!(reopened.journal().phase, Phase::RolledBack, "fault {fault}");
        }
    }

    #[test]
    fn every_journal_flush_fault_point_returns_to_the_old_version() {
        // ExitAuthorized, Applying, 4 x (Intent, Applied), AwaitingHealth.
        for budget in 0..=11 {
            let store = flaky();
            let mut journal = new_journal(store.clone());
            store.budget.set(Some(budget));
            let mut ops = FakeOps::new();
            let mut child = Child::default();
            let _ = apply(&mut journal, &mut ops, &Parent(true), &mut child);

            store.budget.set(None);
            let mut reopened = DurableJournal::open(store.clone()).unwrap().unwrap();
            let outcome = recover(&mut reopened, &mut ops, &mut child).unwrap();
            assert!(
                matches!(outcome, RecoveryOutcome::RolledBack | RecoveryOutcome::KeptOld),
                "budget {budget}: {outcome:?}"
            );
            assert_eq!(ops.non_backup(), original(), "budget {budget}");
        }
    }

    #[test]
    fn a_failed_commit_flush_rolls_back_after_restart() {
        let store = flaky();
        let mut journal = new_journal(store.clone());
        let mut ops = FakeOps::new();
        let mut child = Child::default();
        apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();

        store.budget.set(Some(0));
        let result = conclude_health(&mut journal, &mut ops, &mut child, true);
        assert!(result.is_err());
        assert_ne!(journal.journal().phase, Phase::Committed);

        store.budget.set(None);
        let mut reopened = DurableJournal::open(store).unwrap().unwrap();
        let outcome = recover(&mut reopened, &mut ops, &mut child).unwrap();
        assert_eq!(outcome, RecoveryOutcome::RolledBack);
        assert_eq!(ops.non_backup(), original());
        // Stopped once before the failed rollback and once after the restart.
        assert_eq!(child.stops, 2);
    }

    #[test]
    fn unhealthy_child_is_stopped_before_rollback() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        let mut child = Child::default();
        apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        let health = conclude_health(&mut journal, &mut ops, &mut child, false).unwrap();
        assert_eq!(health, HealthOutcome::RolledBack);
        assert_eq!(child.stops, 1);
        assert_eq!(ops.non_backup(), original());
        assert_eq!(journal.journal().phase, Phase::RolledBack);
    }

    #[test]
    fn a_child_that_cannot_be_stopped_leaves_recovery_required() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        let mut child = Child::default();
        apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        child.fail = true;
        let health = conclude_health(&mut journal, &mut ops, &mut child, false).unwrap();
        assert!(matches!(health, HealthOutcome::RecoveryRequired(_)));
        assert_eq!(ops.files["hane.exe"], NEW_EXE);
        assert_eq!(journal.journal().phase, Phase::RecoveryRequired);
    }

    #[test]
    fn an_unknown_file_hash_is_never_overwritten() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        let mut child = Child::default();
        apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        ops.files.insert("a.dll".to_string(), b"evil".to_vec());
        let health = conclude_health(&mut journal, &mut ops, &mut child, false).unwrap();
        assert!(matches!(health, HealthOutcome::RecoveryRequired(_)));
        assert_eq!(ops.files["a.dll"], b"evil");
        assert_eq!(ops.files["hane.exe"], NEW_EXE);
        assert_eq!(journal.journal().phase, Phase::RecoveryRequired);
    }

    #[test]
    fn a_damaged_backup_stops_rollback_without_changes() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        let mut child = Child::default();
        apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        ops.files.insert("backup/hane.exe".to_string(), b"broken".to_vec());
        let before = ops.files.clone();
        let health = conclude_health(&mut journal, &mut ops, &mut child, false).unwrap();
        assert!(matches!(health, HealthOutcome::RecoveryRequired(_)));
        assert_eq!(ops.files, before);
    }

    #[test]
    fn filesystem_ops_apply_and_roll_back_real_files() {
        let dir = TempDir::new("apply");
        let root = dir.path().join("install");
        let backup = dir.path().join("backup");
        let stage = dir.path().join("stage");
        for sub in [&root, &backup, &stage] {
            fs::create_dir_all(sub).unwrap();
        }
        for (path, data) in original() {
            fs::write(root.join(path), data).unwrap();
        }
        fs::write(stage.join("a.dll"), NEW_DLL).unwrap();
        fs::write(stage.join("added.txt"), NEW_ADDED).unwrap();
        fs::write(stage.join("hane.exe"), NEW_EXE).unwrap();

        let mut ops = FsFileOps::new(root.clone(), backup, stage);
        let mut journal = new_journal(flaky());
        let mut child = Child::default();
        let outcome = apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        assert_eq!(outcome, ApplyOutcome::AwaitingHealth);
        assert_eq!(fs::read(root.join("hane.exe")).unwrap(), NEW_EXE);
        assert!(!root.join("gone.txt").exists());

        let health = conclude_health(&mut journal, &mut ops, &mut child, false).unwrap();
        assert_eq!(health, HealthOutcome::RolledBack);
        for (path, data) in original() {
            assert_eq!(fs::read(root.join(&path)).unwrap(), data, "{path}");
        }
        assert!(!root.join("added.txt").exists());
    }
}
