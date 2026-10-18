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
use crate::pinned::{PinnedDir, PinnedFile};
use crate::root_guard::RootGuard;

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
    /// Binds the next `install_new` / `remove` / `restore` of `rel_path` to the
    /// very file that has the `expected` identity (`None` = absent), failing
    /// closed when it is something else. Called right before each mutation of
    /// an install path. An adapter that can pin the verified file (Windows)
    /// keeps it so the mutation cannot land on a file swapped in after the
    /// check; the default has nothing to pin.
    fn bind_target(&mut self, rel_path: &str, expected: &Option<FileIdentity>) -> Result<()> {
        let _ = (rel_path, expected);
        Ok(())
    }
    /// Fails closed when the placement fixed at the start of the operation is
    /// no longer the one in use (moved or replaced). Checked before the health
    /// commit and before any recovery; the default has nothing to verify.
    fn verify_placement(&self) -> Result<()> {
        Ok(())
    }
    /// Re-verifies, right before `Committed`, everything the install must be
    /// beyond the journaled steps (Windows: the unchanged owned files; macOS:
    /// the bundle facts and the Developer ID signature). The expectation was
    /// fixed from the signed candidate, never learned from the live install.
    /// An error (mismatch or failed query) prevents the commit.
    fn verify_commit_state(&self) -> Result<()> {
        Ok(())
    }
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
    // Every backup is made, verified and made durable in the transaction-private
    // area before the first install or removal, so a backup failure leaves the
    // install root untouched.
    for step in &steps {
        if let (Some(old), Some(backup_rel)) = (&step.old, &step.backup_rel) {
            ensure_live_old(ops, step)?;
            ops.backup(&step.rel_path, backup_rel)?;
            if ops.observe(backup_rel)? != Observed::File(old.clone()) {
                return Err(UpdateError::new(
                    ErrorCode::HashMismatch,
                    format!("backup of {} does not match the installed file", step.rel_path),
                ));
            }
            ensure_live_old(ops, step)?;
        }
    }
    for (index, step) in steps.iter().enumerate() {
        // The intent is durable before the file is touched.
        journal.advance_step(index, StepState::Intent)?;
        // The install directory can change between steps, so each path is
        // re-observed against its journaled old identity (absent when the file
        // is to be added) right before it is replaced or removed.
        ensure_live_old(ops, step)?;
        ops.bind_target(&step.rel_path, &step.old)?;
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

/// Fails closed unless the live path still matches the journaled old identity.
/// Observation errors propagate, so an unreadable path is never mutated.
fn ensure_live_old<O: FileOps>(ops: &O, step: &crate::journal::JournalStep) -> Result<()> {
    if identity_matches(&ops.observe(&step.rel_path)?, &step.old) {
        Ok(())
    } else {
        Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("{} changed during the update", step.rel_path),
        ))
    }
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
    if healthy && ops.verify_placement().is_ok() {
        match verify_before_commit(journal, ops) {
            Ok(()) => {
                if journal.advance_phase(Phase::Committed).is_ok() {
                    return Ok(HealthOutcome::Committed);
                }
            }
            Err(err) => {
                // The files were changed while the child was running. The child
                // must be gone before anything else happens; nothing unknown
                // is overwritten, the recovery below decides from the disk.
                if let Err(stop) = child.stop_and_release_lease() {
                    return Ok(HealthOutcome::RecoveryRequired(
                        match require(
                            journal,
                            format!(
                                "the install changed before the commit ({err}) and the health child could not be stopped: {stop}"
                            ),
                        ) {
                            RecoveryOutcome::RecoveryRequired(why) => why,
                            _ => String::new(),
                        },
                    ));
                }
            }
        }
    }
    Ok(match recover(journal, ops, child)? {
        RecoveryOutcome::RecoveryRequired(why) => HealthOutcome::RecoveryRequired(why),
        RecoveryOutcome::RolledBack | RecoveryOutcome::KeptOld => HealthOutcome::RolledBack,
        other => HealthOutcome::RecoveryRequired(format!("unexpected recovery result {other:?}")),
    })
}

/// Compares the whole install with the state the signed plan requires: every
/// journaled step must hold its `new` identity (absent when removed), then the
/// platform's own checks run. The expectation comes from the journal, which was
/// built from the signed candidate, not from the live disk.
fn verify_before_commit<S: JournalStore, O: FileOps>(
    journal: &DurableJournal<S>,
    ops: &O,
) -> Result<()> {
    for step in &journal.journal().steps {
        if !identity_matches(&ops.observe(&step.rel_path)?, &step.new) {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("{} differs from the signed new version", step.rel_path),
            ));
        }
    }
    ops.verify_commit_state()
}

/// Decides from the journal and the actual disk, then acts. Used both after a
/// failed step and at startup before anything else is initialized.
pub fn recover<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
    child: &mut dyn HealthChild,
) -> Result<RecoveryOutcome> {
    if let Err(err) = ops.verify_placement() {
        // Nothing may be written or rolled back through a pathname that no
        // longer names the operation's placement. The pending journal keeps
        // blocking normal startup.
        let _ = child.stop_and_release_lease();
        return Ok(require(
            journal,
            format!("the placement changed during the update: {err}"),
        ));
    }
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
            // Reject the whole rollback before the first write if any step lacks
            // a durable intent.
            for action in actions.iter() {
                if let Err(why) = check_step_state(journal, action_rel_path(action)) {
                    return Ok(require(journal, why));
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
    let rel_path = action_rel_path(action);
    // Validate the durable step state before any write: a Planned step has no
    // recorded intent, so whatever is on disk is not ours to overwrite.
    let (index, state) = match check_step_state(journal, rel_path) {
        Ok(found) => found,
        Err(why) => return Ok(Some(why)),
    };
    match action {
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
            ops.bind_target(rel_path, expected_current)?;
            ops.restore(rel_path, backup_rel, restore)?;
        }
        RollbackAction::RemoveAdded { rel_path, expected } => {
            if ops.observe(rel_path)? != Observed::File(expected.clone()) {
                return Ok(Some(format!("{rel_path} changed during recovery")));
            }
            ops.bind_target(rel_path, &Some(expected.clone()))?;
            ops.remove(rel_path)?;
        }
    }
    if matches!(state, StepState::Intent | StepState::Applied) {
        journal.advance_step(index, StepState::Restored)?;
    }
    Ok(None)
}

fn action_rel_path(action: &RollbackAction) -> &str {
    match action {
        RollbackAction::RestoreFromBackup { rel_path, .. }
        | RollbackAction::RemoveAdded { rel_path, .. } => rel_path,
    }
}

/// Finds the journal step for `rel_path`; `Err(why)` when it is unknown or was
/// never given a recorded intent (so nothing may be written for it).
fn check_step_state<S: JournalStore>(
    journal: &DurableJournal<S>,
    rel_path: &str,
) -> std::result::Result<(usize, StepState), String> {
    let Some(index) = journal
        .journal()
        .steps
        .iter()
        .position(|step| step.rel_path == rel_path)
    else {
        return Err(format!("{rel_path} is not part of the journal"));
    };
    let state = journal.journal().steps[index].state;
    if state == StepState::Planned {
        return Err(format!("{rel_path} was changed without a recorded intent"));
    }
    Ok((index, state))
}

/// Real-filesystem [`FileOps`]. `root` is the install root, `backup_root` holds
/// `backup/<path>` copies and `stage_root` holds the verified new files.
///
/// The install root's OS identity is fixed when the value is created. Every
/// observation and mutation first checks that the root path still resolves to
/// that same directory, so a root renamed or replaced mid-operation is never
/// written to (or rolled back through) by pathname. A root whose identity
/// cannot be fixed refuses every call.
pub struct FsFileOps {
    root: PathBuf,
    backup_root: PathBuf,
    stage_root: PathBuf,
    guard: Result<RootGuard>,
    /// The verified install file the next mutation is bound to (Windows).
    pinned: Option<(String, PinnedFile)>,
    /// The verified ancestor directories of that target (Windows).
    #[cfg_attr(not(windows), allow(dead_code, reason = "only Windows pins directories"))]
    dir_pins: Option<(String, Vec<PinnedDir>)>,
    /// Owned files the update does not touch, with their signed identities.
    /// Re-verified right before the commit.
    unchanged: Vec<(String, FileIdentity)>,
    /// Runs after the target is bound and before the mutation touches it.
    #[cfg(test)]
    before_mutation: Option<Box<dyn FnMut()>>,
}

impl FsFileOps {
    pub fn new(root: PathBuf, backup_root: PathBuf, stage_root: PathBuf) -> Self {
        let guard = RootGuard::open(&root);
        Self {
            root,
            backup_root,
            stage_root,
            guard,
            pinned: None,
            dir_pins: None,
            unchanged: Vec::new(),
            #[cfg(test)]
            before_mutation: None,
        }
    }

    /// Declares the signed identities of the owned files the update leaves in
    /// place, so the commit re-verifies them too.
    pub fn with_unchanged(mut self, unchanged: Vec<(String, FileIdentity)>) -> Self {
        self.unchanged = unchanged;
        self
    }

    fn mutation_point(&mut self) {
        #[cfg(test)]
        if let Some(hook) = self.before_mutation.as_mut() {
            hook();
        }
    }

    /// The file verified by `bind_target` for exactly this path, if any. A pin
    /// is single-use: any mutation attempt consumes it.
    fn take_pin(&mut self, rel_path: &str) -> Option<PinnedFile> {
        match self.pinned.take() {
            Some((bound, pin)) if bound == rel_path => Some(pin),
            _ => None,
        }
    }

    /// Fails closed unless the install root is still the directory fixed at
    /// creation.
    pub fn verify_root(&self) -> Result<()> {
        match &self.guard {
            Ok(guard) => guard.verify(),
            Err(err) => Err(err.clone()),
        }
    }

    fn locate<'a>(&'a self, rel_path: &'a str) -> Result<(&'a Path, &'a str)> {
        validate_rel_path(rel_path)?;
        self.verify_root()?;
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

    /// Windows: binds every existing ancestor of `rel_path` beneath the
    /// install root to a verified non-reparse directory handle, continuing
    /// from the ancestors already pinned for this path. With `create`, the
    /// missing ones are created one level at a time and pinned right away, so
    /// no name is ever resolved through an unpinned directory. Fails closed
    /// when an ancestor is a link or not a directory.
    #[cfg(windows)]
    fn pin_hierarchy(&mut self, rel_path: &str, create: bool) -> Result<Vec<PinnedDir>> {
        let mut pins = match self.dir_pins.take() {
            Some((bound, pins)) if bound == rel_path => pins,
            _ => Vec::new(),
        };
        let (base, rest) = self.locate(rel_path)?;
        let components: Vec<&str> = rest.split('/').collect();
        let ancestors = &components[..components.len() - 1];
        let mut path = base.to_path_buf();
        for (index, component) in ancestors.iter().enumerate() {
            path.push(component);
            if index < pins.len() {
                continue;
            }
            let pin = match PinnedDir::open(&path)? {
                Some(pin) => pin,
                None if create => {
                    match std::fs::create_dir(&path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(err) => return Err(err.into()),
                    }
                    PinnedDir::open(&path)?.ok_or_else(|| {
                        UpdateError::new(
                            ErrorCode::InstallModified,
                            format!("{} vanished while it was being created", path.display()),
                        )
                    })?
                }
                None => break,
            };
            pins.push(pin);
        }
        Ok(pins)
    }

    #[cfg(not(windows))]
    fn pin_hierarchy(&mut self, _rel_path: &str, _create: bool) -> Result<Vec<PinnedDir>> {
        Ok(Vec::new())
    }

    fn write_atomic(
        &mut self,
        rel_path: &str,
        bytes: &[u8],
        pin: Option<PinnedFile>,
    ) -> Result<()> {
        if rel_path.starts_with(BACKUP_PREFIX) {
            let (base, rest) = self.locate(rel_path)?;
            let path = base.join(rest);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Transaction-owned backup area: the ordinary durable write.
            return durable_write(&path, bytes).map_err(UpdateError::from);
        }
        validate_rel_path(rel_path)?;
        // Held until the file is placed (Windows): the pinned directories
        // cannot be swapped for links meanwhile.
        let _dirs = self.pin_hierarchy(rel_path, true)?;
        let (base, rest) = self.locate(rel_path)?;
        let path = base.join(rest);
        #[cfg(not(windows))]
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        install_write(&path, bytes, pin)
    }
}

static INSTALL_TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes into the install root without touching any file this call did not
/// create. The bytes go into a uniquely named scratch directory made
/// exclusively next to the destination (same volume), then either atomically
/// replace an existing file or are hard-linked into place, which fails rather
/// than overwrite a file that appeared in the meantime. Only the scratch
/// directory and the file this call created are ever removed.
///
/// On Windows an existing file is only ever displaced through the handle that
/// verified it (`pin`); without one the call fails closed.
fn install_write(path: &Path, bytes: &[u8], pin: Option<PinnedFile>) -> Result<()> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        UpdateError::new(ErrorCode::Io, "install path has no parent directory")
    })?;
    let name = path.file_name().ok_or_else(|| {
        UpdateError::new(ErrorCode::Io, "install path has no file name")
    })?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = INSTALL_TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = parent.join(format!(
        ".hane-update-{}-{nanos:x}-{seq}",
        std::process::id()
    ));
    // `create_dir` fails when anything already has that name; nothing is reused.
    std::fs::create_dir(&scratch)?;
    let temp = scratch.join(name);
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        place(&temp, &scratch, name, path, pin)
    })();
    // Remove only what this call created.
    let _ = std::fs::remove_file(&temp);
    let _ = std::fs::remove_dir(&scratch);
    result
}

#[cfg(not(windows))]
fn place(
    temp: &Path,
    _scratch: &Path,
    _name: &std::ffi::OsStr,
    path: &Path,
    _pin: Option<PinnedFile>,
) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => crate::fsutil::replace_file(temp, path)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Atomic no-replace: fails with AlreadyExists if a file showed up.
            std::fs::hard_link(temp, path)?;
        }
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

/// Windows: the verified file is moved aside through its own handle (no other
/// process could rename or replace it meanwhile), then the new file is linked
/// into the now-free name, which fails rather than overwrite anything that
/// appeared. On failure the verified file is moved back without replacing.
/// Whatever stays unresolved is left in the scratch directory, never deleted.
#[cfg(windows)]
fn place(
    temp: &Path,
    scratch: &Path,
    name: &std::ffi::OsStr,
    path: &Path,
    pin: Option<PinnedFile>,
) -> Result<()> {
    match pin {
        Some(pin) => {
            let mut aside_name = name.to_os_string();
            aside_name.push(".aside");
            let aside = scratch.join(aside_name);
            pin.rename_to(&aside)?;
            if let Err(err) = std::fs::hard_link(temp, path) {
                return match pin.rename_to(path) {
                    Ok(()) => Err(err.into()),
                    Err(back) => Err(UpdateError::new(
                        ErrorCode::Io,
                        format!(
                            "{} could not be put back after a failed install: {back}",
                            path.display()
                        ),
                    )),
                };
            }
            drop(pin);
            let _ = std::fs::remove_file(&aside);
            Ok(())
        }
        None => match std::fs::symlink_metadata(path) {
            Ok(_) => Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} was not verified before replacement", path.display()),
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::hard_link(temp, path)?;
                Ok(())
            }
            Err(err) => Err(err.into()),
        },
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
    fn verify_commit_state(&self) -> Result<()> {
        for (rel_path, identity) in &self.unchanged {
            if self.observe(rel_path)? != Observed::File(identity.clone()) {
                return Err(UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{rel_path} differs from the signed inventory"),
                ));
            }
        }
        Ok(())
    }

    fn backup(&mut self, rel_path: &str, backup_rel: &str) -> Result<()> {
        let bytes = self.read_regular(rel_path)?;
        self.write_atomic(backup_rel, &bytes, None)
    }

    fn bind_target(&mut self, rel_path: &str, expected: &Option<FileIdentity>) -> Result<()> {
        self.pinned = None;
        self.dir_pins = None;
        #[cfg(windows)]
        {
            if !rel_path.starts_with(BACKUP_PREFIX) {
                // Bind the existing ancestors before the target itself, so a
                // new file's parents are fixed from the moment it is validated.
                let pins = self.pin_hierarchy(rel_path, false)?;
                self.dir_pins = Some((rel_path.to_string(), pins));
            }
            let (base, rest) = self.locate(rel_path)?;
            let path = base.join(rest);
            let mismatch = || {
                UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{rel_path} changed during the update"),
                )
            };
            // Links on the way were rejected by the preceding observation; an
            // open file also keeps its parent directories from being renamed.
            match (PinnedFile::open(&path)?, expected) {
                (None, None) => {}
                (Some(pin), Some(old)) => {
                    if &pin.identity()? != old {
                        return Err(mismatch());
                    }
                    self.pinned = Some((rel_path.to_string(), pin));
                }
                _ => return Err(mismatch()),
            }
        }
        #[cfg(not(windows))]
        let _ = (rel_path, expected);
        Ok(())
    }

    fn install_new(&mut self, rel_path: &str, expected: &FileIdentity) -> Result<()> {
        let pin = self.take_pin(rel_path);
        self.mutation_point();
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
        self.write_atomic(rel_path, &bytes, pin)
    }

    fn remove(&mut self, rel_path: &str) -> Result<()> {
        let pin = self.take_pin(rel_path);
        self.mutation_point();
        let (base, rest) = self.locate(rel_path)?;
        #[cfg(windows)]
        {
            let _ = (base, rest);
            // Only the verified file, deleted through its own handle.
            let pin = pin.ok_or_else(|| {
                UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{rel_path} was not verified before removal"),
                )
            })?;
            pin.delete()?;
            drop(pin);
        }
        #[cfg(not(windows))]
        {
            let _ = pin;
            std::fs::remove_file(base.join(rest))?;
        }
        Ok(())
    }

    fn restore(&mut self, rel_path: &str, backup_rel: &str, expected: &FileIdentity) -> Result<()> {
        let pin = self.take_pin(rel_path);
        self.mutation_point();
        let bytes = self.read_regular(backup_rel)?;
        if &FileIdentity::of(&bytes) != expected {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                format!("backup of {rel_path} is damaged"),
            ));
        }
        self.write_atomic(rel_path, &bytes, pin)
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
        /// External change made right after the 1-based mutation completes.
        inject_after: Option<(usize, String, Vec<u8>)>,
        /// Backup of this path fails outright.
        fail_backup_of: Option<String>,
        /// Backup of this path is written with damaged content.
        corrupt_backup_of: Option<String>,
        /// Owned files the update leaves alone, verified before the commit.
        unchanged: Vec<(String, FileIdentity)>,
    }

    impl FakeOps {
        fn inject(&mut self) {
            if let Some((at, path, bytes)) = self.inject_after.clone()
                && at == self.mutations
            {
                self.files.insert(path, bytes);
            }
        }

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
        fn verify_commit_state(&self) -> Result<()> {
            for (path, identity) in &self.unchanged {
                if self.observe(path)? != Observed::File(identity.clone()) {
                    return Err(UpdateError::new(ErrorCode::InstallModified, "unchanged file differs"));
                }
            }
            Ok(())
        }

        fn backup(&mut self, rel_path: &str, backup_rel: &str) -> Result<()> {
            self.tick()?;
            if self.fail_backup_of.as_deref() == Some(rel_path) {
                return Err(io_error());
            }
            let mut bytes = self.files.get(rel_path).cloned().ok_or_else(io_error)?;
            if self.corrupt_backup_of.as_deref() == Some(rel_path) {
                bytes = b"damaged".to_vec();
            }
            self.files.insert(backup_rel.to_string(), bytes);
            self.inject();
            Ok(())
        }

        fn install_new(&mut self, rel_path: &str, expected: &FileIdentity) -> Result<()> {
            self.tick()?;
            let bytes = self.staged.get(rel_path).cloned().ok_or_else(io_error)?;
            assert_eq!(&FileIdentity::of(&bytes), expected);
            self.files.insert(rel_path.to_string(), bytes);
            self.changed.push(rel_path.to_string());
            self.inject();
            Ok(())
        }

        fn remove(&mut self, rel_path: &str) -> Result<()> {
            self.tick()?;
            self.files.remove(rel_path);
            self.changed.push(rel_path.to_string());
            self.inject();
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

    fn applied_with_unchanged() -> (DurableJournal<FlakyStore>, FakeOps, Child) {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        ops.unchanged = vec![("user.md".to_string(), FileIdentity::of(b"user data"))];
        let mut child = Child::default();
        let outcome = apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        assert_eq!(outcome, ApplyOutcome::AwaitingHealth);
        (journal, ops, child)
    }

    #[test]
    fn an_exact_match_before_the_commit_still_commits() {
        let (mut journal, mut ops, mut child) = applied_with_unchanged();
        let health = conclude_health(&mut journal, &mut ops, &mut child, true).unwrap();
        assert_eq!(health, HealthOutcome::Committed);
        assert_eq!(journal.journal().phase, Phase::Committed);
    }

    #[test]
    fn a_changed_file_modified_before_the_commit_is_not_committed_and_is_kept() {
        let (mut journal, mut ops, mut child) = applied_with_unchanged();
        ops.files.insert("a.dll".to_string(), b"third party".to_vec());
        let health = conclude_health(&mut journal, &mut ops, &mut child, true).unwrap();
        assert!(matches!(health, HealthOutcome::RecoveryRequired(_)), "{health:?}");
        assert_ne!(journal.journal().phase, Phase::Committed);
        assert!(child.stops >= 1);
        assert_eq!(ops.files["a.dll"], b"third party");
    }

    #[test]
    fn an_unchanged_file_modified_before_the_commit_is_not_committed_and_is_kept() {
        let (mut journal, mut ops, mut child) = applied_with_unchanged();
        ops.files.insert("user.md".to_string(), b"third party".to_vec());
        let health = conclude_health(&mut journal, &mut ops, &mut child, true).unwrap();
        assert_ne!(health, HealthOutcome::Committed);
        assert_ne!(journal.journal().phase, Phase::Committed);
        assert!(child.stops >= 1);
        assert_eq!(ops.files["user.md"], b"third party");
    }

    #[test]
    fn a_child_that_cannot_be_stopped_after_a_mismatch_requires_recovery() {
        let (mut journal, mut ops, mut child) = applied_with_unchanged();
        child.fail = true;
        ops.files.insert("user.md".to_string(), b"third party".to_vec());
        let health = conclude_health(&mut journal, &mut ops, &mut child, true).unwrap();
        assert!(matches!(health, HealthOutcome::RecoveryRequired(_)), "{health:?}");
        assert_eq!(journal.journal().phase, Phase::RecoveryRequired);
        assert_eq!(ops.files["user.md"], b"third party");
    }

    #[test]
    fn begin_and_validate_reject_backup_paths_outside_the_canonical_backup() {
        let canonical = UpdateJournal::begin(&plan(), None).unwrap();
        assert!(canonical.validate().is_ok());
        let replaced = canonical
            .steps
            .iter()
            .position(|step| step.old.is_some())
            .unwrap();
        let added = canonical
            .steps
            .iter()
            .position(|step| step.old.is_none())
            .unwrap();

        for bad in ["user.md", "backup/other.txt"] {
            let mut p = plan();
            p.steps[replaced].backup_rel = Some(bad.to_string());
            let err = UpdateJournal::begin(&p, None).unwrap_err();
            assert_eq!(err.code, ErrorCode::JournalInvalid);

            let mut journal = canonical.clone();
            journal.steps[replaced].backup_rel = Some(bad.to_string());
            assert_eq!(journal.validate().unwrap_err().code, ErrorCode::JournalInvalid);
        }

        let mut p = plan();
        p.steps[added].backup_rel = Some(format!("backup/{}", p.steps[added].rel_path));
        let err = UpdateJournal::begin(&p, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::JournalInvalid);

        let mut journal = canonical;
        journal.steps[added].backup_rel = Some("user.md".to_string());
        assert_eq!(journal.validate().unwrap_err().code, ErrorCode::JournalInvalid);
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
    fn an_owned_file_changed_between_steps_is_kept_and_stops_later_mutations() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        // Three backups come first; after a.dll is installed (mutation 4),
        // gone.txt is edited externally.
        ops.inject_after = Some((4, "gone.txt".to_string(), b"edited".to_vec()));
        let mut child = Child::default();
        let outcome = apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        assert!(matches!(outcome, ApplyOutcome::RecoveryRequired(_)), "{outcome:?}");
        assert_eq!(ops.files["gone.txt"], b"edited");
        assert_eq!(ops.files["hane.exe"], OLD_EXE);
        assert_eq!(ops.mutations, 5);
        assert_eq!(ops.changed, ["a.dll", "added.txt"]);
        assert_eq!(journal.journal().phase, Phase::RecoveryRequired);
    }

    #[test]
    fn an_unknown_file_at_a_planned_addition_is_never_overwritten() {
        let mut journal = new_journal(flaky());
        let mut ops = FakeOps::new();
        // Three backups come first; after a.dll is installed (mutation 4),
        // someone creates added.txt.
        ops.inject_after = Some((4, "added.txt".to_string(), b"user file".to_vec()));
        let mut child = Child::default();
        let outcome = apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
        assert!(matches!(outcome, ApplyOutcome::RecoveryRequired(_)), "{outcome:?}");
        assert_eq!(ops.files["added.txt"], b"user file");
        assert_eq!(ops.mutations, 4);
        assert_eq!(ops.changed, ["a.dll"]);
        assert_eq!(ops.files["hane.exe"], OLD_EXE);
        assert_eq!(journal.journal().phase, Phase::RecoveryRequired);
    }

    #[test]
    fn a_late_backup_failure_leaves_every_install_file_untouched() {
        for corrupt in [false, true] {
            let mut journal = new_journal(flaky());
            let mut ops = FakeOps::new();
            // hane.exe is the last step with a backup.
            if corrupt {
                ops.corrupt_backup_of = Some("hane.exe".to_string());
            } else {
                ops.fail_backup_of = Some("hane.exe".to_string());
            }
            let mut child = Child::default();
            let outcome = apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap();
            assert!(matches!(outcome, ApplyOutcome::RolledBack(_)), "{outcome:?}");
            assert!(ops.changed.is_empty(), "corrupt={corrupt}");
            assert_eq!(ops.non_backup(), original(), "corrupt={corrupt}");
            assert_eq!(ops.mutations, 3, "corrupt={corrupt}");
            assert_eq!(journal.journal().phase, Phase::RolledBack);
        }
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
    fn a_planned_step_with_new_bytes_and_a_backup_is_never_written_by_recovery() {
        // Only the Applying phase is durable; no step intent was recorded.
        let mut journal = new_journal(flaky());
        journal.advance_phase(Phase::ExitAuthorized).unwrap();
        journal.advance_phase(Phase::Applying).unwrap();
        let mut ops = FakeOps::new();
        ops.files.insert("a.dll".to_string(), NEW_DLL.to_vec());
        ops.files.insert("backup/a.dll".to_string(), OLD_DLL.to_vec());
        let before = ops.files.clone();
        let mut child = Child::default();
        let outcome = recover(&mut journal, &mut ops, &mut child).unwrap();
        assert!(matches!(outcome, RecoveryOutcome::RecoveryRequired(_)));
        assert_eq!(ops.files, before);
        assert_eq!(ops.mutations, 0);
        assert_eq!(journal.journal().phase, Phase::RecoveryRequired);
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

    #[cfg(unix)]
    #[test]
    fn a_root_moved_after_the_first_mutation_is_never_written_or_rolled_back_by_path() {
        let dir = TempDir::new("apply-root-moved");
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
        assert_eq!(
            apply(&mut journal, &mut ops, &Parent(true), &mut child).unwrap(),
            ApplyOutcome::AwaitingHealth
        );

        // The mixed install moves away and another directory takes its path.
        let moved = dir.path().join("moved");
        fs::rename(&root, &moved).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("hane.exe"), b"decoy").unwrap();

        assert!(ops.verify_root().is_err());
        assert!(conclude_health(&mut journal, &mut ops, &mut child, false).is_err());
        assert_eq!(fs::read(root.join("hane.exe")).unwrap(), b"decoy");
        assert_eq!(fs::read(moved.join("hane.exe")).unwrap(), NEW_EXE);
        assert!(!root.join("added.txt").exists());
        // The refusal changed nothing, so the transaction stays pending.
        assert_eq!(journal.journal().phase, Phase::AwaitingHealth);
    }

    #[cfg(windows)]
    struct RaceFixture {
        _dir: TempDir,
        root: std::path::PathBuf,
        ops: FsFileOps,
    }

    #[cfg(windows)]
    fn race_fixture() -> RaceFixture {
        let dir = TempDir::new("apply-race");
        let root = dir.path().join("install");
        let backup = dir.path().join("backup");
        let stage = dir.path().join("stage");
        for sub in [&root, &backup, &stage] {
            fs::create_dir_all(sub).unwrap();
        }
        fs::write(root.join("a.dll"), OLD_DLL).unwrap();
        fs::write(root.join("third.tmp"), b"third party").unwrap();
        fs::write(stage.join("a.dll"), NEW_DLL).unwrap();
        fs::write(stage.join("added.txt"), NEW_ADDED).unwrap();
        fs::write(backup.join("a.dll"), OLD_DLL).unwrap();
        let ops = FsFileOps::new(root.clone(), backup, stage);
        RaceFixture {
            _dir: dir,
            root,
            ops,
        }
    }

    /// Hook that, between the identity check and the mutation, tries to move
    /// the target away and to rename a third-party file over it. Returns the
    /// flag that is set when both attempts were refused.
    #[cfg(windows)]
    fn swap_attempt(root: &std::path::Path) -> (Box<dyn FnMut()>, Rc<Cell<bool>>) {
        let refused = Rc::new(Cell::new(false));
        let flag = refused.clone();
        let target = root.join("a.dll");
        let third = root.join("third.tmp");
        let aside = root.join("a.dll.moved");
        let hook = move || {
            let moved = fs::rename(&target, &aside).is_err();
            let replaced = fs::rename(&third, &target).is_err();
            flag.set(moved && replaced);
        };
        (Box::new(hook), refused)
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_keeps_the_verified_file_bound_against_a_swap() {
        let mut fx = race_fixture();
        fx.ops
            .bind_target("a.dll", &Some(FileIdentity::of(OLD_DLL)))
            .unwrap();
        let (hook, refused) = swap_attempt(&fx.root);
        fx.ops.before_mutation = Some(hook);
        fx.ops
            .install_new("a.dll", &FileIdentity::of(NEW_DLL))
            .unwrap();
        assert!(refused.get(), "the target could be swapped after verification");
        assert_eq!(fs::read(fx.root.join("a.dll")).unwrap(), NEW_DLL);
        assert_eq!(fs::read(fx.root.join("third.tmp")).unwrap(), b"third party");
    }

    #[cfg(windows)]
    #[test]
    fn windows_remove_keeps_the_verified_file_bound_against_a_swap() {
        let mut fx = race_fixture();
        fx.ops
            .bind_target("a.dll", &Some(FileIdentity::of(OLD_DLL)))
            .unwrap();
        let (hook, refused) = swap_attempt(&fx.root);
        fx.ops.before_mutation = Some(hook);
        fx.ops.remove("a.dll").unwrap();
        assert!(refused.get(), "the target could be swapped after verification");
        assert!(!fx.root.join("a.dll").exists());
        assert_eq!(fs::read(fx.root.join("third.tmp")).unwrap(), b"third party");
    }

    #[cfg(windows)]
    #[test]
    fn windows_restore_keeps_the_verified_file_bound_against_a_swap() {
        let mut fx = race_fixture();
        fs::write(fx.root.join("a.dll"), NEW_DLL).unwrap();
        fx.ops
            .bind_target("a.dll", &Some(FileIdentity::of(NEW_DLL)))
            .unwrap();
        let (hook, refused) = swap_attempt(&fx.root);
        fx.ops.before_mutation = Some(hook);
        fx.ops
            .restore("a.dll", "backup/a.dll", &FileIdentity::of(OLD_DLL))
            .unwrap();
        assert!(refused.get(), "the target could be swapped after verification");
        assert_eq!(fs::read(fx.root.join("a.dll")).unwrap(), OLD_DLL);
        assert_eq!(fs::read(fx.root.join("third.tmp")).unwrap(), b"third party");
    }

    #[cfg(windows)]
    #[test]
    fn windows_mutation_without_a_verified_binding_changes_nothing() {
        let mut fx = race_fixture();
        assert!(fx.ops.install_new("a.dll", &FileIdentity::of(NEW_DLL)).is_err());
        assert!(fx.ops.remove("a.dll").is_err());
        assert_eq!(fs::read(fx.root.join("a.dll")).unwrap(), OLD_DLL);
        // A binding whose identity differs is refused up front.
        assert!(
            fx.ops
                .bind_target("a.dll", &Some(FileIdentity::of(b"other")))
                .is_err()
        );
        assert_eq!(fs::read(fx.root.join("a.dll")).unwrap(), OLD_DLL);
    }

    #[cfg(windows)]
    #[test]
    fn an_added_file_created_by_a_third_party_first_is_never_overwritten() {
        let mut fx = race_fixture();
        fx.ops.bind_target("added.txt", &None).unwrap();
        let added = fx.root.join("added.txt");
        fx.ops.before_mutation = Some(Box::new(move || {
            fs::write(&added, b"third party").unwrap();
        }));
        assert!(
            fx.ops
                .install_new("added.txt", &FileIdentity::of(NEW_ADDED))
                .is_err()
        );
        assert_eq!(fs::read(fx.root.join("added.txt")).unwrap(), b"third party");
    }

    #[cfg(windows)]
    fn make_junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    #[test]
    fn a_new_files_ancestor_swapped_for_a_junction_never_writes_outside_the_root() {
        let mut fx = race_fixture();
        let stage = fx._dir.path().join("stage");
        let outside = fx._dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(stage.join("sub/deep")).unwrap();
        fs::write(stage.join("sub/deep/added.txt"), NEW_ADDED).unwrap();
        fs::create_dir_all(fx.root.join("sub")).unwrap();

        fx.ops.bind_target("sub/deep/added.txt", &None).unwrap();
        let sub = fx.root.join("sub");
        let moved = fx.root.join("sub.moved");
        let link_outside = outside.clone();
        let refused = Rc::new(Cell::new(false));
        let flag = refused.clone();
        fx.ops.before_mutation = Some(Box::new(move || {
            // Validation is over: try to replace the ancestor with a junction.
            if fs::rename(&sub, &moved).is_ok() {
                make_junction(&sub, &link_outside);
            } else {
                flag.set(true);
            }
        }));
        let result = fx
            .ops
            .install_new("sub/deep/added.txt", &FileIdentity::of(NEW_ADDED));

        assert!(refused.get(), "the pinned ancestor could be swapped");
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            fs::read(fx.root.join("sub/deep/added.txt")).unwrap(),
            NEW_ADDED
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn a_new_file_below_a_junction_ancestor_is_refused_before_any_mutation() {
        let mut fx = race_fixture();
        let stage = fx._dir.path().join("stage");
        let outside = fx._dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(stage.join("sub")).unwrap();
        fs::write(stage.join("sub/added.txt"), NEW_ADDED).unwrap();
        assert!(make_junction(&fx.root.join("sub"), &outside));

        assert!(fx.ops.bind_target("sub/added.txt", &None).is_err());
        assert!(
            fx.ops
                .install_new("sub/added.txt", &FileIdentity::of(NEW_ADDED))
                .is_err()
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    /// Windows displaces an existing file only through a handle whose identity
    /// was checked, so the pin is bound to the expected content first.
    #[cfg(windows)]
    fn pin_existing_for_test(path: &Path, expected: &[u8]) -> Option<PinnedFile> {
        let pin = PinnedFile::open(path)
            .unwrap()
            .expect("the target file exists");
        assert_eq!(pin.identity().unwrap(), FileIdentity::of(expected));
        Some(pin)
    }

    /// Elsewhere the existing file is replaced by path, so no pin is needed.
    #[cfg(not(windows))]
    fn pin_existing_for_test(_path: &Path, _expected: &[u8]) -> Option<PinnedFile> {
        None
    }

    #[test]
    fn install_write_keeps_unknown_neighbours_and_never_overwrites_new_files() {
        let dir = TempDir::new("apply-neighbours");
        let root = dir.path().join("install");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.dll"), OLD_DLL).unwrap();
        fs::write(root.join(".a.dll.tmp"), b"user temp").unwrap();
        fs::write(root.join(".added.txt.tmp"), b"user temp 2").unwrap();

        let pin = pin_existing_for_test(&root.join("a.dll"), OLD_DLL);
        install_write(&root.join("a.dll"), NEW_DLL, pin).unwrap();
        install_write(&root.join("added.txt"), NEW_ADDED, None).unwrap();
        assert_eq!(fs::read(root.join("a.dll")).unwrap(), NEW_DLL);
        assert_eq!(fs::read(root.join("added.txt")).unwrap(), NEW_ADDED);
        assert_eq!(fs::read(root.join(".a.dll.tmp")).unwrap(), b"user temp");
        assert_eq!(fs::read(root.join(".added.txt.tmp")).unwrap(), b"user temp 2");

        // A file that is not owned at the destination is replaced only through
        // the replace path; a vanished-then-reappeared target is never clobbered
        // by the no-replace path.
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with(".hane-update-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// Runs the real replace path on whatever OS executes the tests; on Windows
    /// it exercises `ReplaceFileW`, elsewhere `rename`.
    #[test]
    fn replacing_existing_files_and_rewriting_the_journal_file_succeeds_repeatedly() {
        let dir = TempDir::new("apply-replace-existing");
        let root = dir.path().join("install");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.dll"), OLD_DLL).unwrap();

        let pin = pin_existing_for_test(&root.join("a.dll"), OLD_DLL);
        install_write(&root.join("a.dll"), NEW_DLL, pin).unwrap();
        assert_eq!(fs::read(root.join("a.dll")).unwrap(), NEW_DLL);
        let pin = pin_existing_for_test(&root.join("a.dll"), NEW_DLL);
        install_write(&root.join("a.dll"), OLD_DLL, pin).unwrap();
        assert_eq!(fs::read(root.join("a.dll")).unwrap(), OLD_DLL);

        let journal = dir.path().join("journal.json");
        durable_write(&journal, b"intent").unwrap();
        durable_write(&journal, b"phase").unwrap();
        durable_write(&journal, b"rollback").unwrap();
        assert_eq!(fs::read(&journal).unwrap(), b"rollback");
        assert!(!dir.path().join(".journal.json.tmp").exists());
    }
}
