//! App-owned application update service (Issue #510, stage U3).
//!
//! The service lives for the whole process and is created without any network
//! or archive work. It communicates only after the user presses a button:
//! `check` reads the latest stable release and its signed catalog, `download`
//! fetches the archive into a private work directory, verifies hash, size and
//! ZIP layout, extracts it into a private stage and only then offers it as
//! `Ready`. Applying a Ready update (helper, file replacement, rollback) is not
//! part of this stage, so nothing here reports an update as applied.
//!
//! Asynchronous results are accepted only while the live state still carries
//! the `InstallId + OperationId + CandidateId` they were started with
//! ([`Ticket`]); a cancelled, superseded or repeated operation therefore can
//! never overwrite the current state.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use hane_ui::{AppUpdateController, AppUpdatePhase, AppUpdateSnapshot, PreparedUpdate};
use hane_update::archive::{ArchiveLimits, stage_archive};
use hane_update::catalog::{
    CandidateId, VerifiedCandidate, VerifyContext, confirm_selection, record_seen, verify_catalog,
};
use hane_update::clock::Clock;
use hane_update::download::{ArchiveDownload, CancelToken, Fetcher, PartStore, Transport};
use hane_update::fsutil::{DirPartStore, FileSeenStore, durable_write, hash_regular_file, read_limited};
use hane_update::identity::InstallId;
use hane_update::limits::{MAX_CATALOG_BYTES, MAX_SIGNATURE_BYTES};
use hane_update::model::Target;
use hane_update::trust::{SeenStore, TrustPolicy};
use hane_update::version::{VersionRelation, parse_current_version};
use hane_update::error::io_detail;
use hane_update::{ErrorCode, UpdateError};

use crate::update_diagnostics::{Diagnostics, Identity, Redactor, short_id};
use crate::update_restart::{self, HelperHandoff};

pub(crate) const CATALOG_FILE: &str = "hane-update.json";
pub(crate) const SIGNATURE_FILE: &str = "hane-update.json.sig";
pub(crate) const ARCHIVE_FILE: &str = "archive.zip";
const STAGE_DIR: &str = "stage";
const READY_MARKER: &str = "ready";
const SEEN_FILE: &str = hane_update::platform::lease::SEEN_CATALOG_FILE;

pub type Task = Box<dyn FnOnce() + Send>;
/// Runs a task on a background thread. Returns false when it cannot be run.
pub type Runner = Box<dyn Fn(Task) -> bool + Send + Sync>;

pub struct UpdateServiceConfig {
    pub current_version: String,
    /// `None` when this OS/CPU has no update distribution.
    pub target: Option<Target>,
    pub policy: TrustPolicy,
    pub install_id: InstallId,
    /// `<state root>/updates/<InstallId>`: candidates and work directories.
    pub updates_root: PathBuf,
    /// Private directory for the highest-seen catalog sequence.
    pub receipts_dir: PathBuf,
}

/// What the service needs to hand a Ready update to the helper. A service
/// without it (tests, unsupported placements) never offers a restart.
pub struct RestartWiring {
    /// The supported placement of the running install.
    pub placement: hane_update::platform::Placement,
    /// The running executable, used to re-locate the install before a ticket.
    pub exe: PathBuf,
    /// The running application's own state root.
    pub state_root: PathBuf,
}

/// Everything about one verified, signed release candidate.
struct Offer {
    candidate: VerifiedCandidate,
    catalog: Vec<u8>,
    signature: Vec<u8>,
}

impl Offer {
    fn id(&self) -> &CandidateId {
        self.candidate.candidate_id()
    }
}

enum Stage {
    Idle,
    Checking {
        op: u64,
        cancel: CancelToken,
    },
    UpToDate,
    Available(Arc<Offer>),
    Downloading {
        op: u64,
        offer: Arc<Offer>,
        cancel: CancelToken,
        received: Arc<AtomicU64>,
        verifying: bool,
    },
    Ready {
        offer: Arc<Offer>,
        dir: PathBuf,
    },
    /// The Ready candidate is being removed in the background.
    Discarding {
        op: u64,
        offer: Arc<Offer>,
        dir: PathBuf,
    },
    /// `stage` names where it failed (`check`, `download`, `verify`, ...); the
    /// same code can mean different things at different stages.
    Failed {
        code: ErrorCode,
        stage: &'static str,
    },
    Unavailable {
        code: ErrorCode,
        stage: &'static str,
    },
}

struct State {
    next_op: u64,
    stage: Stage,
    restoring: bool,
    restore_started: bool,
}

/// Identity of one asynchronous operation. A result is applied only when the
/// live state yields an equal ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ticket {
    install: InstallId,
    op: u64,
    candidate: Option<CandidateId>,
}

impl State {
    fn ticket(&self, install: &InstallId) -> Option<Ticket> {
        match &self.stage {
            Stage::Checking { op, .. } => Some(Ticket {
                install: install.clone(),
                op: *op,
                candidate: None,
            }),
            Stage::Downloading { op, offer, .. } => Some(Ticket {
                install: install.clone(),
                op: *op,
                candidate: Some(offer.id().clone()),
            }),
            _ => None,
        }
    }
}

enum CheckOutcome {
    UpToDate,
    Available(Box<Offer>),
}

struct Inner {
    config: UpdateServiceConfig,
    transport: Arc<dyn Transport>,
    clock: Arc<dyn Clock>,
    runner: Runner,
    state: Mutex<State>,
    restart: OnceLock<RestartWiring>,
    /// Unique per service instance (process and instance), so work and trash
    /// directories of different processes never collide.
    owner: String,
    /// Exclusive lock on `leases/<owner>.lock`, held while this service may
    /// use its work or trash directory. The OS releases it when the process
    /// ends, which is what lets another process recognise stale directories.
    lease: Mutex<Option<File>>,
    /// Exclusive lock on `leases/candidate-<id>.candidate`, held while this
    /// service's Ready (or Discarding) candidate lives in the shared
    /// `candidates/<id>` directory. It serialises publication, discard and
    /// the helper handoff across processes.
    candidate_lease: Mutex<Option<File>>,
    /// Local, bounded failure records (Issue #553).
    diag: Diagnostics,
    /// Why the default runner could not start its thread, when it could not.
    spawn_error: Arc<Mutex<Option<String>>>,
}

fn new_owner_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

const LEASE_SUFFIX: &str = ".lock";
const CANDIDATE_LEASE_SUFFIX: &str = ".candidate";

pub struct AppUpdateService {
    inner: Arc<Inner>,
}

fn thread_runner(spawn_error: Arc<Mutex<Option<String>>>) -> Runner {
    Box::new(move |task| {
        match std::thread::Builder::new()
            .name("hane-app-update".to_owned())
            .spawn(task)
        {
            Ok(_) => true,
            Err(error) => {
                *spawn_error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(io_detail(&error));
                false
            }
        }
    })
}

pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn remove_dir_if_present(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Counts bytes as the update crate streams them into the private part file.
struct CountingWriter {
    inner: Box<dyn Write>,
    received: Arc<AtomicU64>,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.received.fetch_add(written as u64, Ordering::Relaxed);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct ProgressPartStore {
    inner: DirPartStore,
    received: Arc<AtomicU64>,
}

impl PartStore for ProgressPartStore {
    fn begin_part(&mut self) -> io::Result<Box<dyn Write>> {
        self.received.store(0, Ordering::Relaxed);
        Ok(Box::new(CountingWriter {
            inner: self.inner.begin_part()?,
            received: Arc::clone(&self.received),
        }))
    }

    fn commit_part(&mut self) -> io::Result<()> {
        self.inner.commit_part()
    }

    fn discard_part(&mut self) {
        self.inner.discard_part();
    }
}

impl AppUpdateService {
    pub fn new(
        config: UpdateServiceConfig,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let spawn_error = Arc::new(Mutex::new(None));
        let runner = thread_runner(Arc::clone(&spawn_error));
        Self::build(config, transport, clock, runner, spawn_error)
    }

    /// Like [`AppUpdateService::new`] with an explicit task runner, so tests
    /// can control when background work happens.
    #[cfg(test)]
    pub fn with_runner(
        config: UpdateServiceConfig,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
        runner: Runner,
    ) -> Self {
        Self::build(config, transport, clock, runner, Arc::new(Mutex::new(None)))
    }

    fn build(
        config: UpdateServiceConfig,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
        runner: Runner,
        spawn_error: Arc<Mutex<Option<String>>>,
    ) -> Self {
        // Without embedded trust material no catalog can ever be verified, so
        // the service says so up front instead of contacting the network.
        let stage = if !config.policy.is_configured() {
            Stage::Unavailable {
                code: ErrorCode::TrustUnconfigured,
                stage: "config",
            }
        } else if config.target.is_none() {
            Stage::Unavailable {
                code: ErrorCode::TargetUnsupported,
                stage: "config",
            }
        } else {
            Stage::Idle
        };
        let owner = new_owner_id();
        let exe = std::env::current_exe().map_or_else(
            |error| format!("unknown ({})", io_detail(&error)),
            |exe| exe.display().to_string(),
        );
        let diag = Diagnostics::new(
            &config.updates_root,
            Identity {
                run: owner.clone(),
                pid: std::process::id(),
                exe,
                install: config.install_id.as_str().to_owned(),
                state: short_id(&config.updates_root.display().to_string()),
                version: config.current_version.clone(),
            },
            Redactor::from_environment(),
        );
        Self {
            inner: Arc::new(Inner {
                config,
                transport,
                clock,
                runner,
                state: Mutex::new(State {
                    next_op: 0,
                    stage,
                    restoring: false,
                    restore_started: false,
                }),
                restart: OnceLock::new(),
                owner,
                lease: Mutex::new(None),
                candidate_lease: Mutex::new(None),
                diag,
                spawn_error,
            }),
        }
    }

    /// Lets the service hand a Ready update to the helper. Called once at
    /// startup, and only for a supported placement; the restart is still
    /// offered only when the trust policy is configured.
    pub fn enable_restart(&self, wiring: RestartWiring) {
        let _ = self.inner.restart.set(wiring);
    }

    /// Records that the running install location cannot be updated. The
    /// service then never downloads, restores or prepares an update, so no
    /// archive is fetched into a receipts directory that is not usable.
    /// A build that is already unavailable keeps its own reason.
    pub fn block_placement(&self, error: &UpdateError, exe: &Path) {
        let blocked = {
            let mut state = self.inner.lock();
            let idle = matches!(state.stage, Stage::Idle);
            if idle {
                state.stage = Stage::Unavailable {
                    code: placement_block_code(error, exe),
                    stage: "placement",
                };
            }
            idle
        };
        if blocked {
            // The original error is kept: the shown code alone cannot say
            // whether the location, the owner or the I/O failed.
            let shown = placement_block_code(error, exe);
            self.inner.diag.record(
                "placement",
                shown.as_str(),
                &format!(
                    "locate failed with {}: {}; exe={}",
                    error.code.as_str(),
                    error.detail,
                    exe.display()
                ),
                None,
            );
        }
    }

    /// The text the user can open or copy from the settings card. Local
    /// records only; no request is made and nothing is changed.
    pub fn diagnostics_text(&self) -> String {
        self.inner.diag.report()
    }
}

/// The reason shown for a placement that failed to resolve. The failure class
/// from `locate` is kept (damaged, I/O, unsupported); only a path inside an
/// App Translocation copy gets its own code, since the user action differs.
fn placement_block_code(error: &UpdateError, exe: &Path) -> ErrorCode {
    if exe.components().any(|c| c.as_os_str() == "AppTranslocation") {
        ErrorCode::AppTranslocated
    } else {
        error.code
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn candidates_dir(&self) -> PathBuf {
        self.config.updates_root.join("candidates")
    }

    fn work_root(&self) -> PathBuf {
        self.config.updates_root.join("work")
    }

    fn trash_root(&self) -> PathBuf {
        self.config.updates_root.join("trash")
    }

    fn leases_dir(&self) -> PathBuf {
        self.config.updates_root.join("leases")
    }

    /// This service's own work directory; no other service uses it.
    fn work_dir(&self) -> PathBuf {
        self.work_root().join(&self.owner)
    }

    /// This service's own trash directory; no other service uses it.
    fn trash_dir(&self) -> PathBuf {
        self.trash_root().join(&self.owner)
    }

    /// Takes the exclusive lease for this service's work and trash directories.
    /// Must succeed before either directory is created.
    fn ensure_lease(&self) -> io::Result<()> {
        let mut lease = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
        if lease.is_some() {
            return Ok(());
        }
        let dir = self.leases_dir();
        create_private_dir(&dir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{}{LEASE_SUFFIX}", self.owner)))?;
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::Error(error) => error,
            fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "update lease is held")
            }
        })?;
        *lease = Some(file);
        Ok(())
    }

    /// Takes the exclusive lease for one shared candidate directory. `None`
    /// means another service (in any process) currently holds it.
    fn acquire_candidate_lease(&self, id: &str) -> io::Result<Option<File>> {
        let dir = self.leases_dir();
        create_private_dir(&dir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("candidate-{id}{CANDIDATE_LEASE_SUFFIX}")))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error),
        }
    }

    /// Removes work and trash directories of owners whose lease can be taken,
    /// i.e. owners that are no longer running. A directory whose lease is
    /// held (an active download or discard in any process) is never touched.
    fn remove_stale_work(&self) {
        let Ok(entries) = fs::read_dir(self.leases_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(owner) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(LEASE_SUFFIX))
                .map(str::to_owned)
            else {
                continue;
            };
            if owner == self.owner {
                continue;
            }
            let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            if file.try_lock().is_err() {
                continue;
            }
            remove_dir_if_present(&self.work_root().join(&owner));
            remove_dir_if_present(&self.trash_root().join(&owner));
            drop(file);
            let _ = fs::remove_file(&path);
        }
    }

    fn seen_store(&self) -> FileSeenStore {
        FileSeenStore::new(self.config.receipts_dir.join(SEEN_FILE))
    }

    /// Runs `task` in the background. If the runner cannot start it, the
    /// operation identified by `op` fails instead of waiting forever.
    fn dispatch(self: &Arc<Self>, op: u64, task: Task) {
        if !(self.runner)(task) {
            let detail = self.spawn_failure_detail();
            {
                let mut state = self.lock();
                let version = match &state.stage {
                    Stage::Downloading { offer, .. } => {
                        Some(offer.candidate.version().to_string())
                    }
                    _ => None,
                };
                let running = match &state.stage {
                    Stage::Checking { op: live, .. } | Stage::Downloading { op: live, .. } => {
                        *live == op
                    }
                    _ => false,
                };
                if running {
                    state.stage = Stage::Failed {
                        code: ErrorCode::Io,
                        stage: "start",
                    };
                    self.diag.note(
                        "start",
                        ErrorCode::Io.as_str(),
                        &detail,
                        version.as_deref(),
                    );
                }
            }
            self.diag.flush();
        }
    }

    /// The recorded reason a background task could not be started.
    fn spawn_failure_detail(&self) -> String {
        self.spawn_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| "the task runner refused to start the task".to_owned())
    }

    // ------------------------------------------------------------ check

    fn start_check(self: &Arc<Self>) {
        let (op, cancel) = {
            let mut state = self.lock();
            if state.restoring {
                return;
            }
            match state.stage {
                Stage::Idle | Stage::UpToDate | Stage::Available(_) | Stage::Failed { .. } => {}
                // A running operation absorbs repeated clicks; a Ready update
                // is never replaced by a newer check; an unavailable service
                // has nothing to check.
                Stage::Checking { .. }
                | Stage::Downloading { .. }
                | Stage::Ready { .. }
                | Stage::Discarding { .. }
                | Stage::Unavailable { .. } => return,
            }
            state.next_op += 1;
            let op = state.next_op;
            let cancel = CancelToken::new();
            state.stage = Stage::Checking {
                op,
                cancel: cancel.clone(),
            };
            (op, cancel)
        };
        let inner = Arc::clone(self);
        self.dispatch(op, Box::new(move || inner.run_check(op, &cancel)));
    }

    fn run_check(&self, op: u64, cancel: &CancelToken) {
        let result = self.check_latest(cancel);
        let mut state = self.lock();
        let expected = Ticket {
            install: self.config.install_id.clone(),
            op,
            candidate: None,
        };
        if state.ticket(&self.config.install_id) != Some(expected) {
            // Cancelled or superseded while the request was in flight.
            return;
        }
        state.stage = match result {
            Ok(CheckOutcome::UpToDate) => Stage::UpToDate,
            Ok(CheckOutcome::Available(offer)) => Stage::Available(Arc::from(offer)),
            Err(error) if error.code == ErrorCode::Cancelled => Stage::Idle,
            Err(error) => {
                // Only a live operation records, so a late result of a
                // cancelled or superseded check never reaches the diagnostics.
                self.diag
                    .note("check", error.code.as_str(), &error.detail, None);
                Stage::Failed {
                    code: error.code,
                    stage: "check",
                }
            }
        };
        drop(state);
        self.diag.flush();
    }

    fn verify_context<'a>(
        &'a self,
        target: Target,
        seen: Option<&'a hane_update::trust::SeenCatalog>,
        expected_tag: Option<&'a str>,
    ) -> VerifyContext<'a> {
        VerifyContext {
            policy: &self.config.policy,
            clock: &*self.clock,
            current_version: &self.config.current_version,
            target,
            seen,
            expected_tag,
        }
    }

    fn check_latest(&self, cancel: &CancelToken) -> Result<CheckOutcome, UpdateError> {
        let target = self.config.target.ok_or_else(|| {
            UpdateError::new(ErrorCode::TargetUnsupported, "no update distribution")
        })?;
        let current = parse_current_version(&self.config.current_version)?;
        let fetcher = Fetcher {
            transport: &*self.transport,
            clock: &*self.clock,
        };
        let deadline = fetcher.metadata_deadline();
        let selection = fetcher.check_latest_within(&current, target, &deadline, cancel)?;
        let (catalog, signature) = fetcher.fetch_catalog_within(&selection, &deadline, cancel)?;
        let seen = self.seen_store().load()?;
        let candidate = verify_catalog(
            &catalog,
            &signature,
            &self.verify_context(target, seen.as_ref(), Some(selection.tag.as_str())),
        )?;
        confirm_selection(&selection, &candidate)?;
        match candidate.relation() {
            VersionRelation::Same => Ok(CheckOutcome::UpToDate),
            VersionRelation::Older => Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "the latest release is older than this build",
            )),
            VersionRelation::Newer => Ok(CheckOutcome::Available(Box::new(Offer {
                candidate,
                catalog,
                signature,
            }))),
        }
    }

    // --------------------------------------------------------- download

    fn start_download(self: &Arc<Self>, candidate_id: &str) {
        let (op, offer, cancel, received) = {
            let mut state = self.lock();
            if state.restoring {
                return;
            }
            let Stage::Available(offer) = &state.stage else {
                return;
            };
            if offer.id().as_str() != candidate_id || !offer.candidate.is_applicable() {
                return;
            }
            let offer = Arc::clone(offer);
            state.next_op += 1;
            let op = state.next_op;
            let cancel = CancelToken::new();
            let received = Arc::new(AtomicU64::new(0));
            state.stage = Stage::Downloading {
                op,
                offer: Arc::clone(&offer),
                cancel: cancel.clone(),
                received: Arc::clone(&received),
                verifying: false,
            };
            (op, offer, cancel, received)
        };
        let inner = Arc::clone(self);
        self.dispatch(
            op,
            Box::new(move || inner.run_download(op, &offer, &cancel, &received)),
        );
    }

    fn run_download(&self, op: u64, offer: &Arc<Offer>, cancel: &CancelToken, received: &Arc<AtomicU64>) {
        let work = self
            .work_dir()
            .join(format!("{op}-{}", offer.id().as_str()));
        let mut step = "write_download";
        let outcome = self.download_into(op, offer, cancel, received, &work, &mut step);
        let version = offer.candidate.version().to_string();

        let mut state = self.lock();
        let expected = Ticket {
            install: self.config.install_id.clone(),
            op,
            candidate: Some(offer.id().clone()),
        };
        let live = state.ticket(&self.config.install_id) == Some(expected);
        if !live {
            // Cancelled or superseded: this operation's private directory is
            // the only thing it may touch.
            remove_dir_if_present(&work);
            return;
        }
        match outcome {
            Ok(()) => {
                let target = self.candidates_dir().join(offer.id().as_str());
                let moved = self.publish_candidate(offer.id().as_str(), &work, &target);
                state.stage = match moved {
                    Ok(()) => Stage::Ready {
                        offer: Arc::clone(offer),
                        dir: target,
                    },
                    Err(error) => {
                        remove_dir_if_present(&work);
                        self.diag.note(
                            "publish",
                            ErrorCode::Io.as_str(),
                            &io_detail(&error),
                            Some(version.as_str()),
                        );
                        Stage::Failed {
                            code: ErrorCode::Io,
                            stage: "publish",
                        }
                    }
                };
            }
            Err(error) => {
                remove_dir_if_present(&work);
                state.stage = if error.code == ErrorCode::Cancelled {
                    Stage::Available(Arc::clone(offer))
                } else {
                    self.diag.note(
                        step,
                        error.code.as_str(),
                        &error.detail,
                        Some(version.as_str()),
                    );
                    Stage::Failed {
                        code: error.code,
                        stage: step,
                    }
                };
            }
        }
        drop(state);
        self.diag.flush();
    }

    /// Moves the verified work directory to the shared candidate directory.
    /// The candidate lease is taken first, so a candidate that another
    /// process still holds (Ready, restarting or being discarded) is never
    /// replaced or deleted. On success this service keeps the lease for as
    /// long as the candidate is Ready or being discarded.
    fn publish_candidate(&self, id: &str, work: &Path, target: &Path) -> io::Result<()> {
        let Some(lease) = self.acquire_candidate_lease(id)? else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "candidate is held by another process",
            ));
        };
        remove_dir_if_present(target);
        create_private_dir(&self.candidates_dir())?;
        fs::rename(work, target)?;
        *self
            .candidate_lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(lease);
        Ok(())
    }

    fn mark_verifying(&self, op: u64, offer: &Offer) {
        let mut state = self.lock();
        let expected = Ticket {
            install: self.config.install_id.clone(),
            op,
            candidate: Some(offer.id().clone()),
        };
        if state.ticket(&self.config.install_id) == Some(expected)
            && let Stage::Downloading { verifying, .. } = &mut state.stage
        {
            *verifying = true;
        }
    }

    /// Fetches, verifies and stages the archive inside `work`. Nothing outside
    /// `work` and the sequence receipt is touched. `step` always names the part
    /// that is running (`write_download`, `download`, `verify`, `save_history`),
    /// so a failure is attributed to where it happened and a local write
    /// failure after the download is not reported as a failed check.
    fn download_into(
        &self,
        op: u64,
        offer: &Offer,
        cancel: &CancelToken,
        received: &Arc<AtomicU64>,
        work: &Path,
        step: &mut &'static str,
    ) -> Result<(), UpdateError> {
        let entry = offer.candidate.target_entry();
        let tag = &offer.candidate.catalog().tag;
        *step = "write_download";
        self.ensure_lease()?;
        create_private_dir(work)?;
        durable_write(&work.join(CATALOG_FILE), &offer.catalog)?;
        durable_write(&work.join(SIGNATURE_FILE), &offer.signature)?;

        let mut store = ProgressPartStore {
            inner: DirPartStore::new(work, ARCHIVE_FILE)?,
            received: Arc::clone(received),
        };
        let fetcher = Fetcher {
            transport: &*self.transport,
            clock: &*self.clock,
        };
        *step = "download";
        fetcher.download_archive(
            &ArchiveDownload {
                tag,
                name: &entry.asset_name,
                size: entry.archive_size,
                sha256: &entry.archive_sha256,
            },
            &mut store,
            cancel,
        )?;
        if cancel.is_cancelled() {
            return Err(UpdateError::new(ErrorCode::Cancelled, "cancelled"));
        }

        self.mark_verifying(op, offer);
        *step = "verify";
        let stage_dir = work.join(STAGE_DIR);
        create_private_dir(&stage_dir)?;
        let mut archive = File::open(work.join(ARCHIVE_FILE))?;
        stage_archive(
            &mut archive,
            &offer.candidate,
            &self.config.policy,
            &ArchiveLimits::default(),
            &stage_dir,
        )?;
        drop(archive);
        if cancel.is_cancelled() {
            return Err(UpdateError::new(ErrorCode::Cancelled, "cancelled"));
        }

        // A candidate is Ready only once its sequence floor is stored.
        *step = "save_history";
        create_private_dir(&self.config.receipts_dir)?;
        let mut seen = self.seen_store();
        record_seen(&mut seen, &offer.candidate)?;
        durable_write(&work.join(READY_MARKER), offer.id().as_str().as_bytes())?;
        Ok(())
    }

    // ----------------------------------------------------------- cancel

    fn cancel_running(&self) {
        let mut state = self.lock();
        match &state.stage {
            Stage::Checking { cancel, .. } => {
                cancel.cancel();
                state.stage = Stage::Idle;
            }
            Stage::Downloading { cancel, offer, .. } => {
                cancel.cancel();
                let offer = Arc::clone(offer);
                state.stage = Stage::Available(offer);
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------- discard

    /// Returns before any filesystem call. The stage becomes `Discarding`, which
    /// keeps check, download, restore and repeated discards away from the
    /// candidate directory until the background task has finished.
    fn discard_ready(self: &Arc<Self>) {
        let (op, offer, dir) = {
            let mut state = self.lock();
            let Stage::Ready { offer, dir } = &state.stage else {
                return;
            };
            let offer = Arc::clone(offer);
            let dir = dir.clone();
            state.next_op += 1;
            let op = state.next_op;
            state.stage = Stage::Discarding {
                op,
                offer: Arc::clone(&offer),
                dir: dir.clone(),
            };
            (op, offer, dir)
        };
        let inner = Arc::clone(self);
        let task_dir = dir.clone();
        let started = (self.runner)(Box::new(move || inner.run_discard(op, &task_dir)));
        if !started {
            // Nothing ran, so the prepared update is untouched: make it
            // Ready again so the user can retry.
            let detail = self.spawn_failure_detail();
            let mut state = self.lock();
            if matches!(&state.stage, Stage::Discarding { op: live, .. } if *live == op) {
                self.diag.note("start", ErrorCode::Io.as_str(), &detail, None);
                state.stage = Stage::Ready { offer, dir };
            }
            drop(state);
            self.diag.flush();
        }
    }

    fn run_discard(&self, op: u64, dir: &Path) {
        let trash = self.trash_dir().join(op.to_string());
        let moved = self
            .ensure_lease()
            .and_then(|()| create_private_dir(&self.trash_dir()))
            .and_then(|()| fs::rename(dir, &trash));
        if moved.is_ok() {
            // The candidate directory is out of the way; only now may another
            // process publish the same CandidateId.
            *self
                .candidate_lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
        {
            let mut state = self.lock();
            let live = matches!(&state.stage, Stage::Discarding { op: live, .. } if *live == op);
            if live {
                state.stage = match (moved.as_ref(), &state.stage) {
                    (Ok(()), _) => Stage::Idle,
                    // The candidate is still in place: keep it retryable.
                    (Err(_), Stage::Discarding { offer, dir, .. }) => Stage::Ready {
                        offer: Arc::clone(offer),
                        dir: dir.clone(),
                    },
                    (Err(_), _) => Stage::Idle,
                };
            }
        }
        match &moved {
            Ok(()) => {
                // The slow deletion runs after the candidate directory is out
                // of the way and the lock is released.
                remove_dir_if_present(&trash);
            }
            Err(error) => {
                self.diag
                    .record("discard", ErrorCode::Io.as_str(), &io_detail(error), None);
            }
        }
    }

    // ---------------------------------------------------------- restore

    fn start_restore(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.restore_started || !matches!(state.stage, Stage::Idle) {
                return;
            }
            state.restore_started = true;
            state.restoring = true;
        }
        let inner = Arc::clone(self);
        if !(self.runner)(Box::new(move || inner.run_restore())) {
            let detail = self.spawn_failure_detail();
            {
                let mut state = self.lock();
                state.restoring = false;
                self.diag.note("start", ErrorCode::Io.as_str(), &detail, None);
            }
            self.diag.flush();
        }
    }

    /// Local re-validation only: no request is made. Anything that does not
    /// verify exactly is removed rather than offered.
    fn run_restore(&self) {
        self.remove_stale_work();
        let mut found: Option<(Offer, PathBuf, File)> = None;
        if let Ok(entries) = fs::read_dir(self.candidates_dir()) {
            for entry in entries.flatten() {
                let dir = entry.path();
                let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                // A candidate held by another process is in use there: it is
                // neither adopted nor removed.
                let Ok(Some(lease)) = self.acquire_candidate_lease(name) else {
                    continue;
                };
                match (found.is_none(), self.restore_candidate(&dir)) {
                    (true, Ok(offer)) => found = Some((offer, dir, lease)),
                    (_, Err(error)) => {
                        // The rejected candidate is removed as before; why it
                        // was rejected is kept for the diagnostics. A candidate
                        // that is merely no longer newer is the normal cleanup
                        // after an applied update, not a failure.
                        if error.code != ErrorCode::NotApplicable {
                            self.diag
                                .note("restore", error.code.as_str(), &error.detail, None);
                        }
                        remove_dir_if_present(&dir);
                    }
                    (false, Ok(_)) => remove_dir_if_present(&dir),
                }
            }
        }
        self.diag.flush();
        let mut state = self.lock();
        state.restoring = false;
        let idle = matches!(state.stage, Stage::Idle);
        if let (true, Some((offer, dir, lease))) = (idle, found) {
            *self
                .candidate_lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(lease);
            state.stage = Stage::Ready {
                offer: Arc::new(offer),
                dir,
            };
        }
    }

    fn restore_candidate(&self, dir: &Path) -> Result<Offer, UpdateError> {
        let missing = |what: &str| UpdateError::new(ErrorCode::AssetMissing, what.to_owned());
        let target = self
            .config
            .target
            .ok_or_else(|| UpdateError::new(ErrorCode::TargetUnsupported, "no target"))?;
        let catalog = read_limited(&dir.join(CATALOG_FILE), MAX_CATALOG_BYTES)?
            .ok_or_else(|| missing("catalog is missing"))?;
        let signature = read_limited(&dir.join(SIGNATURE_FILE), MAX_SIGNATURE_BYTES)?
            .ok_or_else(|| missing("signature is missing"))?;
        let seen = self.seen_store().load()?;
        let candidate = verify_catalog(
            &catalog,
            &signature,
            &self.verify_context(target, seen.as_ref(), None),
        )?;
        if !candidate.is_applicable() {
            return Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "prepared update is not newer than this build",
            ));
        }
        if dir.file_name().and_then(|name| name.to_str()) != Some(candidate.candidate_id().as_str())
        {
            return Err(UpdateError::new(
                ErrorCode::AssetMismatch,
                "candidate directory does not match its catalog",
            ));
        }
        let marker = read_limited(&dir.join(READY_MARKER), 128)?
            .ok_or_else(|| missing("ready marker is missing"))?;
        if marker != candidate.candidate_id().as_str().as_bytes() {
            return Err(UpdateError::new(
                ErrorCode::AssetMismatch,
                "ready marker differs from the catalog",
            ));
        }
        let entry = candidate.target_entry();
        let (size, sha256) = hash_regular_file(&dir.join(ARCHIVE_FILE))?
            .ok_or_else(|| missing("archive is missing"))?;
        if size != entry.archive_size || sha256 != entry.archive_sha256 {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                "prepared archive no longer matches the signed catalog",
            ));
        }
        match fs::symlink_metadata(dir.join(STAGE_DIR)) {
            Ok(meta) if meta.is_dir() => {}
            _ => return Err(missing("stage is missing")),
        }
        Ok(Offer {
            candidate,
            catalog,
            signature,
        })
    }

    /// Whether this build can really apply a Ready update: trust material is
    /// embedded, this OS/CPU has a distribution and the install is at a
    /// supported placement. Everything else stays fail-closed.
    fn restart_supported(&self) -> bool {
        self.config.policy.is_configured()
            && self.config.target.is_some()
            && self.restart.get().is_some()
    }

    /// True while `candidate_id` is still the Ready update and nothing is
    /// restoring it.
    fn is_ready(&self, candidate_id: &str) -> bool {
        let state = self.lock();
        matches!(
            &state.stage,
            Stage::Ready { offer, .. } if !state.restoring && offer.id().as_str() == candidate_id
        )
    }

    /// Re-confirms the Ready candidate and the install location. The ticket is
    /// written only if this still holds.
    ///
    /// The error is returned as is; the handoff's failure sink records it once.
    fn confirm_restart(&self, candidate_id: &str) -> Result<(), UpdateError> {
        if !self.is_ready(candidate_id) {
            return Err(UpdateError::new(
                ErrorCode::TransitionInvalid,
                "the prepared update changed",
            ));
        }
        let wiring = self.restart.get().ok_or_else(|| {
            UpdateError::new(ErrorCode::UnsupportedInstall, "restart is not enabled")
        })?;
        let placement = hane_update::platform::locate(&wiring.exe)?;
        if placement.install_id != wiring.placement.install_id {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the install location changed",
            ));
        }
        Ok(())
    }

    /// Records a failure of the restart preparation before the error is turned
    /// into text, so the code and detail are not lost.
    fn prepare_failure(&self, error: &UpdateError, version: Option<&str>) -> String {
        self.diag
            .record("prepare", error.code.as_str(), &error.detail, version);
        error.to_string()
    }

    /// Starts the helper for the Ready candidate. The lock is not held while
    /// the helper starts. Nothing is stopped or quit here.
    fn prepare_restart(self: &Arc<Self>, candidate_id: &str) -> Result<Box<dyn PreparedUpdate>, String> {
        let unsupported = || "この環境では更新を適用できません".to_owned();
        if !self.restart_supported() {
            return Err(unsupported());
        }
        let wiring = self.restart.get().ok_or_else(unsupported)?;
        let (dir, version) = {
            let state = self.lock();
            match &state.stage {
                Stage::Ready { offer, dir }
                    if !state.restoring && offer.id().as_str() == candidate_id =>
                {
                    (dir.clone(), offer.candidate.version().to_string())
                }
                _ => return Err("準備した更新が変わりました。もう一度お試しください".to_owned()),
            }
        };
        let prepared = update_restart::start_helper(
            &wiring.placement,
            &self.config.policy,
            candidate_id,
            &dir,
            &wiring.state_root,
        )
        .map_err(|err| self.prepare_failure(&err, Some(&version)))?;
        // A failure from here on drops `prepared`, which aborts the helper.
        let coordination = wiring
            .placement
            .coordination()
            .map_err(|err| self.prepare_failure(&err, Some(&version)))?;
        let inner = Arc::clone(self);
        let candidate = candidate_id.to_owned();
        Ok(Box::new(HelperHandoff::new(
            prepared,
            coordination,
            wiring.state_root.clone(),
            Box::new(move || inner.confirm_restart(&candidate)),
            self.handoff_failure_sink(version),
        )))
    }

    /// Records a failure of the ticket write or the helper authorization, which
    /// happen after `prepare_restart` returned.
    fn handoff_failure_sink(self: &Arc<Self>, version: String) -> Box<dyn Fn(&UpdateError) + Send> {
        let inner = Arc::clone(self);
        Box::new(move |error| {
            inner.prepare_failure(error, Some(&version));
        })
    }

    fn snapshot(&self) -> AppUpdateSnapshot {
        let state = self.lock();
        let mut snapshot = AppUpdateSnapshot::idle(self.config.current_version.clone());
        snapshot.restoring = state.restoring;
        snapshot.restart_supported = self.restart_supported();
        snapshot.diagnostics_available = self.diag.has_records();
        let describe = |snapshot: &mut AppUpdateSnapshot, offer: &Offer| {
            snapshot.candidate_id = Some(offer.id().as_str().to_owned());
            snapshot.candidate_version = Some(offer.candidate.version().to_string());
            snapshot.total_bytes = offer.candidate.target_entry().archive_size;
        };
        match &state.stage {
            Stage::Idle => {}
            Stage::Checking { .. } => snapshot.phase = AppUpdatePhase::Checking,
            Stage::UpToDate => snapshot.phase = AppUpdatePhase::UpToDate,
            Stage::Available(offer) => {
                snapshot.phase = AppUpdatePhase::Available;
                describe(&mut snapshot, offer);
            }
            Stage::Downloading {
                offer,
                received,
                verifying,
                ..
            } => {
                snapshot.phase = if *verifying {
                    AppUpdatePhase::Verifying
                } else {
                    AppUpdatePhase::Downloading
                };
                describe(&mut snapshot, offer);
                snapshot.received_bytes = received.load(Ordering::Relaxed);
            }
            Stage::Ready { offer, .. } => {
                snapshot.phase = AppUpdatePhase::Ready;
                describe(&mut snapshot, offer);
            }
            // The removal runs in the background; the UI keeps polling until
            // the service returns to Idle (or Ready after a failure).
            Stage::Discarding { offer, .. } => {
                snapshot.phase = AppUpdatePhase::Discarding;
                describe(&mut snapshot, offer);
            }
            Stage::Failed { code, stage } => {
                snapshot.phase = AppUpdatePhase::Failed;
                snapshot.error_code = Some(code.as_str().to_owned());
                snapshot.error_stage = Some((*stage).to_owned());
            }
            Stage::Unavailable { code, stage } => {
                snapshot.phase = AppUpdatePhase::Unavailable;
                snapshot.error_code = Some(code.as_str().to_owned());
                snapshot.error_stage = Some((*stage).to_owned());
            }
        }
        snapshot
    }
}

impl AppUpdateController for AppUpdateService {
    fn snapshot(&self) -> AppUpdateSnapshot {
        self.inner.snapshot()
    }

    fn check(&self) {
        self.inner.start_check();
    }

    fn download(&self, candidate_id: &str) {
        self.inner.start_download(candidate_id);
    }

    fn cancel(&self) {
        self.inner.cancel_running();
    }

    fn discard_ready(&self) {
        self.inner.discard_ready();
    }

    fn restore_local(&self) {
        self.inner.start_restore();
    }

    fn diagnostics_text(&self) -> Option<String> {
        Some(AppUpdateService::diagnostics_text(self))
    }

    fn prepare_restart(&self, candidate_id: &str) -> Result<Box<dyn PreparedUpdate>, String> {
        self.inner.prepare_restart(candidate_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_update::download::{asset_url, latest_release_url};
    use hane_update::identity::InstallFacts;
    use hane_update::model::{CATALOG_DOMAIN, CATALOG_ASSET_NAME, SIGNATURE_ASSET_NAME};
    use hane_update::testing::{
        FixedClock, Scripted, ScriptedTransport, TEST_KEY_ID, TempDir, WindowsRelease,
        default_payload, release_json_sized, sign_with, test_now, test_policy, windows_release,
    };

    const ARCHIVE_NAME: &str = "hane-windows-x64.zip";

    struct Fixture {
        dir: TempDir,
        transport: Arc<ScriptedTransport>,
        clock: Arc<FixedClock>,
        queue: Arc<Mutex<Vec<Task>>>,
        service: AppUpdateService,
        release: WindowsRelease,
        tag: String,
    }

    fn install_id() -> InstallId {
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "volume",
            install_path: "/opt/hane",
            owner: "user",
        })
        .unwrap()
    }

    fn config(dir: &TempDir, current: &str, policy: TrustPolicy, target: Option<Target>) -> UpdateServiceConfig {
        let id = install_id();
        UpdateServiceConfig {
            current_version: current.to_owned(),
            target,
            policy,
            updates_root: dir.path().join("updates").join(id.as_str()),
            receipts_dir: dir.path().join("receipts"),
            install_id: id,
        }
    }

    fn manual_runner(queue: &Arc<Mutex<Vec<Task>>>) -> Runner {
        let queue = Arc::clone(queue);
        Box::new(move |task| {
            queue.lock().unwrap().push(task);
            true
        })
    }

    fn run_queued(queue: &Arc<Mutex<Vec<Task>>>) {
        loop {
            let tasks: Vec<Task> = std::mem::take(&mut *queue.lock().unwrap());
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                task();
            }
        }
    }

    fn fixture(current: &str, version: &str, sequence: u64) -> Fixture {
        let release = windows_release(version, sequence, default_payload(version));
        let dir = TempDir::new("app-update");
        let transport = Arc::new(ScriptedTransport::new());
        let clock = Arc::new(FixedClock::new(test_now()));
        let queue = Arc::new(Mutex::new(Vec::new()));
        let service = AppUpdateService::with_runner(
            config(&dir, current, test_policy(), Some(Target::WindowsX64)),
            transport.clone(),
            clock.clone(),
            manual_runner(&queue),
        );
        Fixture {
            dir,
            transport,
            clock,
            queue,
            service,
            release,
            tag: format!("v{version}"),
        }
    }

    impl Fixture {
        fn script_check(&self) {
            let json = release_json_sized(
                &self.tag,
                &[
                    (CATALOG_ASSET_NAME, self.release.catalog.len() as u64),
                    (SIGNATURE_ASSET_NAME, self.release.catalog_sig.len() as u64),
                    (ARCHIVE_NAME, self.release.zip.len() as u64),
                ],
                false,
                false,
            );
            self.transport
                .push(&latest_release_url(), Scripted::body(200, json));
            self.transport.push(
                &asset_url(&self.tag, CATALOG_ASSET_NAME),
                Scripted::body(200, self.release.catalog.clone()),
            );
            self.transport.push(
                &asset_url(&self.tag, SIGNATURE_ASSET_NAME),
                Scripted::body(200, self.release.catalog_sig.clone()),
            );
        }

        fn script_archive(&self) {
            self.transport.push(
                &asset_url(&self.tag, ARCHIVE_NAME),
                Scripted::body(200, self.release.zip.clone()),
            );
        }

        fn phase(&self) -> AppUpdatePhase {
            self.service.snapshot().phase
        }

        fn error_code(&self) -> Option<String> {
            self.service.snapshot().error_code
        }

        fn requests(&self) -> usize {
            self.transport.requests().len()
        }

        fn check_now(&self) {
            self.service.check();
            run_queued(&self.queue);
        }

        fn candidate_id(&self) -> String {
            self.service.snapshot().candidate_id.expect("a candidate")
        }

        fn prepare_ready(&self) {
            self.script_check();
            self.script_archive();
            self.check_now();
            assert_eq!(self.phase(), AppUpdatePhase::Available);
            self.service.download(&self.candidate_id());
            run_queued(&self.queue);
            assert_eq!(self.phase(), AppUpdatePhase::Ready);
        }

        fn updates_root(&self) -> PathBuf {
            self.dir.path().join("updates").join(install_id().as_str())
        }
    }

    #[test]
    fn construction_snapshot_and_restore_send_no_request() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        let _ = fixture.service.snapshot();
        fixture.service.restore_local();
        run_queued(&fixture.queue);
        assert_eq!(fixture.requests(), 0);
        assert_eq!(fixture.phase(), AppUpdatePhase::Idle);
        assert!(fixture.queue.lock().unwrap().is_empty());
        let _ = &fixture.clock;
    }

    #[test]
    fn an_unconfigured_build_is_unavailable_and_never_contacts_the_network() {
        let dir = TempDir::new("app-update-unconfigured");
        let transport = Arc::new(ScriptedTransport::new());
        let queue = Arc::new(Mutex::new(Vec::new()));
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", TrustPolicy::production(), Some(Target::WindowsX64)),
            transport.clone(),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Unavailable);
        assert_eq!(snapshot.error_code.as_deref(), Some("trust_unconfigured"));
        service.check();
        service.restore_local();
        run_queued(&queue);
        assert!(transport.requests().is_empty());
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Unavailable);
        assert!(!snapshot.restart_supported);
        assert!(service.prepare_restart(&"a".repeat(64)).is_err());
    }

    #[test]
    fn a_failed_placement_blocks_check_download_and_restore_before_any_request() {
        let dir = TempDir::new("app-update-placement");
        let transport = Arc::new(ScriptedTransport::new());
        let queue = Arc::new(Mutex::new(Vec::new()));
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            transport.clone(),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        let error = UpdateError::new(ErrorCode::UnsupportedInstall, "translocated");
        let exe = Path::new("/private/var/folders/x/AppTranslocation/ABC/d/Hane.app/Contents/MacOS/hane");
        service.block_placement(&error, exe);
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Unavailable);
        assert_eq!(snapshot.error_code.as_deref(), Some("app_translocated"));
        service.check();
        service.restore_local();
        service.download(&"a".repeat(64));
        run_queued(&queue);
        assert!(transport.requests().is_empty());
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Unavailable);
        assert!(service.prepare_restart(&"a".repeat(64)).is_err());
    }

    #[test]
    fn an_owner_mismatched_placement_stays_blocked_with_its_code() {
        let dir = TempDir::new("app-update-owner-mismatch");
        let transport = Arc::new(ScriptedTransport::new());
        let queue = Arc::new(Mutex::new(Vec::new()));
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            transport.clone(),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        let error = UpdateError::new(ErrorCode::IdentityMismatch, "owner differs");
        service.block_placement(&error, Path::new("/Applications/Hane.app/Contents/MacOS/hane"));
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Unavailable);
        assert_eq!(snapshot.error_code.as_deref(), Some("identity_mismatch"));
        service.check();
        service.restore_local();
        service.download(&"a".repeat(64));
        run_queued(&queue);
        assert!(transport.requests().is_empty());
        let after = service.snapshot();
        assert_eq!(after.phase, AppUpdatePhase::Unavailable);
        assert_eq!(after.error_code.as_deref(), Some("identity_mismatch"));
        assert!(!after.restart_supported);
        assert!(service.prepare_restart(&"a".repeat(64)).is_err());
    }

    #[test]
    fn a_placement_failure_keeps_its_class_and_a_normal_service_still_downloads() {
        let error = UpdateError::new(ErrorCode::InstallModified, "damaged");
        assert_eq!(
            placement_block_code(&error, Path::new("/Applications/Hane.app/Contents/MacOS/hane")),
            ErrorCode::InstallModified
        );
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        assert_eq!(fixture.phase(), AppUpdatePhase::Ready);
    }

    #[test]
    fn a_service_without_a_wired_placement_never_offers_or_starts_a_restart() {
        let dir = TempDir::new("app-update-unwired");
        let queue = Arc::new(Mutex::new(Vec::new()));
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        assert!(!service.snapshot().restart_supported);
        assert!(service.prepare_restart(&"a".repeat(64)).is_err());
    }

    #[test]
    fn an_unsupported_target_is_unavailable() {
        let dir = TempDir::new("app-update-target");
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), None),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&Arc::new(Mutex::new(Vec::new()))),
        );
        assert_eq!(
            service.snapshot().error_code.as_deref(),
            Some("target_unsupported")
        );
    }

    #[test]
    fn a_newer_verified_release_is_available_and_the_same_version_is_up_to_date() {
        let newer = fixture("0.26.0", "0.27.0", 5);
        newer.script_check();
        newer.check_now();
        let snapshot = newer.service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Available);
        assert_eq!(snapshot.candidate_version.as_deref(), Some("0.27.0"));
        assert_eq!(newer.requests(), 3);

        let same = fixture("0.27.0", "0.27.0", 5);
        same.script_check();
        same.check_now();
        assert_eq!(same.phase(), AppUpdatePhase::UpToDate);

        let older = fixture("0.28.0", "0.27.0", 5);
        older.script_check();
        older.check_now();
        assert_eq!(older.phase(), AppUpdatePhase::Failed);
        assert_eq!(older.error_code().as_deref(), Some("not_applicable"));
    }

    #[test]
    fn http_and_signature_failures_are_never_reported_as_up_to_date() {
        for (status, code) in [(404, "not_found"), (403, "rate_limited"), (429, "rate_limited")] {
            let fixture = fixture("0.26.0", "0.27.0", 5);
            fixture
                .transport
                .push(&latest_release_url(), Scripted::status(status));
            fixture.check_now();
            assert_eq!(fixture.phase(), AppUpdatePhase::Failed, "HTTP {status}");
            assert_eq!(fixture.error_code().as_deref(), Some(code));
        }

        let mut forged = fixture("0.26.0", "0.27.0", 5);
        forged.release.catalog_sig =
            sign_with(2, TEST_KEY_ID, CATALOG_DOMAIN, &forged.release.catalog);
        forged.script_check();
        forged.check_now();
        assert_eq!(forged.phase(), AppUpdatePhase::Failed);
        assert_eq!(forged.error_code().as_deref(), Some("signature_invalid"));
    }

    #[test]
    fn repeated_clicks_join_one_operation() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        fixture.service.check();
        fixture.service.check();
        fixture.service.check();
        assert_eq!(fixture.queue.lock().unwrap().len(), 1);
        assert_eq!(fixture.phase(), AppUpdatePhase::Checking);
        run_queued(&fixture.queue);
        assert_eq!(fixture.phase(), AppUpdatePhase::Available);
        assert_eq!(fixture.requests(), 3);
    }

    #[test]
    fn a_cancelled_check_cannot_be_overwritten_by_its_late_result() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        fixture.service.check();
        fixture.service.cancel();
        assert_eq!(fixture.phase(), AppUpdatePhase::Idle);
        run_queued(&fixture.queue);
        assert_eq!(
            fixture.phase(),
            AppUpdatePhase::Idle,
            "a result that arrives after cancel must be dropped"
        );

        // A newer check started after the cancel is not disturbed by the old one.
        fixture.script_check();
        fixture.service.check();
        run_queued(&fixture.queue);
        assert_eq!(fixture.phase(), AppUpdatePhase::Available);
    }

    #[test]
    fn download_becomes_ready_only_after_every_verification() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let id = fixture.candidate_id();
        let dir = fixture.updates_root().join("candidates").join(&id);
        assert!(dir.join("archive.zip").is_file());
        assert!(dir.join("stage").join("hane.exe").is_file());
        assert!(dir.join("ready").is_file());
        assert!(fixture.dir.path().join("receipts").join("seen-catalog.json").is_file());
        assert_eq!(fixture.requests(), 4);
        assert!(
            fixture
                .service
                .inner
                .work_dir()
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_none()),
            "the work directory must be emptied by the move to candidates"
        );
    }

    #[test]
    fn a_corrupt_archive_never_becomes_ready_and_leaves_nothing_behind() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        let mut tampered = fixture.release.zip.clone();
        let last = tampered.len() / 2;
        tampered[last] ^= 0xff;
        fixture.transport.push(
            &asset_url(&fixture.tag, ARCHIVE_NAME),
            Scripted::body(200, tampered),
        );
        fixture.check_now();
        fixture.service.download(&fixture.candidate_id());
        run_queued(&fixture.queue);
        assert_eq!(fixture.phase(), AppUpdatePhase::Failed);
        assert_eq!(fixture.error_code().as_deref(), Some("hash_mismatch"));
        assert!(!fixture.updates_root().join("candidates").exists());
        assert!(
            !fixture.dir.path().join("receipts").join("seen-catalog.json").exists(),
            "an unverified archive must not advance the sequence floor"
        );
    }

    #[test]
    fn download_clicks_for_another_candidate_or_repeats_do_not_start_extra_work() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        fixture.check_now();
        fixture.service.download("not-the-candidate");
        assert!(fixture.queue.lock().unwrap().is_empty());
        let id = fixture.candidate_id();
        fixture.service.download(&id);
        fixture.service.download(&id);
        assert_eq!(fixture.queue.lock().unwrap().len(), 1);
    }

    #[test]
    fn cancelling_a_download_returns_to_the_offer_and_the_late_result_is_dropped() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        fixture.script_archive();
        fixture.check_now();
        let id = fixture.candidate_id();
        fixture.service.download(&id);
        assert_eq!(fixture.phase(), AppUpdatePhase::Downloading);
        fixture.service.cancel();
        assert_eq!(fixture.phase(), AppUpdatePhase::Available);
        run_queued(&fixture.queue);
        assert_eq!(fixture.phase(), AppUpdatePhase::Available);
        assert!(!fixture.updates_root().join("candidates").exists());
    }

    #[test]
    fn a_ready_update_is_not_replaced_by_a_new_check_and_survives_a_restart() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let requests = fixture.requests();
        fixture.service.check();
        assert!(fixture.queue.lock().unwrap().is_empty());
        assert_eq!(fixture.phase(), AppUpdatePhase::Ready);
        assert_eq!(fixture.requests(), requests);

        // "Restart": a new service over the same directories, local checks only.
        // The previous process is gone, so its candidate lease is released.
        *fixture.service.inner.candidate_lease.lock().unwrap() = None;
        let queue = Arc::new(Mutex::new(Vec::new()));
        let transport = Arc::new(ScriptedTransport::new());
        let reopened = AppUpdateService::with_runner(
            config(&fixture.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            transport.clone(),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        assert_eq!(reopened.snapshot().phase, AppUpdatePhase::Idle);
        reopened.restore_local();
        assert!(reopened.snapshot().restoring);
        run_queued(&queue);
        let snapshot = reopened.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Ready);
        assert_eq!(snapshot.candidate_version.as_deref(), Some("0.27.0"));
        assert!(transport.requests().is_empty(), "restore must stay local");
    }

    #[test]
    fn a_tampered_prepared_archive_is_discarded_on_restore() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let archive = fixture
            .updates_root()
            .join("candidates")
            .join(fixture.candidate_id())
            .join("archive.zip");
        let mut bytes = fs::read(&archive).unwrap();
        bytes[0] ^= 0xff;
        fs::write(&archive, bytes).unwrap();
        *fixture.service.inner.candidate_lease.lock().unwrap() = None;

        let queue = Arc::new(Mutex::new(Vec::new()));
        let reopened = AppUpdateService::with_runner(
            config(&fixture.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        reopened.restore_local();
        run_queued(&queue);
        assert_eq!(reopened.snapshot().phase, AppUpdatePhase::Idle);
        assert!(!archive.exists());
        let report = reopened.diagnostics_text();
        assert!(report.contains("stage=restore"), "{report}");
    }

    #[test]
    fn an_already_applied_candidate_is_removed_on_restore_without_a_failure_record() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let candidate_dir = fixture
            .updates_root()
            .join("candidates")
            .join(fixture.candidate_id());
        assert!(candidate_dir.is_dir());
        *fixture.service.inner.candidate_lease.lock().unwrap() = None;

        // The update was applied: the running build is now the candidate's version.
        let queue = Arc::new(Mutex::new(Vec::new()));
        let reopened = AppUpdateService::with_runner(
            config(&fixture.dir, "0.27.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        reopened.restore_local();
        run_queued(&queue);
        assert_ne!(reopened.snapshot().phase, AppUpdatePhase::Ready);
        assert!(!candidate_dir.exists());
        let report = reopened.diagnostics_text();
        assert!(!report.contains("stage=restore"), "{report}");
        assert!(!report.contains("not_applicable"), "{report}");
    }

    #[test]
    fn a_prepared_update_for_another_target_is_not_restored() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        *fixture.service.inner.candidate_lease.lock().unwrap() = None;
        let queue = Arc::new(Mutex::new(Vec::new()));
        let reopened = AppUpdateService::with_runner(
            config(&fixture.dir, "0.26.0", test_policy(), Some(Target::MacosArm64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        reopened.restore_local();
        run_queued(&queue);
        assert_eq!(reopened.snapshot().phase, AppUpdatePhase::Idle);
    }

    #[test]
    fn discarding_the_ready_update_removes_it() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let candidates = fixture.updates_root().join("candidates");
        let id = fixture.candidate_id();
        fixture.service.discard_ready();
        // Repeated clicks queue no second task.
        fixture.service.discard_ready();
        fixture.service.discard_ready();
        assert_eq!(fixture.queue.lock().unwrap().len(), 1);
        // Before the task runs nothing was touched and no operation may reuse the path.
        assert!(candidates.join(&id).is_dir());
        assert!(!fixture.updates_root().join("trash").exists());
        let requests = fixture.requests();
        fixture.service.check();
        fixture.service.restore_local();
        fixture.service.download(&id);
        assert_eq!(fixture.queue.lock().unwrap().len(), 1);
        assert_eq!(fixture.requests(), requests);
        // The removal is queued, so the UI sees the busy Discarding phase.
        assert_eq!(fixture.phase(), AppUpdatePhase::Discarding);

        run_queued(&fixture.queue);
        assert_eq!(fixture.phase(), AppUpdatePhase::Idle);
        assert!(candidates.read_dir().unwrap().next().is_none());
        let trash = fixture.service.inner.trash_dir();
        assert!(!trash.exists() || trash.read_dir().unwrap().next().is_none());
    }

    #[test]
    fn restore_in_another_service_does_not_touch_an_active_download() {
        let first = fixture("0.26.0", "0.27.0", 5);
        // The archive route is scripted below with a hook, so the plain
        // `script_archive` response must not be queued ahead of it.
        first.script_check();
        first.check_now();
        let id = first.candidate_id();

        // A second, independent service over the same directories.
        let second_queue = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(AppUpdateService::with_runner(
            config(&first.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&second_queue),
        ));

        // While the first download is in flight (its archive request is being
        // served), the second service starts and finishes its restore.
        let work_seen = Arc::new(Mutex::new(None));
        let zip = first.release.zip.clone();
        {
            let second = Arc::clone(&second);
            let second_queue = Arc::clone(&second_queue);
            let first_work = first.service.inner.work_dir();
            let first_trash = first.service.inner.trash_dir();
            let work_seen = Arc::clone(&work_seen);
            first.transport.push(
                &asset_url(&first.tag, ARCHIVE_NAME),
                Scripted::Custom(Box::new(move || {
                    let existed_before = first_work.is_dir();
                    fs::create_dir_all(&first_trash).unwrap();
                    second.restore_local();
                    run_queued(&second_queue);
                    *work_seen.lock().unwrap() =
                        Some((existed_before, first_work.is_dir(), first_trash.is_dir()));
                    Ok(hane_update::download::Response {
                        status: 200,
                        location: None,
                        content_length: Some(zip.len() as u64),
                        body: Box::new(hane_update::testing::ChunkReader::new(zip, None)),
                    })
                })),
            );
        }
        first.service.download(&id);
        run_queued(&first.queue);

        assert_eq!(
            *work_seen.lock().unwrap(),
            Some((true, true, true)),
            "restore in another service must keep the active work and trash"
        );
        assert_eq!(first.phase(), AppUpdatePhase::Ready);
        assert_eq!(second.snapshot().phase, AppUpdatePhase::Idle);
    }

    #[test]
    fn restore_removes_work_of_a_service_that_is_gone() {
        let first = fixture("0.26.0", "0.27.0", 5);
        first.script_check();
        first.script_archive();
        first.check_now();
        first.service.download(&first.candidate_id());
        let stale_work = first.service.inner.work_dir();
        let stale_trash = first.service.inner.trash_dir();
        run_queued(&first.queue);
        // Leave leftovers as a crashed process would, then release its lease.
        fs::create_dir_all(stale_work.join("leftover")).unwrap();
        fs::create_dir_all(&stale_trash).unwrap();
        *first.service.inner.lease.lock().unwrap() = None;

        let queue = Arc::new(Mutex::new(Vec::new()));
        let second = AppUpdateService::with_runner(
            config(&first.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        second.restore_local();
        run_queued(&queue);
        assert!(!stale_work.exists());
        assert!(!stale_trash.exists());
    }

    #[test]
    fn a_runner_that_cannot_start_the_discard_keeps_the_ready_update_retryable() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let id = fixture.candidate_id();
        let dir = fixture.updates_root().join("candidates").join(&id);

        // Reopen over the same directories with a runner that can be told to refuse.
        *fixture.service.inner.candidate_lease.lock().unwrap() = None;
        let accept = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let queue = Arc::new(Mutex::new(Vec::new()));
        let runner: Runner = {
            let accept = Arc::clone(&accept);
            let queue = Arc::clone(&queue);
            Box::new(move |task| {
                if !accept.load(Ordering::SeqCst) {
                    return false;
                }
                queue.lock().unwrap().push(task);
                true
            })
        };
        let service = AppUpdateService::with_runner(
            config(&fixture.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            runner,
        );
        service.restore_local();
        run_queued(&queue);
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Ready);

        accept.store(false, Ordering::SeqCst);
        service.discard_ready();
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Ready);
        assert!(dir.is_dir(), "a refused task must not lose the prepared update");

        // The retry works and a second click still queues exactly one task.
        accept.store(true, Ordering::SeqCst);
        service.discard_ready();
        service.discard_ready();
        assert_eq!(queue.lock().unwrap().len(), 1);
        // While the removal is queued the UI must see a busy Discarding phase
        // so it keeps polling until the final state is rendered.
        let during = service.snapshot();
        assert_eq!(during.phase, AppUpdatePhase::Discarding);
        assert!(during.is_busy());
        run_queued(&queue);
        assert!(!service.snapshot().is_busy());
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Idle);
        assert!(!dir.exists());
    }

    #[test]
    fn a_second_service_finishing_the_same_candidate_never_removes_the_active_one() {
        let first = fixture("0.26.0", "0.27.0", 5);
        first.prepare_ready();
        let id = first.candidate_id();
        let dir = first.updates_root().join("candidates").join(&id);

        // An independent service over the same directories downloads the
        // same CandidateId while the first one holds it as Ready.
        let second_queue = Arc::new(Mutex::new(Vec::new()));
        let second_transport = Arc::new(ScriptedTransport::new());
        let second = AppUpdateService::with_runner(
            config(&first.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            second_transport.clone(),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&second_queue),
        );
        let json = release_json_sized(
            &first.tag,
            &[
                (CATALOG_ASSET_NAME, first.release.catalog.len() as u64),
                (SIGNATURE_ASSET_NAME, first.release.catalog_sig.len() as u64),
                (ARCHIVE_NAME, first.release.zip.len() as u64),
            ],
            false,
            false,
        );
        second_transport.push(&latest_release_url(), Scripted::body(200, json));
        second_transport.push(
            &asset_url(&first.tag, CATALOG_ASSET_NAME),
            Scripted::body(200, first.release.catalog.clone()),
        );
        second_transport.push(
            &asset_url(&first.tag, SIGNATURE_ASSET_NAME),
            Scripted::body(200, first.release.catalog_sig.clone()),
        );
        second_transport.push(
            &asset_url(&first.tag, ARCHIVE_NAME),
            Scripted::body(200, first.release.zip.clone()),
        );
        second.check();
        run_queued(&second_queue);
        assert_eq!(second.snapshot().phase, AppUpdatePhase::Available);

        // The first service starts discarding; the second download completes
        // before the discard task runs.
        first.service.discard_ready();
        second.download(&id);
        run_queued(&second_queue);
        assert_eq!(second.snapshot().phase, AppUpdatePhase::Failed);
        assert!(dir.join("archive.zip").is_file());
        assert!(dir.join("stage").join("hane.exe").is_file());
        assert!(dir.join("ready").is_file());
        assert_eq!(first.phase(), AppUpdatePhase::Discarding);

        // Ready state is also untouched by a completed download elsewhere.
        run_queued(&first.queue);
        assert_eq!(first.phase(), AppUpdatePhase::Idle);
        assert!(!dir.exists());
    }

    #[test]
    fn restore_in_another_service_leaves_a_candidate_held_elsewhere() {
        let first = fixture("0.26.0", "0.27.0", 5);
        first.prepare_ready();
        let dir = first
            .updates_root()
            .join("candidates")
            .join(first.candidate_id());

        let queue = Arc::new(Mutex::new(Vec::new()));
        let second = AppUpdateService::with_runner(
            config(&first.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        second.restore_local();
        run_queued(&queue);
        assert_eq!(second.snapshot().phase, AppUpdatePhase::Idle);
        assert!(dir.join("ready").is_file());
        assert_eq!(first.phase(), AppUpdatePhase::Ready);
    }

    #[test]
    fn a_runner_that_cannot_start_fails_the_operation_instead_of_hanging() {
        let dir = TempDir::new("app-update-runner");
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            Box::new(|_| false),
        );
        service.check();
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Failed);
        assert_eq!(snapshot.error_stage.as_deref(), Some("start"));
        assert!(!snapshot.is_busy(), "a refused start must not stay busy");
        let report = service.diagnostics_text();
        assert!(report.contains("stage=start"), "{report}");
        assert!(report.contains("refused to start"), "{report}");
        // The failed start is not a success and can be tried again.
        service.check();
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Failed);
    }

    // ------------------------------------------------ diagnostics (#553)

    fn current_section(report: &str) -> &str {
        report
            .split_once("## 以前の起動の記録")
            .map_or(report, |(current, _)| current)
    }

    #[test]
    fn a_failed_check_is_recorded_with_its_stage_and_code() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture
            .transport
            .push(&latest_release_url(), Scripted::status(404));
        fixture.check_now();
        let snapshot = fixture.service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Failed);
        assert_eq!(snapshot.error_stage.as_deref(), Some("check"));
        assert!(snapshot.diagnostics_available);
        let report = fixture.service.diagnostics_text();
        assert!(report.contains("stage=check"), "{report}");
        assert!(report.contains("code=not_found"), "{report}");
        assert!(report.contains("version=0.26.0"), "{report}");
        assert!(report.contains(&format!("PID {}", std::process::id())), "{report}");
        assert!(report.contains(install_id().as_str()), "{report}");
    }

    #[test]
    fn a_write_failure_after_the_download_is_not_reported_as_a_failed_check() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        fixture.script_archive();
        fixture.check_now();
        // The receipts location can no longer be created once the archive is
        // verified; the update itself and the log stay writable.
        fs::write(fixture.dir.path().join("receipts"), b"not a directory").unwrap();
        fixture.service.download(&fixture.candidate_id());
        run_queued(&fixture.queue);
        let snapshot = fixture.service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Failed);
        assert_eq!(snapshot.error_stage.as_deref(), Some("save_history"));
        assert_ne!(snapshot.error_code.as_deref(), Some("not_found"));
        assert!(!fixture.updates_root().join("candidates").exists());
        let report = fixture.service.diagnostics_text();
        assert!(report.contains("stage=save_history"), "{report}");
        assert!(!report.contains("stage=check"), "{report}");
        assert!(report.contains("os_kind="), "the OS error kind must be kept: {report}");
        assert!(report.contains("candidate=0.27.0"), "{report}");
    }

    #[test]
    fn a_candidate_that_cannot_be_moved_keeps_its_cause() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.script_check();
        fixture.script_archive();
        fixture.check_now();
        fs::create_dir_all(fixture.updates_root()).unwrap();
        fs::write(fixture.updates_root().join("candidates"), b"in the way").unwrap();
        fixture.service.download(&fixture.candidate_id());
        run_queued(&fixture.queue);
        let snapshot = fixture.service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Failed);
        assert_eq!(snapshot.error_stage.as_deref(), Some("publish"));
        let report = fixture.service.diagnostics_text();
        assert!(report.contains("stage=publish"), "{report}");
        assert!(report.contains("os_kind="), "{report}");
        assert!(
            fixture
                .service
                .inner
                .work_dir()
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_none()),
            "the private work directory must be cleaned"
        );
    }

    #[test]
    fn a_handoff_failure_after_prepare_is_saved_and_shown_with_its_os_facts() {
        let dir = TempDir::new("app-update-diag-handoff");
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&Arc::new(Mutex::new(Vec::new()))),
        );
        assert!(!service.snapshot().diagnostics_available);
        let sink = service.inner.handoff_failure_sink("0.27.0".to_owned());
        sink(&UpdateError::new(
            ErrorCode::Io,
            "access denied (os_kind=PermissionDenied, os_error=5)",
        ));
        assert!(service.snapshot().diagnostics_available);
        let saved =
            fs::read_to_string(service.inner.config.updates_root.join("diagnostics.log")).unwrap();
        let shared = service.diagnostics_text();
        for text in [&saved, &shared] {
            assert!(text.contains("stage=prepare"), "{text}");
            assert!(text.contains("code=io"), "{text}");
            assert!(text.contains("os_kind=PermissionDenied, os_error=5"), "{text}");
            assert!(text.contains("candidate=0.27.0"), "{text}");
        }
    }

    #[test]
    fn the_restart_reconfirmation_returns_its_own_error_code_without_recording_it() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
        let candidate = fixture.candidate_id();
        let inner = &fixture.service.inner;
        let before = inner.diag.has_records();

        // Restart is not wired up.
        let err = inner.confirm_restart(&candidate).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedInstall);

        let exe = fixture.dir.path().join("missing").join("hane");
        fixture.service.enable_restart(RestartWiring {
            placement: hane_update::platform::Placement {
                target: Target::WindowsX64,
                install_root: fixture.dir.path().join("missing"),
                coordination_parent: fixture.dir.path().to_path_buf(),
                install_id: install_id(),
            },
            exe: exe.clone(),
            state_root: fixture.dir.path().join("state"),
        });

        // Another candidate than the Ready one.
        let err = inner.confirm_restart("another-candidate").unwrap_err();
        assert_eq!(err.code, ErrorCode::TransitionInvalid);

        // The install cannot be located again: the cause is returned as is.
        let located = hane_update::platform::locate(&exe).unwrap_err();
        let err = inner.confirm_restart(&candidate).unwrap_err();
        assert_eq!(err.code, located.code);
        assert_eq!(err.detail, located.detail);

        // Returning an error is not recording it: the failure sink does that once.
        assert_eq!(inner.diag.has_records(), before);
    }

    #[test]
    fn the_unavailable_context_is_recorded_without_allowing_updates() {
        let dir = TempDir::new("app-update-diag-placement");
        let transport = Arc::new(ScriptedTransport::new());
        let queue = Arc::new(Mutex::new(Vec::new()));
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            transport.clone(),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        let error = UpdateError::new(ErrorCode::IdentityMismatch, "owner differs");
        service.block_placement(
            &error,
            Path::new("/Applications/Hane.app/Contents/MacOS/hane"),
        );
        let snapshot = service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Unavailable);
        assert_eq!(snapshot.error_stage.as_deref(), Some("placement"));
        assert!(snapshot.diagnostics_available);
        let report = service.diagnostics_text();
        assert!(report.contains("stage=placement"), "{report}");
        assert!(report.contains("owner differs"), "{report}");
        // Reading the diagnostics re-enables nothing.
        service.check();
        service.download(&"a".repeat(64));
        service.restore_local();
        run_queued(&queue);
        assert!(transport.requests().is_empty());
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Unavailable);
        assert!(service.prepare_restart(&"a".repeat(64)).is_err());
    }

    #[test]
    fn secrets_and_user_names_do_not_reach_the_saved_or_shared_text() {
        let dir = TempDir::new("app-update-diag-secrets");
        let service = AppUpdateService::with_runner(
            config(&dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&Arc::new(Mutex::new(Vec::new()))),
        );
        let error = UpdateError::new(
            ErrorCode::Io,
            "open https://qzxw:hunter2@host.example/asset?X-Amz-Signature=deadbeef failed \
             token=abc123 at /Users/qzxwuser/Library/Hane",
        );
        service.block_placement(&error, Path::new("/Applications/Hane.app/Contents/MacOS/hane"));
        let shared = service.diagnostics_text();
        let saved =
            fs::read_to_string(service.inner.config.updates_root.join("diagnostics.log")).unwrap();
        for text in [&shared, &saved] {
            for leak in ["hunter2", "deadbeef", "abc123", "qzxwuser", "X-Amz"] {
                assert!(!text.contains(leak), "{leak} leaked: {text}");
            }
        }
        assert!(saved.contains("host.example"), "{saved}");
    }

    #[test]
    fn a_cancelled_operations_late_result_does_not_touch_the_diagnostics() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture
            .transport
            .push(&latest_release_url(), Scripted::status(404));
        fixture.service.check();
        fixture.service.cancel();
        run_queued(&fixture.queue);
        let snapshot = fixture.service.snapshot();
        assert_eq!(snapshot.phase, AppUpdatePhase::Idle);
        assert!(!snapshot.diagnostics_available);
        assert!(!fixture.service.diagnostics_text().contains("stage=check"));
    }

    #[test]
    fn records_of_an_earlier_run_are_kept_apart_from_the_current_failure() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture
            .transport
            .push(&latest_release_url(), Scripted::status(404));
        fixture.check_now();
        let first = fixture.service.diagnostics_text();

        let queue = Arc::new(Mutex::new(Vec::new()));
        let reopened = AppUpdateService::with_runner(
            config(&fixture.dir, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        // The earlier record makes the report reachable, but the current
        // service is healthy: nothing is turned into a current failure.
        let reopened_snapshot = reopened.snapshot();
        assert!(reopened_snapshot.diagnostics_available);
        assert_eq!(reopened_snapshot.phase, AppUpdatePhase::Idle);
        assert_eq!(reopened_snapshot.error_code, None);
        assert_eq!(reopened_snapshot.error_stage, None);
        let report = reopened.diagnostics_text();
        assert!(!current_section(&report).contains("stage=check"), "{report}");
        let (_, past) = report.split_once("## 以前の起動の記録").unwrap();
        assert!(past.contains("stage=check") && past.contains("code=not_found"), "{report}");
        assert_ne!(first, report);

        // A different state directory is told apart by its state id.
        let other = TempDir::new("app-update-diag-other-state");
        let other_service = AppUpdateService::with_runner(
            config(&other, "0.26.0", test_policy(), Some(Target::WindowsX64)),
            Arc::new(ScriptedTransport::new()),
            Arc::new(FixedClock::new(test_now())),
            manual_runner(&queue),
        );
        let state_line = |report: &str| {
            report
                .lines()
                .find(|line| line.starts_with("state: "))
                .map(str::to_owned)
        };
        assert_ne!(state_line(&report), state_line(&other_service.diagnostics_text()));
    }

    #[test]
    fn an_unwritable_log_does_not_block_the_update_or_hide_the_failure() {
        // Only the log location is unusable: the update still becomes Ready.
        let ready = fixture("0.26.0", "0.27.0", 5);
        fs::create_dir_all(ready.updates_root().join("diagnostics.log")).unwrap();
        ready.prepare_ready();
        assert_eq!(ready.phase(), AppUpdatePhase::Ready);

        // A failure is still a failure, and the report says it was not saved.
        let failed = fixture("0.26.0", "0.27.0", 5);
        fs::create_dir_all(failed.updates_root().join("diagnostics.log")).unwrap();
        failed
            .transport
            .push(&latest_release_url(), Scripted::status(404));
        failed.check_now();
        assert_eq!(failed.phase(), AppUpdatePhase::Failed);
        let report = failed.service.diagnostics_text();
        assert!(report.contains("保存: できませんでした"), "{report}");
        assert!(report.contains("code=not_found"), "{report}");
    }
}
