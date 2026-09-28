//! App Server process lifecycle: a single-flight coordinator thread that
//! owns start / stop / restart, finite lifecycle timeouts, generation-based
//! invalidation of late events, and the cross-process runtime owner lock.
//!
//! Per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md`, the
//! coordinator tracks two distinct, independently monotonic generations:
//!
//! - `generation` (the runtime/`Child` generation): bumped once per spawned
//!   `Child` (in `do_start`) and once per confirmed stop/crash cleanup (in
//!   `stop_active` / `reap_and_mark_failed`), so every spawn and every
//!   confirmed terminal transition gets its own fresh identity. This is
//!   what `ActiveChild::generation` is fixed to at spawn time, and what a
//!   `ChildEnded`/`RuntimeEvent` is tagged with.
//! - `operation_generation`: assigned once per *lifecycle operation*
//!   (`Start` / `Stop` / `Restart`, one value for the whole command even
//!   when, as in `Restart`, it drives more than one `Child` transition),
//!   the moment the coordinator dequeues it — distinct from, and unrelated
//!   in cadence to, `generation`. It identifies the operation itself, not
//!   whichever `Child` it happens to act on.
//!
//! A lifecycle timeout (e.g. the `initialize` handshake not completing
//! within `start_timeout`) cancels that operation: the coordinator
//! atomically records it as `Failed` (tagging the status with that
//! operation's own `operation_generation`) and replies to the waiting
//! caller — and every coalesced waiter — with that failure immediately,
//! instead of waiting for `Child` cleanup to actually confirm the process is
//! gone first. That cleanup (stop, escalate to a forced kill if needed, and
//! reap) still runs synchronously on the same coordinator thread right
//! afterward, without regressing the status it just set back through an
//! intermediate `Stopping` transition, so the next queued start/stop/restart
//! still cannot begin until it either confirms the process is gone or
//! reports `restart_blocked`. Because every later generation (of either
//! kind) gets a strictly greater number, a completion notification or
//! reader/monitor event arriving late from a timed-out or otherwise stale
//! generation — checked against both its `generation` and
//! `operation_generation` — can never be mistaken for, or revert, the
//! outcome of a subsequent operation.
//!
//! Every lifecycle command (`Start` / `Stop` / `Restart`) is handled to
//! completion by one dedicated coordinator thread before the next queued
//! command is processed, so there is never more than one `Child` spawn in
//! flight at a time. `Start` additionally short-circuits to the current
//! status once already `Ready`, and concurrent `start()` / `restart()`
//! callers are each coalesced client-side (see `AiRuntime::start` and
//! `AiRuntime::restart`) so that N concurrent calls of either kind only
//! spawn/initialize the child once per wave and all callers observe that
//! same single outcome — including when that one attempt fails — instead of
//! each one redoing its own full spawn/initialize (or stop/start) cycle. A
//! wave ends once its one attempt completes, so a later call always starts a
//! new wave and is free to try again. Regular RPC
//! calls (`AiRuntime::call`) do not go through that queue: they borrow a
//! handle to the current generation's transport and run concurrently with
//! each other, so a long-running call cannot block a `stop`/`restart`
//! request.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::mpsc::{self, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::owner_lock::{OwnerLock, OwnerLockGuard};
use crate::rpc::{RpcCore, RpcError, RpcEvent, RpcTransport, ServerRequestHandler};

/// Capacity of the internal bridge queue carrying [`RpcEvent`]s from a
/// child's reader/stderr threads to the forwarding thread that relays them
/// to the caller-supplied `events_tx`. Bounded so a slow consumer cannot
/// grow this queue without bound; the reader/stderr threads use a
/// non-blocking send so a full queue never stalls draining stdout/stderr.
const EVENTS_BRIDGE_CAPACITY: usize = 1024;

// The caller-supplied destination for `RuntimeEvent`s (`AiRuntime::spawn`'s
// `events_tx`) is itself a bounded `SyncSender`, matching the internal
// bridge above: the forwarding thread spawned in `do_start` relays each
// bridged `RpcEvent` with a non-blocking `try_send` rather than a blocking
// `send`, so a slow or absent caller-side consumer can never grow memory
// without bound or stall the forwarding thread. The policy on the two ways
// `try_send` can fail is explicit: a `Full` queue simply drops that one
// event (the caller is falling behind; a dropped diagnostic/notification is
// preferable to unbounded growth or blocking), and a `Disconnected`
// receiver stops the forwarding thread entirely (nobody will ever read from
// it again, so there is no point continuing to drain the bridge).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting,
    Initializing,
    Ready,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub state: RuntimeState,
    pub restart_blocked: bool,
    pub generation: u64,
    /// The lifecycle operation (`Start` / `Stop` / `Restart`) that produced
    /// this status, distinct from `generation`'s `Child`-spawn/cleanup
    /// identity. See the module documentation for how the two generations
    /// differ.
    pub operation_generation: u64,
}

/// An event surfaced to the runtime's caller, tagged with the lifecycle
/// generation it came from. Events from a generation older than the current
/// one (e.g. a straggling notification flushed while a newer generation is
/// already `Ready`) are recognizable as stale by comparing `generation`
/// against [`RuntimeStatus::generation`], instead of being indistinguishable
/// from current-generation events.
#[derive(Debug, Clone)]
pub struct RuntimeEvent {
    pub generation: u64,
    pub kind: RuntimeEventKind,
}

#[derive(Debug, Clone)]
pub enum RuntimeEventKind {
    Notification { method: String, params: Option<Value> },
    Diagnostic(String),
}

#[derive(Debug, Clone)]
pub enum RuntimeError {
    NotReady,
    RestartBlocked,
    OwnerLockUnavailable,
    /// An externally acquired [`OwnerLockGuard`] (via
    /// [`AiRuntime::start_with_owner_lock`]) was acquired against a
    /// different path than this runtime's own `RuntimeConfig::owner_lock_path`:
    /// it proves ownership of a *different* runtime owner lock, not the one
    /// this runtime is scoped to. Rejected before any child is spawned.
    OwnerLockPathMismatch,
    OwnerLock(Arc<io::Error>),
    Spawn(Arc<io::Error>),
    Handshake(RpcError),
    Rpc(RpcError),
    CoordinatorUnavailable,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::NotReady => write!(f, "AI runtime is not ready"),
            RuntimeError::RestartBlocked => write!(
                f,
                "AI runtime restart is blocked until a stale process is confirmed gone"
            ),
            RuntimeError::OwnerLockUnavailable => {
                write!(f, "AI runtime is owned by another Hane process")
            }
            RuntimeError::OwnerLockPathMismatch => {
                write!(f, "the supplied runtime owner lock guard does not belong to this runtime")
            }
            RuntimeError::OwnerLock(e) => write!(f, "failed to acquire runtime owner lock: {e}"),
            RuntimeError::Spawn(e) => write!(f, "failed to start the App Server process: {e}"),
            RuntimeError::Handshake(e) => write!(f, "initialize handshake failed: {e}"),
            RuntimeError::Rpc(e) => write!(f, "request failed: {e}"),
            RuntimeError::CoordinatorUnavailable => {
                write!(f, "AI runtime coordinator is unavailable")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Configuration for one embedded App Server runtime. `binary_path` must be
/// an absolute, explicit path to the bundled standalone executable; it is
/// never resolved via `PATH` (`spawn_child` rejects a non-absolute path
/// outright rather than letting `std::process::Command` fall back to a
/// `PATH` lookup). The standalone App Server binary is invoked directly with
/// `--listen stdio://`: it has no `app-server` subcommand of its own.
///
/// `extra_env` can carry a Custom Provider API key (see `crate::provider`)
/// for injection into this child only. `Debug` is implemented manually
/// rather than derived so that a stray `{:?}` on this config (logs,
/// diagnostics, panics) can never print a secret value placed there;
/// `extra_env` keys are shown, values are not.
#[derive(Clone)]
pub struct RuntimeConfig {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub codex_home: Option<PathBuf>,
    pub extra_env: Vec<(String, String)>,
    pub owner_lock_path: PathBuf,
    pub start_timeout: Duration,
    pub stop_grace_timeout: Duration,
    pub stop_force_timeout: Duration,
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_env: Vec<(&str, &str)> =
            self.extra_env.iter().map(|(k, _)| (k.as_str(), "<redacted>")).collect();
        f.debug_struct("RuntimeConfig")
            .field("binary_path", &self.binary_path)
            .field("args", &self.args)
            .field("codex_home", &self.codex_home)
            .field("extra_env", &redacted_env)
            .field("owner_lock_path", &self.owner_lock_path)
            .field("start_timeout", &self.start_timeout)
            .field("stop_grace_timeout", &self.stop_grace_timeout)
            .field("stop_force_timeout", &self.stop_force_timeout)
            .finish()
    }
}

impl RuntimeConfig {
    pub fn new(binary_path: impl Into<PathBuf>, owner_lock_path: impl Into<PathBuf>) -> Self {
        RuntimeConfig {
            binary_path: binary_path.into(),
            args: vec!["--listen".to_string(), "stdio://".to_string()],
            codex_home: None,
            extra_env: Vec::new(),
            owner_lock_path: owner_lock_path.into(),
            start_timeout: Duration::from_secs(20),
            stop_grace_timeout: Duration::from_secs(5),
            stop_force_timeout: Duration::from_secs(5),
        }
    }

    fn spawn_child(&self) -> io::Result<Child> {
        if !self.binary_path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "AI runtime binary_path must be an absolute path, got {:?}; refusing to fall back to a PATH lookup",
                    self.binary_path
                ),
            ));
        }
        let mut cmd = StdCommand::new(&self.binary_path);
        cmd.args(&self.args);
        if let Some(home) = &self.codex_home {
            cmd.env("CODEX_HOME", home);
        }
        cmd.envs(self.extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.spawn()
    }
}

struct SharedState {
    status: RuntimeState,
    restart_blocked: bool,
    generation: u64,
    operation_generation: u64,
    /// The `settings_generation` the `RuntimeConfig` *currently applied* to
    /// this coordinator was built for. Set at construction (`spawn`/
    /// `spawn_with_configured_settings_generation`) and updated only when a
    /// `reconfigure` actually swaps `config` on the coordinator thread —
    /// never by anything a caller separately claims per RPC call. This is
    /// what [`AiRuntime::configured_settings_generation`] reads, so a
    /// generation-gated caller (see `with_generation_checked_lock` in
    /// `crate::connect`) can bind its check to what this runtime has
    /// actually applied, not only to its own bookkeeping.
    config_generation: u64,
    ready_transport: Option<Arc<RpcCore>>,
    /// When `Some`, a `start()` is already in flight and this holds the
    /// reply channels of every additional concurrent caller that attached to
    /// it instead of enqueueing its own redundant `Start` command. The
    /// leader (the caller who found this `None` and set it) fans the single
    /// outcome out to every attached waiter once the one underlying
    /// spawn/initialize attempt completes, success or failure, so a failing
    /// attempt is never repeated within the same wave; a later `start()`
    /// call, arriving once this has been reset to `None`, starts a new wave
    /// and is free to try again.
    start_inflight: Option<Vec<Sender<Result<RuntimeStatus, RuntimeError>>>>,
    /// Same coalescing as `start_inflight`, for `restart()`.
    restart_inflight: Option<Vec<Sender<Result<RuntimeStatus, RuntimeError>>>>,
}

struct ActiveChild {
    generation: u64,
    operation_generation: u64,
    child: Child,
    transport: RpcTransport,
}

enum LifecycleCommand {
    Start,
    /// Starts the runtime using an owner lock guard the caller already
    /// acquired itself (e.g. to guard an out-of-runtime `AiSettings` save
    /// per ADR-0032 section 8), carrying that exact same guard over to this
    /// coordinator instead of releasing it and letting `do_start` acquire a
    /// fresh one. See [`AiRuntime::start_with_owner_lock`].
    StartWithOwnerLock(OwnerLockGuard),
    Stop,
    Restart,
    /// Switches the coordinator's own `RuntimeConfig` to a new one (e.g. a
    /// fresh Custom Provider Base URL/API key/model after a
    /// `settings_generation`-affecting save) and restarts against it, all on
    /// this same coordinator thread. The `u64` is the `settings_generation`
    /// the new `RuntimeConfig` was built for; it becomes this coordinator's
    /// `config_generation` as soon as the swap happens, before the restart
    /// itself is even attempted. See [`AiRuntime::reconfigure`].
    Reconfigure(RuntimeConfig, u64),
}

/// A boxed callback carrying the owner lock guard this coordinator currently
/// holds (if any) onto the coordinator thread. See
/// [`AiRuntime::with_owner_lock`].
type OwnerLockCallback = Box<dyn FnOnce(Option<&OwnerLockGuard>) + Send>;

enum CoordinatorMessage {
    Lifecycle(LifecycleCommand, Sender<Result<RuntimeStatus, RuntimeError>>),
    ChildEnded { generation: u64, operation_generation: u64 },
    /// Runs `f` synchronously on the coordinator thread, passing it the
    /// owner lock guard this coordinator currently holds (if any). See
    /// [`AiRuntime::with_owner_lock`].
    WithOwnerLock(OwnerLockCallback),
    Shutdown,
}

pub struct AiRuntime {
    cmd_tx: Sender<CoordinatorMessage>,
    coordinator: Option<JoinHandle<()>>,
    shared: Arc<Mutex<SharedState>>,
}

impl AiRuntime {
    pub fn spawn(
        config: RuntimeConfig,
        handler: Arc<dyn ServerRequestHandler>,
        events_tx: SyncSender<RuntimeEvent>,
    ) -> AiRuntime {
        AiRuntime::spawn_with_configured_settings_generation(config, 0, handler, events_tx)
    }

    /// Same as [`Self::spawn`], but additionally records
    /// `configured_settings_generation` as the `settings_generation` this
    /// initial `RuntimeConfig` was built for, exactly as a later
    /// [`Self::reconfigure`] keeps updated on every subsequent config swap.
    /// See [`Self::configured_settings_generation`].
    pub fn spawn_with_configured_settings_generation(
        config: RuntimeConfig,
        configured_settings_generation: u64,
        handler: Arc<dyn ServerRequestHandler>,
        events_tx: SyncSender<RuntimeEvent>,
    ) -> AiRuntime {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let self_tx = cmd_tx.clone();
        let shared = Arc::new(Mutex::new(SharedState {
            status: RuntimeState::Stopped,
            restart_blocked: false,
            generation: 0,
            operation_generation: 0,
            config_generation: configured_settings_generation,
            ready_transport: None,
            start_inflight: None,
            restart_inflight: None,
        }));
        let shared_for_thread = shared.clone();
        let coordinator = thread::spawn(move || {
            run_coordinator(cmd_rx, self_tx, shared_for_thread, config, handler, events_tx);
        });
        AiRuntime {
            cmd_tx,
            coordinator: Some(coordinator),
            shared,
        }
    }

    fn send_lifecycle(&self, cmd: LifecycleCommand) -> Result<RuntimeStatus, RuntimeError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.cmd_tx
            .send(CoordinatorMessage::Lifecycle(cmd, reply_tx))
            .map_err(|_| RuntimeError::CoordinatorUnavailable)?;
        reply_rx.recv().map_err(|_| RuntimeError::CoordinatorUnavailable)?
    }

    /// Starts the runtime. Concurrent `start()` callers are coalesced: only
    /// the first ("leader") caller actually enqueues a `Start` lifecycle
    /// command; every other caller that arrives while that one is still in
    /// flight attaches to it and receives the exact same outcome once it
    /// completes, instead of each one independently spawning and
    /// initializing its own child. This holds even when the leader's attempt
    /// fails: every attached caller observes that same failure rather than
    /// the coordinator repeating the spawn/initialize attempt once per
    /// queued `Start` command.
    pub fn start(&self) -> Result<RuntimeStatus, RuntimeError> {
        let (tx, rx) = mpsc::channel();
        let is_leader = {
            let mut guard = self.shared.lock().unwrap();
            match &mut guard.start_inflight {
                Some(waiters) => {
                    waiters.push(tx);
                    false
                }
                None => {
                    guard.start_inflight = Some(Vec::new());
                    true
                }
            }
        };

        if !is_leader {
            return rx.recv().map_err(|_| RuntimeError::CoordinatorUnavailable)?;
        }

        let result = self.send_lifecycle(LifecycleCommand::Start);
        let waiters = {
            let mut guard = self.shared.lock().unwrap();
            guard.start_inflight.take().unwrap_or_default()
        };
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
        result
    }

    pub fn stop(&self) -> Result<RuntimeStatus, RuntimeError> {
        self.send_lifecycle(LifecycleCommand::Stop)
    }

    /// Restarts the runtime. Concurrent `restart()` callers are coalesced:
    /// only the first ("leader") caller actually enqueues a `Restart`
    /// lifecycle command; every other caller that arrives while that one is
    /// still in flight attaches to it and receives the exact same outcome
    /// once it completes, instead of each one independently stopping and
    /// respawning the child.
    pub fn restart(&self) -> Result<RuntimeStatus, RuntimeError> {
        let (tx, rx) = mpsc::channel();
        let is_leader = {
            let mut guard = self.shared.lock().unwrap();
            match &mut guard.restart_inflight {
                Some(waiters) => {
                    waiters.push(tx);
                    false
                }
                None => {
                    guard.restart_inflight = Some(Vec::new());
                    true
                }
            }
        };

        if !is_leader {
            return rx.recv().map_err(|_| RuntimeError::CoordinatorUnavailable)?;
        }

        let result = self.send_lifecycle(LifecycleCommand::Restart);
        let waiters = {
            let mut guard = self.shared.lock().unwrap();
            guard.restart_inflight.take().unwrap_or_default()
        };
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
        result
    }

    /// Switches to `new_config`: stops whatever child is currently active
    /// under the old config (if any) and starts a fresh one under
    /// `new_config`, entirely on the coordinator thread this `AiRuntime`
    /// already owns. Unlike `start`/`restart`, concurrent calls are *not*
    /// coalesced: each call carries its own `new_config`, and silently
    /// applying only one coalesced leader's config while discarding the
    /// others' would be wrong, so every call is queued and handled in turn.
    /// `new_configured_settings_generation` is the `settings_generation`
    /// `new_config` was built for; it becomes what
    /// [`Self::configured_settings_generation`] reports as soon as the
    /// coordinator swaps to `new_config`, before the restart itself is even
    /// attempted.
    ///
    /// This is the supported way to apply a `settings_generation`-affecting
    /// AI settings change (a new Base URL, API key, model or connection
    /// method) to a running embedded App Server. Do not drop this
    /// `AiRuntime` and construct a new one with a different `RuntimeConfig`
    /// instead: that would tear down the coordinator thread and briefly
    /// release the runtime owner lock entirely (see the module docs and
    /// `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section 8's
    /// "runtime owner lock → AI settings lock" ordering), opening a window
    /// for a different Hane process to become the new owner before this one
    /// restarts. `reconfigure` never releases the owner lock to a would-be
    /// new owner in between: unlike a plain `stop`/`restart`, the internal
    /// stop step keeps the very same `OwnerLockGuard` alive (it is never
    /// dropped, and no new one is ever acquired) all the way from before the
    /// old child stops through the new child reaching `Ready`, on this same
    /// coordinator thread, so no other process can ever observe the lock as
    /// free during a reconfigure.
    pub fn reconfigure(
        &self,
        new_config: RuntimeConfig,
        new_configured_settings_generation: u64,
    ) -> Result<RuntimeStatus, RuntimeError> {
        self.send_lifecycle(LifecycleCommand::Reconfigure(new_config, new_configured_settings_generation))
    }

    /// Returns the `settings_generation` the `RuntimeConfig` *currently
    /// applied* to this coordinator was built for — set at construction (see
    /// [`Self::spawn_with_configured_settings_generation`]) and updated only
    /// by [`Self::reconfigure`], independent of whatever a caller separately
    /// tracks per call. Updated as soon as `reconfigure` structurally swaps
    /// `config`, *before* the restart against it is even attempted, so this
    /// can report a generation the runtime is not actually `Ready` for yet
    /// (or ever, if that restart goes on to fail). A generation-gated caller
    /// must not treat this number alone as proof the runtime is actually
    /// serving it; see [`Self::ready_configured_settings_generation`], which
    /// `crate::connect::with_generation_checked_lock` uses instead for
    /// exactly that reason.
    pub fn configured_settings_generation(&self) -> u64 {
        self.shared.lock().unwrap().config_generation
    }

    /// Returns `Some(settings_generation)` for the `RuntimeConfig` currently
    /// applied to this coordinator, but only while this runtime is actually
    /// `Ready` for it -- i.e. a spawned/reconfigured `Child` has completed the
    /// `initialize` handshake, not merely that the coordinator has
    /// structurally swapped `config` (which [`Self::reconfigure`] does as
    /// soon as the old child stops, before the new one is even spawned; see
    /// the module docs). Returns `None` for every other state
    /// (`Stopped`/`Starting`/`Initializing`/`Stopping`/`Failed`), so a
    /// generation-gated caller (see `with_generation_checked_lock` in
    /// `crate::connect`) can require an actually-`Ready` runtime and a
    /// matching generation together, under one lock acquisition, instead of
    /// checking [`Self::configured_settings_generation`] in isolation (which
    /// can already report a new generation the coordinator has only
    /// structurally moved on to, while a restart against it is still in
    /// flight or has failed) and separately racing a `snapshot()` call
    /// against a concurrent state transition.
    pub fn ready_configured_settings_generation(&self) -> Option<u64> {
        let guard = self.shared.lock().unwrap();
        (guard.status == RuntimeState::Ready).then_some(guard.config_generation)
    }

    /// Starts the runtime using a runtime owner lock guard the caller
    /// already acquired itself, e.g. immediately after using it to guard an
    /// out-of-runtime `AiSettings` save per ADR-0032 section 8 ("runtime
    /// owner lock → AI settings lock" order, with the save-then-start
    /// sequence holding the same guard throughout). Unlike [`Self::start`],
    /// this never attempts its own fresh acquisition: the exact same guard
    /// is carried over to the coordinator thread and held continuously, so
    /// no other process can ever observe the lock as free between the
    /// settings save and this runtime becoming its owner. If this runtime is
    /// already `Ready`, `owner` is simply dropped (releasing whatever
    /// separate OS lock file handle it held) and the current status is
    /// returned, exactly like [`Self::start`].
    pub fn start_with_owner_lock(&self, owner: OwnerLockGuard) -> Result<RuntimeStatus, RuntimeError> {
        self.send_lifecycle(LifecycleCommand::StartWithOwnerLock(owner))
    }

    /// Runs `f` synchronously on this runtime's own lifecycle coordinator
    /// thread, passing it `Some(&OwnerLockGuard)` if this `AiRuntime` is
    /// currently the runtime owner (actively running or between an
    /// unconfirmed stop and a later start where the guard is still carried
    /// over), or `None` otherwise. Per ADR-0032 section 8's "runtime owner
    /// lock → AI settings lock" order, this is how a caller performs an
    /// `AiSettings` save while this process's own active runtime already
    /// owns the runtime owner lock, without needing (and being unable to,
    /// per OS `flock` semantics, which treat a second independently opened
    /// file handle to the same lock file as a distinct holder even within
    /// the same process) a second, independent acquisition of the same lock
    /// file. Serialized with every other lifecycle command on the same
    /// coordinator thread, so `f` can never observe the guard disappearing
    /// partway through.
    pub fn with_owner_lock<F, R>(&self, f: F) -> Result<R, RuntimeError>
    where
        F: FnOnce(Option<&OwnerLockGuard>) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let boxed: OwnerLockCallback = Box::new(move |owner| {
            let _ = tx.send(f(owner));
        });
        self.cmd_tx
            .send(CoordinatorMessage::WithOwnerLock(boxed))
            .map_err(|_| RuntimeError::CoordinatorUnavailable)?;
        rx.recv().map_err(|_| RuntimeError::CoordinatorUnavailable)
    }

    /// Sends a request while the runtime is `Ready`. Runs on the calling
    /// thread against the current generation's transport, independent of
    /// the lifecycle coordinator, so it never blocks a concurrent stop or
    /// restart and is never retried automatically on failure.
    pub fn call(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value, RuntimeError> {
        let core = {
            let guard = self.shared.lock().unwrap();
            match (guard.status, &guard.ready_transport) {
                (RuntimeState::Ready, Some(core)) => core.clone(),
                _ => return Err(RuntimeError::NotReady),
            }
        };
        core.call(method, params, timeout).map_err(RuntimeError::Rpc)
    }

    pub fn snapshot(&self) -> RuntimeStatus {
        read_status(&self.shared)
    }

    pub fn shutdown(mut self) -> RuntimeStatus {
        let _ = self.cmd_tx.send(CoordinatorMessage::Shutdown);
        if let Some(handle) = self.coordinator.take() {
            let _ = handle.join();
        }
        read_status(&self.shared)
    }
}

impl Drop for AiRuntime {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(CoordinatorMessage::Shutdown);
        if let Some(handle) = self.coordinator.take() {
            let _ = handle.join();
        }
    }
}

fn read_status(shared: &Arc<Mutex<SharedState>>) -> RuntimeStatus {
    let guard = shared.lock().unwrap();
    RuntimeStatus {
        state: guard.status,
        restart_blocked: guard.restart_blocked,
        generation: guard.generation,
        operation_generation: guard.operation_generation,
    }
}

/// Updates the runtime status atomically: `state`, `restart_blocked`,
/// `generation`, and `operation_generation` all become visible together
/// under one lock acquisition, so a lifecycle timeout can record its
/// operation as cancelled and transition to `Failed` in a single step
/// instead of exposing an intermediate, inconsistent snapshot.
fn set_status(
    shared: &Arc<Mutex<SharedState>>,
    state: RuntimeState,
    restart_blocked: bool,
    generation: u64,
    operation_generation: u64,
) {
    let mut guard = shared.lock().unwrap();
    guard.status = state;
    guard.restart_blocked = restart_blocked;
    guard.generation = generation;
    guard.operation_generation = operation_generation;
}

fn run_coordinator(
    rx: mpsc::Receiver<CoordinatorMessage>,
    self_tx: Sender<CoordinatorMessage>,
    shared: Arc<Mutex<SharedState>>,
    mut config: RuntimeConfig,
    handler: Arc<dyn ServerRequestHandler>,
    events_tx: SyncSender<RuntimeEvent>,
) {
    let mut current: Option<ActiveChild> = None;
    // Child/runtime generation: bumped once per spawned `Child` (in
    // `do_start`) and once per confirmed stop/crash cleanup (in
    // `stop_active` / `reap_and_mark_failed`) — so every spawn and every
    // confirmed terminal transition gets its own fresh identity.
    // `ActiveChild::generation` is fixed at spawn time and is what gates a
    // specific `ChildEnded`/`RuntimeEvent` against `current`; this counter
    // is the source of every fresh value handed out to either, so a
    // timed-out or otherwise cancelled generation can never be reused by a
    // later one.
    let mut generation: u64 = 0;
    // Operation generation: bumped once per dequeued lifecycle command
    // (`Start` / `Stop` / `Restart`, and `Shutdown`), independent of
    // `generation` above — it identifies the *operation*, not whichever
    // `Child` it acts on. A `Restart` keeps one value across both its
    // stop and start steps, since it is a single lifecycle operation.
    let mut operation_generation: u64 = 0;
    let mut owner_guard: Option<OwnerLockGuard> = None;

    for msg in rx {
        match msg {
            CoordinatorMessage::Lifecycle(cmd, reply) => {
                operation_generation += 1;
                let op_gen = operation_generation;
                let result = match cmd {
                    LifecycleCommand::Reconfigure(new_config, new_configured_settings_generation) => {
                        // Stop whatever is active under the *old* config
                        // first (its own stop timeouts still apply), then
                        // swap `config` and start fresh under the new one.
                        // `release_owner_lock: false` keeps `owner_guard`
                        // held by this coordinator across the whole
                        // stop-then-start sequence (see `do_start`'s own
                        // handling of an already-held `owner_guard`), instead
                        // of dropping and immediately re-acquiring the OS
                        // lock file, which would open a window for a
                        // different Hane process to become the owner in
                        // between.
                        match stop_active(
                            &mut current,
                            &mut generation,
                            &mut owner_guard,
                            &config,
                            &shared,
                            &events_tx,
                            RuntimeState::Stopping,
                            RuntimeState::Stopped,
                            op_gen,
                            false,
                        ) {
                            Ok(_) => {
                                config = new_config;
                                // Record the new config's generation as soon
                                // as the swap itself happens, before the
                                // restart below is even attempted: a caller
                                // gating on `configured_settings_generation`
                                // (see `crate::connect::with_generation_checked_lock`)
                                // must never observe this coordinator still
                                // reporting the *old* generation once it has
                                // structurally moved on to a new `config`,
                                // regardless of whether the restart below
                                // goes on to succeed.
                                {
                                    let mut state = shared.lock().unwrap();
                                    state.config_generation = new_configured_settings_generation;
                                }
                                do_start(
                                    op_gen,
                                    &mut current,
                                    &mut generation,
                                    &mut owner_guard,
                                    None,
                                    &config,
                                    &handler,
                                    &events_tx,
                                    &self_tx,
                                    &shared,
                                    &reply,
                                )
                            }
                            Err(e) => Some(Err(e)),
                        }
                    }
                    LifecycleCommand::StartWithOwnerLock(owner) => do_start(
                        op_gen,
                        &mut current,
                        &mut generation,
                        &mut owner_guard,
                        Some(owner),
                        &config,
                        &handler,
                        &events_tx,
                        &self_tx,
                        &shared,
                        &reply,
                    ),
                    other => handle_lifecycle(
                        other,
                        op_gen,
                        &mut current,
                        &mut generation,
                        &mut owner_guard,
                        &config,
                        &handler,
                        &events_tx,
                        &self_tx,
                        &shared,
                        &reply,
                    ),
                };
                if let Some(result) = result {
                    let _ = reply.send(result);
                }
            }
            CoordinatorMessage::WithOwnerLock(f) => {
                f(owner_guard.as_ref());
            }
            CoordinatorMessage::ChildEnded {
                generation: g,
                operation_generation: og,
            } => {
                // Gate against both generations: a reader/monitor event only
                // applies to the exact `Child` (and the exact operation that
                // spawned it) `current` still holds, never to whatever a
                // later operation has since moved on to.
                let matches_current =
                    current.as_ref().map(|c| (c.generation, c.operation_generation)) == Some((g, og));
                if matches_current {
                    reap_and_mark_failed(
                        &mut current,
                        &mut generation,
                        &mut owner_guard,
                        &config,
                        &shared,
                        &events_tx,
                    );
                }
            }
            CoordinatorMessage::Shutdown => {
                operation_generation += 1;
                let op_gen = operation_generation;
                let outcome = stop_active(
                    &mut current,
                    &mut generation,
                    &mut owner_guard,
                    &config,
                    &shared,
                    &events_tx,
                    RuntimeState::Stopping,
                    RuntimeState::Stopped,
                    op_gen,
                    true,
                );
                // A `RestartBlocked` outcome means `stop_active` put the
                // still-alive (or at least unconfirmed-dead) `ActiveChild`
                // back into `current` and left `owner_guard` untouched. This
                // coordinator thread is about to exit, so simply letting
                // `current`/`owner_guard` fall out of scope here would drop
                // the `Child` (which does not kill it) and release the owner
                // lock while that process might still be running, letting a
                // later runtime start a second instance concurrently. Hand
                // both off to a background worker that keeps confirming the
                // child's exit and only then releases the lock, instead of
                // ever dropping it on unconfirmed cleanup.
                if outcome.is_err()
                    && let (Some(active), Some(guard)) = (current.take(), owner_guard.take())
                {
                    spawn_shutdown_cleanup_worker(active, guard, events_tx.clone());
                }
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_lifecycle(
    cmd: LifecycleCommand,
    op_gen: u64,
    current: &mut Option<ActiveChild>,
    generation: &mut u64,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    handler: &Arc<dyn ServerRequestHandler>,
    events_tx: &SyncSender<RuntimeEvent>,
    self_tx: &Sender<CoordinatorMessage>,
    shared: &Arc<Mutex<SharedState>>,
    reply: &Sender<Result<RuntimeStatus, RuntimeError>>,
) -> Option<Result<RuntimeStatus, RuntimeError>> {
    match cmd {
        LifecycleCommand::Start => do_start(
            op_gen, current, generation, owner_guard, None, config, handler, events_tx, self_tx, shared, reply,
        ),
        LifecycleCommand::Stop => Some(stop_active(
            current,
            generation,
            owner_guard,
            config,
            shared,
            events_tx,
            RuntimeState::Stopping,
            RuntimeState::Stopped,
            op_gen,
            true,
        )),
        LifecycleCommand::Restart => {
            if let Err(e) = stop_active(
                current,
                generation,
                owner_guard,
                config,
                shared,
                events_tx,
                RuntimeState::Stopping,
                RuntimeState::Stopped,
                op_gen,
                true,
            ) {
                return Some(Err(e));
            }
            do_start(
                op_gen, current, generation, owner_guard, None, config, handler, events_tx, self_tx, shared, reply,
            )
        }
        LifecycleCommand::Reconfigure(_, _) => {
            unreachable!("Reconfigure is handled directly in run_coordinator, before reaching handle_lifecycle")
        }
        LifecycleCommand::StartWithOwnerLock(_) => {
            unreachable!("StartWithOwnerLock is handled directly in run_coordinator, before reaching handle_lifecycle")
        }
    }
}

fn try_reap(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(Some(_)))
}

#[allow(clippy::too_many_arguments)]
fn do_start(
    op_gen: u64,
    current: &mut Option<ActiveChild>,
    generation: &mut u64,
    owner_guard: &mut Option<OwnerLockGuard>,
    external_owner: Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    handler: &Arc<dyn ServerRequestHandler>,
    events_tx: &SyncSender<RuntimeEvent>,
    self_tx: &Sender<CoordinatorMessage>,
    shared: &Arc<Mutex<SharedState>>,
    reply: &Sender<Result<RuntimeStatus, RuntimeError>>,
) -> Option<Result<RuntimeStatus, RuntimeError>> {
    // `external_owner` is only ever `Some` for `StartWithOwnerLock`, carrying
    // a guard the caller acquired itself. Confirm it actually proves
    // ownership of *this* runtime's own owner lock before doing anything
    // else: a guard acquired against a different path must be rejected
    // outright, never silently accepted (and possibly carried into
    // `owner_guard`) as if it were equivalent proof.
    if let Some(external) = &external_owner
        && external.path() != config.owner_lock_path
    {
        return Some(Err(RuntimeError::OwnerLockPathMismatch));
    }

    if current.is_some() {
        let status = read_status(shared);
        if status.state == RuntimeState::Ready {
            // Already running under this coordinator's own owner lock: an
            // `external_owner` supplied here (only possible via
            // `StartWithOwnerLock`) is redundant and is simply dropped here,
            // releasing whatever separate OS lock file handle it held. It
            // must never be assigned to `owner_guard`, which would replace
            // (and thereby release) the guard actually backing the
            // already-active child.
            return Some(Ok(status));
        }
        // A previous stop attempt could not confirm the old child had
        // exited. Re-check now; only proceed once it is actually gone.
        let reaped = {
            let active = current.as_mut().expect("checked is_some above");
            try_reap(&mut active.child)
        };
        if reaped {
            *current = None;
            *owner_guard = None;
        } else {
            return Some(Err(RuntimeError::RestartBlocked));
        }
    }

    set_status(shared, RuntimeState::Starting, false, *generation, op_gen);

    // `owner_guard` is already `Some` exactly when `Reconfigure` stopped the
    // old child with `release_owner_lock: false`: reuse that same guard
    // instead of acquiring a fresh one, so the OS lock is held continuously
    // from before the old child stopped through this new child's spawn, with
    // no window for a different Hane process to become the owner in
    // between. Every other caller reaches this with `owner_guard` already
    // `None` (the invariant the `current.is_some()` branch above restores it
    // to before falling through). `external_owner` is `Some` only for
    // `StartWithOwnerLock`, carrying over a guard the caller acquired itself
    // before this coordinator ever existed; every other caller passes
    // `None` here, falling through to a genuinely fresh acquisition.
    let guard = match owner_guard.take() {
        Some(carried) => carried,
        None => match external_owner {
            Some(external) => external,
            None => {
                let lock = OwnerLock::new(&config.owner_lock_path);
                match lock.try_acquire() {
                    Ok(Some(g)) => g,
                    Ok(None) => {
                        set_status(shared, RuntimeState::Stopped, false, *generation, op_gen);
                        return Some(Err(RuntimeError::OwnerLockUnavailable));
                    }
                    Err(e) => {
                        set_status(shared, RuntimeState::Failed, false, *generation, op_gen);
                        return Some(Err(RuntimeError::OwnerLock(Arc::new(e))));
                    }
                }
            }
        },
    };

    *generation += 1;
    let g = *generation;

    let mut child = match config.spawn_child() {
        Ok(c) => c,
        Err(e) => {
            set_status(shared, RuntimeState::Failed, false, g, op_gen);
            return Some(Err(RuntimeError::Spawn(Arc::new(e))));
        }
    };

    let stdin = child.stdin.take().expect("child spawned with piped stdin");
    let stdout = child.stdout.take().expect("child spawned with piped stdout");
    let stderr = child.stderr.take().expect("child spawned with piped stderr");

    let self_tx_clone = self_tx.clone();
    let on_closed: Box<dyn FnOnce() + Send> = Box::new(move || {
        let _ = self_tx_clone.send(CoordinatorMessage::ChildEnded {
            generation: g,
            operation_generation: op_gen,
        });
    });

    let (bridge_tx, bridge_rx) = mpsc::sync_channel::<RpcEvent>(EVENTS_BRIDGE_CAPACITY);
    let runtime_events_tx = events_tx.clone();
    thread::spawn(move || {
        for event in bridge_rx {
            let kind = match event {
                RpcEvent::Notification { method, params } => RuntimeEventKind::Notification { method, params },
                RpcEvent::Diagnostic(msg) => RuntimeEventKind::Diagnostic(msg),
            };
            // Non-blocking: a slow caller-side consumer must never stall
            // this forwarding thread (which would in turn back up the
            // bounded bridge above and eventually the reader/stderr
            // threads' own `try_send`). A full queue drops this one event;
            // a disconnected receiver means nobody will ever read again, so
            // stop draining the bridge instead of looping forever.
            match runtime_events_tx.try_send(RuntimeEvent { generation: g, kind }) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    });

    let transport = RpcTransport::spawn(
        Box::new(stdout),
        Box::new(stdin),
        Some(Box::new(stderr)),
        handler.clone(),
        bridge_tx,
        on_closed,
    );

    set_status(shared, RuntimeState::Initializing, false, g, op_gen);

    let core = transport.handle();
    // Schema per the bundled App Server's `initialize` request: `clientInfo`
    // carries name/title/version, and `capabilities` explicitly disables the
    // experimental API and attestation requests this client does not use.
    let init_result = core.call(
        "initialize",
        Some(serde_json::json!({
            "clientInfo": {
                "name": "hane",
                "title": "Hane",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "experimentalApi": false,
                "requestAttestation": false,
            },
        })),
        config.start_timeout,
    );

    *current = Some(ActiveChild {
        generation: g,
        operation_generation: op_gen,
        child,
        transport,
    });
    *owner_guard = Some(guard);

    match init_result {
        Ok(_) => {
            if core.notify("initialized", None).is_err() {
                // The transport is already gone: this operation is
                // cancelled just like a timeout below. Record `Failed`
                // (tagged with this operation's own generation) and reply to
                // the caller — and every coalesced waiter — immediately,
                // instead of only doing so once `Child` cleanup actually
                // confirms the process is gone.
                set_status(shared, RuntimeState::Failed, false, g, op_gen);
                let _ = reply.send(Err(RuntimeError::Handshake(RpcError::Disconnected)));
                let _ = stop_active(
                    current,
                    generation,
                    owner_guard,
                    config,
                    shared,
                    events_tx,
                    RuntimeState::Failed,
                    RuntimeState::Failed,
                    op_gen,
                    true,
                );
                return None;
            }
            {
                let mut state = shared.lock().unwrap();
                state.status = RuntimeState::Ready;
                state.restart_blocked = false;
                state.generation = g;
                state.operation_generation = op_gen;
                state.ready_transport = Some(core);
            }
            Some(Ok(read_status(shared)))
        }
        Err(err) => {
            // The handshake timed out: this operation is cancelled.
            // Atomically record it as cancelled and `Failed` (tagged with
            // this operation's own `operation_generation`, distinct from
            // whichever `Child` generation it spawned) and reply to the
            // waiting caller — and every coalesced waiter — right away,
            // instead of only doing so once `Child` cleanup below actually
            // confirms the process is gone. That cleanup still proceeds
            // straight to reaping the generation this operation spawned,
            // synchronously on this same coordinator thread, so the next
            // queued start/stop/restart still cannot begin before it
            // confirms the outcome; it must not regress the status just set
            // here back through an intermediate `Stopping` transition.
            set_status(shared, RuntimeState::Failed, false, g, op_gen);
            let _ = reply.send(Err(RuntimeError::Handshake(err)));
            let _ = stop_active(
                current,
                generation,
                owner_guard,
                config,
                shared,
                events_tx,
                RuntimeState::Failed,
                RuntimeState::Failed,
                op_gen,
                true,
            );
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stop_active(
    current: &mut Option<ActiveChild>,
    generation: &mut u64,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    shared: &Arc<Mutex<SharedState>>,
    events_tx: &SyncSender<RuntimeEvent>,
    in_progress_state: RuntimeState,
    state_on_success: RuntimeState,
    op_gen: u64,
    release_owner_lock: bool,
) -> Result<RuntimeStatus, RuntimeError> {
    let mut active = match current.take() {
        Some(a) => a,
        None => return Ok(read_status(shared)),
    };

    let child_generation = active.generation;
    // `in_progress_state` is `Stopping` for a normal `Stop`/`Restart` call,
    // or `Failed` when this cleanup follows a lifecycle-timeout cancellation
    // that already recorded `Failed` before calling here — in that case this
    // re-asserts the same state instead of regressing it to `Stopping`.
    set_status(shared, in_progress_state, false, child_generation, op_gen);
    {
        let mut state = shared.lock().unwrap();
        state.ready_transport = None;
    }

    // Close our write side first: a well-behaved server treats stdin EOF as
    // its cue to exit on its own.
    active.transport.request_shutdown();

    let poll_interval = Duration::from_millis(20);
    let outcome = perform_stop_sequence(
        &mut active.child,
        |c| c.try_wait().map(|s| s.is_some()),
        |c| c.kill(),
        config.stop_grace_timeout,
        config.stop_force_timeout,
        poll_interval,
    );

    match outcome {
        StopOutcome::Exited => {
            drop(active);
            // `release_owner_lock` is `false` only for the `Reconfigure`
            // path: it needs the owner lock to stay held by this same
            // coordinator, uninterrupted, from before this stop through the
            // subsequent `do_start` for the new config, so no other Hane
            // process can ever become the owner in between. Every other
            // caller (`Stop`/`Restart`/`Shutdown`) releases it here as
            // before.
            if release_owner_lock {
                *owner_guard = None;
            }
            // The generation being stopped is now confirmed gone. Hand out a
            // fresh operation generation for this terminal transition itself
            // instead of reusing the now-defunct child's own generation, so
            // this stop/cancel operation has its own identity that a later
            // start/restart's generation can never collide with.
            *generation += 1;
            set_status(shared, state_on_success, false, *generation, op_gen);
            Ok(read_status(shared))
        }
        StopOutcome::RestartBlocked => {
            *current = Some(active);
            set_status(shared, RuntimeState::Failed, true, child_generation, op_gen);
            let _ = events_tx.try_send(RuntimeEvent {
                generation: child_generation,
                kind: RuntimeEventKind::Diagnostic(
                    "runtime cleanup could not be confirmed; restart is blocked".to_string(),
                ),
            });
            Err(RuntimeError::RestartBlocked)
        }
    }
}

/// Confirms, in the background, that a child whose cleanup could not be
/// confirmed within `stop_active`'s bounded grace/force timeouts (during
/// coordinator `Shutdown`) has actually exited before releasing the runtime
/// owner lock on its behalf. The coordinator thread that owned `active` and
/// `owner_guard` is exiting right after handing them off here, so nothing
/// else keeps checking on this child; this worker keeps polling for however
/// long it takes, and never drops `owner_guard` (which would release the
/// lock to a would-be new owner) until `try_wait` actually confirms the
/// process is gone. If exit can never be confirmed, the lock is simply held
/// for as long as this process runs, which is the safe default.
///
/// The actual "never release before confirmed" guarantee is structural, via
/// `wait_for_confirmation_then_release` below: `active`/`owner_guard` are
/// moved into the single `resource` it owns, and it only drops that
/// `resource` after its own loop returns, which cannot happen before
/// `try_wait` first reports the child gone.
fn spawn_shutdown_cleanup_worker(
    active: ActiveChild,
    owner_guard: OwnerLockGuard,
    events_tx: SyncSender<RuntimeEvent>,
) {
    let child_generation = active.generation;
    thread::spawn(move || {
        let poll_interval = Duration::from_millis(20);
        wait_for_confirmation_then_release(
            (active, owner_guard),
            |(active, _owner_guard)| matches!(active.child.try_wait(), Ok(Some(_))),
            poll_interval,
            thread::sleep,
        );
        let _ = events_tx.try_send(RuntimeEvent {
            generation: child_generation,
            kind: RuntimeEventKind::Diagnostic(
                "runtime shutdown confirmed the previously unconfirmed child had exited; owner lock released"
                    .to_string(),
            ),
        });
    });
}

/// Polls `try_wait` (sleeping `poll_interval` between attempts via the
/// injected `sleep`) until it reports the held `resource` may be released,
/// and only then drops it. Kept generic over `resource`/`try_wait`/`sleep`
/// so the one property that matters — `resource` can never be dropped
/// before `try_wait` first returns `true` — is exercised by a fully
/// deterministic unit test below, instead of only by
/// `spawn_shutdown_cleanup_worker`'s real `ActiveChild`/`OwnerLockGuard`,
/// whose exit timing depends on real process/OS scheduling and so cannot
/// deterministically prove the same ordering.
fn wait_for_confirmation_then_release<R, F1, F2>(
    mut resource: R,
    mut try_wait: F1,
    poll_interval: Duration,
    mut sleep: F2,
) where
    F1: FnMut(&mut R) -> bool,
    F2: FnMut(Duration),
{
    loop {
        if try_wait(&mut resource) {
            break;
        }
        sleep(poll_interval);
    }
    drop(resource);
}

fn reap_and_mark_failed(
    current: &mut Option<ActiveChild>,
    generation: &mut u64,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    shared: &Arc<Mutex<SharedState>>,
    events_tx: &SyncSender<RuntimeEvent>,
) {
    let mut active = match current.take() {
        Some(a) => a,
        None => return,
    };

    let child_generation = active.generation;
    // No new lifecycle operation was initiated here (this is an
    // asynchronously detected crash, not a coordinator-dequeued command), so
    // the resulting `Failed` status is reported under the operation that had
    // spawned/owned the now-crashed child, rather than minting a new one.
    let op_gen = active.operation_generation;
    // Publish `Failed` before running the stop sequence below: leaving the
    // published state at `Ready` while cleanup is still in flight would let
    // callers observe a stale, already-wrong status.
    set_status(shared, RuntimeState::Failed, false, child_generation, op_gen);
    {
        let mut state = shared.lock().unwrap();
        state.ready_transport = None;
    }

    // Close our write side first: a well-behaved server treats stdin EOF as
    // its cue to exit on its own. Without this, a reader-thread-detected EOF
    // or a server-request handler panic can leave the child process itself
    // still running with no shutdown signal, so `perform_stop_sequence` below
    // would have to wait out the full grace timeout before it kills it.
    active.transport.request_shutdown();

    let poll_interval = Duration::from_millis(20);
    let outcome = perform_stop_sequence(
        &mut active.child,
        |c| c.try_wait().map(|s| s.is_some()),
        |c| c.kill(),
        config.stop_grace_timeout,
        config.stop_force_timeout,
        poll_interval,
    );

    match outcome {
        StopOutcome::Exited => {
            drop(active);
            *owner_guard = None;
            // Same fresh-generation treatment as a confirmed `stop_active`
            // exit: this crash-cleanup operation gets its own identity,
            // distinct from the crashed child's own generation.
            *generation += 1;
            set_status(shared, RuntimeState::Failed, false, *generation, op_gen);
            let _ = events_tx.try_send(RuntimeEvent {
                generation: child_generation,
                kind: RuntimeEventKind::Diagnostic("runtime exited unexpectedly".to_string()),
            });
        }
        StopOutcome::RestartBlocked => {
            *current = Some(active);
            set_status(shared, RuntimeState::Failed, true, child_generation, op_gen);
            let _ = events_tx.try_send(RuntimeEvent {
                generation: child_generation,
                kind: RuntimeEventKind::Diagnostic(
                    "runtime exited unexpectedly and cleanup could not be confirmed".to_string(),
                ),
            });
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StopOutcome {
    Exited,
    RestartBlocked,
}

fn perform_stop_sequence<C, F1, F2>(
    handle: &mut C,
    mut try_wait: F1,
    mut kill: F2,
    grace_timeout: Duration,
    force_timeout: Duration,
    poll_interval: Duration,
) -> StopOutcome
where
    F1: FnMut(&mut C) -> io::Result<bool>,
    F2: FnMut(&mut C) -> io::Result<()>,
{
    if wait_until(handle, &mut try_wait, grace_timeout, poll_interval) {
        return StopOutcome::Exited;
    }
    let _ = kill(handle);
    if wait_until(handle, &mut try_wait, force_timeout, poll_interval) {
        return StopOutcome::Exited;
    }
    StopOutcome::RestartBlocked
}

fn wait_until<C, F>(handle: &mut C, try_wait: &mut F, timeout: Duration, poll_interval: Duration) -> bool
where
    F: FnMut(&mut C) -> io::Result<bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(true) = try_wait(handle) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        thread::sleep(poll_interval.min(deadline - now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProcess {
        exited: bool,
        exits_after_kill: bool,
        kill_calls: u32,
    }

    #[test]
    fn exits_within_grace_period_without_killing() {
        let mut process = FakeProcess {
            exited: true,
            exits_after_kill: false,
            kill_calls: 0,
        };
        let outcome = perform_stop_sequence(
            &mut process,
            |p: &mut FakeProcess| Ok(p.exited),
            |p: &mut FakeProcess| {
                p.kill_calls += 1;
                Ok(())
            },
            Duration::from_millis(50),
            Duration::from_millis(50),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, StopOutcome::Exited);
        assert_eq!(process.kill_calls, 0);
    }

    #[test]
    fn escalates_to_kill_when_grace_period_elapses() {
        let mut process = FakeProcess {
            exited: false,
            exits_after_kill: true,
            kill_calls: 0,
        };
        let outcome = perform_stop_sequence(
            &mut process,
            |p: &mut FakeProcess| Ok(p.exited),
            |p: &mut FakeProcess| {
                p.kill_calls += 1;
                p.exited = p.exits_after_kill;
                Ok(())
            },
            Duration::from_millis(20),
            Duration::from_millis(50),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, StopOutcome::Exited);
        assert_eq!(process.kill_calls, 1);
    }

    #[test]
    fn reports_restart_blocked_when_process_never_confirms_exit() {
        let mut process = FakeProcess {
            exited: false,
            exits_after_kill: false,
            kill_calls: 0,
        };
        let outcome = perform_stop_sequence(
            &mut process,
            |p: &mut FakeProcess| Ok(p.exited),
            |p: &mut FakeProcess| {
                p.kill_calls += 1;
                Ok(())
            },
            Duration::from_millis(15),
            Duration::from_millis(15),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, StopOutcome::RestartBlocked);
        assert_eq!(process.kill_calls, 1);
    }

    #[test]
    fn spawn_child_rejects_a_non_absolute_binary_path_instead_of_falling_back_to_path_lookup() {
        let config = RuntimeConfig::new("codex-app-server", "/tmp/hane-ai-test-owner.lock");
        let err = config.spawn_child().expect_err("a bare filename must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn default_args_invoke_the_standalone_binary_without_an_app_server_subcommand() {
        let config = RuntimeConfig::new("/opt/hane/codex", "/tmp/hane-ai-test-owner.lock");
        assert_eq!(config.args, vec!["--listen".to_string(), "stdio://".to_string()]);
    }

    #[test]
    fn with_owner_lock_reports_none_before_the_runtime_ever_starts() {
        use crate::rpc::RejectAllServerRequests;

        let dir = std::env::temp_dir().join(format!(
            "hane-ai-with-owner-lock-test-{}-{}",
            std::process::id(),
            "with_owner_lock_reports_none_before_the_runtime_ever_starts"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = RuntimeConfig::new("/nonexistent/hane-ai-test-binary", dir.join("runtime.lock"));
        let (events_tx, _events_rx) = mpsc::sync_channel(8);
        let handler: Arc<dyn ServerRequestHandler> = Arc::new(RejectAllServerRequests);
        let runtime = AiRuntime::spawn(config, handler, events_tx);

        // Never started: no `Start`/`StartWithOwnerLock` was ever issued, so
        // this coordinator does not (and must not) hold the owner lock yet.
        let has_owner = runtime.with_owner_lock(|owner| owner.is_some()).unwrap();
        assert!(!has_owner, "no owner lock should be held before Start is ever issued");

        let _ = runtime.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_formatting_never_prints_an_extra_env_secret_value() {
        let mut config = RuntimeConfig::new("/opt/hane/codex", "/tmp/hane-ai-test-owner.lock");
        config
            .extra_env
            .push(("HANE_AI_PROVIDER_KEY".to_string(), "sk-super-secret-value".to_string()));
        let formatted = format!("{config:?}");
        assert!(formatted.contains("HANE_AI_PROVIDER_KEY"), "env var name should still be visible");
        assert!(
            !formatted.contains("sk-super-secret-value"),
            "extra_env values must never appear in Debug output, got: {formatted}"
        );
    }

    /// Deterministic (no real process, no real thread, no real sleep)
    /// counterpart to `spawn_shutdown_cleanup_worker`'s use of
    /// `wait_for_confirmation_then_release`: `Probe` stands in for the
    /// `(ActiveChild, OwnerLockGuard)` tuple the real worker holds — like
    /// the owner lock, its resource is only "released" (here: marks itself
    /// dropped) when actually dropped. `try_wait` asserts, on every single
    /// poll, that the probe has not been released yet, and only reports
    /// exit confirmed on the third attempt; this exercises every iteration
    /// of the loop instead of just its final outcome.
    #[test]
    fn wait_for_confirmation_then_release_never_drops_the_resource_before_confirmation() {
        struct Probe {
            released: Arc<Mutex<bool>>,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                *self.released.lock().unwrap() = true;
            }
        }

        let released = Arc::new(Mutex::new(false));
        let probe = Probe {
            released: released.clone(),
        };
        let mut attempts = 0u32;

        wait_for_confirmation_then_release(
            probe,
            |_resource| {
                attempts += 1;
                assert!(
                    !*released.lock().unwrap(),
                    "resource (standing in for the owner lock) must not be released before \
                     try_wait first confirms exit"
                );
                attempts >= 3
            },
            Duration::from_millis(0),
            |_| {},
        );

        assert_eq!(attempts, 3, "try_wait should be polled until it first reports exit confirmed");
        assert!(
            *released.lock().unwrap(),
            "resource must be released once try_wait confirms exit"
        );
    }
}
