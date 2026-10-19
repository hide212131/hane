//! Update journal, durable advancement and the recovery decision.
//!
//! The journal records intent; the disk is the evidence. Recovery therefore
//! compares every step with the actual file identity and never guesses: if a
//! file is neither the recorded old nor the recorded new content, or a needed
//! backup is not intact, the result is `RecoveryRequired` and nothing is
//! overwritten or deleted.
//!
//! Not provided here (later stage): the OS process lease, the native
//! replace/rename calls, and any claim of power-loss durability on a real OS.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, hash_regular_file_under, read_limited};
use crate::identity::{
    ApplyPlan, FileIdentity, InstallId, ReleaseIdentity, TransactionId, UnchangedFile,
};
use crate::limits::{MAX_ENTRIES, MAX_JOURNAL_BYTES};
use crate::paths::{collision_key, validate_rel_path};
use crate::util::{is_sha256_hex, parse_strict};

pub const JOURNAL_SCHEMA: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Prepared,
    ExitAuthorized,
    Applying,
    AwaitingHealth,
    Committed,
    RollingBack,
    RolledBack,
    RecoveryRequired,
}

impl Phase {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Phase::Committed | Phase::RolledBack | Phase::RecoveryRequired
        )
    }

    fn may_follow(self, next: Phase) -> bool {
        use Phase::*;
        matches!(
            (self, next),
            (Prepared, ExitAuthorized)
                | (Prepared, RolledBack)
                | (ExitAuthorized, Applying)
                | (ExitAuthorized, RolledBack)
                | (Applying, AwaitingHealth)
                | (Applying, RollingBack)
                | (AwaitingHealth, Committed)
                | (AwaitingHealth, RollingBack)
                | (RollingBack, RolledBack)
        ) || (!self.is_terminal() && next == RecoveryRequired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepState {
    Planned,
    Intent,
    Applied,
    Restored,
}

impl StepState {
    fn may_follow(self, next: StepState) -> bool {
        use StepState::*;
        matches!(
            (self, next),
            (Planned, Intent) | (Intent, Applied) | (Intent, Restored) | (Applied, Restored)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalStep {
    pub rel_path: String,
    pub old: Option<FileIdentity>,
    pub new: Option<FileIdentity>,
    pub backup_rel: Option<String>,
    pub state: StepState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateJournal {
    pub schema: u64,
    pub transaction_id: TransactionId,
    pub install_id: InstallId,
    pub plan_hash: String,
    pub source: ReleaseIdentity,
    pub destination: ReleaseIdentity,
    pub ticket_sha256: Option<String>,
    pub phase: Phase,
    pub steps: Vec<JournalStep>,
    /// Owned files the update leaves in place, with the identities fixed from
    /// the signed inventory. They must hold these identities at every point
    /// the install is accepted (before health, at commit, and in receipts).
    /// The key is always written, even when empty, and a journal without it is
    /// rejected: a missing list is never read as "nothing to verify".
    pub unchanged: Vec<UnchangedFile>,
    /// Incremented on every durable update; detects a stale writer.
    pub generation: u64,
}

impl UpdateJournal {
    pub fn begin(plan: &ApplyPlan, ticket_sha256: Option<String>) -> Result<Self> {
        let journal = Self {
            schema: JOURNAL_SCHEMA,
            transaction_id: plan.transaction_id.clone(),
            install_id: plan.install_id.clone(),
            plan_hash: plan.plan_hash()?,
            source: plan.source.clone(),
            destination: plan.destination.clone(),
            ticket_sha256,
            phase: Phase::Prepared,
            steps: plan
                .steps
                .iter()
                .map(|step| JournalStep {
                    rel_path: step.rel_path.clone(),
                    old: step.old.clone(),
                    new: step.new.clone(),
                    backup_rel: step.backup_rel.clone(),
                    state: StepState::Planned,
                })
                .collect(),
            unchanged: plan.unchanged.clone(),
            generation: 0,
        };
        journal.validate()?;
        Ok(journal)
    }

    /// Structural validation; applied to everything read back from disk.
    pub fn validate(&self) -> Result<()> {
        let invalid = |why: &str| Err(UpdateError::new(ErrorCode::JournalInvalid, why.to_string()));
        if self.schema != JOURNAL_SCHEMA {
            return invalid("unknown journal schema");
        }
        if !is_sha256_hex(&self.plan_hash) {
            return invalid("plan hash is not SHA-256 hex");
        }
        if let Some(ticket) = &self.ticket_sha256
            && !is_sha256_hex(ticket)
        {
            return invalid("ticket hash is not SHA-256 hex");
        }
        if self.steps.len() > MAX_ENTRIES {
            return invalid("too many journal steps");
        }
        self.source.validate()?;
        self.destination.validate()?;
        for step in &self.steps {
            validate_rel_path(&step.rel_path)?;
            if step.old.is_none() && step.new.is_none() {
                return invalid("step changes nothing");
            }
            match (&step.old, &step.backup_rel) {
                (Some(_), Some(backup)) => {
                    validate_rel_path(backup)?;
                    if backup != &format!("backup/{}", step.rel_path) {
                        return invalid("step backup is not its own backup path");
                    }
                }
                (Some(_), None) => {
                    return invalid("step replaces a file without a backup");
                }
                (None, Some(_)) => {
                    return invalid("step without an old file has a backup");
                }
                (None, None) => {}
            }
            for id in [&step.old, &step.new].into_iter().flatten() {
                if !is_sha256_hex(&id.sha256) {
                    return invalid("step hash is not SHA-256 hex");
                }
            }
        }
        if self.unchanged.len() > MAX_ENTRIES {
            return invalid("too many unchanged files");
        }
        // Every owned path appears once, whether changed or not.
        let mut seen = std::collections::BTreeSet::new();
        for path in self
            .steps
            .iter()
            .map(|step| step.rel_path.as_str())
            .chain(self.unchanged.iter().map(|file| file.rel_path.as_str()))
        {
            if !seen.insert(collision_key(path)) {
                return invalid("a path is listed more than once");
            }
        }
        for file in &self.unchanged {
            validate_rel_path(&file.rel_path)?;
            if file.rel_path.starts_with("backup/") {
                return invalid("an unchanged file lives in the backup area");
            }
            if !is_sha256_hex(&file.identity.sha256) {
                return invalid("unchanged file hash is not SHA-256 hex");
            }
        }
        Ok(())
    }

    pub fn transition(&mut self, next: Phase) -> Result<()> {
        if !self.phase.may_follow(next) {
            return Err(UpdateError::new(
                ErrorCode::TransitionInvalid,
                format!("cannot move from {:?} to {next:?}", self.phase),
            ));
        }
        self.phase = next;
        Ok(())
    }

    pub fn set_step(&mut self, index: usize, next: StepState) -> Result<()> {
        let forward = matches!(next, StepState::Intent | StepState::Applied);
        let allowed_phase = if forward {
            self.phase == Phase::Applying
        } else {
            matches!(self.phase, Phase::RollingBack)
        };
        if !allowed_phase {
            return Err(UpdateError::new(
                ErrorCode::TransitionInvalid,
                format!("step change {next:?} is not allowed in {:?}", self.phase),
            ));
        }
        let step = self.steps.get_mut(index).ok_or_else(|| {
            UpdateError::new(ErrorCode::TransitionInvalid, "step index out of range")
        })?;
        if !step.state.may_follow(next) {
            return Err(UpdateError::new(
                ErrorCode::TransitionInvalid,
                format!("step cannot move from {:?} to {next:?}", step.state),
            ));
        }
        step.state = next;
        Ok(())
    }
}

/// Persistence seam for the journal. `store` must be atomic: a reader sees the
/// previous or the new journal, never a torn one.
pub trait JournalStore {
    fn load(&self) -> Result<Option<UpdateJournal>>;
    fn store(&mut self, journal: &UpdateJournal) -> Result<()>;
}

/// A journal whose every change is persisted before it is considered made.
///
/// If `store` fails the in-memory journal is left unchanged. The on-disk copy
/// may or may not have advanced (a crash after the flush looks identical), so
/// the next change re-reads the store and refuses to continue when the
/// generation no longer matches; the caller must then run recovery, which
/// trusts the disk.
pub struct DurableJournal<S: JournalStore> {
    store: S,
    current: UpdateJournal,
}

impl<S: JournalStore> DurableJournal<S> {
    pub fn create(mut store: S, journal: UpdateJournal) -> Result<Self> {
        journal.validate()?;
        if store.load()?.is_some() {
            return Err(UpdateError::new(
                ErrorCode::StaleJournal,
                "a journal already exists for this installation",
            ));
        }
        store.store(&journal)?;
        Ok(Self {
            store,
            current: journal,
        })
    }

    pub fn open(store: S) -> Result<Option<Self>> {
        Ok(store.load()?.map(|current| Self { store, current }))
    }

    pub fn journal(&self) -> &UpdateJournal {
        &self.current
    }

    fn persist(&mut self, mut next: UpdateJournal) -> Result<()> {
        match self.store.load()? {
            Some(on_disk)
                if on_disk.generation == self.current.generation
                    && on_disk.transaction_id == self.current.transaction_id => {}
            _ => {
                return Err(UpdateError::new(
                    ErrorCode::StaleJournal,
                    "journal on disk no longer matches this writer",
                ));
            }
        }
        next.generation = self.current.generation + 1;
        self.store.store(&next)?;
        self.current = next;
        Ok(())
    }

    pub fn advance_phase(&mut self, next_phase: Phase) -> Result<()> {
        let mut next = self.current.clone();
        next.transition(next_phase)?;
        self.persist(next)
    }

    pub fn advance_step(&mut self, index: usize, state: StepState) -> Result<()> {
        let mut next = self.current.clone();
        next.set_step(index, state)?;
        self.persist(next)
    }
}

/// `<private>/<TransactionId>/journal.json` style single-file store.
pub struct FileJournalStore {
    path: PathBuf,
}

impl FileJournalStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl JournalStore for FileJournalStore {
    fn load(&self) -> Result<Option<UpdateJournal>> {
        let Some(bytes) = read_limited(&self.path, MAX_JOURNAL_BYTES)? else {
            return Ok(None);
        };
        let journal: UpdateJournal = parse_strict(&bytes).map_err(|err| {
            UpdateError::new(ErrorCode::JournalInvalid, format!("unreadable journal: {err}"))
        })?;
        journal.validate()?;
        Ok(Some(journal))
    }

    fn store(&mut self, journal: &UpdateJournal) -> Result<()> {
        journal.validate()?;
        let bytes = serde_json::to_vec(journal)
            .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(UpdateError::new(
                ErrorCode::LimitExceeded,
                "journal exceeds the size limit",
            ));
        }
        durable_write(&self.path, &bytes).map_err(UpdateError::from)
    }
}

/// Shareable in-process store, so a "restart" can reopen the same state.
#[derive(Debug, Clone, Default)]
pub struct SharedJournalStore {
    inner: Arc<Mutex<Option<UpdateJournal>>>,
}

impl JournalStore for SharedJournalStore {
    fn load(&self) -> Result<Option<UpdateJournal>> {
        Ok(self.inner.lock().map(|g| g.clone()).unwrap_or_default())
    }

    fn store(&mut self, journal: &UpdateJournal) -> Result<()> {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some(journal.clone());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    Missing,
    File(FileIdentity),
}

/// What is actually on disk, relative to the install root. Implementations
/// must not follow links.
pub trait DiskView {
    fn observe(&self, rel_path: &str) -> Result<Observed>;
}

pub struct FsDiskView {
    root: PathBuf,
}

impl FsDiskView {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }
}

impl DiskView for FsDiskView {
    fn observe(&self, rel_path: &str) -> Result<Observed> {
        validate_rel_path(rel_path)?;
        let components: Vec<&str> = rel_path.split('/').collect();
        Ok(match hash_regular_file_under(&self.root, &components)? {
            None => Observed::Missing,
            Some((size, sha256)) => Observed::File(FileIdentity { size, sha256 }),
        })
    }
}

pub(crate) fn identity_matches(observed: &Observed, expected: &Option<FileIdentity>) -> bool {
    match (observed, expected) {
        (Observed::Missing, None) => true,
        (Observed::File(actual), Some(wanted)) => actual == wanted,
        _ => false,
    }
}

/// The first journaled unchanged file whose live state is not its recorded
/// identity. An observation failure is an error, never a match.
pub(crate) fn first_unchanged_mismatch<'a>(
    journal: &'a UpdateJournal,
    disk: &dyn DiskView,
) -> Result<Option<&'a str>> {
    for file in &journal.unchanged {
        let observed = disk.observe(&file.rel_path)?;
        if !identity_matches(&observed, &Some(file.identity.clone())) {
            return Ok(Some(file.rel_path.as_str()));
        }
    }
    Ok(None)
}

/// Every step must still be at its pre-update identity (and an added file's
/// path must be free). A modified or foreign file stops the update.
pub fn check_plan_preconditions(plan: &ApplyPlan, disk: &dyn DiskView) -> Result<()> {
    for step in &plan.steps {
        let observed = disk.observe(&step.rel_path)?;
        if !identity_matches(&observed, &step.old) {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not in its installed state", step.rel_path),
            ));
        }
    }
    for file in &plan.unchanged {
        let observed = disk.observe(&file.rel_path)?;
        if !identity_matches(&observed, &Some(file.identity.clone())) {
            return Err(UpdateError::new(
                ErrorCode::InstallModified,
                format!("{} is not in its installed state", file.rel_path),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackAction {
    /// Put the verified backup back (the current file is `expected_current`).
    RestoreFromBackup {
        rel_path: String,
        backup_rel: String,
        restore: FileIdentity,
        expected_current: Option<FileIdentity>,
    },
    /// Remove a file the update added (only if it still has this identity).
    RemoveAdded {
        rel_path: String,
        expected: FileIdentity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    NoJournal,
    /// The old version is intact; start it. The staged update is discarded and
    /// never re-applied automatically.
    KeepOld,
    /// The new version was durably committed; keep it.
    KeepNew,
    RollBack {
        actions: Vec<RollbackAction>,
        /// The health-check child must be confirmed gone before acting.
        require_child_exit: bool,
    },
    /// Do not touch anything; manual recovery from a signed ZIP is needed.
    RecoveryRequired(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepDisk {
    Old,
    New,
    Unknown,
}

fn classify(step: &JournalStep, disk: &dyn DiskView) -> Result<StepDisk> {
    let observed = disk.observe(&step.rel_path)?;
    Ok(if identity_matches(&observed, &step.old) {
        StepDisk::Old
    } else if identity_matches(&observed, &step.new) {
        StepDisk::New
    } else {
        StepDisk::Unknown
    })
}

fn required(why: impl Into<String>) -> RecoveryDecision {
    RecoveryDecision::RecoveryRequired(why.into())
}

/// Decides what to do after a restart. An observation failure is an error (the
/// caller retries); it is never turned into a guess.
pub fn decide_recovery(
    journal: Option<&UpdateJournal>,
    install_id: &InstallId,
    disk: &dyn DiskView,
) -> Result<RecoveryDecision> {
    let Some(journal) = journal else {
        return Ok(RecoveryDecision::NoJournal);
    };
    if &journal.install_id != install_id {
        return Ok(required("journal belongs to a different installation"));
    }
    match journal.phase {
        Phase::RecoveryRequired => Ok(required("an earlier recovery did not complete")),
        Phase::Committed => {
            for step in &journal.steps {
                if !identity_matches(&disk.observe(&step.rel_path)?, &step.new) {
                    return Ok(required(format!(
                        "committed update does not match the disk at {}",
                        step.rel_path
                    )));
                }
            }
            if let Some(path) = first_unchanged_mismatch(journal, disk)? {
                return Ok(required(format!(
                    "committed update does not match the disk at unchanged {path}"
                )));
            }
            Ok(RecoveryDecision::KeepNew)
        }
        Phase::RolledBack | Phase::Prepared | Phase::ExitAuthorized => {
            for step in &journal.steps {
                if classify(step, disk)? != StepDisk::Old {
                    return Ok(required(format!(
                        "{} changed although no update was applied",
                        step.rel_path
                    )));
                }
            }
            if let Some(path) = first_unchanged_mismatch(journal, disk)? {
                return Ok(required(format!(
                    "{path} changed although no update was applied"
                )));
            }
            Ok(RecoveryDecision::KeepOld)
        }
        Phase::Applying | Phase::AwaitingHealth | Phase::RollingBack => {
            let mut actions = Vec::new();
            for step in &journal.steps {
                match classify(step, disk)? {
                    StepDisk::Old => {}
                    StepDisk::Unknown => {
                        // Windows replaces a verified file in two steps (move the
                        // verified file aside, then link the new one in). A stop
                        // between them leaves the pathname absent while the
                        // verified backup still holds the old content. Absence
                        // is the only extra state accepted; any other content is
                        // a third party's and stops recovery.
                        if let (Some(old), Some(backup_rel)) = (&step.old, &step.backup_rel)
                            && disk.observe(&step.rel_path)? == Observed::Missing
                            && disk.observe(backup_rel)? == Observed::File(old.clone())
                        {
                            actions.push(RollbackAction::RestoreFromBackup {
                                rel_path: step.rel_path.clone(),
                                backup_rel: backup_rel.clone(),
                                restore: old.clone(),
                                expected_current: None,
                            });
                            continue;
                        }
                        return Ok(required(format!(
                            "{} matches neither the old nor the new content",
                            step.rel_path
                        )));
                    }
                    StepDisk::New => match (&step.old, &step.backup_rel) {
                        (Some(old), Some(backup_rel)) => {
                            let backup = disk.observe(backup_rel)?;
                            if backup != Observed::File(old.clone()) {
                                return Ok(required(format!(
                                    "backup of {} is missing or damaged",
                                    step.rel_path
                                )));
                            }
                            actions.push(RollbackAction::RestoreFromBackup {
                                rel_path: step.rel_path.clone(),
                                backup_rel: backup_rel.clone(),
                                restore: old.clone(),
                                expected_current: step.new.clone(),
                            });
                        }
                        (None, _) => {
                            let Some(added) = step.new.clone() else {
                                return Ok(required("journal step changes nothing"));
                            };
                            actions.push(RollbackAction::RemoveAdded {
                                rel_path: step.rel_path.clone(),
                                expected: added,
                            });
                        }
                        (Some(_), None) => {
                            return Ok(required(format!("{} has no backup recorded", step.rel_path)));
                        }
                    },
                }
            }
            if actions.is_empty() {
                // Only an install that is wholly at the old version, including
                // the files the update never touches, is kept.
                if let Some(path) = first_unchanged_mismatch(journal, disk)? {
                    return Ok(required(format!(
                        "{path} changed during the update and is not the signed file"
                    )));
                }
                Ok(RecoveryDecision::KeepOld)
            } else {
                Ok(RecoveryDecision::RollBack {
                    actions,
                    require_child_exit: journal.phase == Phase::AwaitingHealth,
                })
            }
        }
    }
}
