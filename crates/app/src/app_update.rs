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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hane_ui::{AppUpdateController, AppUpdatePhase, AppUpdateSnapshot};
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
use hane_update::{ErrorCode, UpdateError};

const CATALOG_FILE: &str = "hane-update.json";
const SIGNATURE_FILE: &str = "hane-update.json.sig";
const ARCHIVE_FILE: &str = "archive.zip";
const STAGE_DIR: &str = "stage";
const READY_MARKER: &str = "ready";
const SEEN_FILE: &str = "seen-catalog.json";

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
    Failed {
        code: ErrorCode,
    },
    Unavailable {
        code: ErrorCode,
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
}

pub struct AppUpdateService {
    inner: Arc<Inner>,
}

fn thread_runner() -> Runner {
    Box::new(|task| {
        std::thread::Builder::new()
            .name("hane-app-update".to_owned())
            .spawn(task)
            .is_ok()
    })
}

fn create_private_dir(path: &Path) -> io::Result<()> {
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
        Self::with_runner(config, transport, clock, thread_runner())
    }

    /// Like [`AppUpdateService::new`] with an explicit task runner, so tests
    /// can control when background work happens.
    pub fn with_runner(
        config: UpdateServiceConfig,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
        runner: Runner,
    ) -> Self {
        // Without embedded trust material no catalog can ever be verified, so
        // the service says so up front instead of contacting the network.
        let stage = if !config.policy.is_configured() {
            Stage::Unavailable {
                code: ErrorCode::TrustUnconfigured,
            }
        } else if config.target.is_none() {
            Stage::Unavailable {
                code: ErrorCode::TargetUnsupported,
            }
        } else {
            Stage::Idle
        };
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
            }),
        }
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

    fn seen_store(&self) -> FileSeenStore {
        FileSeenStore::new(self.config.receipts_dir.join(SEEN_FILE))
    }

    /// Runs `task` in the background. If the runner cannot start it, the
    /// operation identified by `op` fails instead of waiting forever.
    fn dispatch(self: &Arc<Self>, op: u64, task: Task) {
        if !(self.runner)(task) {
            let mut state = self.lock();
            let running = match &state.stage {
                Stage::Checking { op: live, .. } | Stage::Downloading { op: live, .. } => {
                    *live == op
                }
                _ => false,
            };
            if running {
                state.stage = Stage::Failed { code: ErrorCode::Io };
            }
        }
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
            Err(error) => Stage::Failed { code: error.code },
        };
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
            .work_root()
            .join(format!("{op}-{}", offer.id().as_str()));
        let outcome = self.download_into(op, offer, cancel, received, &work);

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
                remove_dir_if_present(&target);
                let moved = create_private_dir(&self.candidates_dir())
                    .and_then(|()| fs::rename(&work, &target));
                state.stage = match moved {
                    Ok(()) => Stage::Ready {
                        offer: Arc::clone(offer),
                        dir: target,
                    },
                    Err(_) => {
                        remove_dir_if_present(&work);
                        Stage::Failed { code: ErrorCode::Io }
                    }
                };
            }
            Err(error) => {
                remove_dir_if_present(&work);
                state.stage = if error.code == ErrorCode::Cancelled {
                    Stage::Available(Arc::clone(offer))
                } else {
                    Stage::Failed { code: error.code }
                };
            }
        }
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
    /// `work` and the sequence receipt is touched.
    fn download_into(
        &self,
        op: u64,
        offer: &Offer,
        cancel: &CancelToken,
        received: &Arc<AtomicU64>,
        work: &Path,
    ) -> Result<(), UpdateError> {
        let entry = offer.candidate.target_entry();
        let tag = &offer.candidate.catalog().tag;
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

    fn discard_ready(self: &Arc<Self>) {
        let trashed = {
            let mut state = self.lock();
            let Stage::Ready { dir, .. } = &state.stage else {
                return;
            };
            let dir = dir.clone();
            state.next_op += 1;
            let trash = self.trash_root().join(state.next_op.to_string());
            let moved =
                create_private_dir(&self.trash_root()).and_then(|()| fs::rename(&dir, &trash));
            if moved.is_err() {
                return;
            }
            state.stage = Stage::Idle;
            trash
        };
        // The slow deletion happens off the lock and off the UI thread; the
        // candidate directory is already out of the way.
        let _ = (self.runner)(Box::new(move || remove_dir_if_present(&trashed)));
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
            self.lock().restoring = false;
        }
    }

    /// Local re-validation only: no request is made. Anything that does not
    /// verify exactly is removed rather than offered.
    fn run_restore(&self) {
        remove_dir_if_present(&self.work_root());
        remove_dir_if_present(&self.trash_root());
        let mut found: Option<(Offer, PathBuf)> = None;
        if let Ok(entries) = fs::read_dir(self.candidates_dir()) {
            for entry in entries.flatten() {
                let dir = entry.path();
                match (found.is_none(), self.restore_candidate(&dir)) {
                    (true, Ok(offer)) => found = Some((offer, dir)),
                    _ => remove_dir_if_present(&dir),
                }
            }
        }
        let mut state = self.lock();
        state.restoring = false;
        let idle = matches!(state.stage, Stage::Idle);
        if let (true, Some((offer, dir))) = (idle, found) {
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

    fn snapshot(&self) -> AppUpdateSnapshot {
        let state = self.lock();
        let mut snapshot = AppUpdateSnapshot::idle(self.config.current_version.clone());
        snapshot.restoring = state.restoring;
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
                describe(&mut snapshot, &**offer);
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
                describe(&mut snapshot, &**offer);
                snapshot.received_bytes = received.load(Ordering::Relaxed);
            }
            Stage::Ready { offer, .. } => {
                snapshot.phase = AppUpdatePhase::Ready;
                describe(&mut snapshot, &**offer);
            }
            Stage::Failed { code } => {
                snapshot.phase = AppUpdatePhase::Failed;
                snapshot.error_code = Some(code.as_str().to_owned());
            }
            Stage::Unavailable { code } => {
                snapshot.phase = AppUpdatePhase::Unavailable;
                snapshot.error_code = Some(code.as_str().to_owned());
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
                .updates_root()
                .join("work")
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
    }

    #[test]
    fn a_prepared_update_for_another_target_is_not_restored() {
        let fixture = fixture("0.26.0", "0.27.0", 5);
        fixture.prepare_ready();
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
        fixture.service.discard_ready();
        assert_eq!(fixture.phase(), AppUpdatePhase::Idle);
        run_queued(&fixture.queue);
        assert!(candidates.read_dir().unwrap().next().is_none());
        assert!(!fixture.updates_root().join("trash").exists() || fixture.updates_root().join("trash").read_dir().unwrap().next().is_none());
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
        assert_eq!(service.snapshot().phase, AppUpdatePhase::Failed);
    }
}
