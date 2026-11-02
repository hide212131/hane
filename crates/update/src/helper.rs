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
//! [`crate::windows_apply`], or exchanges the macOS bundle through
//! [`crate::macos_apply`], and holds the `transaction` and `startup` leases
//! through the limited-health child ([`crate::health`]) up to the durable commit
//! or the rollback. It reports an applied update only after `Committed` is
//! durable. The macOS path is chosen only by a verified macOS placement and
//! needs a [`BundleHost`], which exists on macOS builds only; everywhere else
//! it is refused as `NotApplicable`.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::apply::{self, FsFileOps, RecoveryOutcome};
use crate::clock::{Clock, SystemClock};
use crate::elevation::{ElevatedProcess, ElevationOs};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::health::{ProcessLauncher, StopTiming};
use crate::identity::{InstallId, TransactionId};
use crate::journal::{DurableJournal, FileJournalStore};
use crate::macos_apply::{self, BundleApplyInputs, BundleHost};
use crate::model::Target;
use crate::platform::acl;
use crate::platform::lease::{Coordination, Lease, LeaseKind};
use crate::protocol::{
    Body, Frame, Mode, Nonce, RecoveryReport, bounded_reason, read_frame, write_frame,
};
use crate::trust::TrustPolicy;
use crate::windows_apply::{
    ApplyInputs, CandidateSource, NoChild, SystemFreeSpace, apply_and_settle, stage_candidate,
    valid_candidate_id,
};

pub const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
pub const PARENT_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_GATE_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_POLL: Duration = Duration::from_millis(25);

/// How long after `Prepared` an `ExitAuthorized` is still honoured.
pub const AUTHORIZE_WINDOW: Duration = Duration::from_secs(600);
const SESSION_NONCE_FILE: &str = "session.nonce";
const SESSION_NONCE_TAKEN: &str = "session.nonce.taken";

/// The process that really started this one, as the OS records it. `None`
/// means it cannot be established, which the helper treats as a refusal.
pub fn os_parent_id() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(windows)]
    {
        crate::platform::windows::parent_process_id()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Parent side: publishes the one-shot session secret for the helper copy of
/// `transaction` in the owner-only transaction directory, before the helper is
/// spawned. The helper adopts nothing from the wire; it only accepts this value.
pub fn publish_session_nonce(
    coord: &Coordination,
    transaction: &TransactionId,
    nonce: &Nonce,
) -> Result<()> {
    let dir = coord.transaction_dir(transaction);
    let path = dir.join(SESSION_NONCE_FILE);
    let _ = std::fs::remove_file(dir.join(SESSION_NONCE_TAKEN));
    let _ = std::fs::remove_file(&path);
    crate::fsutil::durable_write(&path, nonce.as_str().as_bytes())?;
    if let Err(err) = acl::secure_new(&path, false, 0o600).and_then(|()| acl::verify(&path, false)) {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(())
}

/// Removes an unconsumed published secret (the spawn or handshake failed).
pub fn discard_session_nonce(coord: &Coordination, transaction: &TransactionId) {
    let dir = coord.transaction_dir(transaction);
    let _ = std::fs::remove_file(dir.join(SESSION_NONCE_FILE));
    let _ = std::fs::remove_file(dir.join(SESSION_NONCE_TAKEN));
}

/// Helper side: consumes the published secret exactly once. The file is first
/// renamed away (atomic, so only one process can win it), then verified to be
/// owner-only, read and deleted. A helper started by anything that did not get
/// the parent's published secret, or a second launch, finds nothing and fails.
pub fn take_session_nonce(coord: &Coordination, transaction: &TransactionId) -> Result<Nonce> {
    let dir = coord.transaction_dir(transaction);
    let taken = dir.join(SESSION_NONCE_TAKEN);
    std::fs::rename(dir.join(SESSION_NONCE_FILE), &taken).map_err(|_| {
        UpdateError::new(ErrorCode::IdentityMismatch, "no session secret was published for this helper")
    })?;
    let outcome = acl::verify(&taken, false).and_then(|()| {
        let raw = crate::fsutil::read_limited(&taken, 128)?.ok_or_else(|| {
            UpdateError::new(ErrorCode::NotFound, "the session secret vanished")
        })?;
        let text = String::from_utf8(raw)
            .map_err(|_| UpdateError::new(ErrorCode::IdentityMismatch, "malformed session secret"))?;
        Nonce::parse(&text)
    });
    let _ = std::fs::remove_file(&taken);
    outcome
}

// ------------------------------------------- protected-placement bootstrap

/// Opens the bootstrap request line. It is sent over the verified local channel,
/// never on a command line, and it is not logged.
const BOOTSTRAP_MAGIC: &str = "HANE-BOOTSTRAP-1";
const BOOTSTRAP_LINE_LIMIT: usize = 512;
/// The single byte with which the elevated helper reports that its coordination
/// directory and helper copy exist.
const BOOTSTRAP_ACK: u8 = 0xA5;
/// How long the application waits for the elevated helper's bootstrap.
pub const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);

/// What the application asks the elevated, installed helper to create after the
/// consent. The helper checks every part against what it derives itself; the
/// one-time `nonce` becomes the secret of the framed session that follows.
#[derive(Debug)]
pub struct BootstrapRequest {
    pub install_id: InstallId,
    pub transaction: TransactionId,
    pub nonce: Nonce,
}

fn write_bootstrap_request<W: Write>(
    writer: &mut W,
    install_id: &InstallId,
    transaction: &TransactionId,
    nonce: &Nonce,
) -> Result<()> {
    let line = format!(
        "{BOOTSTRAP_MAGIC} {} {} {}\n",
        install_id.as_str(),
        transaction.as_str(),
        nonce.as_str()
    );
    writer.write_all(line.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// Reads one bounded request line. A malformed or over-long line is refused with
/// a fixed message that never repeats what was received.
fn read_bootstrap_request<R: Read>(reader: &mut R) -> Result<BootstrapRequest> {
    let refuse = |why: &str| UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned());
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return Err(UpdateError::new(
                    ErrorCode::Truncated,
                    "the application closed the channel before the bootstrap request",
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= BOOTSTRAP_LINE_LIMIT {
            return Err(refuse("the bootstrap request is too long"));
        }
        line.push(byte[0]);
    }
    let text = String::from_utf8(line).map_err(|_| refuse("the bootstrap request is malformed"))?;
    let mut parts = text.split(' ');
    let (Some(magic), Some(install), Some(transaction), Some(nonce), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(refuse("the bootstrap request is malformed"));
    };
    if magic != BOOTSTRAP_MAGIC {
        return Err(refuse("the bootstrap request is malformed"));
    }
    Ok(BootstrapRequest {
        install_id: InstallId::parse(install)?,
        transaction: TransactionId::parse(transaction)?,
        nonce: Nonce::parse(nonce)?,
    })
}

/// What the elevated helper holds once its bootstrap succeeded.
pub struct Bootstrapped {
    pub coordination: Coordination,
    pub transaction: TransactionId,
    pub nonce: Nonce,
}

fn same_helper_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Helper side of the protected-placement bootstrap. It runs only after the
/// application has proven, through the OS, to be the requesting account's
/// process of the same session behind the pipes. `placement` and `source` are
/// derived and verified by the helper itself (its own location, the signed
/// inventory); the request only names the transaction and carries the secret.
///
/// Nothing is created before the request is checked against them: the install
/// must be this one, the running image must be the installed helper and the
/// transaction must be new. Then, and only then, the coordination directory, the
/// transaction directory and the staged helper copy are created, owned by the
/// requesting user so that the same account's standard application can stage the
/// candidate and the state binding into them. On a failure after the first
/// directory exists the transaction is removed again.
pub fn serve_bootstrap<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    placement: &crate::platform::Placement,
    source: &crate::platform::HelperSource,
    own_exe: &Path,
) -> Result<Bootstrapped> {
    let request = read_bootstrap_request(reader)?;
    let mismatch = |why: &str| UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned());
    if request.install_id != placement.install_id {
        return Err(mismatch("the request is not for this installation"));
    }
    if !same_helper_path(&source.path, own_exe) {
        return Err(mismatch("the running helper is not the installed helper of this installation"));
    }
    let BootstrapRequest { transaction, nonce, .. } = request;
    let coordination_dir = placement
        .coordination_parent
        .join(placement.install_id.coordination_dir_name());
    if std::fs::symlink_metadata(coordination_dir.join(crate::platform::lease::PRIVATE_DIR).join(transaction.as_str()))
        .is_ok()
    {
        return Err(mismatch("the transaction already exists"));
    }
    let coordination = Coordination::create_user_owned(&placement.coordination_parent, &placement.install_id)?;
    // Records settled by earlier updates are retired while this side can still
    // write the protected parent.
    crate::helper_anchor::sweep_settled(&coordination);
    // The identity of the signed installed helper is recorded in the protected
    // coordination parent before anything is staged or mutated: after a partial
    // apply the live install no longer yields it, and the user-writable staging
    // directory cannot be a trust anchor.
    if let Err(err) = crate::helper_anchor::write(&coordination, &transaction, &source.identity) {
        let _ = crate::helper_anchor::retire(&coordination, &transaction);
        return Err(err);
    }
    if let Err(err) = crate::platform::stage_helper_copy_user_owned(&coordination, &transaction, source) {
        let _ = coordination.remove_unjournaled_transaction(&transaction);
        let _ = crate::helper_anchor::retire(&coordination, &transaction);
        return Err(err);
    }
    if let Err(err) = writer.write_all(&[BOOTSTRAP_ACK]).and_then(|()| writer.flush()) {
        let _ = coordination.remove_unjournaled_transaction(&transaction);
        let _ = crate::helper_anchor::retire(&coordination, &transaction);
        return Err(err.into());
    }
    Ok(Bootstrapped { coordination, transaction, nonce })
}

/// Application side: waits for the helper's acknowledgement of its bootstrap.
/// The wait is bounded; on a timeout the caller stops the helper, which closes
/// the channel and ends the blocked read.
fn await_bootstrap_ack(
    mut reader: Box<dyn Read + Send>,
    timeout: Duration,
) -> Result<Box<dyn Read + Send>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("hane-bootstrap-ack".to_owned())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            let outcome: Result<Box<dyn Read + Send>> = match reader.read(&mut byte) {
                Ok(1) if byte[0] == BOOTSTRAP_ACK => Ok(reader),
                Ok(0) => Err(UpdateError::new(
                    ErrorCode::Truncated,
                    "the elevated helper closed the channel during its bootstrap",
                )),
                Ok(_) => Err(UpdateError::new(
                    ErrorCode::IdentityMismatch,
                    "the elevated helper did not acknowledge its bootstrap",
                )),
                Err(err) => Err(err.into()),
            };
            let _ = sender.send(outcome);
        })?;
    match receiver.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => Err(UpdateError::new(
            ErrorCode::Timeout,
            "the elevated helper did not finish its bootstrap in time",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(UpdateError::new(
            ErrorCode::Truncated,
            "the bootstrap acknowledgement ended",
        )),
    }
}

/// Removes the transaction a failed bootstrap left in the coordination
/// directory, once the helper is certainly stopped. Best effort: what cannot be
/// verified stays for the next cleanup.
fn discard_failed_bootstrap(
    placement: &crate::platform::Placement,
    transaction: &TransactionId,
    helper_stopped: bool,
) {
    if !helper_stopped {
        return;
    }
    if let Ok(Some(coordination)) = placement.existing_coordination() {
        let _ = coordination.remove_unjournaled_transaction(transaction);
    }
}

/// The consent route of a known protected placement. Nothing is written to the
/// protected install parent before the consent, and nothing by this
/// (unelevated) side at all: the ordinary application keeps its requests in its
/// own process, listens on a local channel that only an elevated token of the
/// same account can open, and starts the *installed* helper, pinned by the
/// signed inventory's identity, through one consent request. Only after the OS
/// reports the connected process to be that very launched process, elevated, in
/// this session and running as the requesting account does the elevated helper
/// create the coordination directory and the staged helper copy; the
/// application then stages the candidate and the state binding into the
/// directories the helper made for the requesting user and completes the
/// ordinary `Hello`/`Prepared` handshake. The application is never closed here.
fn prepare_protected_with(
    placement: &crate::platform::Placement,
    source: impl FnOnce() -> Result<crate::platform::HelperSource>,
    candidate_id: &str,
    candidate: &CandidateSource<'_>,
    state_root: &Path,
    os: &dyn ElevationOs,
) -> Result<PreparedRestart> {
    use crate::elevation as el;
    // A build that cannot finish an elevated apply must not show a prompt.
    if !os.standard_restart_available() {
        return Err(el::protected_placement_error());
    }
    let source = source()?;
    // Inputs of the user side that the elevated helper will use are checked
    // before the prompt, so a request that cannot succeed never asks for consent.
    crate::state_root::validate_root(state_root)?;
    let transaction = TransactionId::generate()?;
    let nonce = Nonce::generate()?;
    let connection = el::connect_bootstrap(
        os,
        &placement.install_root,
        &source.path,
        &source.identity,
        &placement.install_id,
        el::CONNECT_TIMEOUT,
    )
    .map_err(|failure| failure.error)?;
    let el::ElevatedConnection { process, reader, mut writer } = connection;

    let acknowledged = write_bootstrap_request(&mut writer, &placement.install_id, &transaction, &nonce)
        .and_then(|()| await_bootstrap_ack(reader, BOOTSTRAP_TIMEOUT));
    let reader = match acknowledged {
        Ok(reader) => reader,
        Err(err) => {
            let failure = el::stop_helper(process, err);
            discard_failed_bootstrap(placement, &transaction, failure.helper_stopped);
            return Err(failure.error);
        }
    };
    let coordination = match placement.existing_coordination() {
        Ok(Some(coordination)) => coordination,
        other => {
            let err = match other {
                Err(err) => err,
                _ => UpdateError::new(ErrorCode::NotFound, "the elevated helper did not create the coordination directory"),
            };
            let failure = el::stop_helper(process, err);
            return Err(failure.error);
        }
    };
    // From the first file this side stages until `PreparedRestart` owns the
    // transaction, any early return cleans this transaction (and only this one)
    // up, once the helper is certainly stopped.
    let mut cleanup = PreparingTransaction {
        coordination: &coordination,
        transaction: &transaction,
        helper_stopped: false,
        armed: true,
    };
    let staged = stage_candidate(&coordination, &transaction, candidate)
        .map(|_| ())
        .and_then(|()| crate::state_root::publish_binding(&coordination, &transaction, state_root));
    if let Err(err) = staged {
        let failure = el::stop_helper(process, err);
        cleanup.helper_stopped = failure.helper_stopped;
        return Err(failure.error);
    }
    let request = HelloRequest {
        mode: Mode::Prepare,
        install_id: placement.install_id.clone(),
        transaction_id: transaction.clone(),
        candidate_id: Some(candidate_id.to_owned()),
    };
    match HelperLink::handshake(reader, writer, &request, nonce, Some(process.pid()), ELEVATED_PREPARE_TIMEOUT) {
        Ok(link) => {
            cleanup.armed = false;
            drop(cleanup);
            Ok(PreparedRestart {
                spawned: Some(RunningHelper::Elevated(ElevatedHelper { link, process })),
                coordination,
                transaction,
            })
        }
        Err(err) => {
            let failure = el::stop_helper(process, err);
            cleanup.helper_stopped = failure.helper_stopped;
            Err(failure.error)
        }
    }
}

// ------------------------------------------------------------ helper side

/// What the helper is allowed to be asked, derived from its own location.
pub struct HelperEnv {
    pub coordination: Coordination,
    pub transaction: TransactionId,
    /// The OS-reported parent; must equal the `Hello` pid.
    pub parent_pid: u32,
    pub own_pid: u32,
    pub gate_timeout: Duration,
    pub parent_exit_timeout: Duration,
    /// `ExitAuthorized` arriving later than this after `Prepared` is stale.
    pub authorize_window: Duration,
}

impl HelperEnv {
    /// For the real helper process: identity from the executable's location.
    /// Without a verifiable OS parent there is no session.
    pub fn from_current_process() -> Result<Self> {
        let exe = std::env::current_exe()?;
        let (coordination, transaction) = Coordination::from_helper_exe(&exe)?;
        let parent_pid = os_parent_id().ok_or_else(|| {
            UpdateError::new(ErrorCode::IdentityMismatch, "the parent process cannot be verified")
        })?;
        Ok(Self {
            coordination,
            transaction,
            parent_pid,
            own_pid: std::process::id(),
            gate_timeout: STARTUP_GATE_TIMEOUT,
            parent_exit_timeout: PARENT_EXIT_TIMEOUT,
            authorize_window: AUTHORIZE_WINDOW,
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
    /// Mutable so the backend can hand the `runtime` lease to the limited
    /// child; `transaction` and `startup` stay held until the call returns.
    pub leases: &'a mut HeldLeases,
}

pub struct RecoverContext<'a> {
    pub env: &'a HelperEnv,
    pub leases: &'a HeldLeases,
}

/// The OS-specific work. Called only after every guard of the session passed.
pub trait HelperBackend {
    /// Runs with the parent gone and the runtime lease held exclusively. `Ok`
    /// means the update is applied *and* durably committed; any other end has
    /// restored the old version (or stopped as `RecoveryRequired`) first.
    fn apply(&mut self, ctx: &mut ApplyContext<'_>) -> Result<()>;
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
    session_nonce: &Nonce,
    backend: &mut dyn HelperBackend,
) -> Result<SessionEnd> {
    let first = read_frame(reader)?.ok_or_else(|| {
        UpdateError::new(ErrorCode::Truncated, "the application closed the pipe before Hello")
    })?;
    // The session nonce is the secret the real parent published before the
    // spawn. `Hello` must present it; it never chooses it. A mismatch gets no
    // reply at all.
    let nonce = session_nonce;
    first.check(nonce)?;
    let Body::Hello {
        mode,
        install_id,
        transaction_id,
        candidate_id,
        parent_pid,
    } = first.body
    else {
        return Err(refuse(writer, nonce, unexpected("first frame")));
    };

    let identity_ok = InstallId::parse(&install_id)
        .is_ok_and(|id| &id == env.coordination.install_id())
        && TransactionId::parse(&transaction_id).is_ok_and(|id| id == env.transaction);
    if !identity_ok {
        return Err(refuse(
            writer,
            nonce,
            UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the request is not for this installation and transaction",
            ),
        ));
    }
    if env.parent_pid != parent_pid {
        return Err(refuse(
            writer,
            nonce,
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
                nonce,
                UpdateError::new(ErrorCode::StaleJournal, "another updater owns this install"),
            ));
        }
        Err(err) => return Err(refuse(writer, nonce, err)),
    };
    let startup = coord
        .acquire_within(LeaseKind::Startup, &[&transaction], env.gate_timeout)
        .map_err(|err| refuse(writer, nonce, err))?;
    let mut leases = HeldLeases {
        transaction,
        startup,
        runtime: None,
    };
    // Dropped before `leases`, while this helper still owns the transaction.
    let _secrets = TerminalSecrets {
        coordination: coord,
        transaction: &env.transaction,
    };

    write_frame(
        writer,
        &Frame::new(
            nonce,
            Body::Prepared {
                install_id,
                transaction_id: transaction_id.clone(),
                helper_pid: env.own_pid,
            },
        ),
    )?;

    let prepared_at = Instant::now();
    match mode {
        Mode::Prepare => {
            // A cut pipe or an invalid frame before authorization is an abort.
            let Ok(Some(next)) = read_frame(reader) else {
                return Ok(SessionEnd::Aborted);
            };
            next.check(nonce)?;
            match next.body {
                Body::Abort => return Ok(SessionEnd::Aborted),
                Body::ExitAuthorized { transaction_id: authorized }
                    if authorized == transaction_id => {}
                _ => return Err(unexpected("frame while waiting for ExitAuthorized")),
            }
            if prepared_at.elapsed() >= env.authorize_window {
                return Err(UpdateError::new(
                    ErrorCode::Timeout,
                    "ExitAuthorized arrived after the authorization window",
                ));
            }
            let Some(runtime) = wait_for_parent_exit(coord, &leases, env.parent_exit_timeout)?
            else {
                return Ok(SessionEnd::ParentStillRunning);
            };
            leases.runtime = Some(runtime);
            // The parent is gone, so nothing else can reclaim this transaction
            // until the helper has made its journal durable. The guard drops
            // before `leases`, while the leases are still held.
            let mut cleanup = UnjournaledCleanup {
                coordination: coord,
                transaction: &env.transaction,
                armed: true,
            };
            let applied = backend.apply(&mut ApplyContext {
                env,
                candidate_id: candidate_id.as_deref(),
                leases: &mut leases,
            });
            // A panic skips this line and leaves the guard armed.
            cleanup.armed = applied.is_err();
            applied?;
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
                    nonce,
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

/// Helper-owned cleanup for the window between `ExitAuthorized` and the durable
/// journal. If the apply ends in failure (or a panic) before any journal exists,
/// the transaction's archive, stage and helper copy are removed; the check for
/// journal traces and path ownership is `remove_unjournaled_transaction`, and
/// whatever it cannot verify stays for the next startup sweep.
struct UnjournaledCleanup<'a> {
    coordination: &'a Coordination,
    transaction: &'a TransactionId,
    armed: bool,
}

impl Drop for UnjournaledCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            discard_unstarted_transaction(self.coordination, self.transaction);
        }
    }
}

/// Removes the temporary proxy credentials of the helper's own transaction when
/// its session ends in any way: aborted, parent still running, applied (the
/// limited child has already taken its credentials), rolled back or recovered.
/// A failed removal keeps the record that names them, so the next cleanup of
/// this transaction retries.
struct TerminalSecrets<'a> {
    coordination: &'a Coordination,
    transaction: &'a TransactionId,
}

impl Drop for TerminalSecrets<'_> {
    fn drop(&mut self) {
        let _ = release_secrets(self.coordination, self.transaction);
    }
}

/// The helper's cleanup of a transaction's temporary proxy credentials. Production
/// uses the OS store; unit tests use an in-memory store so that they never touch
/// the Keychain / Credential Manager.
#[cfg(not(test))]
fn release_secrets(coordination: &Coordination, transaction: &TransactionId) -> bool {
    crate::state_root::release_transaction_secrets(coordination, transaction)
}

#[cfg(test)]
fn release_secrets(coordination: &Coordination, transaction: &TransactionId) -> bool {
    crate::state_root::release_transaction_secrets_with(
        coordination,
        transaction,
        tests::fake_secrets(),
    )
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
    /// The OS seam of the macOS bundle exchange; `None` where there is none.
    bundle_host: Option<Box<dyn BundleHost>>,
    /// Start the limited-health child with the user's unelevated token (the
    /// elevated helper), never with the token of this process.
    standard_token_children: bool,
}

/// The real macOS host on macOS builds; nothing anywhere else, so a macOS
/// journal or placement can never be acted on by another OS.
#[cfg(target_os = "macos")]
fn system_bundle_host() -> Option<Box<dyn BundleHost>> {
    Some(Box::new(crate::platform::macos::MacHost))
}

#[cfg(not(target_os = "macos"))]
fn system_bundle_host() -> Option<Box<dyn BundleHost>> {
    None
}

impl PlatformBackend {
    pub fn new(policy: TrustPolicy, clock: Arc<dyn Clock>) -> Self {
        Self {
            policy,
            clock,
            bundle_host: system_bundle_host(),
            standard_token_children: false,
        }
    }

    /// For the elevated helper: the new version must not inherit its
    /// administrator token.
    pub fn with_standard_token_children(mut self) -> Self {
        self.standard_token_children = true;
        self
    }

    /// Replaces the OS seam of the bundle exchange (deterministic tests).
    pub fn with_bundle_host(mut self, host: Box<dyn BundleHost>) -> Self {
        self.bundle_host = Some(host);
        self
    }

    pub fn production() -> Self {
        Self::new(TrustPolicy::production(), Arc::new(SystemClock::new()))
    }
}

impl HelperBackend for PlatformBackend {
    fn apply(&mut self, ctx: &mut ApplyContext<'_>) -> Result<()> {
        let env = ctx.env;
        let candidate_id = ctx
            .candidate_id
            .filter(|id| valid_candidate_id(id))
            .ok_or_else(|| {
                UpdateError::new(ErrorCode::IdentityMismatch, "the request names no valid candidate")
            })?;
        let coordination = &env.coordination;
        let placement = crate::platform::find_install(coordination)?;
        let runtime_held = ctx.leases.runtime.is_some();
        match placement.target {
            Target::WindowsX64 => {
                let mut launcher = ProcessLauncher {
                    runtime: &mut ctx.leases.runtime,
                    timing: StopTiming::default(),
                    standard_token: self.standard_token_children,
                };
                apply_and_settle(
                    &ApplyInputs {
                        free_space: &SystemFreeSpace,
                        coordination,
                        transaction: &env.transaction,
                        candidate_id,
                        placement: &placement,
                        policy: &self.policy,
                        clock: &*self.clock,
                        runtime_held,
                    },
                    &mut launcher,
                )
            }
            Target::MacosArm64 => {
                let host = self.bundle_host.as_deref().ok_or_else(|| {
                    UpdateError::new(
                        ErrorCode::NotApplicable,
                        "this OS has no macOS bundle adapter",
                    )
                })?;
                let mut launcher = ProcessLauncher {
                    runtime: &mut ctx.leases.runtime,
                    timing: StopTiming::default(),
                    standard_token: false,
                };
                macos_apply::apply_and_settle(
                    &BundleApplyInputs {
                        coordination,
                        transaction: &env.transaction,
                        candidate_id,
                        placement: &placement,
                        policy: &self.policy,
                        clock: &*self.clock,
                        host,
                        runtime_held,
                    },
                    &mut launcher,
                )
            }
        }
    }

    fn recover(&mut self, ctx: &RecoverContext<'_>) -> Result<RecoveryReport> {
        recover_pending_with(
            &ctx.env.coordination,
            &ctx.env.transaction,
            self.bundle_host.as_deref(),
        )
    }
}

/// Settles the transaction's journal with the OS seam of this build.
pub fn recover_pending(coord: &Coordination, transaction: &TransactionId) -> Result<RecoveryReport> {
    let host = system_bundle_host();
    recover_pending_with(coord, transaction, host.as_deref())
}

/// Settles the transaction's journal. A Windows file-set journal is rolled back
/// file by file; a macOS bundle journal by exchanging the bundles back, decided
/// from the bundle identities on disk. Without the matching adapter a journal
/// that needs a rollback stays `RecoveryRequired` and nothing is changed.
pub fn recover_pending_with(
    coord: &Coordination,
    transaction: &TransactionId,
    bundle_host: Option<&dyn BundleHost>,
) -> Result<RecoveryReport> {
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
    let recovered = if placement.target == Target::MacosArm64 {
        match bundle_host {
            Some(host) => {
                macos_apply::recover_bundle(coord, transaction, &placement, host, &mut journal)
            }
            None => Ok(RecoveryOutcome::RecoveryRequired(
                "this OS has no macOS bundle adapter".to_owned(),
            )),
        }
    } else {
        let ops = FsFileOps::new(
            placement.install_root.clone(),
            txn_dir.join("backup"),
            txn_dir.join("stage"),
        );
        #[cfg(windows)]
        let ops = ops.with_scratch_ledger(coord, transaction);
        let mut ops = GuardedOps {
            inner: ops,
            may_mutate: cfg!(windows),
        };
        apply::recover(&mut journal, &mut ops, &mut NoChild)
    };
    let outcome = match recovered {
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

    fn bind_target(
        &mut self,
        rel_path: &str,
        expected: &Option<crate::identity::FileIdentity>,
    ) -> Result<()> {
        self.gate()?;
        self.inner.bind_target(rel_path, expected)
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

/// Entry point of the `hane-updater` binary. It takes no argument, or exactly
/// [`crate::elevation::ELEVATED_ARGUMENT`] when the application started it
/// through the Windows consent prompt; the process exit code is 0 only for a
/// session that ended cleanly.
pub fn helper_main() -> i32 {
    let mut arguments = std::env::args_os().skip(1);
    match (arguments.next(), arguments.next()) {
        (None, _) => standard_main(),
        (Some(argument), None) if argument == crate::elevation::ELEVATED_ARGUMENT => elevated_main(),
        _ => 2,
    }
}

/// Refuses every apply while the build cannot restart the new version with the
/// requesting user's token (see [`crate::elevation::STANDARD_RESTART_AVAILABLE`]):
/// the elevated helper would otherwise start the limited-health child with the
/// administrator token. Recovery is not an apply and is passed through.
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows elevated entry point builds it")
)]
struct ElevatedBackend<B: HelperBackend> {
    inner: B,
    standard_restart: bool,
}

impl<B: HelperBackend> HelperBackend for ElevatedBackend<B> {
    fn apply(&mut self, ctx: &mut ApplyContext<'_>) -> Result<()> {
        if !self.standard_restart {
            return Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "the elevated helper cannot restart the new version with the user's token",
            ));
        }
        self.inner.apply(ctx)
    }

    fn recover(&mut self, ctx: &RecoverContext<'_>) -> Result<RecoveryReport> {
        self.inner.recover(ctx)
    }
}

#[cfg(windows)]
fn elevated_main() -> i32 {
    let Ok(exe) = std::env::current_exe() else {
        return 3;
    };
    // A helper that runs from a transaction directory is a staged copy; one that
    // runs from the install itself is the installed helper behind the
    // protected-placement bootstrap, which has no transaction yet.
    match Coordination::from_helper_exe(&exe) {
        Ok((coordination, transaction)) => elevated_copy_main(coordination, transaction),
        Err(_) => elevated_installed_main(&exe),
    }
}

#[cfg(windows)]
fn elevated_copy_main(coordination: Coordination, transaction: TransactionId) -> i32 {
    // The new version must be startable with the user's unelevated token. This
    // is established before the application is contacted, so a helper that
    // could only start it as an administrator ends before any change.
    if crate::platform::windows::standard_token_available().is_err() {
        return 7;
    }
    // The application's endpoint must exist and must be the requesting
    // account's process in this session before anything is read or consumed.
    let Ok(pipes) = crate::platform::windows::connect_helper_pipes(&transaction) else {
        return 6;
    };
    let Ok(nonce) = take_session_nonce(&coordination, &transaction) else {
        return 5;
    };
    let env = HelperEnv {
        coordination,
        transaction,
        // The OS-reported owner of the pipe, never a value from the wire.
        parent_pid: pipes.server_pid,
        own_pid: std::process::id(),
        gate_timeout: STARTUP_GATE_TIMEOUT,
        parent_exit_timeout: PARENT_EXIT_TIMEOUT,
        authorize_window: AUTHORIZE_WINDOW,
    };
    run_elevated_session(pipes, &env, &nonce)
}

/// The installed helper started through the consent prompt of a protected
/// placement. It derives the placement and its own signed identity itself,
/// requires the application's endpoint to be the requesting account's process in
/// this session, and only then creates the coordination directory and the staged
/// copy the application asks for (see [`serve_bootstrap`]).
#[cfg(windows)]
fn elevated_installed_main(exe: &Path) -> i32 {
    if crate::platform::windows::standard_token_available().is_err() {
        return 7;
    }
    let Ok(placement) = crate::platform::placement_of_installed_helper(exe) else {
        return 3;
    };
    let Ok(source) = crate::platform::installed_helper(&placement, &TrustPolicy::production()) else {
        return 3;
    };
    let Ok(mut pipes) = crate::platform::windows::connect_bootstrap_pipes(&placement.install_id) else {
        return 6;
    };
    let Ok(bootstrapped) = serve_bootstrap(&mut pipes.reader, &mut pipes.writer, &placement, &source, exe)
    else {
        return 5;
    };
    let env = HelperEnv {
        coordination: bootstrapped.coordination,
        transaction: bootstrapped.transaction,
        parent_pid: pipes.server_pid,
        own_pid: std::process::id(),
        gate_timeout: STARTUP_GATE_TIMEOUT,
        parent_exit_timeout: PARENT_EXIT_TIMEOUT,
        authorize_window: AUTHORIZE_WINDOW,
    };
    run_elevated_session(pipes, &env, &bootstrapped.nonce)
}

#[cfg(windows)]
fn run_elevated_session(
    mut pipes: crate::platform::windows::HelperPipes,
    env: &HelperEnv,
    nonce: &Nonce,
) -> i32 {
    let mut backend = ElevatedBackend {
        inner: PlatformBackend::production().with_standard_token_children(),
        standard_restart: crate::elevation::STANDARD_RESTART_AVAILABLE,
    };
    match run_session(&mut pipes.reader, &mut pipes.writer, env, nonce, &mut backend) {
        Ok(SessionEnd::Applied | SessionEnd::Recovered(_) | SessionEnd::Aborted) => 0,
        Ok(SessionEnd::ParentStillRunning) => 4,
        Err(_) => 1,
    }
}

#[cfg(not(windows))]
fn elevated_main() -> i32 {
    2
}

fn standard_main() -> i32 {
    let Ok(env) = HelperEnv::from_current_process() else {
        return 3;
    };
    // Consumed before any pipe is read: a launch that is not the parent's
    // spawn has no published secret and never gets as far as `Hello`.
    let Ok(nonce) = take_session_nonce(&env.coordination, &env.transaction) else {
        return 5;
    };
    let mut backend = PlatformBackend::production();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let result = run_session(&mut stdin.lock(), &mut stdout.lock(), &env, &nonce, &mut backend);
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
        nonce: Nonce,
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
    spawn_helper_checked(exe, request, timeout).map_err(|(err, _)| err)
}

/// [`spawn_helper`] that also reports, on failure, whether the helper is
/// certain not to be running (it never started, or its exit was confirmed).
fn spawn_helper_checked(
    exe: &Path,
    request: &HelloRequest,
    timeout: Duration,
) -> std::result::Result<SpawnedHelper, (UpdateError, bool)> {
    let (coordination, transaction) = Coordination::from_helper_exe(exe).map_err(|err| (err, true))?;
    if transaction != request.transaction_id {
        return Err((
            UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the helper copy belongs to another transaction",
            ),
            true,
        ));
    }
    // The secret exists before the helper does, and only this process can have
    // put it there; the helper reads it from the private directory, not from
    // anything the first frame says.
    let nonce = Nonce::generate().map_err(|err| (err, true))?;
    publish_session_nonce(&coordination, &transaction, &nonce).map_err(|err| (err, true))?;
    let spawned = helper_command(exe).and_then(|mut command| {
        command.spawn().map_err(|err| {
            crate::elevation::normal_start_error(err.raw_os_error().map(|raw| raw as u32), exe)
        })
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            discard_session_nonce(&coordination, &transaction);
            return Err((err, true));
        }
    };
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let stopped = child.wait().is_ok();
        discard_session_nonce(&coordination, &transaction);
        return Err((UpdateError::new(ErrorCode::Io, "the helper pipes are unavailable"), stopped));
    };
    match HelperLink::handshake(stdout, stdin, request, nonce, Some(child.id()), timeout) {
        Ok(link) => Ok(SpawnedHelper { link, child }),
        Err(err) => {
            let _ = child.kill();
            let stopped = child.wait().is_ok();
            discard_session_nonce(&coordination, &transaction);
            Err((err, stopped))
        }
    }
}

/// A helper started through the Windows consent prompt, with the process
/// handle the application got at launch and its verified local channel.
struct ElevatedHelper {
    link: HelperLink<Box<dyn Write + Send>>,
    process: Box<dyn ElevatedProcess>,
}

/// The helper of a prepared restart: a normally started child, or an elevated
/// process behind the consent prompt.
enum RunningHelper {
    Standard(SpawnedHelper),
    Elevated(ElevatedHelper),
}

impl RunningHelper {
    fn link_pid(&self) -> u32 {
        match self {
            Self::Standard(spawned) => spawned.link.helper_pid(),
            Self::Elevated(elevated) => elevated.link.helper_pid(),
        }
    }
}

/// How long the application waits for the elevated helper's `Prepared`.
const ELEVATED_PREPARE_TIMEOUT: Duration = Duration::from_secs(30);

/// The inputs that identify one helper launch. `trusted` is the identity taken
/// from the signed inventory, never from the rewritable helper record. `None`
/// means no trusted identity could be derived; an elevated launch is then refused.
#[derive(Clone, Copy)]
struct HelperLaunch<'a> {
    copy: &'a Path,
    trusted: Option<&'a crate::identity::FileIdentity>,
    request: &'a HelloRequest,
    coordination: &'a Coordination,
    install_root: &'a Path,
}

/// Chooses the route from the install's classification and the current token's
/// verified write access (see [`crate::elevation::choose_route`]), before the
/// helper is started and before anything is applied. A writable install starts
/// the helper normally and never prompts, whatever the OS says about the helper's
/// name (`ERROR_ELEVATION_REQUIRED` included: the failure is returned as it is).
/// Only a known protected placement without write access goes through one
/// consent prompt; an unknown placement or an undetermined access is refused.
/// Returns the failure together with whether the helper is certain not to be
/// running.
fn start_running_helper(
    launch: &HelperLaunch<'_>,
    os: &dyn ElevationOs,
    prepare_timeout: Duration,
    normal: impl FnOnce() -> std::result::Result<SpawnedHelper, (UpdateError, bool)>,
) -> std::result::Result<RunningHelper, (UpdateError, bool)> {
    use crate::elevation::{Route, StandardAttempt, choose_route};
    let access = os.placement_access(launch.install_root);
    match choose_route(access.protected, access.write, StandardAttempt::NotTried) {
        Route::Standard => normal().map(RunningHelper::Standard),
        Route::Elevated(_) => {
            start_elevated_helper(launch, os, prepare_timeout, crate::elevation::protected_placement_error())
                .map(RunningHelper::Elevated)
        }
        Route::Refused(reason) => Err((crate::elevation::route_refused_error(&reason), true)),
    }
}

/// The consent route: publishes the one-shot secret, asks for consent once,
/// requires the connected process to be the launched, elevated helper of the
/// requesting account, and completes the same `Hello`/`Prepared` handshake as
/// the normal start over the verified channel.
fn start_elevated_helper(
    launch: &HelperLaunch<'_>,
    os: &dyn ElevationOs,
    prepare_timeout: Duration,
    cause: UpdateError,
) -> std::result::Result<ElevatedHelper, (UpdateError, bool)> {
    let HelperLaunch { copy, trusted, request, coordination, install_root } = *launch;
    // A build that cannot finish an elevated apply must not show a prompt.
    if !os.standard_restart_available() {
        return Err((cause, true));
    }
    // Without a trusted identity the helper would only be bound to what a
    // user-writable file says, so nothing elevated is started.
    let Some(trusted) = trusted else {
        return Err((
            UpdateError::new(
                ErrorCode::IdentityMismatch,
                "no trusted helper identity is available for the elevated launch",
            ),
            true,
        ));
    };
    let transaction = &request.transaction_id;
    let nonce = Nonce::generate().map_err(|err| (err, true))?;
    publish_session_nonce(coordination, transaction, &nonce).map_err(|err| (err, true))?;
    let connection = match crate::elevation::connect_elevated(
        os,
        install_root,
        copy,
        trusted,
        transaction,
        crate::elevation::CONNECT_TIMEOUT,
    ) {
        Ok(connection) => connection,
        Err(failure) => {
            discard_session_nonce(coordination, transaction);
            return Err((failure.error, failure.helper_stopped));
        }
    };
    let crate::elevation::ElevatedConnection { process, reader, writer } = connection;
    match HelperLink::handshake(reader, writer, request, nonce, Some(process.pid()), prepare_timeout) {
        Ok(link) => Ok(ElevatedHelper { link, process }),
        Err(err) => {
            let failure = crate::elevation::stop_helper(process, err);
            discard_session_nonce(coordination, transaction);
            Err((failure.error, failure.helper_stopped))
        }
    }
}

/// How long startup waits for the verdict of a recovery helper.
const RECOVERY_VERDICT_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs the staged helper `copy` of a pending `transaction` in recovery mode and
/// returns its verdict. `trusted` is the identity derived from the signed
/// inventory, never from the staged record; the elevated launch is pinned to it
/// and refused when it is `None`. The route is decided as for an apply: a writable install starts
/// the copy normally, a known protected placement without write access asks for
/// one consent prompt, and anything else is refused before a prompt or a change.
/// A declined prompt or an unavailable elevated route is reported as
/// `RecoveryRequired` with the manual guidance; the journal and the backup are
/// never touched here.
pub(crate) fn recover_with_helper_copy(
    os: &dyn ElevationOs,
    placement: &crate::platform::Placement,
    coordination: &Coordination,
    transaction: &TransactionId,
    copy: &Path,
    trusted: Option<&crate::identity::FileIdentity>,
) -> Result<RecoveryReport> {
    let request = HelloRequest {
        mode: Mode::Recover,
        install_id: placement.install_id.clone(),
        transaction_id: transaction.clone(),
        candidate_id: None,
    };
    let launch = HelperLaunch {
        copy,
        trusted,
        request: &request,
        coordination,
        install_root: &placement.install_root,
    };
    let running = start_running_helper(&launch, os, ELEVATED_PREPARE_TIMEOUT, || {
        spawn_helper_checked(copy, &request, PREPARE_TIMEOUT)
    });
    // A helper that is slow is left alone: it may be mid-rollback, and the next
    // start settles whatever it leaves from the journal.
    match running {
        Ok(RunningHelper::Standard(mut spawned)) => {
            let report = spawned.link.await_recovered(RECOVERY_VERDICT_TIMEOUT)?;
            let _ = spawned.child.wait();
            Ok(report)
        }
        Ok(RunningHelper::Elevated(mut elevated)) => {
            let report = elevated.link.await_recovered(RECOVERY_VERDICT_TIMEOUT)?;
            let _ = elevated.process.wait_exit(ABORT_EXIT_GRACE);
            Ok(report)
        }
        Err((err, _)) => {
            if crate::elevation::is_declined(&err) {
                Ok(RecoveryReport::RecoveryRequired(
                    crate::elevation::RECOVERY_DECLINED_MESSAGE.to_owned(),
                ))
            } else if crate::elevation::elevation_message(&err).is_some() {
                Ok(RecoveryReport::RecoveryRequired(
                    crate::elevation::RECOVERY_UNAVAILABLE_MESSAGE.to_owned(),
                ))
            } else {
                Err(err)
            }
        }
    }
}

/// A helper that answered `Prepared` for a restart. The application may quit
/// only while it holds one, and only after its own shutdown work is done.
/// Dropping it without [`PreparedRestart::authorize_exit`] aborts the helper.
pub struct PreparedRestart {
    spawned: Option<RunningHelper>,
    coordination: Coordination,
    transaction: TransactionId,
}

/// Removes the staged files of a transaction whose helper has exited without
/// ever writing a journal. Best effort: whatever cannot be verified stays. The
/// temporary proxy credentials go first; if one cannot be removed the directory
/// (and the record naming it) stays so that a later cleanup can retry.
pub(crate) fn discard_unstarted_transaction(coordination: &Coordination, transaction: &TransactionId) {
    if release_secrets(coordination, transaction) {
        let _ = coordination.remove_unjournaled_transaction(transaction);
    }
}

/// Cleans up a transaction that failed before `PreparedRestart` took it over.
/// Nothing is removed unless the helper is certainly stopped; the journal and
/// path checks are made by `remove_unjournaled_transaction`, and anything
/// unverifiable stays.
struct PreparingTransaction<'a> {
    coordination: &'a Coordination,
    transaction: &'a TransactionId,
    helper_stopped: bool,
    armed: bool,
}

impl Drop for PreparingTransaction<'_> {
    fn drop(&mut self) {
        if !self.armed || !self.helper_stopped {
            return;
        }
        crate::state_root::discard_binding(self.coordination, self.transaction);
        // Releases the temporary proxy credentials and the environment record
        // first; a failed release keeps the directory for the next cleanup.
        discard_unstarted_transaction(self.coordination, self.transaction);
    }
}

impl PreparedRestart {
    pub fn transaction(&self) -> &TransactionId {
        &self.transaction
    }

    pub fn helper_pid(&self) -> Option<u32> {
        self.spawned.as_ref().map(RunningHelper::link_pid)
    }

    /// Whether the helper runs elevated, behind the Windows consent prompt.
    pub fn is_elevated(&self) -> bool {
        matches!(self.spawned, Some(RunningHelper::Elevated(_)))
    }

    /// Lets the helper proceed once this process has exited.
    pub fn authorize_exit(mut self) -> Result<()> {
        match self.spawned.as_mut() {
            Some(RunningHelper::Standard(spawned)) => spawned.link.authorize_exit(),
            Some(RunningHelper::Elevated(elevated)) => elevated.link.authorize_exit(),
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
        if let Some(running) = self.spawned.take() {
            // Only a confirmed exit allows the cleanup; a helper that may
            // still be running keeps its transaction directory (and journal)
            // for recovery. The wait is bounded because this runs on the UI
            // thread and a hung helper must not freeze the application.
            let exited = match running {
                RunningHelper::Standard(mut spawned) => {
                    spawned.link.abort();
                    confirm_child_exit(&mut spawned.child, ABORT_EXIT_GRACE, ABORT_KILL_WAIT)
                }
                RunningHelper::Elevated(mut elevated) => {
                    elevated.link.abort();
                    confirm_elevated_exit(elevated.process.as_mut(), ABORT_EXIT_GRACE, ABORT_KILL_WAIT)
                }
            };
            if exited {
                discard_unstarted_transaction(&self.coordination, &self.transaction);
            }
        }
    }
}

/// [`confirm_child_exit`] for an elevated process: it gets `grace` to end by
/// itself after `Abort`, then a best-effort termination and `kill_wait` more.
fn confirm_elevated_exit(process: &mut dyn ElevatedProcess, grace: Duration, kill_wait: Duration) -> bool {
    if process.wait_exit(grace).is_some() {
        return true;
    }
    process.terminate();
    process.wait_exit(kill_wait).is_some()
}

/// How long an aborted helper may take to exit by itself.
const ABORT_EXIT_GRACE: Duration = Duration::from_secs(2);
/// How long to wait for the exit after the helper was killed.
const ABORT_KILL_WAIT: Duration = Duration::from_secs(2);

/// Waits at most `grace` for `child` to exit, then kills it and waits at most
/// `kill_wait` more. Returns whether the exit was confirmed.
fn confirm_child_exit(child: &mut Child, grace: Duration, kill_wait: Duration) -> bool {
    fn wait_bounded(child: &mut Child, limit: Duration) -> Option<bool> {
        let deadline = Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Some(true),
                Ok(None) => {}
                Err(_) => return Some(false),
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(RUNTIME_POLL);
        }
    }
    match wait_bounded(child, grace) {
        Some(confirmed) => confirmed,
        None => {
            let _ = child.kill();
            wait_bounded(child, kill_wait).unwrap_or(false)
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
///
/// `state_root` is the running application's actual state root (the one its
/// `FileStateStore` resolved, default or `HANE_STATE_DIR`). It is validated and
/// bound to this transaction before the helper exists, and later reaches the
/// limited child only through the private health capability.
///
/// The helper is started normally and without any prompt when the install is
/// writable. Only a known protected placement that the current user cannot write
/// makes the explicit "restart and update" operation go through one Windows
/// consent prompt (see [`crate::elevation`]); the application is never closed by
/// this function.
pub fn prepare_restart(
    placement: &crate::platform::Placement,
    policy: &crate::trust::TrustPolicy,
    candidate_id: &str,
    candidate: &CandidateSource<'_>,
    state_root: &Path,
) -> Result<PreparedRestart> {
    prepare_restart_with(
        placement,
        policy,
        candidate_id,
        candidate,
        state_root,
        crate::platform::system_elevation().as_ref(),
    )
}

/// [`prepare_restart`] with the elevation OS seam injected.
pub fn prepare_restart_with(
    placement: &crate::platform::Placement,
    policy: &crate::trust::TrustPolicy,
    candidate_id: &str,
    candidate: &CandidateSource<'_>,
    state_root: &Path,
    os: &dyn ElevationOs,
) -> Result<PreparedRestart> {
    if !valid_candidate_id(candidate_id) {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "malformed candidate id"));
    }
    // The route is decided before any state is created or published, and a
    // route that is not the plain one never writes to the install's parent from
    // here. A writable install keeps the ordinary path and never prompts. A
    // known protected placement the user cannot write goes to the pinned
    // bootstrap: one consent prompt for the installed helper, and only after the
    // peer is verified does the elevated side create the coordination directory
    // and the staged helper copy (see `prepare_protected_with`). An unknown
    // placement or an undetermined access is refused with the reason before any
    // prompt and before anything is staged; a refused route leaves nothing
    // behind.
    {
        use crate::elevation::{Route, StandardAttempt, choose_route};
        let access = os.placement_access(&placement.install_root);
        match choose_route(access.protected, access.write, StandardAttempt::NotTried) {
            Route::Standard => {}
            Route::Elevated(_) => {
                return prepare_protected_with(
                    placement,
                    || crate::platform::installed_helper(placement, policy),
                    candidate_id,
                    candidate,
                    state_root,
                    os,
                );
            }
            Route::Refused(reason) => return Err(crate::elevation::route_refused_error(&reason)),
        }
    }
    let coordination = placement.coordination()?;
    let transaction = TransactionId::generate()?;
    let source = crate::platform::installed_helper(placement, policy)?;
    // From the first staged file until `PreparedRestart` owns the transaction,
    // any early return cleans this transaction (and only this one) up.
    let mut cleanup = PreparingTransaction {
        coordination: &coordination,
        transaction: &transaction,
        helper_stopped: true,
        armed: true,
    };
    let copy = crate::platform::stage_helper_copy(&coordination, &transaction, &source)?;
    stage_candidate(&coordination, &transaction, candidate)?;
    crate::state_root::publish_binding(&coordination, &transaction, state_root)?;
    // The limited child's environment is published later, by the handoff that
    // owns the real OS secret store, before the application can quit
    // (`state_root::publish_environment_with_secrets`).
    let request = HelloRequest {
        mode: Mode::Prepare,
        install_id: placement.install_id.clone(),
        transaction_id: transaction.clone(),
        candidate_id: Some(candidate_id.to_owned()),
    };
    let launch = HelperLaunch {
        copy: &copy,
        trusted: Some(&source.identity),
        request: &request,
        coordination: &coordination,
        install_root: &placement.install_root,
    };
    let spawned = match start_running_helper(
        &launch,
        os,
        ELEVATED_PREPARE_TIMEOUT,
        || spawn_helper_checked(&copy, &request, PREPARE_TIMEOUT),
    ) {
        Ok(spawned) => spawned,
        Err((err, stopped)) => {
            cleanup.helper_stopped = stopped;
            return Err(err);
        }
    };
    cleanup.armed = false;
    // The guard borrows `coordination` and `transaction`; end that borrow
    // before they move into `PreparedRestart`.
    drop(cleanup);
    Ok(PreparedRestart {
        spawned: Some(spawned),
        coordination,
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

    /// In-memory stand-in for the OS secret store, shared by every helper test
    /// (sessions also run on spawned threads). Transaction ids are unique per
    /// test, so tests do not see each other's entries.
    #[derive(Default)]
    pub(super) struct FakeSecrets {
        entries: Mutex<std::collections::BTreeMap<String, String>>,
        failing: Mutex<std::collections::BTreeSet<String>>,
    }

    impl crate::state_root::TransientSecretStore for FakeSecrets {
        fn put(&self, reference: &str, secret: &str) -> Result<()> {
            self.entries
                .lock()
                .unwrap()
                .insert(reference.to_owned(), secret.to_owned());
            Ok(())
        }

        fn take(&self, reference: &str) -> Result<Option<String>> {
            Ok(self.entries.lock().unwrap().remove(reference))
        }

        fn delete(&self, reference: &str) -> Result<()> {
            if self.failing.lock().unwrap().contains(reference) {
                return Err(UpdateError::new(ErrorCode::Io, "the store refused the delete"));
            }
            self.entries.lock().unwrap().remove(reference);
            Ok(())
        }
    }

    pub(super) fn fake_secrets() -> &'static FakeSecrets {
        static STORE: std::sync::OnceLock<FakeSecrets> = std::sync::OnceLock::new();
        STORE.get_or_init(FakeSecrets::default)
    }

    fn fake_has(reference: &str) -> bool {
        fake_secrets().entries.lock().unwrap().contains_key(reference)
    }

    /// Each fixture needs its own InstallId: the runtime lease registry is
    /// process-global, so fixtures sharing one id would contend with each other
    /// when tests run in parallel.
    fn unique_install_id() -> InstallId {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = format!("/x/{}/{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: &path,
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
        install: InstallId,
        txn: TransactionId,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("helper");
        let install = unique_install_id();
        let coord = Coordination::create(dir.path(), &install).unwrap();
        Fixture {
            _dir: dir,
            coord,
            install,
            txn: TransactionId::generate().unwrap(),
        }
    }

    impl Fixture {
        fn env(&self) -> HelperEnv {
            HelperEnv {
                coordination: self.coord.clone(),
                transaction: self.txn.clone(),
                parent_pid: 42,
                own_pid: 4242,
                gate_timeout: Duration::from_millis(100),
                parent_exit_timeout: Duration::from_millis(150),
                authorize_window: Duration::from_secs(60),
            }
        }

        fn hello(&self, nonce: &Nonce, mode: Mode) -> Frame {
            Frame::new(
                nonce,
                Body::Hello {
                    mode,
                    install_id: self.install.as_str().to_owned(),
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
        fn apply(&mut self, ctx: &mut ApplyContext<'_>) -> Result<()> {
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

    /// Runs a session whose published secret is `nonce`.
    fn run_with(
        fx: &Fixture,
        nonce: &Nonce,
        input: &[Frame],
        spy: &mut Spy,
    ) -> (Result<SessionEnd>, Vec<Frame>) {
        let mut reader = wire(input);
        let mut output = Vec::new();
        let result = run_session(&mut reader, &mut output, &fx.env(), nonce, spy);
        (result, replies(&output))
    }

    /// The legitimate parent: the published secret is the one its frames carry.
    fn run(fx: &Fixture, input: &[Frame], spy: &mut Spy) -> (Result<SessionEnd>, Vec<Frame>) {
        let nonce = input
            .first()
            .and_then(|frame| Nonce::parse(&frame.nonce).ok())
            .unwrap_or_else(|| Nonce::generate().unwrap());
        run_with(fx, &nonce, input, spy)
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
        // Each independent handshake gets its own fixture and transaction so
        // state left by one case cannot leak into the next.
        let nonce = Nonce::generate().unwrap();
        let attacker = Nonce::generate().unwrap();

        let fx = fixture();
        let mut spy = Spy::default();
        let (result, _) = run(
            &fx,
            &[fx.hello(&nonce, Mode::Prepare), fx.authorized(&attacker)],
            &mut spy,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::IdentityMismatch);
        assert_eq!(spy.applied, 0);

        let fx = fixture();
        let mut spy = Spy::default();
        let (result, _) = run(
            &fx,
            &[fx.hello(&nonce, Mode::Prepare), fx.hello(&nonce, Mode::Prepare)],
            &mut spy,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::TransitionInvalid);
        assert_eq!(spy.applied, 0);

        let fx = fixture();
        let mut spy = Spy::default();
        let mut stale = fx.authorized(&nonce);
        stale.body = Body::ExitAuthorized {
            transaction_id: TransactionId::generate().unwrap().as_str().to_owned(),
        };
        let (result, _) = run(&fx, &[fx.hello(&nonce, Mode::Prepare), stale], &mut spy);
        assert_eq!(result.unwrap_err().code, ErrorCode::TransitionInvalid);
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

    /// Runs a Recover session, retrying only while the transaction lease is
    /// transiently held. Tests run in parallel threads, and another test
    /// spawning a child process can briefly duplicate this test's lock
    /// descriptors (`flock` is released only when every duplicate is gone, i.e.
    /// at the child's `exec` on macOS), so a just-released lease may be
    /// observed as held for a short moment. The result is otherwise unchanged:
    /// every other outcome is returned immediately.
    fn run_recover_settled(
        fx: &Fixture,
        nonce: &Nonce,
    ) -> (Result<SessionEnd>, Vec<Frame>, Spy) {
        run_recover_retrying(fx, nonce, false)
    }

    /// Like `run_recover_settled`, but for a call made after the fixture has
    /// dropped its own runtime lease: a `Blocked` verdict can then only be the
    /// same transient duplicate on the runtime lock, so it is retried too.
    fn run_recover_after_release(
        fx: &Fixture,
        nonce: &Nonce,
    ) -> (Result<SessionEnd>, Vec<Frame>, Spy) {
        run_recover_retrying(fx, nonce, true)
    }

    fn run_recover_retrying(
        fx: &Fixture,
        nonce: &Nonce,
        retry_blocked: bool,
    ) -> (Result<SessionEnd>, Vec<Frame>, Spy) {
        let started = Instant::now();
        loop {
            let mut spy = Spy::default();
            let (result, out) = run(fx, &[fx.hello(nonce, Mode::Recover)], &mut spy);
            let transient = match &result {
                Err(err) => err.code == ErrorCode::StaleJournal,
                Ok(SessionEnd::Recovered(RecoveryReport::Blocked(_))) => retry_blocked,
                Ok(_) => false,
            };
            if !transient || started.elapsed() >= Duration::from_secs(10) {
                return (result, out, spy);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn recovery_is_blocked_while_another_hane_process_runs() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        {
            let _running = parent_runtime(&fx);
            let (result, out, spy) = run_recover_settled(&fx, &nonce);
            assert!(matches!(
                result.unwrap(),
                SessionEnd::Recovered(RecoveryReport::Blocked(_))
            ));
            assert_eq!(spy.recovered, 0);
            assert!(matches!(out[1].body, Body::Recovered { .. }));
        }
        let (result, _, spy) = run_recover_after_release(&fx, &nonce);
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
        let mut leases = HeldLeases {
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
                .apply(&mut ApplyContext {
                    env: &env,
                    candidate_id: id,
                    leases: &mut leases,
                })
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::IdentityMismatch, "{id:?}");
        }
        // A well-formed id still needs an installation that matches the
        // coordination directory; the temporary directory has none.
        let well_formed = "a".repeat(64);
        let err = backend
            .apply(&mut ApplyContext {
                env: &env,
                candidate_id: Some(well_formed.as_str()),
                leases: &mut leases,
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn a_hello_with_a_nonce_the_parent_never_published_is_ignored() {
        let fx = fixture();
        let published = Nonce::generate().unwrap();
        let forged = Nonce::generate().unwrap();
        let mut spy = Spy::default();
        // Another process launched the staged helper and brought its own nonce.
        let (result, out) = run_with(
            &fx,
            &published,
            &[fx.hello(&forged, Mode::Prepare), fx.authorized(&forged)],
            &mut spy,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::IdentityMismatch);
        assert!(out.is_empty(), "a forged Hello gets no reply");
        assert_eq!(spy.applied, 0);
        // The genuine parent still completes the handshake.
        let (result, _) = run_with(
            &fx,
            &published,
            &[fx.hello(&published, Mode::Prepare), fx.authorized(&published)],
            &mut spy,
        );
        assert_eq!(result.unwrap(), SessionEnd::Applied);
    }

    #[test]
    fn a_late_exit_authorization_is_stale() {
        let fx = fixture();
        let nonce = Nonce::generate().unwrap();
        let mut env = fx.env();
        env.authorize_window = Duration::ZERO;
        let mut reader = wire(&[fx.hello(&nonce, Mode::Prepare), fx.authorized(&nonce)]);
        let mut output = Vec::new();
        let mut spy = Spy::default();
        let result = run_session(&mut reader, &mut output, &env, &nonce, &mut spy);
        assert_eq!(result.unwrap_err().code, ErrorCode::Timeout);
        assert_eq!(spy.applied, 0);
    }

    #[test]
    fn the_session_secret_is_published_by_the_parent_and_consumed_once() {
        let fx = fixture();
        fx.coord.ensure_helper_dir(&fx.txn).unwrap();
        // A direct launch of the staged helper finds no published secret.
        assert_eq!(
            take_session_nonce(&fx.coord, &fx.txn).unwrap_err().code,
            ErrorCode::IdentityMismatch
        );
        let nonce = Nonce::generate().unwrap();
        publish_session_nonce(&fx.coord, &fx.txn, &nonce).unwrap();
        assert_eq!(take_session_nonce(&fx.coord, &fx.txn).unwrap(), nonce);
        // A replayed or second launch gets nothing.
        assert!(take_session_nonce(&fx.coord, &fx.txn).is_err());
        // An unconsumed secret can be withdrawn.
        publish_session_nonce(&fx.coord, &fx.txn, &nonce).unwrap();
        discard_session_nonce(&fx.coord, &fx.txn);
        assert!(take_session_nonce(&fx.coord, &fx.txn).is_err());
    }

    #[test]
    fn the_helper_knows_its_real_parent() {
        // The test process was started by something, and the OS says who.
        assert!(os_parent_id().is_some_and(|pid| pid != std::process::id()));
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
        env.parent_pid = std::process::id();
        let nonce = Nonce::generate().unwrap();
        let helper_nonce = nonce.clone();
        let helper_out_for_close = helper_out.clone();
        let thread = std::thread::spawn(move || {
            let mut reader = helper_in;
            let mut writer = helper_out;
            let result =
                run_session(&mut reader, &mut writer, &env, &helper_nonce, &mut Spy::default());
            helper_out_for_close.close();
            result
        });
        let request = HelloRequest {
            mode,
            install_id: fx.install.clone(),
            transaction_id: fx.txn.clone(),
            candidate_id: Some("cand".to_owned()),
        };
        let link = HelperLink::handshake(
            app_from_helper,
            app_to_helper.clone(),
            &request,
            nonce,
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

        // Another test's child spawn can briefly duplicate this fixture's
        // transaction lock descriptor, so a helper refusal whose reason is
        // exactly `stale_journal:` (surfaced as NotApplicable) is retried;
        // any other outcome is used as is.
        let started = Instant::now();
        let (link, thread, to_helper) = loop {
            let (link, thread, to_helper) = connect(&fx, Mode::Prepare, Some(4242));
            let transient = matches!(
                &link,
                Err(err) if err.code == ErrorCode::NotApplicable
                    && err.detail.starts_with("the helper refused: stale_journal:")
            );
            if !transient || started.elapsed() >= Duration::from_secs(10) {
                break (link, thread, to_helper);
            }
            to_helper.close();
            let _ = thread.join();
            std::thread::sleep(Duration::from_millis(20));
        };
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
            install_id: fx.install.clone(),
            transaction_id: fx.txn.clone(),
            candidate_id: None,
        };
        let err = HelperLink::handshake(
            app_from_helper,
            app_to_helper,
            &request,
            Nonce::generate().unwrap(),
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
                install_id: fx.install.as_str().to_owned(),
                transaction_id: fx.txn.as_str().to_owned(),
                helper_pid: 9,
            },
        );
        let mut out = helper_out.clone();
        write_frame(&mut out, &forged).unwrap();
        let request = HelloRequest {
            mode: Mode::Prepare,
            install_id: fx.install.clone(),
            transaction_id: fx.txn.clone(),
            candidate_id: None,
        };
        let err = HelperLink::handshake(
            app_from_helper_reader,
            app_to_helper,
            &request,
            Nonce::generate().unwrap(),
            None,
            Duration::from_secs(2),
        )
        .err()
        .unwrap();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
        helper_out.close();
    }

    /// A helper that ignores `Abort` must not block the caller forever: it is
    /// killed after the grace period and its exit is confirmed.
    #[cfg(unix)]
    #[test]
    fn a_hung_helper_is_killed_after_a_bounded_wait() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let started = Instant::now();
        assert!(confirm_child_exit(
            &mut child,
            Duration::from_millis(100),
            Duration::from_secs(5)
        ));
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(child.try_wait().unwrap().is_some());
    }

    /// A helper whose exit cannot be confirmed keeps its transaction: the
    /// cleanup in `abort_in_place` runs only when this returns `true`.
    #[cfg(unix)]
    #[test]
    fn an_unconfirmed_exit_is_reported_and_never_waits_forever() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        // No time to confirm even after the kill: the answer is "unknown".
        let confirmed = confirm_child_exit(&mut child, Duration::ZERO, Duration::ZERO);
        // The kill signal may or may not have been reaped yet, but it returns.
        let _ = confirmed;
        let _ = child.kill();
        let _ = child.wait();

        let mut exited = Command::new("true").spawn().unwrap();
        assert!(confirm_child_exit(
            &mut exited,
            Duration::from_secs(5),
            Duration::from_secs(1)
        ));
    }

    /// Stages a file inside the transaction's helper directory.
    fn stage_marker(coord: &Coordination, txn: &TransactionId) -> std::path::PathBuf {
        let marker = coord.ensure_helper_dir(txn).unwrap().join("marker");
        std::fs::write(&marker, b"staged").unwrap();
        marker
    }

    fn drop_guard(fx: &Fixture, helper_stopped: bool) {
        let _guard = PreparingTransaction {
            coordination: &fx.coord,
            transaction: &fx.txn,
            helper_stopped,
            armed: true,
        };
    }

    #[test]
    fn a_failed_preparation_removes_only_its_own_transaction() {
        let fx = fixture();
        let other = TransactionId::generate().unwrap();
        let own_marker = stage_marker(&fx.coord, &fx.txn);
        let other_marker = stage_marker(&fx.coord, &other);
        drop_guard(&fx, true);
        assert!(!own_marker.exists());
        assert!(!fx.coord.transaction_dir(&fx.txn).exists());
        assert!(other_marker.exists(), "another transaction stays");
    }

    #[test]
    fn a_failed_preparation_keeps_the_transaction_when_the_helper_may_run() {
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        drop_guard(&fx, false);
        assert!(marker.exists());
    }

    #[test]
    fn a_failed_preparation_keeps_a_journaled_transaction() {
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        std::fs::write(fx.coord.journal_path(&fx.txn), b"{}").unwrap();
        drop_guard(&fx, true);
        assert!(marker.exists());
    }

    /// A backend whose apply fails the way a candidate revalidation, extraction
    /// or capacity check does; optionally after a journal file exists.
    struct FailingApply {
        journal_first: bool,
        panic: bool,
    }

    impl HelperBackend for FailingApply {
        fn apply(&mut self, ctx: &mut ApplyContext<'_>) -> Result<()> {
            if self.journal_first {
                std::fs::write(ctx.env.coordination.journal_path(&ctx.env.transaction), b"{}")?;
            }
            if self.panic {
                panic!("helper crashed before the journal");
            }
            Err(UpdateError::new(ErrorCode::Io, "candidate revalidation failed"))
        }

        fn recover(&mut self, _ctx: &RecoverContext<'_>) -> Result<RecoveryReport> {
            Ok(RecoveryReport::Nothing)
        }
    }

    fn run_failing(fx: &Fixture, mut backend: FailingApply) -> Result<SessionEnd> {
        let nonce = Nonce::generate().unwrap();
        let mut reader = wire(&[fx.hello(&nonce, Mode::Prepare), fx.authorized(&nonce)]);
        let mut output = Vec::new();
        run_session(&mut reader, &mut output, &fx.env(), &nonce, &mut backend)
    }

    #[test]
    fn a_helper_side_failure_before_the_journal_removes_its_own_transaction_only() {
        let fx = fixture();
        let other = TransactionId::generate().unwrap();
        let own_marker = stage_marker(&fx.coord, &fx.txn);
        let other_marker = stage_marker(&fx.coord, &other);
        let result = run_failing(&fx, FailingApply { journal_first: false, panic: false });
        assert_eq!(result.unwrap_err().code, ErrorCode::Io);
        assert!(!own_marker.exists());
        assert!(!fx.coord.transaction_dir(&fx.txn).exists());
        assert!(other_marker.exists(), "another transaction stays");
    }

    #[test]
    fn a_helper_crash_before_the_journal_removes_its_own_transaction() {
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_failing(&fx, FailingApply { journal_first: false, panic: true })
        }));
        assert!(outcome.is_err());
        assert!(!marker.exists());
    }

    #[test]
    fn a_helper_side_failure_after_the_journal_keeps_the_transaction() {
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        let result = run_failing(&fx, FailingApply { journal_first: true, panic: false });
        assert!(result.is_err());
        assert!(marker.exists());
        assert!(fx.coord.journal_path(&fx.txn).exists());
    }

    #[test]
    fn an_unstarted_cleanup_sweeps_orphaned_secrets_without_a_record() {
        use crate::state_root::TransientSecretStore;
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        let reference = format!("{}.3", fx.txn.as_str());
        fake_secrets().put(&reference, "orphan").unwrap();
        discard_unstarted_transaction(&fx.coord, &fx.txn);
        assert!(!fake_has(&reference));
        assert!(!marker.exists());
    }

    #[test]
    fn a_failed_secret_delete_keeps_the_transaction_for_a_retry() {
        use crate::state_root::TransientSecretStore;
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        let reference = format!("{}.0", fx.txn.as_str());
        fake_secrets().put(&reference, "secret").unwrap();
        fake_secrets().failing.lock().unwrap().insert(reference.clone());
        discard_unstarted_transaction(&fx.coord, &fx.txn);
        assert!(fake_has(&reference), "the secret stays tracked");
        assert!(marker.exists(), "the transaction stays for the next cleanup");
        fake_secrets().failing.lock().unwrap().remove(&reference);
        discard_unstarted_transaction(&fx.coord, &fx.txn);
        assert!(!fake_has(&reference));
        assert!(!marker.exists());
    }

    #[test]
    fn a_prepared_handoff_disarms_the_cleanup() {
        let fx = fixture();
        let marker = stage_marker(&fx.coord, &fx.txn);
        {
            let mut guard = PreparingTransaction {
                coordination: &fx.coord,
                transaction: &fx.txn,
                helper_stopped: true,
                armed: true,
            };
            guard.armed = false;
        }
        assert!(marker.exists());
    }

    // ------------------------------------------------ the elevated start

    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::elevation::{
        AcceptedPeer, ElevatedListener, LaunchFailure, PeerFacts, normal_start_error,
    };

    const ELEVATED_TEST_TIMEOUT: Duration = Duration::from_millis(400);
    const HELPER_PID: u32 = 4242;
    const USER_SID: &str = "S-1-5-21-1";

    /// Plays the consent prompt and the elevated helper: `launch` starts the
    /// real `run_session` on a thread with the secret the application published,
    /// over in-memory pipes, so only the OS facts are scripted.
    struct ConsentOs {
        coord: Coordination,
        txn: TransactionId,
        peer: Mutex<PeerFacts>,
        consent: AtomicBool,
        available: bool,
        /// The classification and write access the route is chosen from.
        access: crate::elevation::PlacementAccess,
        /// A helper that does not hold the published secret.
        helper_nonce: Option<Nonce>,
        pending: Mutex<Option<(PipeReader, PipeWriter)>>,
        closer: Mutex<Option<PipeWriter>>,
        thread: Mutex<Option<std::thread::JoinHandle<Result<SessionEnd>>>>,
        exited: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    fn genuine_peer() -> PeerFacts {
        PeerFacts {
            pid: HELPER_PID,
            session_id: 1,
            token_sid: USER_SID.to_owned(),
            token_elevated: true,
        }
    }

    impl ConsentOs {
        fn new(fx: &Fixture) -> Self {
            Self {
                coord: fx.coord.clone(),
                txn: fx.txn.clone(),
                peer: Mutex::new(genuine_peer()),
                consent: AtomicBool::new(true),
                available: true,
                access: crate::elevation::PlacementAccess {
                    protected: true,
                    write: crate::elevation::WriteAccess::Denied,
                },
                helper_nonce: None,
                pending: Mutex::new(None),
                closer: Mutex::new(None),
                thread: Mutex::new(None),
                exited: Arc::new(AtomicBool::new(false)),
                log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn events(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }

        fn helper_result(&self) -> Result<SessionEnd> {
            let handle = self.thread.lock().unwrap().take().expect("a helper was launched");
            handle.join().expect("the helper thread did not panic")
        }
    }

    struct ConsentListener {
        peer: PeerFacts,
        ends: Option<(PipeReader, PipeWriter)>,
    }

    impl ElevatedListener for ConsentListener {
        fn accept(
            &mut self,
            _timeout: Duration,
            _helper_alive: &mut dyn FnMut() -> bool,
        ) -> Result<AcceptedPeer> {
            let (reader, writer) = self.ends.take().ok_or_else(|| {
                UpdateError::new(ErrorCode::TransitionInvalid, "the endpoint accepts one connection")
            })?;
            Ok(AcceptedPeer {
                facts: self.peer.clone(),
                reader: Box::new(reader),
                writer: Box::new(writer),
            })
        }
    }

    struct ConsentProcess {
        exited: Arc<AtomicBool>,
        closer: PipeWriter,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ElevatedProcess for ConsentProcess {
        fn pid(&self) -> u32 {
            HELPER_PID
        }

        fn wait_exit(&mut self, timeout: Duration) -> Option<u32> {
            let deadline = Instant::now() + timeout;
            loop {
                if self.exited.load(Ordering::SeqCst) {
                    return Some(0);
                }
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn terminate(&mut self) {
            self.log.lock().unwrap().push("terminate");
            self.closer.close();
        }
    }

    impl ElevationOs for ConsentOs {
        fn standard_restart_available(&self) -> bool {
            self.available
        }

        fn placement_access(&self, _install_root: &Path) -> crate::elevation::PlacementAccess {
            self.access
        }

        fn current_principals(&self, _install_root: &Path) -> Result<(String, String)> {
            Ok((USER_SID.to_owned(), USER_SID.to_owned()))
        }

        fn session_id(&self) -> Result<u32> {
            Ok(1)
        }

        fn listen(&self, _transaction: &TransactionId) -> Result<Box<dyn ElevatedListener>> {
            self.log.lock().unwrap().push("listen");
            let (helper_in, app_to_helper) = pipe();
            let (app_from_helper, helper_out) = pipe();
            *self.pending.lock().unwrap() = Some((helper_in, helper_out));
            *self.closer.lock().unwrap() = Some(app_to_helper.clone());
            Ok(Box::new(ConsentListener {
                peer: self.peer.lock().unwrap().clone(),
                ends: Some((app_from_helper, app_to_helper)),
            }))
        }

        fn launch(
            &self,
            _helper: &Path,
            _trusted: &crate::identity::FileIdentity,
        ) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure> {
            self.log.lock().unwrap().push("launch");
            if !self.consent.load(Ordering::SeqCst) {
                return Err(LaunchFailure {
                    stage: "consent-launch",
                    api: "ShellExecuteExW",
                    os_error: Some(crate::elevation::ERROR_CANCELLED),
                    path: "C:\\h\\hane-updater.exe".to_owned(),
                });
            }
            let (mut helper_in, mut helper_out) = self
                .pending
                .lock()
                .unwrap()
                .take()
                .expect("the endpoint was created before the launch");
            let closer = self.closer.lock().unwrap().clone().expect("the endpoint exists");
            let env = HelperEnv {
                coordination: self.coord.clone(),
                transaction: self.txn.clone(),
                parent_pid: std::process::id(),
                own_pid: HELPER_PID,
                gate_timeout: Duration::from_millis(100),
                parent_exit_timeout: Duration::from_millis(150),
                authorize_window: Duration::from_secs(60),
            };
            let exited = Arc::clone(&self.exited);
            exited.store(false, Ordering::SeqCst);
            let override_nonce = self.helper_nonce.clone();
            let end = helper_out.clone();
            let thread = std::thread::spawn(move || {
                let nonce = match override_nonce {
                    Some(nonce) => Ok(nonce),
                    None => take_session_nonce(&env.coordination, &env.transaction),
                };
                let result = nonce.and_then(|nonce| {
                    run_session(&mut helper_in, &mut helper_out, &env, &nonce, &mut Spy::default())
                });
                end.close();
                exited.store(true, Ordering::SeqCst);
                result
            });
            *self.thread.lock().unwrap() = Some(thread);
            Ok(Box::new(ConsentProcess {
                exited: Arc::clone(&self.exited),
                closer,
                log: Arc::clone(&self.log),
            }))
        }
    }

    type Started = std::result::Result<RunningHelper, (UpdateError, bool)>;

    fn start_with(
        fx: &Fixture,
        os: &dyn ElevationOs,
        normal: (UpdateError, bool),
    ) -> Started {
        fx.coord.ensure_helper_dir(&fx.txn).unwrap();
        let hello = HelloRequest {
            mode: Mode::Prepare,
            install_id: fx.install.clone(),
            transaction_id: fx.txn.clone(),
            candidate_id: Some("cand".to_owned()),
        };
        let trusted = crate::identity::FileIdentity::of(b"helper");
        let launch = HelperLaunch {
            copy: Path::new("C:\\h\\hane-updater.exe"),
            trusted: Some(&trusted),
            request: &hello,
            coordination: &fx.coord,
            install_root: Path::new("C:\\hane"),
        };
        start_running_helper(
            &launch,
            os,
            ELEVATED_TEST_TIMEOUT,
            || Err(normal),
        )
    }

    fn elevation_required() -> (UpdateError, bool) {
        (normal_start_error(Some(740), Path::new("C:\\h\\hane-updater.exe")), true)
    }

    /// Like `start_with` for a 740 start, retrying only while the transaction
    /// lease is transiently held by a duplicate descriptor of another test's
    /// child process (see `the_application_requires_prepared_from_the_expected_helper`).
    fn start_elevated_settled(fx: &Fixture, os: &ConsentOs) -> Started {
        let started = Instant::now();
        loop {
            let result = start_with(fx, os, elevation_required());
            let transient = matches!(
                &result,
                Err((err, _)) if err.code == ErrorCode::NotApplicable
                    && err.detail.starts_with("the helper refused: stale_journal:")
            );
            if !transient || started.elapsed() >= Duration::from_secs(10) {
                return result;
            }
            let _ = os.helper_result();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Runs the production route selection of `start_running_helper` and reports
    /// how often the normal start was attempted.
    fn route_with(
        fx: &Fixture,
        os: &dyn ElevationOs,
        normal: (UpdateError, bool),
    ) -> (Started, usize) {
        fx.coord.ensure_helper_dir(&fx.txn).unwrap();
        let hello = HelloRequest {
            mode: Mode::Prepare,
            install_id: fx.install.clone(),
            transaction_id: fx.txn.clone(),
            candidate_id: Some("cand".to_owned()),
        };
        let normal_starts = std::cell::Cell::new(0_usize);
        let trusted = crate::identity::FileIdentity::of(b"helper");
        let launch = HelperLaunch {
            copy: Path::new("C:\\h\\hane-worker.exe"),
            trusted: Some(&trusted),
            request: &hello,
            coordination: &fx.coord,
            install_root: Path::new("C:\\hane"),
        };
        let result = start_running_helper(
            &launch,
            os,
            ELEVATED_TEST_TIMEOUT,
            || {
                normal_starts.set(normal_starts.get() + 1);
                Err(normal)
            },
        );
        (result, normal_starts.get())
    }

    fn access(protected: bool, write: crate::elevation::WriteAccess) -> crate::elevation::PlacementAccess {
        crate::elevation::PlacementAccess { protected, write }
    }

    fn recovery_with(fx: &Fixture, os: &ConsentOs) -> Result<RecoveryReport> {
        let trusted = crate::identity::FileIdentity::of(b"helper");
        recovery_with_trust(fx, os, Some(&trusted))
    }

    fn recovery_with_trust(
        fx: &Fixture,
        os: &ConsentOs,
        trusted: Option<&crate::identity::FileIdentity>,
    ) -> Result<RecoveryReport> {
        fx.coord.ensure_helper_dir(&fx.txn).unwrap();
        let placement = crate::platform::Placement {
            target: Target::WindowsX64,
            install_root: std::path::PathBuf::from("C:\\hane"),
            coordination_parent: std::path::PathBuf::from("C:\\"),
            install_id: fx.install.clone(),
        };
        recover_with_helper_copy(
            os,
            &placement,
            &fx.coord,
            &fx.txn,
            Path::new("C:\\h\\hane-updater.exe"),
            trusted,
        )
    }

    #[test]
    fn an_elevated_recovery_without_a_trusted_identity_never_prompts() {
        let fx = fixture();
        let os = ConsentOs::new(&fx);
        let err = recovery_with_trust(&fx, &os, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
        assert!(os.events().is_empty(), "no launch and no prompt: {:?}", os.events());
        assert!(fx.coord.transaction_dir(&fx.txn).exists(), "the transaction is kept");
    }

    #[test]
    fn a_declined_recovery_prompt_keeps_the_transaction_and_asks_for_manual_recovery() {
        let fx = fixture();
        let os = ConsentOs::new(&fx);
        os.consent.store(false, Ordering::SeqCst);
        let report = recovery_with(&fx, &os).unwrap();
        assert_eq!(
            report,
            RecoveryReport::RecoveryRequired(crate::elevation::RECOVERY_DECLINED_MESSAGE.to_owned())
        );
        assert!(fx.coord.transaction_dir(&fx.txn).exists(), "the transaction is kept");
    }

    #[test]
    fn recovery_of_an_unknown_placement_never_prompts() {
        use crate::elevation::WriteAccess;
        let fx = fixture();
        let mut os = ConsentOs::new(&fx);
        os.access = access(false, WriteAccess::Denied);
        let report = recovery_with(&fx, &os).unwrap();
        assert!(matches!(report, RecoveryReport::RecoveryRequired(_)));
        assert!(os.events().is_empty(), "no endpoint and no prompt: {:?}", os.events());
    }

    #[test]
    fn a_writable_install_never_prompts_whatever_the_start_error_says() {
        use crate::elevation::WriteAccess;
        let fx = fixture();
        let path = Path::new("C:\\h\\hane-worker.exe");
        let failures = [
            // A name-derived 740, access denied, an unknown error, a refused prompt.
            (normal_start_error(Some(740), path), true),
            (normal_start_error(Some(5), path), true),
            (normal_start_error(None, path), true),
            (normal_start_error(Some(1223), path), true),
            (UpdateError::new(ErrorCode::Io, "the helper pipes are unavailable"), true),
            (normal_start_error(Some(740), path), false),
        ];
        for protected in [false, true] {
            for (error, stopped) in &failures {
                let mut os = ConsentOs::new(&fx);
                os.access = access(protected, WriteAccess::Writable);
                let (result, normal_starts) = route_with(&fx, &os, (error.clone(), *stopped));
                let (returned, returned_stopped) = result.err().unwrap();
                assert_eq!(&returned, error);
                assert_eq!(returned_stopped, *stopped);
                assert_eq!(normal_starts, 1);
                assert!(os.events().is_empty(), "no endpoint and no prompt: {:?}", os.events());
            }
        }
    }

    #[test]
    fn an_unknown_or_nonprotected_placement_is_refused_without_a_prompt_or_a_start() {
        use crate::elevation::WriteAccess;
        let fx = fixture();
        for facts in [
            access(false, WriteAccess::Denied),
            access(false, WriteAccess::Unknown),
            access(true, WriteAccess::Unknown),
        ] {
            let mut os = ConsentOs::new(&fx);
            os.access = facts;
            let (result, normal_starts) = route_with(&fx, &os, elevation_required());
            let (error, stopped) = result.err().unwrap();
            assert!(error.detail.starts_with("the update route was refused"), "{error:?}");
            assert!(!crate::elevation::is_elevation_required(&error));
            assert!(stopped, "no helper was started");
            assert_eq!(normal_starts, 0);
            assert!(os.events().is_empty(), "{:?}", os.events());
        }
    }

    #[test]
    fn a_protected_placement_without_write_access_goes_to_the_consent_prompt_first() {
        let fx = fixture();
        let os = ConsentOs::new(&fx);
        let started = Instant::now();
        let (result, normal_starts) = loop {
            let (result, normal_starts) = route_with(&fx, &os, (UpdateError::new(ErrorCode::Io, "unused"), true));
            let transient = matches!(
                &result,
                Err((err, _)) if err.code == ErrorCode::NotApplicable
                    && err.detail.starts_with("the helper refused: stale_journal:")
            );
            if !transient || started.elapsed() >= Duration::from_secs(10) {
                break (result, normal_starts);
            }
            let _ = os.helper_result();
            std::thread::sleep(Duration::from_millis(20));
        };
        let running = result.expect("an elevated helper");
        assert_eq!(normal_starts, 0, "the update is not tried unelevated first");
        assert!(matches!(running, RunningHelper::Elevated(_)));
        assert_eq!(os.events(), ["listen", "launch"]);
        drop(PreparedRestart {
            spawned: Some(running),
            coordination: fx.coord.clone(),
            transaction: fx.txn.clone(),
        });
        assert_eq!(os.helper_result().unwrap(), SessionEnd::Aborted);
    }

    #[test]
    fn a_build_without_the_standard_restart_keeps_the_original_error_and_shows_no_prompt() {
        let fx = fixture();
        let mut os = ConsentOs::new(&fx);
        os.available = false;
        let (error, stopped) = start_with(&fx, &os, elevation_required()).err().unwrap();
        assert!(crate::elevation::is_elevation_required(&error));
        assert!(stopped);
        assert!(os.events().is_empty(), "{:?}", os.events());
    }

    /// A placement whose coordination parent cannot be created (it lies below a
    /// regular file), like a protected parent the user may not write.
    fn unwritable_parent_placement(fx: &Fixture) -> (crate::platform::Placement, std::path::PathBuf) {
        let blocker = fx._dir.path().join("blocker");
        std::fs::write(&blocker, b"file").unwrap();
        let placement = crate::platform::Placement {
            target: Target::WindowsX64,
            install_root: std::path::PathBuf::from("C:\\hane"),
            coordination_parent: blocker.join("parent"),
            install_id: fx.install.clone(),
        };
        (placement, blocker)
    }

    fn prepare_unwritable(fx: &Fixture, os: &dyn ElevationOs) -> UpdateError {
        let (placement, blocker) = unwritable_parent_placement(fx);
        let candidate = fx._dir.path().join("candidate");
        let source = CandidateSource { catalog: &candidate, signature: &candidate, archive: &candidate, seen: None };
        let err = prepare_restart_with(
            &placement,
            &crate::testing::test_policy(),
            &"a".repeat(64),
            &source,
            fx._dir.path(),
            os,
        )
        .err()
        .expect("the restart is not prepared");
        // Nothing was created: the blocking file is untouched and no state exists.
        assert!(blocker.is_file());
        assert_eq!(std::fs::read(&blocker).unwrap(), b"file");
        assert!(!fx._dir.path().join("candidate").exists());
        err
    }

    #[test]
    fn a_protected_placement_without_restart_support_fails_before_any_state_is_created() {
        let fx = fixture();
        let mut os = ConsentOs::new(&fx);
        os.available = false;
        let err = prepare_unwritable(&fx, &os);
        assert!(crate::elevation::is_elevation_required(&err), "{err:?}");
        assert!(os.events().is_empty(), "{:?}", os.events());
    }

    #[test]
    fn a_protected_placement_whose_installed_helper_cannot_be_verified_is_refused_before_the_prompt() {
        let fx = fixture();
        // Restart support is available and consent would be given, but the
        // installed helper cannot be verified here (no supported placement):
        // the request stops with no endpoint, no prompt and no state.
        let os = ConsentOs::new(&fx);
        let err = prepare_unwritable(&fx, &os);
        assert!(!crate::elevation::is_declined(&err), "{err:?}");
        assert!(os.events().is_empty(), "{:?}", os.events());
    }

    #[test]
    fn an_unknown_placement_or_undetermined_access_is_refused_before_any_state_is_created() {
        use crate::elevation::WriteAccess;
        let fx = fixture();
        for facts in [
            access(false, WriteAccess::Denied),
            access(false, WriteAccess::Unknown),
            access(true, WriteAccess::Unknown),
        ] {
            let mut os = ConsentOs::new(&fx);
            os.access = facts;
            let err = prepare_unwritable(&fx, &os);
            assert!(err.detail.starts_with("the update route was refused"), "{err:?}");
            assert!(!crate::elevation::is_elevation_required(&err));
            assert!(os.events().is_empty(), "{:?}", os.events());
        }
    }

    #[test]
    fn a_writable_placement_keeps_the_normal_start_and_fails_only_at_the_state() {
        use crate::elevation::WriteAccess;
        let fx = fixture();
        let mut os = ConsentOs::new(&fx);
        os.access = access(false, WriteAccess::Writable);
        let err = prepare_unwritable(&fx, &os);
        assert!(!err.detail.starts_with("the update route was refused"), "{err:?}");
        assert!(!crate::elevation::is_elevation_required(&err));
        assert!(os.events().is_empty(), "{:?}", os.events());
    }

    #[test]
    fn a_740_start_prepares_the_elevated_helper_and_nothing_applies_before_the_exit_is_authorized() {
        let fx = fixture();
        let os = ConsentOs::new(&fx);
        let running = start_elevated_settled(&fx, &os).expect("an elevated helper");
        assert!(matches!(running, RunningHelper::Elevated(_)));
        assert_eq!(running.link_pid(), HELPER_PID);
        assert_eq!(os.events(), ["listen", "launch"]);
        let prepared = PreparedRestart {
            spawned: Some(running),
            coordination: fx.coord.clone(),
            transaction: fx.txn.clone(),
        };
        assert!(prepared.is_elevated());
        assert_eq!(prepared.helper_pid(), Some(HELPER_PID));
        // Prepared was answered, yet the helper has neither applied nor ended:
        // it waits for the application's own authorization.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!os.exited.load(Ordering::SeqCst));
        prepared.authorize_exit().unwrap();
        assert_eq!(os.helper_result().unwrap(), SessionEnd::Applied);
    }

    #[test]
    fn a_dropped_elevated_handoff_aborts_the_helper_without_applying() {
        let fx = fixture();
        let os = ConsentOs::new(&fx);
        let running = start_elevated_settled(&fx, &os).expect("an elevated helper");
        drop(PreparedRestart {
            spawned: Some(running),
            coordination: fx.coord.clone(),
            transaction: fx.txn.clone(),
        });
        assert_eq!(os.helper_result().unwrap(), SessionEnd::Aborted);
    }

    #[test]
    fn a_declined_prompt_leaves_everything_in_place_and_a_retry_succeeds() {
        let fx = fixture();
        let os = ConsentOs::new(&fx);
        os.consent.store(false, Ordering::SeqCst);
        let (error, stopped) = start_with(&fx, &os, elevation_required()).err().unwrap();
        assert!(crate::elevation::is_declined(&error));
        assert!(stopped, "no helper exists after a refused prompt");
        assert_eq!(os.events(), ["listen", "launch"]);
        // The one-shot secret of the refused attempt is gone.
        assert!(take_session_nonce(&fx.coord, &fx.txn).is_err());

        os.consent.store(true, Ordering::SeqCst);
        let running = start_elevated_settled(&fx, &os).expect("the retry prepares");
        drop(PreparedRestart {
            spawned: Some(running),
            coordination: fx.coord.clone(),
            transaction: fx.txn.clone(),
        });
        assert_eq!(os.helper_result().unwrap(), SessionEnd::Aborted);
    }

    #[test]
    fn a_peer_that_is_not_the_launched_elevated_helper_never_prepares() {
        let fx = fixture();
        let mut other_process = genuine_peer();
        other_process.pid = HELPER_PID + 1;
        let mut other_session = genuine_peer();
        other_session.session_id = 2;
        let mut other_account = genuine_peer();
        other_account.token_sid = "S-1-5-21-2".to_owned();
        let mut not_elevated = genuine_peer();
        not_elevated.token_elevated = false;
        for forged in [other_process, other_session, other_account, not_elevated] {
            let os = ConsentOs::new(&fx);
            *os.peer.lock().unwrap() = forged;
            let (error, stopped) = start_with(&fx, &os, elevation_required()).err().unwrap();
            assert_eq!(error.code, ErrorCode::IdentityMismatch);
            assert!(stopped, "the launched process was stopped and its exit confirmed");
            assert_eq!(os.events().last(), Some(&"terminate"));
            // The helper never saw `Hello`, let alone an authorization.
            assert!(os.helper_result().is_err());
        }
    }

    #[test]
    fn a_helper_without_the_published_secret_never_prepares() {
        let fx = fixture();
        let mut os = ConsentOs::new(&fx);
        os.helper_nonce = Some(Nonce::generate().unwrap());
        let (error, stopped) = start_with(&fx, &os, elevation_required()).err().unwrap();
        // The helper drops the session without a reply: the application sees a
        // silent or closed link, never `Prepared`.
        assert!(matches!(error.code, ErrorCode::Timeout | ErrorCode::Truncated), "{error:?}");
        assert!(stopped);
        assert_eq!(os.events().last(), Some(&"terminate"));
        assert_eq!(os.helper_result().unwrap_err().code, ErrorCode::IdentityMismatch);
        // The unused secret of the failed attempt was withdrawn.
        assert!(take_session_nonce(&fx.coord, &fx.txn).is_err());
    }

    #[test]
    fn the_elevated_helper_refuses_to_apply_without_the_standard_restart() {
        let fx = fixture();
        let transaction = fx.coord.try_acquire(LeaseKind::Transaction, &[]).unwrap().unwrap();
        let startup = fx
            .coord
            .try_acquire(LeaseKind::Startup, &[&transaction])
            .unwrap()
            .unwrap();
        let mut leases = HeldLeases { transaction, startup, runtime: None };
        let env = fx.env();
        let mut backend = ElevatedBackend { inner: Spy::default(), standard_restart: false };
        let err = backend
            .apply(&mut ApplyContext { env: &env, candidate_id: Some("cand"), leases: &mut leases })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotApplicable);
        assert_eq!(backend.inner.applied, 0);

        backend.standard_restart = true;
        leases.runtime = Some(parent_runtime(&fx));
        backend
            .apply(&mut ApplyContext { env: &env, candidate_id: Some("cand"), leases: &mut leases })
            .unwrap();
        assert_eq!(backend.inner.applied, 1);
    }

    // ---------------------------------- the protected-placement bootstrap

    use crate::identity::FileIdentity;
    use crate::platform::{HelperSource, Placement};

    const INSTALLED_HELPER: &[u8] = b"installed helper";

    /// A protected install: the installed helper, a coordination parent that is
    /// empty, a Ready candidate and the application's state root.
    struct Protected {
        dir: TempDir,
        placement: Placement,
        source: HelperSource,
        parent: std::path::PathBuf,
        candidate: std::path::PathBuf,
        state_root: std::path::PathBuf,
    }

    fn protected() -> Protected {
        let dir = TempDir::new("protected-bootstrap");
        let install_root = dir.path().join("hane");
        std::fs::create_dir(&install_root).unwrap();
        let helper = install_root.join("hane-updater");
        std::fs::write(&helper, INSTALLED_HELPER).unwrap();
        let parent = dir.path().join("protected-parent");
        std::fs::create_dir(&parent).unwrap();
        let candidate = dir.path().join("candidate");
        std::fs::create_dir(&candidate).unwrap();
        for name in ["catalog.json", "catalog.sig", "archive.zip"] {
            std::fs::write(candidate.join(name), name.as_bytes()).unwrap();
        }
        let state_root = dir.path().join("state");
        std::fs::create_dir(&state_root).unwrap();
        crate::platform::acl::secure_new(&state_root, true, 0o700).unwrap();
        let placement = Placement {
            target: Target::WindowsX64,
            install_root,
            coordination_parent: parent.clone(),
            install_id: unique_install_id(),
        };
        let source = HelperSource { path: helper, identity: FileIdentity::of(INSTALLED_HELPER) };
        Protected { dir, placement, source, parent, candidate, state_root }
    }

    impl Protected {
        fn parent_entries(&self) -> usize {
            std::fs::read_dir(&self.parent).unwrap().count()
        }

        fn coordination(&self) -> Coordination {
            self.placement.existing_coordination().unwrap().expect("the helper created it")
        }

        fn prepare(&self, os: &dyn ElevationOs) -> Result<PreparedRestart> {
            let (catalog, signature, archive) = (
                self.candidate.join("catalog.json"),
                self.candidate.join("catalog.sig"),
                self.candidate.join("archive.zip"),
            );
            let candidate = CandidateSource { catalog: &catalog, signature: &signature, archive: &archive, seen: None };
            prepare_protected_with(
                &self.placement,
                || Ok(self.source.clone()),
                &"a".repeat(64),
                &candidate,
                &self.state_root,
                os,
            )
        }
    }

    /// Plays the consent prompt and the elevated, installed helper: `launch_installed`
    /// runs the real `serve_bootstrap` and `run_session` on a thread over in-memory
    /// pipes. The helper derives its placement and image itself, so tests can make
    /// it disagree with the request.
    struct BootstrapOs {
        helper_placement: Placement,
        helper_source: HelperSource,
        helper_exe: std::path::PathBuf,
        parent: std::path::PathBuf,
        peer: Mutex<PeerFacts>,
        consent: AtomicBool,
        available: bool,
        /// Whether the coordination parent was still untouched when the prompt was shown.
        parent_clean_at_prompt: AtomicBool,
        pending: Mutex<Option<(PipeReader, PipeWriter)>>,
        closer: Mutex<Option<PipeWriter>>,
        thread: Mutex<Option<std::thread::JoinHandle<Result<SessionEnd>>>>,
        exited: Arc<AtomicBool>,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl BootstrapOs {
        fn new(fx: &Protected) -> Self {
            Self {
                helper_placement: fx.placement.clone(),
                helper_source: fx.source.clone(),
                helper_exe: fx.source.path.clone(),
                parent: fx.parent.clone(),
                peer: Mutex::new(genuine_peer()),
                consent: AtomicBool::new(true),
                available: true,
                parent_clean_at_prompt: AtomicBool::new(false),
                pending: Mutex::new(None),
                closer: Mutex::new(None),
                thread: Mutex::new(None),
                exited: Arc::new(AtomicBool::new(false)),
                log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn events(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }

        fn helper_result(&self) -> Result<SessionEnd> {
            let handle = self.thread.lock().unwrap().take().expect("a helper was launched");
            handle.join().expect("the helper thread did not panic")
        }
    }

    impl ElevationOs for BootstrapOs {
        fn standard_restart_available(&self) -> bool {
            self.available
        }

        fn placement_access(&self, _install_root: &Path) -> crate::elevation::PlacementAccess {
            crate::elevation::PlacementAccess {
                protected: true,
                write: crate::elevation::WriteAccess::Denied,
            }
        }

        fn current_principals(&self, _install_root: &Path) -> Result<(String, String)> {
            Ok((USER_SID.to_owned(), USER_SID.to_owned()))
        }

        fn session_id(&self) -> Result<u32> {
            Ok(1)
        }

        fn listen(&self, _transaction: &TransactionId) -> Result<Box<dyn ElevatedListener>> {
            panic!("the bootstrap never uses a transaction-named endpoint");
        }

        fn launch(
            &self,
            _helper: &Path,
            _trusted: &FileIdentity,
        ) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure> {
            panic!("the bootstrap never starts a staged copy");
        }

        fn listen_bootstrap(&self, _install_id: &InstallId) -> Result<Box<dyn ElevatedListener>> {
            self.log.lock().unwrap().push("listen_bootstrap");
            let (helper_in, app_to_helper) = pipe();
            let (app_from_helper, helper_out) = pipe();
            *self.pending.lock().unwrap() = Some((helper_in, helper_out));
            *self.closer.lock().unwrap() = Some(app_to_helper.clone());
            Ok(Box::new(ConsentListener {
                peer: self.peer.lock().unwrap().clone(),
                ends: Some((app_from_helper, app_to_helper)),
            }))
        }

        fn launch_installed(
            &self,
            _helper: &Path,
            _trusted: &FileIdentity,
        ) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure> {
            self.log.lock().unwrap().push("launch_installed");
            self.parent_clean_at_prompt.store(
                std::fs::read_dir(&self.parent).unwrap().count() == 0,
                Ordering::SeqCst,
            );
            if !self.consent.load(Ordering::SeqCst) {
                return Err(LaunchFailure {
                    stage: "consent-launch",
                    api: "ShellExecuteExW",
                    os_error: Some(crate::elevation::ERROR_CANCELLED),
                    path: "C:\\hane\\hane-updater.exe".to_owned(),
                });
            }
            let (mut helper_in, mut helper_out) = self
                .pending
                .lock()
                .unwrap()
                .take()
                .expect("the endpoint was created before the launch");
            let closer = self.closer.lock().unwrap().clone().expect("the endpoint exists");
            let placement = self.helper_placement.clone();
            let source = self.helper_source.clone();
            let exe = self.helper_exe.clone();
            let exited = Arc::clone(&self.exited);
            exited.store(false, Ordering::SeqCst);
            let end = helper_out.clone();
            let thread = std::thread::spawn(move || {
                let result = serve_bootstrap(&mut helper_in, &mut helper_out, &placement, &source, &exe)
                    .and_then(|bootstrapped| {
                        let env = HelperEnv {
                            coordination: bootstrapped.coordination,
                            transaction: bootstrapped.transaction,
                            parent_pid: std::process::id(),
                            own_pid: HELPER_PID,
                            gate_timeout: Duration::from_millis(100),
                            parent_exit_timeout: Duration::from_millis(150),
                            authorize_window: Duration::from_secs(60),
                        };
                        run_session(
                            &mut helper_in,
                            &mut helper_out,
                            &env,
                            &bootstrapped.nonce,
                            &mut Spy::default(),
                        )
                    });
                end.close();
                exited.store(true, Ordering::SeqCst);
                result
            });
            *self.thread.lock().unwrap() = Some(thread);
            Ok(Box::new(ConsentProcess {
                exited: Arc::clone(&self.exited),
                closer,
                log: Arc::clone(&self.log),
            }))
        }
    }

    #[test]
    fn a_protected_placement_prompts_once_and_the_parent_is_written_only_by_the_verified_helper() {
        let fx = protected();
        let os = BootstrapOs::new(&fx);
        assert_eq!(fx.parent_entries(), 0);
        let prepared = fx.prepare(&os).expect("the bootstrap prepares the restart");
        assert_eq!(os.events(), ["listen_bootstrap", "launch_installed"]);
        assert!(
            os.parent_clean_at_prompt.load(Ordering::SeqCst),
            "nothing was written to the protected parent before the consent"
        );
        assert!(prepared.is_elevated());
        assert_eq!(prepared.helper_pid(), Some(HELPER_PID));

        // The elevated helper made the coordination directory and the staged
        // copy; this side staged the candidate and the state binding into it.
        let coordination = fx.coordination();
        let transaction = prepared.transaction().clone();
        let txn_dir = coordination.transaction_dir(&transaction);
        let staged = txn_dir.join("helper").join(crate::platform::STAGED_HELPER_FILE_NAME);
        assert_eq!(std::fs::read(&staged).unwrap(), INSTALLED_HELPER);
        assert!(txn_dir.join("candidate").join("archive.zip").is_file());
        assert_eq!(
            crate::state_root::read_binding(&coordination, &transaction).unwrap(),
            fx.state_root
        );
        crate::platform::acl::verify_user_owned(coordination.dir(), true).unwrap();
        crate::platform::acl::verify_user_owned(&txn_dir, true).unwrap();
        crate::platform::acl::verify_user_owned(&staged, false).unwrap();
        // The application is not asked to quit by the preparation.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!os.exited.load(Ordering::SeqCst));

        // Dropping the handoff before any authorization aborts the helper and
        // removes the transaction; the old version was never touched.
        drop(prepared);
        assert_eq!(os.helper_result().unwrap(), SessionEnd::Aborted);
        assert!(!txn_dir.exists());
    }

    #[test]
    fn a_declined_prompt_creates_nothing_in_the_protected_parent_and_a_retry_succeeds() {
        let fx = protected();
        let os = BootstrapOs::new(&fx);
        os.consent.store(false, Ordering::SeqCst);
        let err = fx.prepare(&os).err().expect("the prompt was declined");
        assert!(crate::elevation::is_declined(&err), "{err:?}");
        assert_eq!(os.events(), ["listen_bootstrap", "launch_installed"]);
        assert!(os.parent_clean_at_prompt.load(Ordering::SeqCst));
        assert_eq!(fx.parent_entries(), 0, "a refused consent leaves the parent untouched");

        os.consent.store(true, Ordering::SeqCst);
        let prepared = fx.prepare(&os).expect("the retry prepares");
        drop(prepared);
        assert_eq!(os.helper_result().unwrap(), SessionEnd::Aborted);
    }

    #[test]
    fn a_build_without_the_standard_restart_shows_no_prompt_and_writes_nothing() {
        let fx = protected();
        let mut os = BootstrapOs::new(&fx);
        os.available = false;
        let err = fx.prepare(&os).err().expect("no prompt without the standard restart");
        assert!(crate::elevation::is_elevation_required(&err), "{err:?}");
        assert!(os.events().is_empty(), "{:?}", os.events());
        assert_eq!(fx.parent_entries(), 0);
    }

    #[test]
    fn a_peer_that_is_not_the_launched_elevated_helper_never_makes_the_helper_write_anything() {
        let mut other_process = genuine_peer();
        other_process.pid = HELPER_PID + 1;
        let mut other_session = genuine_peer();
        other_session.session_id = 2;
        let mut other_account = genuine_peer();
        other_account.token_sid = "S-1-5-21-2".to_owned();
        let mut not_elevated = genuine_peer();
        not_elevated.token_elevated = false;
        for forged in [other_process, other_session, other_account, not_elevated] {
            let fx = protected();
            let os = BootstrapOs::new(&fx);
            *os.peer.lock().unwrap() = forged;
            let err = fx.prepare(&os).err().expect("a forged peer is refused");
            assert_eq!(err.code, ErrorCode::IdentityMismatch);
            assert_eq!(os.events().last(), Some(&"terminate"));
            // The request was never sent, so the helper created nothing.
            assert_eq!(fx.parent_entries(), 0);
            assert!(os.helper_result().is_err());
        }
    }

    #[test]
    fn a_helper_that_derives_another_install_or_image_refuses_before_creating_anything() {
        // Another install than the one the application asked for.
        let fx = protected();
        let mut os = BootstrapOs::new(&fx);
        os.helper_placement.install_id = unique_install_id();
        let err = fx.prepare(&os).err().expect("the helper refused");
        assert_eq!(err.code, ErrorCode::Truncated, "{err:?}");
        assert_eq!(fx.parent_entries(), 0);
        assert_eq!(os.helper_result().unwrap_err().code, ErrorCode::IdentityMismatch);

        // A running image that is not the installed helper of the install.
        let fx = protected();
        let mut os = BootstrapOs::new(&fx);
        let other = fx.dir.path().join("other-helper");
        std::fs::write(&other, INSTALLED_HELPER).unwrap();
        os.helper_exe = other;
        let err = fx.prepare(&os).err().expect("the helper refused");
        assert_eq!(err.code, ErrorCode::Truncated, "{err:?}");
        assert_eq!(fx.parent_entries(), 0);
        assert_eq!(os.helper_result().unwrap_err().code, ErrorCode::IdentityMismatch);
    }

    #[test]
    fn a_staging_failure_after_the_bootstrap_stops_the_helper_and_removes_the_transaction() {
        let fx = protected();
        std::fs::remove_file(fx.candidate.join("archive.zip")).unwrap();
        let os = BootstrapOs::new(&fx);
        let err = fx.prepare(&os).err().expect("the candidate cannot be staged");
        assert!(!crate::elevation::is_declined(&err), "{err:?}");
        assert_eq!(os.events().last(), Some(&"terminate"));
        assert!(os.helper_result().is_err(), "the helper never got a Hello");
        let private = fx.coordination().dir().join(crate::platform::lease::PRIVATE_DIR);
        assert_eq!(
            std::fs::read_dir(&private).unwrap().count(),
            0,
            "the failed transaction was removed and the install is untouched"
        );
    }

    #[test]
    fn a_state_root_that_cannot_be_bound_is_refused_before_the_prompt() {
        let mut fx = protected();
        fx.state_root = fx.dir.path().join("missing-state");
        let os = BootstrapOs::new(&fx);
        let err = fx.prepare(&os).err().expect("an unusable state root is refused");
        assert!(!crate::elevation::is_declined(&err), "{err:?}");
        assert!(os.events().is_empty(), "{:?}", os.events());
        assert_eq!(fx.parent_entries(), 0);
    }

    fn request_bytes(install: &InstallId, transaction: &TransactionId, nonce: &Nonce) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_bootstrap_request(&mut bytes, install, transaction, nonce).unwrap();
        bytes
    }

    #[test]
    fn the_bootstrap_request_round_trips_and_malformed_ones_are_refused_without_echoing_them() {
        let install = unique_install_id();
        let transaction = TransactionId::generate().unwrap();
        let nonce = Nonce::generate().unwrap();
        let bytes = request_bytes(&install, &transaction, &nonce);
        let read = read_bootstrap_request(&mut Cursor::new(bytes.clone())).unwrap();
        assert_eq!(read.install_id, install);
        assert_eq!(read.transaction, transaction);
        assert_eq!(read.nonce.as_str(), nonce.as_str());

        let secret = nonce.as_str().to_owned();
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"HANE-BOOTSTRAP-1 only-one-field\n".to_vec(),
            format!("OTHER-MAGIC {} {} {secret}\n", install.as_str(), transaction.as_str()).into_bytes(),
            format!("HANE-BOOTSTRAP-1 {} {} {secret} extra\n", install.as_str(), transaction.as_str())
                .into_bytes(),
            format!("HANE-BOOTSTRAP-1 {} {} not-a-nonce\n", install.as_str(), transaction.as_str()).into_bytes(),
            vec![b'a'; BOOTSTRAP_LINE_LIMIT + 8],
            // No terminating newline: the channel closed mid-request.
            bytes[..bytes.len() - 1].to_vec(),
        ];
        cases.push(b"\xff\xfe\n".to_vec());
        for case in cases {
            let err = read_bootstrap_request(&mut Cursor::new(case)).unwrap_err();
            assert!(!err.to_string().contains(&secret), "{err:?}");
        }
    }

    #[test]
    fn serve_bootstrap_creates_user_owned_state_only_after_checking_the_request() {
        let fx = protected();
        let transaction = TransactionId::generate().unwrap();
        let nonce = Nonce::generate().unwrap();

        // Another install: refused with nothing created.
        let other = request_bytes(&unique_install_id(), &transaction, &nonce);
        let mut ack = Vec::new();
        let err = serve_bootstrap(
            &mut Cursor::new(other),
            &mut ack,
            &fx.placement,
            &fx.source,
            &fx.source.path,
        )
        .err()
        .unwrap();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
        assert!(ack.is_empty());
        assert_eq!(fx.parent_entries(), 0);

        // The right request creates the directories and the copy, owned by the
        // user, and acknowledges with the single byte.
        let request = request_bytes(&fx.placement.install_id, &transaction, &nonce);
        let mut ack = Vec::new();
        let boot = serve_bootstrap(
            &mut Cursor::new(request.clone()),
            &mut ack,
            &fx.placement,
            &fx.source,
            &fx.source.path,
        )
        .unwrap();
        assert_eq!(ack, [BOOTSTRAP_ACK]);
        assert_eq!(boot.transaction, transaction);
        assert_eq!(boot.nonce.as_str(), nonce.as_str());
        let staged = boot
            .coordination
            .transaction_dir(&transaction)
            .join("helper")
            .join(crate::platform::STAGED_HELPER_FILE_NAME);
        assert_eq!(std::fs::read(&staged).unwrap(), INSTALLED_HELPER);
        crate::platform::acl::verify_user_owned(boot.coordination.dir(), true).unwrap();
        crate::platform::acl::verify_user_owned(&staged, false).unwrap();
        crate::platform::staged_helper(&boot.coordination, &transaction).unwrap();

        // The same transaction cannot be bootstrapped twice.
        let mut again = Vec::new();
        let err = serve_bootstrap(
            &mut Cursor::new(request),
            &mut again,
            &fx.placement,
            &fx.source,
            &fx.source.path,
        )
        .err()
        .unwrap();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);
        assert!(again.is_empty());
        assert!(staged.is_file(), "the first transaction is untouched");
    }

    #[test]
    fn serve_bootstrap_rolls_back_when_the_acknowledgement_cannot_be_sent() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let fx = protected();
        let transaction = TransactionId::generate().unwrap();
        let request = request_bytes(&fx.placement.install_id, &transaction, &Nonce::generate().unwrap());
        let err = serve_bootstrap(
            &mut Cursor::new(request),
            &mut BrokenWriter,
            &fx.placement,
            &fx.source,
            &fx.source.path,
        )
        .err()
        .unwrap();
        assert_eq!(err.code, ErrorCode::Io);
        let coordination = fx.coordination();
        assert!(!coordination.transaction_dir(&transaction).exists());
    }
}
