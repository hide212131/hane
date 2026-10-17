//! The limited-health child and the durable commit (Issue #510, U4).
//!
//! *Helper side.* After the owned files are replaced the helper releases its
//! exclusive `runtime` lease, keeps `transaction` and `startup`, and starts the
//! new `hane.exe` (re-verified against the current signed inventory) with piped
//! stdio only, no arguments and an empty environment. A one-time capability in
//! the owner-only transaction directory is the only thing that lets that
//! process into health mode; there is no flag or environment variable for it.
//! The helper then waits up to [`HEALTH_TIMEOUT`] for `Healthy` from *that*
//! child (nonce, installation, transaction, candidate, version and the OS
//! process id it spawned). Only a verified `Healthy` lets it move the journal to
//! `Committed` durably, and only afterwards does it send `Commit`. Every other
//! path stops the child, waits for the OS to confirm that it ended and that the
//! `runtime` lease is free, and then rolls back. A child that cannot be stopped
//! leaves `RecoveryRequired` instead of an overwrite.
//!
//! *Child side* ([`detect_child`], [`ChildSession`]). Called before the normal
//! startup gate. A normal launch finds no capability that names its parent as
//! the helper and therefore goes through the gate. The real child takes the
//! capability once, checks that the OS parent is the helper, takes the shared
//! `runtime` lease (the helper still holds `startup`, so no ordinary launch can
//! slip in), reports `Healthy` and waits for `Commit`. Until then it starts no
//! editor, AI, autosave, migration or open request. If the helper dies it may
//! continue only when the journal is durably `Committed`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::apply::{self, FileOps, HealthChild, HealthOutcome};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{durable_write, read_limited};
use crate::identity::{InstallId, TransactionId};
use crate::journal::{DurableJournal, FileJournalStore, JournalStore, Phase};
use crate::platform::lease::{Coordination, Lease, LeaseKind, PRIVATE_DIR};
use crate::platform::{self, acl};
use crate::protocol::{Body, Frame, Nonce, read_frame, write_frame};
use crate::startup::RuntimeGuard;
use crate::windows_apply::valid_candidate_id;

/// How long the helper waits for `Healthy` from the limited child.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(45);
/// How long a child that lost its helper keeps waiting for `Commit`.
pub const COMMIT_WAIT: Duration = Duration::from_secs(60);
const STOP_GRACE: Duration = Duration::from_secs(3);
const LEASE_RELEASE_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(25);

const CAPABILITY_FILE: &str = "health.capability";
const CAPABILITY_TAKEN: &str = "health.capability.taken";
const CAPABILITY_LIMIT: u64 = 512;
const MAX_VERSION_BYTES: usize = 64;

fn unexpected(what: &str) -> UpdateError {
    UpdateError::new(ErrorCode::TransitionInvalid, format!("unexpected {what}"))
}

// ------------------------------------------------------------------ link

enum Incoming {
    Frame(Box<Frame>),
    Closed,
    Failed(UpdateError),
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R) -> Result<Receiver<Incoming>> {
    let (sender, incoming) = mpsc::channel();
    std::thread::Builder::new()
        .name("hane-health-link".to_owned())
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
    Ok(incoming)
}

fn next_frame(incoming: &Receiver<Incoming>, nonce: &Nonce, timeout: Duration) -> Result<Frame> {
    match incoming.recv_timeout(timeout) {
        Ok(Incoming::Frame(frame)) => {
            frame.check(nonce)?;
            Ok(*frame)
        }
        Ok(Incoming::Closed) => Err(UpdateError::new(
            ErrorCode::Truncated,
            "the private health pipe was closed",
        )),
        Ok(Incoming::Failed(err)) => Err(err),
        Err(RecvTimeoutError::Timeout) => {
            Err(UpdateError::new(ErrorCode::Timeout, "no answer on the private health pipe in time"))
        }
        Err(RecvTimeoutError::Disconnected) => Err(UpdateError::new(
            ErrorCode::Truncated,
            "the private health pipe ended",
        )),
    }
}

/// What the helper requires of the child it spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildIdentity {
    pub install_id: InstallId,
    pub transaction: TransactionId,
    pub candidate_id: String,
    pub version: String,
    /// The process id the OS gave the spawned child.
    pub pid: u32,
}

/// The helper's end of the private pipes to its limited child.
pub struct ChildLink {
    writer: Option<Box<dyn Write + Send>>,
    incoming: Receiver<Incoming>,
    nonce: Nonce,
}

impl ChildLink {
    pub fn new<R: Read + Send + 'static>(
        reader: R,
        writer: Box<dyn Write + Send>,
        nonce: Nonce,
    ) -> Result<Self> {
        Ok(Self {
            writer: Some(writer),
            incoming: spawn_reader(reader)?,
            nonce,
        })
    }

    /// Requires exactly one valid `Healthy` that matches `expected` in every
    /// field. A wrong nonce, a foreign identifier, another process id, a
    /// malformed or cut frame, a silent child and any other message all fail.
    pub fn await_healthy(&mut self, expected: &ChildIdentity, timeout: Duration) -> Result<()> {
        let frame = next_frame(&self.incoming, &self.nonce, timeout)?;
        match frame.body {
            Body::Healthy {
                install_id,
                transaction_id,
                candidate_id,
                version,
                child_pid,
            } => {
                let same = install_id == expected.install_id.as_str()
                    && transaction_id == expected.transaction.as_str()
                    && candidate_id == expected.candidate_id
                    && version == expected.version
                    && child_pid == expected.pid;
                if same {
                    Ok(())
                } else {
                    Err(UpdateError::new(
                        ErrorCode::IdentityMismatch,
                        "Healthy does not come from the expected child, transaction and version",
                    ))
                }
            }
            _ => Err(unexpected("message while waiting for Healthy")),
        }
    }

    pub fn send_commit(&mut self, transaction: &TransactionId) -> Result<()> {
        let writer = self.writer.as_mut().ok_or_else(|| {
            UpdateError::new(ErrorCode::Truncated, "the child pipe is already closed")
        })?;
        write_frame(
            writer,
            &Frame::new(
                &self.nonce,
                Body::Commit {
                    transaction_id: transaction.as_str().to_owned(),
                },
            ),
        )
    }

    /// Closes the helper's write end; the child reads end-of-stream.
    pub fn close(&mut self) {
        self.writer = None;
    }
}

// --------------------------------------------------------- child process

/// The spawned child as the OS reports it.
pub trait ChildProcess {
    fn id(&self) -> u32;
    /// `true` once the OS reports the process has ended.
    fn has_exited(&mut self) -> io::Result<bool>;
    fn kill(&mut self) -> io::Result<()>;
    /// Blocks until the OS confirms the process has ended.
    fn wait(&mut self) -> io::Result<()>;
}

impl ChildProcess for Child {
    fn id(&self) -> u32 {
        Child::id(self)
    }

    fn has_exited(&mut self) -> io::Result<bool> {
        Ok(self.try_wait()?.is_some())
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn wait(&mut self) -> io::Result<()> {
        Child::wait(self).map(|_| ())
    }
}

/// The limited child as the commit/rollback orchestration sees it.
pub trait LimitedChild {
    /// `Ok` only for a verified `Healthy` from this transaction's real child.
    fn await_healthy(&mut self, timeout: Duration) -> Result<()>;
    /// Called only after `Committed` is durable.
    fn send_commit(&mut self) -> Result<()>;
    fn as_health_child(&mut self) -> &mut dyn HealthChild;
}

#[derive(Debug, Clone, Copy)]
pub struct StopTiming {
    /// How long a child gets to end after its pipe closes before it is killed.
    pub grace: Duration,
    /// How long the `runtime` lease may stay held after the child ended.
    pub lease_wait: Duration,
}

impl Default for StopTiming {
    fn default() -> Self {
        Self {
            grace: STOP_GRACE,
            lease_wait: LEASE_RELEASE_WAIT,
        }
    }
}

/// A limited child owned by the helper, or the record that none could be
/// started. Stopping it is the only way to a rollback: the child must have
/// ended (per the OS) and the `runtime` lease must be free, which the helper
/// then holds exclusively until it is done.
pub struct ProcessChild<P: ChildProcess> {
    process: Option<P>,
    link: Option<ChildLink>,
    launch_error: Option<UpdateError>,
    identity: ChildIdentity,
    coordination: Coordination,
    reacquired: Option<Lease>,
    timing: StopTiming,
}

impl<P: ChildProcess> ProcessChild<P> {
    pub fn new(
        process: P,
        link: ChildLink,
        identity: ChildIdentity,
        coordination: Coordination,
        timing: StopTiming,
    ) -> Self {
        Self {
            process: Some(process),
            link: Some(link),
            launch_error: None,
            identity,
            coordination,
            reacquired: None,
            timing,
        }
    }

    /// No child exists. It can never be healthy, and stopping it only needs
    /// the `runtime` lease to be free again.
    pub fn failed(
        error: UpdateError,
        identity: ChildIdentity,
        coordination: Coordination,
        timing: StopTiming,
    ) -> Self {
        Self {
            process: None,
            link: None,
            launch_error: Some(error),
            identity,
            coordination,
            reacquired: None,
            timing,
        }
    }

    /// The exclusive `runtime` lease taken back after the child ended.
    pub fn holds_runtime(&self) -> bool {
        self.reacquired.is_some()
    }

    fn end_process(&mut self) -> Result<()> {
        let grace = self.timing.grace;
        let Some(process) = self.process.as_mut() else {
            return Ok(());
        };
        let started = Instant::now();
        let mut exited = false;
        while started.elapsed() < grace {
            if process.has_exited()? {
                exited = true;
                break;
            }
            std::thread::sleep(POLL);
        }
        if !exited {
            if let Err(err) = process.kill()
                && !process.has_exited()?
            {
                return Err(UpdateError::new(
                    ErrorCode::Io,
                    format!("the health child could not be terminated: {err}"),
                ));
            }
            process.wait()?;
        }
        self.process = None;
        Ok(())
    }
}

impl<P: ChildProcess> HealthChild for ProcessChild<P> {
    fn stop_and_release_lease(&mut self) -> Result<()> {
        if let Some(link) = self.link.as_mut() {
            link.close();
        }
        self.end_process()?;
        if self.reacquired.is_some() {
            return Ok(());
        }
        let started = Instant::now();
        loop {
            if let Some(lease) = self.coordination.try_acquire(LeaseKind::RuntimeExclusive, &[])? {
                self.reacquired = Some(lease);
                return Ok(());
            }
            if started.elapsed() >= self.timing.lease_wait {
                return Err(UpdateError::new(
                    ErrorCode::Timeout,
                    "the runtime lease is still held after the health child ended",
                ));
            }
            std::thread::sleep(POLL);
        }
    }
}

impl<P: ChildProcess> LimitedChild for ProcessChild<P> {
    fn await_healthy(&mut self, timeout: Duration) -> Result<()> {
        if let Some(err) = &self.launch_error {
            return Err(err.clone());
        }
        let link = self.link.as_mut().ok_or_else(|| {
            UpdateError::new(ErrorCode::Truncated, "the health child has no pipe")
        })?;
        link.await_healthy(&self.identity, timeout)
    }

    fn send_commit(&mut self) -> Result<()> {
        let link = self.link.as_mut().ok_or_else(|| {
            UpdateError::new(ErrorCode::Truncated, "the health child has no pipe")
        })?;
        link.send_commit(&self.identity.transaction)
    }

    fn as_health_child(&mut self) -> &mut dyn HealthChild {
        self
    }
}

impl<P: ChildProcess> Drop for ProcessChild<P> {
    fn drop(&mut self) {
        // Never leave the helper's own child running behind it.
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

// ------------------------------------------------------------ the launch

/// Everything the helper derived about the child it is about to start.
pub struct LaunchRequest<'a> {
    pub exe: &'a Path,
    pub version: &'a str,
    pub candidate_id: &'a str,
    pub coordination: &'a Coordination,
    pub transaction: &'a TransactionId,
}

/// Starts the limited child. Both methods return a child; a start that failed
/// is a child that is never healthy.
pub trait HealthLauncher {
    /// Releases the `runtime` lease and starts the child.
    fn launch(&mut self, request: &LaunchRequest<'_>) -> Box<dyn LimitedChild>;
    /// The executable could not be verified, so nothing was started.
    fn failed(&mut self, request_error: UpdateError, request: &LaunchFailure<'_>) -> Box<dyn LimitedChild>;
    fn health_timeout(&self) -> Duration {
        HEALTH_TIMEOUT
    }
}

/// What a launcher needs to stand in for a child that never started.
pub struct LaunchFailure<'a> {
    pub candidate_id: &'a str,
    pub coordination: &'a Coordination,
    pub transaction: &'a TransactionId,
}

/// The real launcher: it spawns the verified `hane.exe` as a limited child.
/// `runtime` is the helper's exclusive `runtime` lease, released just before
/// the child starts so that the child can take the shared one.
pub struct ProcessLauncher<'a> {
    pub runtime: &'a mut Option<Lease>,
    pub timing: StopTiming,
}

fn child_command(exe: &Path) -> Command {
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
    command
}

fn identity_for(request: &LaunchFailure<'_>, version: &str, pid: u32) -> ChildIdentity {
    ChildIdentity {
        install_id: request.coordination.install_id().clone(),
        transaction: request.transaction.clone(),
        candidate_id: request.candidate_id.to_owned(),
        version: version.to_owned(),
        pid,
    }
}

impl HealthLauncher for ProcessLauncher<'_> {
    fn launch(&mut self, request: &LaunchRequest<'_>) -> Box<dyn LimitedChild> {
        let failure = LaunchFailure {
            candidate_id: request.candidate_id,
            coordination: request.coordination,
            transaction: request.transaction,
        };
        // The child needs the shared lease; the helper keeps transaction and
        // startup. If the spawn fails, the lease is simply taken back when the
        // failed child is stopped.
        if let Some(lease) = self.runtime.take() {
            lease.release();
        }
        match spawn_limited(request) {
            Ok((process, link, pid)) => Box::new(ProcessChild::new(
                process,
                link,
                identity_for(&failure, request.version, pid),
                request.coordination.clone(),
                self.timing,
            )),
            Err(err) => {
                discard_capability(request.coordination, request.transaction);
                self.failed(err, &failure)
            }
        }
    }

    fn failed(&mut self, request_error: UpdateError, request: &LaunchFailure<'_>) -> Box<dyn LimitedChild> {
        if let Some(lease) = self.runtime.take() {
            lease.release();
        }
        Box::new(ProcessChild::<Child>::failed(
            request_error,
            identity_for(request, "", 0),
            request.coordination.clone(),
            self.timing,
        ))
    }
}

fn spawn_limited(request: &LaunchRequest<'_>) -> Result<(Child, ChildLink, u32)> {
    let nonce = Nonce::generate()?;
    let capability = Capability {
        nonce: nonce.clone(),
        helper_pid: std::process::id(),
        install_id: request.coordination.install_id().clone(),
        transaction: request.transaction.clone(),
        candidate_id: request.candidate_id.to_owned(),
        version: request.version.to_owned(),
    };
    publish_capability(request.coordination, request.transaction, &capability)?;
    let mut child = match child_command(request.exe).spawn() {
        Ok(child) => child,
        Err(err) => {
            discard_capability(request.coordination, request.transaction);
            return Err(err.into());
        }
    };
    let pid = child.id();
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        discard_capability(request.coordination, request.transaction);
        return Err(UpdateError::new(ErrorCode::Io, "the health child pipes are unavailable"));
    };
    match ChildLink::new(stdout, Box::new(stdin), nonce) {
        Ok(link) => Ok((child, link, pid)),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            discard_capability(request.coordination, request.transaction);
            Err(err)
        }
    }
}

// ----------------------------------------------------------- the verdict

/// How the helper's health step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub outcome: HealthOutcome,
    /// Why the update was not kept, when it was not.
    pub reason: Option<UpdateError>,
}

/// Waits for the child's verdict, then commits or rolls back.
///
/// `Commit` is sent only after the journal reports `Committed` durably. If that
/// write fails, or the child is unhealthy, the child is stopped (and the OS
/// confirms it ended and the lease is free) before any file is restored.
pub fn conclude_with_child<S: JournalStore, O: FileOps>(
    journal: &mut DurableJournal<S>,
    ops: &mut O,
    child: &mut dyn LimitedChild,
    timeout: Duration,
) -> Result<Settled> {
    let verdict = child.await_healthy(timeout);
    let healthy = verdict.is_ok();
    let outcome = apply::conclude_health(journal, ops, child.as_health_child(), healthy)?;
    let mut reason = verdict.err();
    if outcome == HealthOutcome::Committed {
        // Durable first. A failed notification leaves the child to the
        // journal check of `ChildSession`; the update itself is committed.
        let _ = child.send_commit();
    } else if reason.is_none() {
        reason = Some(UpdateError::new(
            ErrorCode::Io,
            "the commit could not be made durable",
        ));
    }
    Ok(Settled { outcome, reason })
}

// ------------------------------------------------------------ capability

/// The one-time capability that admits a process to health mode.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Capability {
    nonce: Nonce,
    helper_pid: u32,
    install_id: InstallId,
    transaction: TransactionId,
    candidate_id: String,
    version: String,
}

impl Capability {
    fn encode(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n",
            self.nonce.as_str(),
            self.helper_pid,
            self.install_id.as_str(),
            self.transaction.as_str(),
            self.candidate_id,
            self.version
        )
    }

    fn parse(text: &str) -> Result<Self> {
        let bad = || UpdateError::new(ErrorCode::IdentityMismatch, "malformed health capability");
        let mut lines = text.lines();
        let (Some(nonce), Some(pid), Some(install), Some(txn), Some(candidate), Some(version), None) = (
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
            lines.next(),
        ) else {
            return Err(bad());
        };
        if !valid_candidate_id(candidate) || version.is_empty() || version.len() > MAX_VERSION_BYTES {
            return Err(bad());
        }
        Ok(Self {
            nonce: Nonce::parse(nonce)?,
            helper_pid: pid.parse().map_err(|_| bad())?,
            install_id: InstallId::parse(install)?,
            transaction: TransactionId::parse(txn)?,
            candidate_id: candidate.to_owned(),
            version: version.to_owned(),
        })
    }
}

fn capability_path(coordination: &Coordination, transaction: &TransactionId) -> PathBuf {
    coordination.transaction_dir(transaction).join(CAPABILITY_FILE)
}

fn publish_capability(
    coordination: &Coordination,
    transaction: &TransactionId,
    capability: &Capability,
) -> Result<()> {
    discard_capability(coordination, transaction);
    let path = capability_path(coordination, transaction);
    durable_write(&path, capability.encode().as_bytes())?;
    if let Err(err) = acl::secure_new(&path, false, 0o600).and_then(|()| acl::verify(&path, false)) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok(())
}

/// Removes an unconsumed capability.
pub fn discard_capability(coordination: &Coordination, transaction: &TransactionId) {
    let dir = coordination.transaction_dir(transaction);
    let _ = fs::remove_file(dir.join(CAPABILITY_FILE));
    let _ = fs::remove_file(dir.join(CAPABILITY_TAKEN));
}

// ------------------------------------------------------------ child side

pub enum ChildEntry {
    /// An ordinary launch: run the startup gate.
    NotChild,
    /// The real limited child of a helper.
    Child(Box<ChildSession>),
    /// This was meant to be the limited child but cannot be. Never start.
    Refused(String),
}

/// A process admitted to health mode. It holds the shared `runtime` lease.
pub struct ChildSession {
    lease: Lease,
    capability: Capability,
    coordination: Coordination,
}

/// Decides whether this process is a helper's limited child. Only a process
/// whose OS parent is the helper named by a capability in the owner-only
/// transaction directory, while that helper holds the `transaction` lease, can
/// be one; everything else is an ordinary launch and meets the startup gate.
pub fn detect_child(exe: &Path, own_version: &str) -> ChildEntry {
    let Ok(placement) = platform::locate(exe) else {
        return ChildEntry::NotChild;
    };
    let Ok(Some(coordination)) = placement.existing_coordination() else {
        return ChildEntry::NotChild;
    };
    detect_in(&coordination, own_version, crate::helper::os_parent_id())
}

fn detect_in(coordination: &Coordination, own_version: &str, parent: Option<u32>) -> ChildEntry {
    let private = coordination.dir().join(PRIVATE_DIR);
    let Ok(entries) = fs::read_dir(&private) else {
        return ChildEntry::NotChild;
    };
    let mut found: Vec<TransactionId> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(transaction) = TransactionId::parse(&name) else {
            continue;
        };
        if fs::symlink_metadata(entry.path().join(CAPABILITY_FILE)).is_ok() {
            found.push(transaction);
        }
    }
    let (Some(transaction), 1) = (found.first().cloned(), found.len()) else {
        return ChildEntry::NotChild;
    };
    let path = capability_path(coordination, &transaction);
    let Ok(text) = read_capability(&path) else {
        return ChildEntry::NotChild;
    };
    let Ok(capability) = Capability::parse(&text) else {
        return ChildEntry::NotChild;
    };
    if capability.transaction != transaction
        || &capability.install_id != coordination.install_id()
        || parent != Some(capability.helper_pid)
    {
        return ChildEntry::NotChild;
    }
    // The helper holds `transaction` for as long as it can answer. A free
    // lease means a stale capability: drop it and let the gate decide.
    match coordination.try_acquire(LeaseKind::Transaction, &[]) {
        Ok(None) => {}
        Ok(Some(lease)) => {
            drop(lease);
            discard_capability(coordination, &transaction);
            return ChildEntry::NotChild;
        }
        Err(_) => return ChildEntry::NotChild,
    }
    if capability.version != own_version {
        return ChildEntry::Refused(
            "the started version is not the version the update installed".to_owned(),
        );
    }
    if take_capability(coordination, &transaction, &text).is_err() {
        return ChildEntry::Refused("the health capability could not be taken".to_owned());
    }
    match coordination.try_acquire(LeaseKind::RuntimeShared, &[]) {
        Ok(Some(lease)) => ChildEntry::Child(Box::new(ChildSession {
            lease,
            capability,
            coordination: coordination.clone(),
        })),
        Ok(None) => ChildEntry::Refused("the runtime lease is not available".to_owned()),
        Err(err) => ChildEntry::Refused(crate::protocol::bounded_reason(&err.to_string())),
    }
}

fn read_capability(path: &Path) -> Result<String> {
    acl::verify(path, false)?;
    let raw = read_limited(path, CAPABILITY_LIMIT)?
        .ok_or_else(|| UpdateError::new(ErrorCode::NotFound, "the health capability vanished"))?;
    String::from_utf8(raw)
        .map_err(|_| UpdateError::new(ErrorCode::IdentityMismatch, "malformed health capability"))
}

/// Consumes the capability exactly once: the rename is atomic, so only one
/// process can win it, and what was won must be what was checked.
fn take_capability(coordination: &Coordination, transaction: &TransactionId, expected: &str) -> Result<()> {
    let dir = coordination.transaction_dir(transaction);
    let taken = dir.join(CAPABILITY_TAKEN);
    fs::rename(dir.join(CAPABILITY_FILE), &taken)?;
    let outcome = read_capability(&taken).and_then(|text| {
        if text == expected {
            Ok(())
        } else {
            Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the health capability changed while it was taken",
            ))
        }
    });
    let _ = fs::remove_file(&taken);
    outcome
}

impl ChildSession {
    pub fn transaction(&self) -> &TransactionId {
        &self.capability.transaction
    }

    /// Reports `Healthy` after `check` (read-only inspection of the settings
    /// and the restart ticket; it must not write anything) and waits for
    /// `Commit`. The returned guard keeps the shared `runtime` lease; normal
    /// startup may follow only if this returns `Ok`.
    pub fn run<R, W>(
        self,
        reader: R,
        mut writer: W,
        wait: Duration,
        check: impl FnOnce() -> Result<()>,
    ) -> Result<RuntimeGuard>
    where
        R: Read + Send + 'static,
        W: Write,
    {
        check()?;
        let incoming = spawn_reader(reader)?;
        let cap = &self.capability;
        let sent = write_frame(
            &mut writer,
            &Frame::new(
                &cap.nonce,
                Body::Healthy {
                    install_id: cap.install_id.as_str().to_owned(),
                    transaction_id: cap.transaction.as_str().to_owned(),
                    candidate_id: cap.candidate_id.clone(),
                    version: cap.version.clone(),
                    child_pid: std::process::id(),
                },
            ),
        );
        let answer = sent.and_then(|()| next_frame(&incoming, &cap.nonce, wait));
        let proceed = match answer {
            Ok(Frame {
                body: Body::Commit { transaction_id },
                ..
            }) => transaction_id == cap.transaction.as_str(),
            // A helper that died after committing is recognized by the journal.
            _ => self.committed_durably(),
        };
        if proceed {
            Ok(RuntimeGuard::new(self.lease))
        } else {
            Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "the update was not committed",
            ))
        }
    }

    fn committed_durably(&self) -> bool {
        let store = FileJournalStore::new(self.coordination.journal_path(&self.capability.transaction));
        matches!(
            store.load(),
            Ok(Some(journal))
                if journal.phase == Phase::Committed
                    && journal.transaction_id == self.capability.transaction
                    && &journal.install_id == self.coordination.install_id()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Cursor;
    use std::rc::Rc;

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

    fn candidate() -> String {
        "c".repeat(64)
    }

    fn identity(transaction: &TransactionId) -> ChildIdentity {
        ChildIdentity {
            install_id: install_id(),
            transaction: transaction.clone(),
            candidate_id: candidate(),
            version: "0.26.0".to_owned(),
            pid: 77,
        }
    }

    fn healthy(nonce: &Nonce, identity: &ChildIdentity) -> Frame {
        Frame::new(
            nonce,
            Body::Healthy {
                install_id: identity.install_id.as_str().to_owned(),
                transaction_id: identity.transaction.as_str().to_owned(),
                candidate_id: identity.candidate_id.clone(),
                version: identity.version.clone(),
                child_pid: identity.pid,
            },
        )
    }

    fn wire(frames: &[Frame]) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        for frame in frames {
            write_frame(&mut bytes, frame).unwrap();
        }
        Cursor::new(bytes)
    }

    fn link(input: Cursor<Vec<u8>>, nonce: &Nonce) -> ChildLink {
        ChildLink::new(input, Box::new(io::sink()), nonce.clone()).unwrap()
    }

    #[test]
    fn only_a_matching_healthy_from_the_spawned_child_is_accepted() {
        let txn = TransactionId::generate().unwrap();
        let expected = identity(&txn);
        let nonce = Nonce::generate().unwrap();
        let wait = Duration::from_secs(5);

        assert!(link(wire(&[healthy(&nonce, &expected)]), &nonce)
            .await_healthy(&expected, wait)
            .is_ok());

        let mut wrong_pid = expected.clone();
        wrong_pid.pid = 78;
        let mut wrong_txn = expected.clone();
        wrong_txn.transaction = TransactionId::generate().unwrap();
        let mut wrong_install = expected.clone();
        wrong_install.install_id = InstallId::parse(&"a".repeat(32)).unwrap();
        let mut wrong_candidate = expected.clone();
        wrong_candidate.candidate_id = "d".repeat(64);
        let mut wrong_version = expected.clone();
        wrong_version.version = "0.25.0".to_owned();
        for claimed in [wrong_pid, wrong_txn, wrong_install, wrong_candidate, wrong_version] {
            let err = link(wire(&[healthy(&nonce, &claimed)]), &nonce)
                .await_healthy(&expected, wait)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::IdentityMismatch, "{claimed:?}");
        }

        // A frame under another session's nonce is never accepted.
        let other = Nonce::generate().unwrap();
        let err = link(wire(&[healthy(&other, &expected)]), &nonce)
            .await_healthy(&expected, wait)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IdentityMismatch);

        // Any other message is not health.
        let err = link(wire(&[Frame::new(&nonce, Body::Abort)]), &nonce)
            .await_healthy(&expected, wait)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::TransitionInvalid);
    }

    #[test]
    fn a_cut_malformed_or_silent_pipe_is_not_health() {
        let txn = TransactionId::generate().unwrap();
        let expected = identity(&txn);
        let nonce = Nonce::generate().unwrap();
        let wait = Duration::from_secs(5);

        let err = link(Cursor::new(Vec::new()), &nonce)
            .await_healthy(&expected, wait)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Truncated);

        let mut cut = wire(&[healthy(&nonce, &expected)]).into_inner();
        cut.truncate(cut.len() - 3);
        let err = link(Cursor::new(cut), &nonce)
            .await_healthy(&expected, wait)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Truncated);

        let garbage = b"{\"protocol\":1,\"nonce\":\"x\",\"body\":\"Nope\"}";
        let mut malformed = (garbage.len() as u32).to_le_bytes().to_vec();
        malformed.extend_from_slice(garbage);
        assert!(link(Cursor::new(malformed), &nonce)
            .await_healthy(&expected, wait)
            .is_err());

        // A pipe that stays open without a word times out.
        struct Silent;
        impl Read for Silent {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(400));
                Ok(0)
            }
        }
        let mut silent = ChildLink::new(Silent, Box::new(io::sink()), nonce.clone()).unwrap();
        let err = silent.await_healthy(&expected, Duration::from_millis(30)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Timeout);
    }

    #[derive(Default)]
    struct Observed {
        killed: Cell<bool>,
        waited: Cell<bool>,
    }

    struct FakeProcess {
        seen: Rc<Observed>,
        /// Reports "still running" until killed.
        stubborn: bool,
        kill_fails: bool,
    }

    impl ChildProcess for FakeProcess {
        fn id(&self) -> u32 {
            77
        }

        fn has_exited(&mut self) -> io::Result<bool> {
            Ok(!self.stubborn || self.seen.killed.get())
        }

        fn kill(&mut self) -> io::Result<()> {
            if self.kill_fails {
                return Err(io::Error::other("denied"));
            }
            self.seen.killed.set(true);
            Ok(())
        }

        fn wait(&mut self) -> io::Result<()> {
            self.seen.waited.set(true);
            Ok(())
        }
    }

    struct Fixture {
        _dir: TempDir,
        coordination: Coordination,
        transaction: TransactionId,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("health");
        let coordination = Coordination::create(dir.path(), &install_id()).unwrap();
        let transaction = TransactionId::generate().unwrap();
        coordination.ensure_helper_dir(&transaction).unwrap();
        Fixture {
            _dir: dir,
            coordination,
            transaction,
        }
    }

    fn timing() -> StopTiming {
        StopTiming {
            grace: Duration::from_millis(40),
            lease_wait: Duration::from_millis(120),
        }
    }

    fn process_child(fx: &Fixture, process: FakeProcess) -> ProcessChild<FakeProcess> {
        let nonce = Nonce::generate().unwrap();
        ProcessChild::new(
            process,
            link(Cursor::new(Vec::new()), &nonce),
            identity(&fx.transaction),
            fx.coordination.clone(),
            timing(),
        )
    }

    #[test]
    fn a_child_is_stopped_and_the_lease_taken_back_before_rollback_may_start() {
        let fx = fixture();
        let seen = Rc::new(Observed::default());
        let mut child = process_child(
            &fx,
            FakeProcess {
                seen: Rc::clone(&seen),
                stubborn: true,
                kill_fails: false,
            },
        );
        child.stop_and_release_lease().unwrap();
        assert!(seen.killed.get() && seen.waited.get());
        assert!(child.holds_runtime());
        // The helper now owns the runtime lease: nobody else can run.
        assert!(fx.coordination.try_acquire(LeaseKind::RuntimeShared, &[]).unwrap().is_none());
        // Stopping again is harmless.
        child.stop_and_release_lease().unwrap();
    }

    #[test]
    fn a_child_that_cannot_be_ended_or_still_holds_the_lease_is_not_released() {
        let fx = fixture();
        let seen = Rc::new(Observed::default());
        let mut unkillable = process_child(
            &fx,
            FakeProcess {
                seen: Rc::clone(&seen),
                stubborn: true,
                kill_fails: true,
            },
        );
        assert!(unkillable.stop_and_release_lease().is_err());
        assert!(!unkillable.holds_runtime());
        drop(unkillable);

        // The process ended, but something still holds the runtime lease.
        let held = fx
            .coordination
            .try_acquire(LeaseKind::RuntimeShared, &[])
            .unwrap()
            .unwrap();
        let mut leaking = process_child(
            &fx,
            FakeProcess {
                seen: Rc::new(Observed::default()),
                stubborn: false,
                kill_fails: false,
            },
        );
        let err = leaking.stop_and_release_lease().unwrap_err();
        assert_eq!(err.code, ErrorCode::Timeout);
        assert!(!leaking.holds_runtime());
        drop(held);
        leaking.stop_and_release_lease().unwrap();
        assert!(leaking.holds_runtime());
    }

    #[test]
    fn a_launch_that_never_started_is_never_healthy() {
        let fx = fixture();
        let mut child = ProcessChild::<FakeProcess>::failed(
            UpdateError::new(ErrorCode::Io, "spawn failed"),
            identity(&fx.transaction),
            fx.coordination.clone(),
            timing(),
        );
        assert_eq!(child.await_healthy(Duration::from_millis(10)).unwrap_err().code, ErrorCode::Io);
        child.stop_and_release_lease().unwrap();
        assert!(child.holds_runtime());
    }

    fn capability(fx: &Fixture, parent: u32) -> Capability {
        Capability {
            nonce: Nonce::generate().unwrap(),
            helper_pid: parent,
            install_id: install_id(),
            transaction: fx.transaction.clone(),
            candidate_id: candidate(),
            version: "0.26.0".to_owned(),
        }
    }

    #[test]
    fn the_capability_round_trips_and_rejects_malformed_text() {
        let fx = fixture();
        let cap = capability(&fx, 4242);
        let text = cap.encode();
        assert_eq!(Capability::parse(&text).unwrap(), cap);
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let with = |index: usize, value: &str| {
            let mut changed = lines.clone();
            changed[index] = value.to_owned();
            changed.join("\n") + "\n"
        };
        for bad in [
            String::new(),
            with(1, "pid"),
            with(4, "../escape"),
            with(5, ""),
            format!("{text}extra\n"),
        ] {
            assert!(Capability::parse(&bad).is_err(), "{bad:?}");
        }
    }

    fn hold_transaction(fx: &Fixture) -> Lease {
        fx.coordination
            .try_acquire(LeaseKind::Transaction, &[])
            .unwrap()
            .expect("the helper's transaction lease")
    }

    #[test]
    fn only_the_helpers_real_child_enters_health_mode() {
        let fx = fixture();
        let _helper = hold_transaction(&fx);
        publish_capability(&fx.coordination, &fx.transaction, &capability(&fx, 4242)).unwrap();

        // An ordinary launch has some other parent and goes through the gate;
        // it does not consume the capability.
        for parent in [None, Some(1), Some(4243)] {
            assert!(matches!(
                detect_in(&fx.coordination, "0.26.0", parent),
                ChildEntry::NotChild
            ));
        }
        assert!(capability_path(&fx.coordination, &fx.transaction).exists());

        // The right parent but another version is refused, not started.
        assert!(matches!(
            detect_in(&fx.coordination, "0.25.0", Some(4242)),
            ChildEntry::Refused(_)
        ));
        assert!(capability_path(&fx.coordination, &fx.transaction).exists());

        let ChildEntry::Child(session) = detect_in(&fx.coordination, "0.26.0", Some(4242)) else {
            panic!("the helper's child must be admitted");
        };
        assert_eq!(session.transaction(), &fx.transaction);
        // One time only, and the child holds the shared runtime lease.
        assert!(!capability_path(&fx.coordination, &fx.transaction).exists());
        assert!(matches!(
            detect_in(&fx.coordination, "0.26.0", Some(4242)),
            ChildEntry::NotChild
        ));
        assert!(
            fx.coordination
                .try_acquire(LeaseKind::RuntimeExclusive, &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_stale_capability_without_a_live_helper_is_dropped() {
        let fx = fixture();
        publish_capability(&fx.coordination, &fx.transaction, &capability(&fx, 4242)).unwrap();
        // Nobody holds the transaction lease, so no helper can answer.
        assert!(matches!(
            detect_in(&fx.coordination, "0.26.0", Some(4242)),
            ChildEntry::NotChild
        ));
        assert!(!capability_path(&fx.coordination, &fx.transaction).exists());
    }

    #[test]
    fn a_foreign_capability_is_ignored() {
        let fx = fixture();
        let _helper = hold_transaction(&fx);
        let mut other = capability(&fx, 4242);
        other.install_id = InstallId::parse(&"a".repeat(32)).unwrap();
        publish_capability(&fx.coordination, &fx.transaction, &other).unwrap();
        assert!(matches!(
            detect_in(&fx.coordination, "0.26.0", Some(4242)),
            ChildEntry::NotChild
        ));
        // No capability at all.
        discard_capability(&fx.coordination, &fx.transaction);
        assert!(matches!(
            detect_in(&fx.coordination, "0.26.0", Some(4242)),
            ChildEntry::NotChild
        ));
    }

    /// The child as admitted, with the helper's `transaction` lease it needs.
    fn admitted(fx: &Fixture) -> (ChildSession, Lease) {
        let helper = hold_transaction(fx);
        publish_capability(&fx.coordination, &fx.transaction, &capability(fx, 4242)).unwrap();
        match detect_in(&fx.coordination, "0.26.0", Some(4242)) {
            ChildEntry::Child(session) => (*session, helper),
            _ => panic!("the helper's child must be admitted"),
        }
    }

    #[test]
    fn the_child_continues_only_after_commit_or_a_durable_committed_journal() {
        let wait = Duration::from_secs(2);
        let nonce_of = |session: &ChildSession| session.capability.nonce.clone();

        // Commit for this transaction.
        let fx = fixture();
        let (session, _helper) = admitted(&fx);
        let nonce = nonce_of(&session);
        let commit = Frame::new(
            &nonce,
            Body::Commit {
                transaction_id: fx.transaction.as_str().to_owned(),
            },
        );
        let mut sent = Vec::new();
        let guard = session.run(wire(&[commit]), &mut sent, wait, || Ok(()));
        assert!(guard.is_ok());
        let first = read_frame(&mut sent.as_slice()).unwrap().unwrap();
        assert!(matches!(first.body, Body::Healthy { child_pid, .. } if child_pid == std::process::id()));

        // A Commit for another transaction, a wrong nonce, or a cut pipe with
        // no durable Committed all keep the child from starting.
        let fx = fixture();
        let (session, _helper) = admitted(&fx);
        let foreign = Frame::new(
            &nonce_of(&session),
            Body::Commit {
                transaction_id: TransactionId::generate().unwrap().as_str().to_owned(),
            },
        );
        assert!(session.run(wire(&[foreign]), io::sink(), wait, || Ok(())).is_err());

        let fx = fixture();
        let (session, _helper) = admitted(&fx);
        let forged = Frame::new(
            &Nonce::generate().unwrap(),
            Body::Commit {
                transaction_id: fx.transaction.as_str().to_owned(),
            },
        );
        assert!(session.run(wire(&[forged]), io::sink(), wait, || Ok(())).is_err());

        let fx = fixture();
        let (session, _helper) = admitted(&fx);
        assert!(session
            .run(Cursor::new(Vec::new()), io::sink(), wait, || Ok(()))
            .is_err());

        // A failed read-only check never reports health.
        let fx = fixture();
        let (session, _helper) = admitted(&fx);
        let mut sent = Vec::new();
        let err = session
            .run(Cursor::new(Vec::new()), &mut sent, wait, || {
                Err(UpdateError::new(ErrorCode::Io, "settings unreadable"))
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(sent.is_empty(), "no Healthy before the check passed");
    }

    #[test]
    fn a_dead_helper_is_survived_only_after_a_durable_commit() {
        use crate::identity::{ApplyPlan, FileIdentity, PlanStep, ReleaseIdentity};
        use crate::journal::UpdateJournal;

        let fx = fixture();
        let plan = ApplyPlan {
            install_id: install_id(),
            transaction_id: fx.transaction.clone(),
            source: ReleaseIdentity {
                target: "x86_64-pc-windows-msvc".to_owned(),
                version: "0.25.0".to_owned(),
                catalog_sha256: None,
                inventory_sha256: None,
                bundle_id: None,
                team_id: None,
            },
            destination: ReleaseIdentity {
                target: "x86_64-pc-windows-msvc".to_owned(),
                version: "0.26.0".to_owned(),
                catalog_sha256: None,
                inventory_sha256: None,
                bundle_id: None,
                team_id: None,
            },
            steps: vec![PlanStep {
                rel_path: "a.dll".to_owned(),
                old: Some(FileIdentity::of(b"old")),
                new: Some(FileIdentity::of(b"new")),
                backup_rel: Some("backup/a.dll".to_owned()),
            }],
            unchanged: Vec::new(),
        };
        let mut journal = DurableJournal::create(
            FileJournalStore::new(fx.coordination.journal_path(&fx.transaction)),
            UpdateJournal::begin(&plan, None).unwrap(),
        )
        .unwrap();
        for phase in [Phase::ExitAuthorized, Phase::Applying, Phase::AwaitingHealth] {
            journal.advance_phase(phase).unwrap();
        }
        let (session, _helper) = admitted(&fx);
        assert!(!session.committed_durably(), "AwaitingHealth is not a commit");
        journal.advance_phase(Phase::Committed).unwrap();
        assert!(session.committed_durably());
    }
}
