//! Both ends of the helper handshake.
//!
//! *Helper side* ([`run_session`]): validates the application's `Hello`
//! against the identity it derives from its own location, takes the
//! `transaction` then `startup` leases, answers `Prepared`, and only after a
//! valid `ExitAuthorized` **and** the parent's death (the runtime lease turning
//! free) hands over to a [`HelperBackend`]. A cut pipe, a wrong nonce, a stale
//! or foreign identifier, or a parent that is still alive ends the session
//! without any mutation.
//!
//! *Application side* ([`HelperLink`], [`spawn_helper`]): spawns the verified
//! helper copy with piped stdio only, no arguments and a minimal environment,
//! and requires `Prepared` from that very child before it reports success.
//!
//! [`PlatformBackend`] applies the Windows owned-file set through
//! [`crate::windows_apply`]. The macOS bundle swap, the limited-health child and
//! the durable commit are not connected yet, so it never reports an applied
//! update: the replaced files are restored before it answers.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::apply::{self, FsFileOps, RecoveryOutcome};
use crate::clock::{Clock, SystemClock};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::identity::{InstallId, TransactionId};
use crate::journal::{DurableJournal, FileJournalStore};
use crate::model::Target;
use crate::platform::lease::{Coordination, Lease, LeaseKind};
use crate::protocol::{
    Body, Frame, Mode, Nonce, RecoveryReport, bounded_reason, read_frame, write_frame,
};
use crate::trust::TrustPolicy;
use crate::windows_apply::{
    ApplyInputs, CandidateSource, NoChild, apply_and_settle, stage_candidate, valid_candidate_id,
};

pub const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
pub const PARENT_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_GATE_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_POLL: Duration = Duration::from_millis(25);

/// The parent process id as the OS reports it, where `std` can report it.
pub fn os_parent_id() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

// ------------------------------------------------------------ helper side

/// What the helper is allowed to be asked, derived from its own location.
pub struct HelperEnv {
    pub coordination: Coordination,
    pub transaction: TransactionId,
    /// The OS-reported parent, when available; must equal the `Hello` pid.
    pub parent_pid: Option<u32>,
    pub own_pid: u32,
    pub gate_timeout: Duration,
    pub parent_exit_timeout: Duration,
}

impl HelperEnv {
    /// For the real helper process: identity from the executable's location.
    pub fn from_current_process() -> Result<Self> {
        let exe = std::env::current_exe()?;
        let (coordination, transaction) = Coordination::from_helper_exe(&exe)?;
        Ok(Self {
            coordination,
            transaction,
            parent_pid: os_parent_id(),
            own_pid: std::process::id(),
            gate_timeout: STARTUP_GATE_TIMEOUT,
            parent_exit_timeout: PARENT_EXIT_TIMEOUT,
        })
    }
}

/// Leases the helper holds while it works. Dropping releases them in order.
pub struct HeldLeases {
    pub transaction: Lease,
    pub startup: Lease,
    pub runtime: Option<Lease>,
}

pub struct ApplyContext<'a> {
    pub env: &'a HelperEnv,
    pub candidate_id: Option<&'a str>,
    pub leases: &'a HeldLeases,
}

pub struct RecoverContext<'a> {
    pub env: &'a HelperEnv,
    pub leases: &'a HeldLeases,
}

/// The OS-specific work. Called only after every guard of the session passed.
pub trait HelperBackend {
    /// Runs with the parent gone and the runtime lease held exclusively.
    fn apply(&mut self, ctx: &ApplyContext<'_>) -> Result<()>;
    /// Runs with all three leases held; returns what it concluded.
    fn recover(&mut self, ctx: &RecoverContext<'_>) -> Result<RecoveryReport>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// The application aborted or the pipe was cut before authorization.
    Aborted,
    /// The parent did not exit in time; nothing was changed.
    ParentStillRunning,
    Applied,
    Recovered(RecoveryReport),
}

fn refuse<W: Write>(writer: &mut W, nonce: &Nonce, err: UpdateError) -> UpdateError {
    let _ = write_frame(
        writer,
        &Frame::new(
            nonce,
            Body::Refused {
                reason: bounded_reason(&err.to_string()),
            },
        ),
    );
    err
}

fn unexpected(what: &str) -> UpdateError {
    UpdateError::new(ErrorCode::TransitionInvalid, format!("unexpected {what}"))
}

/// Runs one helper session over the inherited pipes.
pub fn run_session<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    env: &HelperEnv,
    backend: &mut dyn HelperBackend,
) -> Result<SessionEnd> {
    let first = read_frame(reader)?.ok_or_else(|| {
        UpdateError::new(ErrorCode::Truncated, "the application closed the pipe before Hello")
    })?;
    // The first frame fixes the session nonce; every later frame must repeat it.
    let nonce = Nonce::parse(&first.nonce)?;
    first.check(&nonce)?;
    let Body::Hello {
        mode,
        install_id,
        transaction_id,
        candidate_id,
        parent_pid,
    } = first.body
    else {
        return Err(refuse(writer, &nonce, unexpected("first frame")));
    };

    let identity_ok = InstallId::parse(&install_id)
        .is_ok_and(|id| &id == env.coordination.install_id())
        && TransactionId::parse(&transaction_id).is_ok_and(|id| id == env.transaction);
    if !identity_ok {
        return Err(refuse(
            writer,
            &nonce,
            UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the request is not for this installation and transaction",
            ),
        ));
    }
    if env.parent_pid.is_some_and(|actual| actual != parent_pid) {
        return Err(refuse(
            writer,
            &nonce,
            UpdateError::new(ErrorCode::IdentityMismatch, "the caller is not the real parent"),
        ));
    }

    // transaction -> startup -> runtime, never the other way.
    let coord = &env.coordination;
    let transaction = match coord.try_acquire(LeaseKind::Transaction, &[]) {
        Ok(Some(lease)) => lease,
        Ok(None) => {
            return Err(refuse(
                writer,
                &nonce,
                UpdateError::new(ErrorCode::StaleJournal, "another updater owns this install"),
            ));
        }
        Err(err) => return Err(refuse(writer, &nonce, err)),
    };
    let startup = coord
        .acquire_within(LeaseKind::Startup, &[&transaction], env.gate_timeout)
        .map_err(|err| refuse(writer, &nonce, err))?;
    let mut leases = HeldLeases {
        transaction,
        startup,
        runtime: None,
    };

    write_frame(
        writer,
        &Frame::new(
            &nonce,
            Body::Prepared {
                install_id,
                transaction_id: transaction_id.clone(),
                helper_pid: env.own_pid,
            },
        ),
    )?;

    match mode {
        Mode::Prepare => {
            // A cut pipe or an invalid frame before authorization is an abort.
            let Ok(Some(next)) = read_frame(reader) else {
                return Ok(SessionEnd::Aborted);
            };
            next.check(&nonce)?;
            match next.body {
                Body::Abort => return Ok(SessionEnd::Aborted),
                Body::ExitAuthorized { transaction_id: authorized }
                    if authorized == transaction_id => {}
                _ => return Err(unexpected("frame while waiting for ExitAuthorized")),
            }
            let Some(runtime) = wait_for_parent_exit(coord, &leases, env.parent_exit_timeout)?
            else {
                return Ok(SessionEnd::ParentStillRunning);
            };
            leases.runtime = Some(runtime);
            backend.apply(&ApplyContext {
                env,
                candidate_id: candidate_id.as_deref(),
                leases: &leases,
            })?;
            Ok(SessionEnd::Applied)
        }
        Mode::Recover => {
            let report = match coord.try_acquire(
                LeaseKind::RuntimeExclusive,
                &[&leases.transaction, &leases.startup],
            )? {
                None => RecoveryReport::Blocked(
                    "another Hane process is running; close it and start again".to_owned(),
                ),
                Some(runtime) => {
                    leases.runtime = Some(runtime);
                    backend.recover(&RecoverContext {
                        env,
                        leases: &leases,
                    })?
                }
            };
            write_frame(
                writer,
                &Frame::new(
                    &nonce,
                    Body::Recovered {
                        transaction_id,
                        report: report.clone(),
                    },
                ),
            )?;
            Ok(SessionEnd::Recovered(report))
        }
    }
}

/// The parent's death is observed through the OS: its shared runtime lease
/// (held for its whole lifetime) becomes free for an exclusive request.
fn wait_for_parent_exit(
    coord: &Coordination,
    leases: &HeldLeases,
    timeout: Duration,
) -> Result<Option<Lease>> {
    let started = Instant::now();
    loop {
        if let Some(lease) = coord.try_acquire(
            LeaseKind::RuntimeExclusive,
            &[&leases.transaction, &leases.startup],
        )? {
            return Ok(Some(lease));
        }
        if started.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(RUNTIME_POLL);
    }
}

/// Real backend. It recovers a pending journal with the journal-first engine
/// and applies the Windows owned-file set. The helper derives every input
/// itself: the install from the coordination directory, the candidate from its
/// own transaction directory and the requested (validated) candidate id.
pub struct PlatformBackend {
    policy: TrustPolicy,
    clock: Arc<dyn Clock>,
}

impl PlatformBackend {
    pub fn new(policy: TrustPolicy, clock: Arc<dyn Clock>) -> Self {
        Self { policy, clock }
    }

    pub fn production() -> Self {
        Self::new(TrustPolicy::production(), Arc::new(SystemClock::new()))
    }
}

impl HelperBackend for PlatformBackend {
    fn apply(&mut self, ctx: &ApplyContext<'_>) -> Result<()> {
        let candidate_id = ctx
            .candidate_id
            .filter(|id| valid_candidate_id(id))
            .ok_or_else(|| {
                UpdateError::new(ErrorCode::IdentityMismatch, "the request names no valid candidate")
            })?;
        let coordination = &ctx.env.coordination;
        let placement = crate::platform::find_install(coordination)?;
        if placement.target != Target::WindowsX64 {
            return Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "the macOS bundle swap is not connected yet",
            ));
        }
        apply_and_settle(&ApplyInputs {
            coordination,
            transaction: &ctx.env.transaction,
            candidate_id,
            placement: &placement,
            policy: &self.policy,
            clock: &*self.clock,
            runtime_held: ctx.leases.runtime.is_some(),
        })
    }

    fn recover(&mut self, ctx: &RecoverContext<'_>) -> Result<RecoveryReport> {
        recover_pending(&ctx.env.coordination, &ctx.env.transaction)
    }
}

/// Settles the transaction's journal. Mutation is attempted only for the
/// Windows file-set layout; on macOS a journal that needs rollback stays
/// `RecoveryRequired` until the bundle-swap adapter exists.
pub fn recover_pending(coord: &Coordination, transaction: &TransactionId) -> Result<RecoveryReport> {
    let txn_dir = coord.transaction_dir(transaction);
    let store = FileJournalStore::new(coord.journal_path(transaction));
    let Some(mut journal) = DurableJournal::open(store)? else {
        return Ok(RecoveryReport::Nothing);
    };
    if journal.journal().transaction_id != *transaction
        || &journal.journal().install_id != coord.install_id()
    {
        return Ok(RecoveryReport::RecoveryRequired(
            "the journal belongs to another transaction or installation".to_owned(),
        ));
    }
    let placement = crate::platform::find_install(coord)?;
    let ops = FsFileOps::new(
        placement.install_root.clone(),
        txn_dir.join("backup"),
        txn_dir.join("stage"),
    );
    let mut ops = GuardedOps {
        inner: ops,
        may_mutate: cfg!(windows),
    };
    let outcome = match apply::recover(&mut journal, &mut ops, &mut NoChild) {
        Ok(outcome) => outcome,
        Err(err) => RecoveryOutcome::RecoveryRequired(err.to_string()),
    };
    Ok(match outcome {
        RecoveryOutcome::Nothing => RecoveryReport::Nothing,
        RecoveryOutcome::KeptOld => RecoveryReport::KeptOld,
        RecoveryOutcome::KeptNew => RecoveryReport::KeptNew,
        RecoveryOutcome::RolledBack => RecoveryReport::RolledBack,
        RecoveryOutcome::RecoveryRequired(why) => {
            RecoveryReport::RecoveryRequired(bounded_reason(&why))
        }
    })
}

/// [`apply::FileOps`] that can be switched to read-only.
struct GuardedOps {
    inner: FsFileOps,
    may_mutate: bool,
}

impl GuardedOps {
    fn gate(&self) -> Result<()> {
        if self.may_mutate {
            Ok(())
        } else {
            Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "this platform's rollback adapter is not connected yet",
            ))
        }
    }
}

impl crate::journal::DiskView for GuardedOps {
    fn observe(&self, rel_path: &str) -> Result<crate::journal::Observed> {
        self.inner.observe(rel_path)
    }
}

impl apply::FileOps for GuardedOps {
    fn backup(&mut self, rel_path: &str, backup_rel: &str) -> Result<()> {
        self.gate()?;
        self.inner.backup(rel_path, backup_rel)
    }

    fn install_new(&mut self, rel_path: &str, expected: &crate::identity::FileIdentity) -> Result<()> {
        self.gate()?;
        self.inner.install_new(rel_path, expected)
    }

    fn remove(&mut self, rel_path: &str) -> Result<()> {
        self.gate()?;
        self.inner.remove(rel_path)
    }

    fn restore(
        &mut self,
        rel_path: &str,
        backup_rel: &str,
        expected: &crate::identity::FileIdentity,
    ) -> Result<()> {
        self.gate()?;
        self.inner.restore(rel_path, backup_rel, expected)
    }
}

/// Entry point of the `hane-updater` binary. Takes no arguments; the process
/// exit code is 0 only for a session that ended cleanly.
pub fn helper_main() -> i32 {
    if std::env::args_os().count() > 1 {
        return 2;
    }
    let Ok(env) = HelperEnv::from_current_process() else {
        return 3;
    };
    let mut backend = PlatformBackend::production();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let result = run_session(&mut stdin.lock(), &mut stdout.lock(), &env, &mut backend);
    match result {
        Ok(SessionEnd::Applied | SessionEnd::Recovered(_) | SessionEnd::Aborted) => 0,
        Ok(SessionEnd::ParentStillRunning) => 4,
        Err(_) => 1,
    }
}

// ------------------------------------------------------- application side

enum Incoming {
    Frame(Box<Frame>),
    Closed,
    Failed(UpdateError),
}

/// The application's end of one helper session.
pub struct HelperLink<W: Write> {
    writer: W,
    incoming: Receiver<Incoming>,
    nonce: Nonce,
    transaction: TransactionId,
    helper_pid: u32,
}

/// What the application asks the helper to do.
pub struct HelloRequest {
    pub mode: Mode,
    pub install_id: InstallId,
    pub transaction_id: TransactionId,
    pub candidate_id: Option<String>,
}

impl<W: Write> HelperLink<W> {
    /// Sends `Hello` and requires a matching `Prepared`. `expected_helper_pid`
    /// is the id of the child this process spawned; a different id is rejected.
    pub fn handshake<R: Read + Send + 'static>(
        mut reader: R,
        mut writer: W,
        request: &HelloRequest,
        expected_helper_pid: Option<u32>,
        timeout: Duration,
    ) -> Result<Self> {
        let (sender, incoming) = mpsc::channel();
        std::thread::Builder::new()
            .name("hane-helper-link".to_owned())
            .spawn(move || {
                loop {
                    let item = match read_frame(&mut reader) {
                        Ok(Some(frame)) => Incoming::Frame(Box::new(frame)),
                        Ok(None) => Incoming::Closed,
                        Err(err) => Incoming::Failed(err),
                    };
                    let last = !matches!(item, Incoming::Frame(_));
                    if sender.send(item).is_err() || last {
                        return;
                    }
                }
            })?;
        let nonce = Nonce::generate()?;
        write_frame(
            &mut writer,
            &Frame::new(
                &nonce,
                Body::Hello {
                    mode: request.mode,
                    install_id: request.install_id.as_str().to_owned(),
                    transaction_id: request.transaction_id.as_str().to_owned(),
                    candidate_id: request.candidate_id.clone(),
                    parent_pid: std::process::id(),
                },
            ),
        )?;
        let mut link = Self {
            writer,
            incoming,
            nonce,
            transaction: request.transaction_id.clone(),
            helper_pid: 0,
        };
        let frame = link.next_frame(timeout)?;
        match frame.body {
            Body::Prepared {
                install_id,
                transaction_id,
                helper_pid,
            } => {
                let same = install_id == request.install_id.as_str()
                    && transaction_id == request.transaction_id.as_str()
                    && expected_helper_pid.is_none_or(|pid| pid == helper_pid);
                if !same {
                    return Err(UpdateError::new(
                        ErrorCode::IdentityMismatch,
                        "Prepared does not come from the expected helper and transaction",
                    ));
                }
                link.helper_pid = helper_pid;
                Ok(link)
            }
            Body::Refused { reason } => Err(UpdateError::new(
                ErrorCode::NotApplicable,
                format!("the helper refused: {reason}"),
            )),
            _ => Err(unexpected("reply to Hello")),
        }
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Frame> {
        match self.incoming.recv_timeout(timeout) {
            Ok(Incoming::Frame(frame)) => {
                frame.check(&self.nonce)?;
                Ok(*frame)
            }
            Ok(Incoming::Closed) => Err(UpdateError::new(
                ErrorCode::Truncated,
                "the helper closed the pipe",
            )),
            Ok(Incoming::Failed(err)) => Err(err),
            Err(RecvTimeoutError::Timeout) => {
                Err(UpdateError::new(ErrorCode::Timeout, "the helper did not answer in time"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(UpdateError::new(
                ErrorCode::Truncated,
                "the helper link ended",
            )),
        }
    }

    pub fn helper_pid(&self) -> u32 {
        self.helper_pid
    }

    /// Lets the helper proceed once this process has exited. Call it only
    /// after the application has finished its own shutdown work.
    pub fn authorize_exit(&mut self) -> Result<()> {
        write_frame(
            &mut self.writer,
            &Frame::new(
                &self.nonce,
                Body::ExitAuthorized {
                    transaction_id: self.transaction.as_str().to_owned(),
                },
            ),
        )
    }

    /// Tells the helper to give up. Best effort: a closed pipe aborts it too.
    pub fn abort(&mut self) {
        let _ = write_frame(&mut self.writer, &Frame::new(&self.nonce, Body::Abort));
    }

    /// Waits for the recovery verdict of a `Mode::Recover` session.
    pub fn await_recovered(&mut self, timeout: Duration) -> Result<RecoveryReport> {
        let frame = self.next_frame(timeout)?;
        match frame.body {
            Body::Recovered {
                transaction_id,
                report,
            } if transaction_id == self.transaction.as_str() => Ok(report),
            Body::Refused { reason } => Err(UpdateError::new(
                ErrorCode::NotApplicable,
                format!("the helper refused: {reason}"),
            )),
            _ => Err(unexpected("reply while waiting for recovery")),
        }
    }
}

/// A spawned helper process together with its private link.
pub struct SpawnedHelper {
    pub link: HelperLink<ChildStdin>,
    pub child: Child,
}

/// The helper's command: piped stdio, no arguments and an empty environment
/// (the helper derives the owner of its `InstallId` from the OS principal it
/// runs as, the same one as the parent). Apply and startup recovery both spawn
/// through here.
fn helper_command(exe: &Path) -> Result<Command> {
    let mut command = Command::new(exe);
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        // CREATE_NO_WINDOW
        command.creation_flags(0x0800_0000);
    }
    Ok(command)
}

/// Spawns `exe` (a verified helper copy) with piped stdio, no arguments and a
/// minimal environment, and completes the `Hello`/`Prepared` handshake. A
/// helper that does not prepare in time is stopped; it has changed nothing.
pub fn spawn_helper(exe: &Path, request: &HelloRequest, timeout: Duration) -> Result<SpawnedHelper> {
    let mut child = helper_command(exe)?.spawn()?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(UpdateError::new(ErrorCode::Io, "the helper pipes are unavailable"));
    };
    match HelperLink::handshake(stdout, stdin, request, Some(child.id()), timeout) {
        Ok(link) => Ok(SpawnedHelper { link, child }),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(err)
        }
    }
}

/// A helper that answered `Prepared` for a restart. The application may quit
/// only while it holds one, and only after its own shutdown work is done.
/// Dropping it without [`PreparedRestart::authorize_exit`] aborts the helper.
pub struct PreparedRestart {
    spawned: Option<SpawnedHelper>,
    transaction: TransactionId,
}

impl PreparedRestart {
    pub fn transaction(&self) -> &TransactionId {
        &self.transaction
    }

    pub fn helper_pid(&self) -> Option<u32> {
        self.spawned.as_ref().map(|spawned| spawned.link.helper_pid())
    }

    /// Lets the helper proceed once this process has exited.
    pub fn authorize_exit(mut self) -> Result<()> {
        match self.spawned.as_mut() {
            Some(spawned) => spawned.link.authorize_exit(),
            None => Err(unexpected("authorization of a finished handoff")),
        }
        .inspect(|()| {
            // The helper owns itself from here; it waits for this process's
            // runtime lease to disappear.
            self.spawned = None;
        })
    }

    pub fn abort(mut self) {
        self.abort_in_place();
    }

    fn abort_in_place(&mut self) {
        if let Some(mut spawned) = self.spawned.take() {
            spawned.link.abort();
            let _ = spawned.child.wait();
        }
    }
}

impl Drop for PreparedRestart {
    fn drop(&mut self) {
        self.abort_in_place();
    }
}

/// Verifies the installed helper, stages its copy and the Ready candidate into
/// the transaction directory, and completes the `Prepared` handshake. Nothing
/// in the install is changed and nothing exits: on any failure the application
/// simply stays open.
pub fn prepare_restart(
    placement: &crate::platform::Placement,
    policy: &crate::trust::TrustPolicy,
    candidate_id: &str,
    candidate: &CandidateSource<'_>,
) -> Result<PreparedRestart> {
    if !valid_candidate_id(candidate_id) {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "malformed candidate id"));
    }
    let coordination = placement.coordination()?;
    let transaction = TransactionId::generate()?;
    let source = crate::platform::installed_helper(placement, policy)?;
    let copy = crate::platform::stage_helper_copy(&coordination, &transaction, &source)?;
    stage_candidate(&coordination, &transaction, candidate)?;
    let spawned = spawn_helper(
        &copy,
        &HelloRequest {
            mode: Mode::Prepare,
            install_id: placement.install_id.clone(),
            transaction_id: transaction.clone(),
            candidate_id: Some(candidate_id.to_owned()),
        },
        PREPARE_TIMEOUT,
    )?;
    Ok(PreparedRestart {
        spawned: Some(spawned),
        transaction,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{self, Cursor};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::identity::InstallFacts;
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

    /// The helper re-derives the owner from its own OS principal, so no owner
    /// is passed through the environment. `spawn_helper` (apply) and startup
    /// recovery both build their command here.
    #[test]
    fn the_helper_environment_carries_no_owner() {
        let command = helper_command(Path::new("hane-updater")).unwrap();
        let envs: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .filter_map(|(key, value)| Some((key.to_str()?.to_owned(), value?.to_str()?.to_owned())))
            .collect();
        assert!(
            envs.keys().all(|key| key == "SystemRoot"),
            "no ambient environment is restored: {envs:?}"
        );
    }

    struct Fixture {
        _dir: TempDir,
        coord: Coordination,
        txn: TransactionId,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("helper");
        let coord = Coordination::create(dir.path(), &install_id()).unwrap();
        Fixture {
            _dir: dir,
            coord,
            txn: TransactionId::generate().unwrap(),
        }
    }

    impl Fixture {
        fn env(&self) -> HelperEnv {
            HelperEnv {
                coordination: self.coord.clone(),
                transaction: self.txn.clone(),
                parent_pid: Some(42),
                own_pid: 4242,
                gate_timeout: Duration::from_millis(100),
                parent_exit_timeout: Duration::from_millis(150),
            }
        }

        fn hello(&self, nonce: &Nonce, mode: Mode) -> Frame {
            Frame::new(
                nonce,
                Body::Hello {
                    mode,
                    install_id: install_id().as_str().to_owned(),
                    transaction_id: self.txn.as_str().to_owned(),
                    candidate_id: Some("cand".to_owned()),
                    parent_pid: 42,
                },
            )
        }

        fn authorized(&self, nonce: &Nonce) -> Frame {
            Frame::new(
                nonce,
                Body::ExitAuthorized {
                    transaction_id: self.txn.as_str().to_owned(),
                },
            )
        }
    }

    #[derive(Default)]
    struct Spy {
        applied: usize,
        recovered: usize,
    }

    impl HelperBackend for Spy {
        fn apply(&mut self, ctx: &ApplyContext<'_>) -> Result<()> {
            assert!(ctx.leases.runtime.is_some(), "apply needs the runtime lease");
            assert_eq!(ctx.candidate_id, Some("cand"));
            self.applied += 1;
            Ok(())
        }

        fn recover(&mut self, ctx: &RecoverContext<'_>) -> Result<RecoveryReport> {
            assert!(ctx.leases.runtime.is_some());
            self.recovered += 1;
            Ok(RecoveryReport::KeptOld)
        }
    }

    fn wire(frames: &[Frame]) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        for frame in frames {
            write_frame(&mut bytes, frame).unwrap();
        }
        Cursor::new(bytes)
    }

    fn replies(output: &[u8]) -> Vec<Frame> {
        let mut reader = output;
        let mut frames = Vec::new();
        while let Some(frame) = read_frame(&mut reader).unwrap() {
            frames.push(frame);
        }
        frames
    }

    fn run(fx: &Fixture, input: &[Frame], spy: &mut Spy) -> (Result<SessionEnd>, Vec<Frame>) {
        let mut reader = wire(input);
        let mut output = Vec::new();
        let result = run_session(&mut reader, &mut output, &fx.env(), spy);
        (result, replies(&output))
    }

    /// Plays the application: holds the runtime lease until `release` runs.
    fn parent_runtime(fx: &Fixture) -> Lease {
        fx.coord
            .try_acquire(LeaseKind::RuntimeShared, &[])
            .unwrap()
            .expect("parent runtime lease")
    }

    #[test]
    fn apply_runs_only_after_authorization_and_the_parent_lease_is_gone() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        let mut spy = Spy::default();
        let (result, out) = run(&fx, &[fx.hello(&nonce, Mode::Prepare), fx.authorized(&nonce)], &mut spy);
        assert_eq!(result.unwrap(), SessionEnd::Applied);
        assert_eq!(spy.applied, 1);
        assert!(matches!(
            out[0].body,
            Body::Prepared { helper_pid: 4242, .. }
        ));
    }

    #[test]
    fn a_living_parent_blocks_every_mutation() {
        let fx = fixture();
        let _parent = parent_runtime(&fx);
        let nonce = Nonce::generate().unwrap();
        let mut spy = Spy::default();
        let (result, out) = run(&fx, &[fx.hello(&nonce, Mode::Prepare), fx.authorized(&nonce)], &mut spy);
        assert_eq!(result.unwrap(), SessionEnd::ParentStillRunning);
        assert_eq!(spy.applied, 0);
        assert!(matches!(out[0].body, Body::Prepared { .. }));
    }

    #[test]
    fn a_cut_pipe_or_abort_before_authorization_never_applies() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        for input in [
            vec![fx.hello(&nonce, Mode::Prepare)],
            vec![fx.hello(&nonce, Mode::Prepare), Frame::new(&nonce, Body::Abort)],
        ] {
            let mut spy = Spy::default();
            let (result, _) = run(&fx, &input, &mut spy);
            assert_eq!(result.unwrap(), SessionEnd::Aborted);
            assert_eq!(spy.applied, 0);
        }
        let mut spy = Spy::default();
        let (result, out) = run(&fx, &[], &mut spy);
        assert_eq!(result.unwrap_err().code, ErrorCode::Truncated);
        assert!(out.is_empty());
    }

    #[test]
    fn a_spoofed_nonce_or_replayed_hello_never_applies() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        let attacker = Nonce::generate().unwrap();
        let mut spy = Spy::default();
        let (result, _) = run(
            &fx,
            &[fx.hello(&nonce, Mode::Prepare), fx.authorized(&attacker)],
            &mut spy,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::IdentityMismatch);

        let (result, _) = run(
            &fx,
            &[fx.hello(&nonce, Mode::Prepare), fx.hello(&nonce, Mode::Prepare)],
            &mut spy,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::TransitionInvalid);

        let mut stale = fx.authorized(&nonce);
        stale.body = Body::ExitAuthorized {
            transaction_id: TransactionId::generate().unwrap().as_str().to_owned(),
        };
        let (result, _) = run(&fx, &[fx.hello(&nonce, Mode::Prepare), stale], &mut spy);
        assert!(result.is_err());
        assert_eq!(spy.applied, 0);
    }

    #[test]
    fn a_foreign_or_stale_identity_is_refused_before_any_lease() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        let mut wrong_txn = fx.hello(&nonce, Mode::Prepare);
        if let Body::Hello { transaction_id, .. } = &mut wrong_txn.body {
            *transaction_id = TransactionId::generate().unwrap().as_str().to_owned();
        }
        let mut wrong_install = fx.hello(&nonce, Mode::Prepare);
        if let Body::Hello { install_id, .. } = &mut wrong_install.body {
            *install_id = "f".repeat(32);
        }
        let mut wrong_parent = fx.hello(&nonce, Mode::Prepare);
        if let Body::Hello { parent_pid, .. } = &mut wrong_parent.body {
            *parent_pid = 7;
        }
        let mut invalid = fx.hello(&nonce, Mode::Prepare);
        if let Body::Hello { transaction_id, .. } = &mut invalid.body {
            *transaction_id = "not-a-transaction".to_owned();
        }
        for hello in [wrong_txn, wrong_install, wrong_parent, invalid] {
            let mut spy = Spy::default();
            let (result, out) = run(&fx, &[hello, fx.authorized(&nonce)], &mut spy);
            assert_eq!(result.unwrap_err().code, ErrorCode::IdentityMismatch);
            assert!(matches!(out[0].body, Body::Refused { .. }));
            assert_eq!(spy.applied, 0);
        }
        // Nothing was left locked by the refusals.
        assert!(
            fx.coord
                .try_acquire(LeaseKind::Transaction, &[])
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn an_invalid_nonce_in_hello_is_rejected() {
        let fx = fixture();
        let mut hello = fx.hello(&Nonce::generate().unwrap(), Mode::Prepare);
        hello.nonce = "short".to_owned();
        let mut spy = Spy::default();
        let (result, out) = run(&fx, &[hello], &mut spy);
        assert!(result.is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn two_helpers_cannot_own_the_same_install() {
        let fx = fixture();
        let _other = fx
            .coord
            .try_acquire(LeaseKind::Transaction, &[])
            .unwrap()
            .unwrap();
        let nonce = Nonce::generate().unwrap();
        let mut spy = Spy::default();
        let (result, out) = run(&fx, &[fx.hello(&nonce, Mode::Prepare), fx.authorized(&nonce)], &mut spy);
        assert_eq!(result.unwrap_err().code, ErrorCode::StaleJournal);
        assert!(matches!(out[0].body, Body::Refused { .. }));
        assert_eq!(spy.applied, 0);
    }

    #[test]
    fn recovery_is_blocked_while_another_hane_process_runs() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        {
            let _running = parent_runtime(&fx);
            let mut spy = Spy::default();
            let (result, out) = run(&fx, &[fx.hello(&nonce, Mode::Recover)], &mut spy);
            assert!(matches!(
                result.unwrap(),
                SessionEnd::Recovered(RecoveryReport::Blocked(_))
            ));
            assert_eq!(spy.recovered, 0);
            assert!(matches!(out[1].body, Body::Recovered { .. }));
        }
        let mut spy = Spy::default();
        let (result, _) = run(&fx, &[fx.hello(&nonce, Mode::Recover)], &mut spy);
        assert_eq!(
            result.unwrap(),
            SessionEnd::Recovered(RecoveryReport::KeptOld)
        );
        assert_eq!(spy.recovered, 1);
    }

    #[test]
    fn the_platform_backend_refuses_an_unnamed_or_unplaced_candidate() {
        let fx = fixture();
        let env = fx.env();
        let transaction = fx.coord.try_acquire(LeaseKind::Transaction, &[]).unwrap().unwrap();
        let startup = fx
            .coord
            .try_acquire(LeaseKind::Startup, &[&transaction])
            .unwrap()
            .unwrap();
        let leases = HeldLeases {
            transaction,
            startup,
            runtime: None,
        };
        let mut backend = PlatformBackend::new(
            crate::testing::test_policy(),
            Arc::new(crate::testing::FixedClock::new(crate::testing::test_now())),
        );
        // Neither a missing nor a path-like candidate id reaches the disk.
        for id in [None, Some("cand"), Some("../../escape")] {
            let err = backend
                .apply(&ApplyContext {
                    env: &env,
                    candidate_id: id,
                    leases: &leases,
                })
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::IdentityMismatch, "{id:?}");
        }
        // A well-formed id still needs an installation that matches the
        // coordination directory; the temporary directory has none.
        let well_formed = "a".repeat(64);
        let err = backend
            .apply(&ApplyContext {
                env: &env,
                candidate_id: Some(well_formed.as_str()),
                leases: &leases,
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    // ------------------------------------------------ application side

    /// One direction of an in-memory pipe.
    struct PipeReader {
        queue: Arc<Mutex<VecDeque<u8>>>,
        closed: Arc<Mutex<bool>>,
    }

    #[derive(Clone)]
    struct PipeWriter {
        queue: Arc<Mutex<VecDeque<u8>>>,
        closed: Arc<Mutex<bool>>,
    }

    fn pipe() -> (PipeReader, PipeWriter) {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let closed = Arc::new(Mutex::new(false));
        (
            PipeReader {
                queue: Arc::clone(&queue),
                closed: Arc::clone(&closed),
            },
            PipeWriter { queue, closed },
        )
    }

    impl Read for PipeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                {
                    let mut queue = self.queue.lock().unwrap();
                    if !queue.is_empty() {
                        let n = buf.len().min(queue.len());
                        for slot in buf.iter_mut().take(n) {
                            *slot = queue.pop_front().unwrap();
                        }
                        return Ok(n);
                    }
                    if *self.closed.lock().unwrap() {
                        return Ok(0);
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Write for PipeWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.queue.lock().unwrap().extend(buf.iter().copied());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl PipeWriter {
        fn close(&self) {
            *self.closed.lock().unwrap() = true;
        }
    }

    /// Runs a real helper session on a thread against the application end.
    fn connect(
        fx: &Fixture,
        mode: Mode,
        expected_pid: Option<u32>,
    ) -> (
        Result<HelperLink<PipeWriter>>,
        std::thread::JoinHandle<Result<SessionEnd>>,
        PipeWriter,
    ) {
        let (helper_in, app_to_helper) = pipe();
        let (app_from_helper, helper_out) = pipe();
        let mut env = fx.env();
        env.parent_pid = Some(std::process::id());
        let helper_out_for_close = helper_out.clone();
        let thread = std::thread::spawn(move || {
            let mut reader = helper_in;
            let mut writer = helper_out;
            let result = run_session(&mut reader, &mut writer, &env, &mut Spy::default());
            helper_out_for_close.close();
            result
        });
        let request = HelloRequest {
            mode,
            install_id: install_id(),
            transaction_id: fx.txn.clone(),
            candidate_id: Some("cand".to_owned()),
        };
        let link = HelperLink::handshake(
            app_from_helper,
            app_to_helper.clone(),
            &request,
            expected_pid,
            Duration::from_secs(5),
        );
        (link, thread, app_to_helper)
    }

    #[test]
    fn the_application_requires_prepared_from_the_expected_helper() {
        let fx = fixture();
        // The in-process helper reports pid 4242; a child with another id is rejected.
        let (link, thread, to_helper) = connect(&fx, Mode::Prepare, Some(1));
        assert_eq!(link.err().unwrap().code, ErrorCode::IdentityMismatch);
        to_helper.close();
        let _ = thread.join().unwrap();

        let (link, thread, to_helper) = connect(&fx, Mode::Prepare, Some(4242));
        let mut link = link.unwrap();
        assert_eq!(link.helper_pid(), 4242);
        link.authorize_exit().unwrap();
        assert_eq!(thread.join().unwrap().unwrap(), SessionEnd::Applied);
        drop(to_helper);
    }

    #[test]
    fn an_application_that_aborts_leaves_the_helper_unapplied() {
        let fx = fixture();
        let (link, thread, _to_helper) = connect(&fx, Mode::Prepare, Some(4242));
        let mut link = link.unwrap();
        link.abort();
        assert_eq!(thread.join().unwrap().unwrap(), SessionEnd::Aborted);
    }

    #[test]
    fn the_application_receives_the_recovery_verdict() {
        let fx = fixture();
        let (link, thread, _to_helper) = connect(&fx, Mode::Recover, Some(4242));
        let mut link = link.unwrap();
        assert_eq!(
            link.await_recovered(Duration::from_secs(5)).unwrap(),
            RecoveryReport::KeptOld
        );
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn a_refusing_or_silent_helper_is_not_prepared() {
        let fx = fixture();
        let _owner = fx
            .coord
            .try_acquire(LeaseKind::Transaction, &[])
            .unwrap()
            .unwrap();
        let (link, thread, _to_helper) = connect(&fx, Mode::Prepare, Some(4242));
        assert_eq!(link.err().unwrap().code, ErrorCode::NotApplicable);
        let _ = thread.join();

        // A helper that never answers times out.
        let (helper_in, app_to_helper) = pipe();
        let (app_from_helper, helper_out) = pipe();
        drop(helper_in);
        let request = HelloRequest {
            mode: Mode::Prepare,
            install_id: install_id(),
            transaction_id: fx.txn.clone(),
            candidate_id: None,
        };
        let err = HelperLink::handshake(
            app_from_helper,
            app_to_helper,
            &request,
            None,
            Duration::from_millis(80),
        )
        .err()
        .unwrap();
        assert_eq!(err.code, ErrorCode::Timeout);
        helper_out.close();
    }

    #[test]
    fn a_spoofed_prepared_with_the_wrong_nonce_is_rejected() {
        let fx = fixture();
        let (app_from_helper_reader, helper_out) = pipe();
        let (_helper_in, app_to_helper) = pipe();
        let forged = Frame::new(
            &Nonce::generate().unwrap(),
            Body::Prepared {
                install_id: install_id().as_str().to_owned(),
                transaction_id: fx.txn.as_str().to_owned(),
                helper_pid: 9,
            },
        );
        let mut out = helper_out.clone();
        write_frame(&mut out, &forged).unwrap();
        let request = HelloRequest {
            mode: Mode::Prepare,
            install_id: install_id(),
            transaction_id: fx.txn.clone(),
            candidate_id: None,
        };
        let err = HelperLink::handshake(
            app_from_helper_reader,
            app_to_helper,
            &request,
            None,
            Duration::from_secs(2),
        )
        .err()
        .unwrap();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
        helper_out.close();
    }
}
