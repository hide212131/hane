//! App Server process lifecycle: a single-flight coordinator thread that
//! owns start / stop / restart, finite lifecycle timeouts, generation-based
//! invalidation of late events, and the cross-process runtime owner lock.
//!
//! Every lifecycle command (`Start` / `Stop` / `Restart`) is handled to
//! completion by one dedicated coordinator thread before the next queued
//! command is processed, so concurrent callers are naturally serialized and
//! observe the same outcome instead of racing to spawn duplicate children.
//! Regular RPC calls (`AiRuntime::call`) do not go through that queue: they
//! borrow a handle to the current generation's transport and run
//! concurrently with each other, so a long-running call cannot block a
//! `stop`/`restart` request.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::owner_lock::{OwnerLock, OwnerLockGuard};
use crate::rpc::{RpcCore, RpcError, RpcEvent, RpcTransport, ServerRequestHandler};

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
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Notification { method: String, params: Option<Value> },
    Diagnostic(String),
}

#[derive(Debug)]
pub enum RuntimeError {
    NotReady,
    RestartBlocked,
    OwnerLockUnavailable,
    OwnerLock(io::Error),
    Spawn(io::Error),
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
/// an explicit path to the bundled executable; it is never resolved via
/// `PATH`.
#[derive(Debug, Clone)]
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

impl RuntimeConfig {
    pub fn new(binary_path: impl Into<PathBuf>, owner_lock_path: impl Into<PathBuf>) -> Self {
        RuntimeConfig {
            binary_path: binary_path.into(),
            args: vec![
                "app-server".to_string(),
                "--listen".to_string(),
                "stdio://".to_string(),
            ],
            codex_home: None,
            extra_env: Vec::new(),
            owner_lock_path: owner_lock_path.into(),
            start_timeout: Duration::from_secs(20),
            stop_grace_timeout: Duration::from_secs(5),
            stop_force_timeout: Duration::from_secs(5),
        }
    }

    fn spawn_child(&self) -> io::Result<Child> {
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
    ready_transport: Option<Arc<RpcCore>>,
}

struct ActiveChild {
    generation: u64,
    child: Child,
    transport: RpcTransport,
}

enum LifecycleCommand {
    Start,
    Stop,
    Restart,
}

enum CoordinatorMessage {
    Lifecycle(LifecycleCommand, Sender<Result<RuntimeStatus, RuntimeError>>),
    ChildEnded { generation: u64 },
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
        events_tx: Sender<RuntimeEvent>,
    ) -> AiRuntime {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let self_tx = cmd_tx.clone();
        let shared = Arc::new(Mutex::new(SharedState {
            status: RuntimeState::Stopped,
            restart_blocked: false,
            generation: 0,
            ready_transport: None,
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

    pub fn start(&self) -> Result<RuntimeStatus, RuntimeError> {
        self.send_lifecycle(LifecycleCommand::Start)
    }

    pub fn stop(&self) -> Result<RuntimeStatus, RuntimeError> {
        self.send_lifecycle(LifecycleCommand::Stop)
    }

    pub fn restart(&self) -> Result<RuntimeStatus, RuntimeError> {
        self.send_lifecycle(LifecycleCommand::Restart)
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
    }
}

fn set_status(shared: &Arc<Mutex<SharedState>>, state: RuntimeState, restart_blocked: bool, generation: u64) {
    let mut guard = shared.lock().unwrap();
    guard.status = state;
    guard.restart_blocked = restart_blocked;
    guard.generation = generation;
}

fn run_coordinator(
    rx: mpsc::Receiver<CoordinatorMessage>,
    self_tx: Sender<CoordinatorMessage>,
    shared: Arc<Mutex<SharedState>>,
    config: RuntimeConfig,
    handler: Arc<dyn ServerRequestHandler>,
    events_tx: Sender<RuntimeEvent>,
) {
    let mut current: Option<ActiveChild> = None;
    let mut generation: u64 = 0;
    let mut owner_guard: Option<OwnerLockGuard> = None;

    for msg in rx {
        match msg {
            CoordinatorMessage::Lifecycle(cmd, reply) => {
                let result = handle_lifecycle(
                    cmd,
                    &mut current,
                    &mut generation,
                    &mut owner_guard,
                    &config,
                    &handler,
                    &events_tx,
                    &self_tx,
                    &shared,
                );
                let _ = reply.send(result);
            }
            CoordinatorMessage::ChildEnded { generation: g } => {
                let matches_current = current.as_ref().map(|c| c.generation) == Some(g);
                if matches_current {
                    reap_and_mark_failed(&mut current, &mut owner_guard, &config, &shared, &events_tx);
                }
            }
            CoordinatorMessage::Shutdown => {
                let _ = stop_active(
                    &mut current,
                    &mut owner_guard,
                    &config,
                    &shared,
                    &events_tx,
                    RuntimeState::Stopped,
                );
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_lifecycle(
    cmd: LifecycleCommand,
    current: &mut Option<ActiveChild>,
    generation: &mut u64,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    handler: &Arc<dyn ServerRequestHandler>,
    events_tx: &Sender<RuntimeEvent>,
    self_tx: &Sender<CoordinatorMessage>,
    shared: &Arc<Mutex<SharedState>>,
) -> Result<RuntimeStatus, RuntimeError> {
    match cmd {
        LifecycleCommand::Start => do_start(
            current, generation, owner_guard, config, handler, events_tx, self_tx, shared,
        ),
        LifecycleCommand::Stop => stop_active(
            current,
            owner_guard,
            config,
            shared,
            events_tx,
            RuntimeState::Stopped,
        ),
        LifecycleCommand::Restart => {
            stop_active(
                current,
                owner_guard,
                config,
                shared,
                events_tx,
                RuntimeState::Stopped,
            )?;
            do_start(
                current, generation, owner_guard, config, handler, events_tx, self_tx, shared,
            )
        }
    }
}

fn try_reap(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(Some(_)))
}

#[allow(clippy::too_many_arguments)]
fn do_start(
    current: &mut Option<ActiveChild>,
    generation: &mut u64,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    handler: &Arc<dyn ServerRequestHandler>,
    events_tx: &Sender<RuntimeEvent>,
    self_tx: &Sender<CoordinatorMessage>,
    shared: &Arc<Mutex<SharedState>>,
) -> Result<RuntimeStatus, RuntimeError> {
    if current.is_some() {
        let status = read_status(shared);
        if status.state == RuntimeState::Ready {
            return Ok(status);
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
            return Err(RuntimeError::RestartBlocked);
        }
    }

    set_status(shared, RuntimeState::Starting, false, *generation);

    let lock = OwnerLock::new(&config.owner_lock_path);
    let guard = match lock.try_acquire() {
        Ok(Some(g)) => g,
        Ok(None) => {
            set_status(shared, RuntimeState::Stopped, false, *generation);
            return Err(RuntimeError::OwnerLockUnavailable);
        }
        Err(e) => {
            set_status(shared, RuntimeState::Failed, false, *generation);
            return Err(RuntimeError::OwnerLock(e));
        }
    };

    *generation += 1;
    let g = *generation;

    let mut child = match config.spawn_child() {
        Ok(c) => c,
        Err(e) => {
            set_status(shared, RuntimeState::Failed, false, g);
            return Err(RuntimeError::Spawn(e));
        }
    };

    let stdin = child.stdin.take().expect("child spawned with piped stdin");
    let stdout = child.stdout.take().expect("child spawned with piped stdout");
    let stderr = child.stderr.take().expect("child spawned with piped stderr");

    let self_tx_clone = self_tx.clone();
    let on_closed: Box<dyn FnOnce() + Send> = Box::new(move || {
        let _ = self_tx_clone.send(CoordinatorMessage::ChildEnded { generation: g });
    });

    let (bridge_tx, bridge_rx) = mpsc::channel::<RpcEvent>();
    let runtime_events_tx = events_tx.clone();
    thread::spawn(move || {
        for event in bridge_rx {
            let mapped = match event {
                RpcEvent::Notification { method, params } => RuntimeEvent::Notification { method, params },
                RpcEvent::Diagnostic(msg) => RuntimeEvent::Diagnostic(msg),
            };
            let _ = runtime_events_tx.send(mapped);
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

    set_status(shared, RuntimeState::Initializing, false, g);

    let core = transport.handle();
    let init_result = core.call(
        "initialize",
        Some(serde_json::json!({
            "clientInfo": {"name": "hane", "version": env!("CARGO_PKG_VERSION")}
        })),
        config.start_timeout,
    );

    *current = Some(ActiveChild {
        generation: g,
        child,
        transport,
    });
    *owner_guard = Some(guard);

    match init_result {
        Ok(_) => {
            if core.notify("initialized", None).is_err() {
                let _ = stop_active(
                    current,
                    owner_guard,
                    config,
                    shared,
                    events_tx,
                    RuntimeState::Failed,
                );
                return Err(RuntimeError::Handshake(RpcError::Disconnected));
            }
            {
                let mut state = shared.lock().unwrap();
                state.status = RuntimeState::Ready;
                state.restart_blocked = false;
                state.generation = g;
                state.ready_transport = Some(core);
            }
            Ok(read_status(shared))
        }
        Err(err) => {
            let _ = stop_active(
                current,
                owner_guard,
                config,
                shared,
                events_tx,
                RuntimeState::Failed,
            );
            Err(RuntimeError::Handshake(err))
        }
    }
}

fn stop_active(
    current: &mut Option<ActiveChild>,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    shared: &Arc<Mutex<SharedState>>,
    events_tx: &Sender<RuntimeEvent>,
    state_on_success: RuntimeState,
) -> Result<RuntimeStatus, RuntimeError> {
    let mut active = match current.take() {
        Some(a) => a,
        None => return Ok(read_status(shared)),
    };

    let generation = active.generation;
    set_status(shared, RuntimeState::Stopping, false, generation);
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
            *owner_guard = None;
            set_status(shared, state_on_success, false, generation);
            Ok(read_status(shared))
        }
        StopOutcome::RestartBlocked => {
            *current = Some(active);
            set_status(shared, RuntimeState::Failed, true, generation);
            let _ = events_tx.send(RuntimeEvent::Diagnostic(
                "runtime cleanup could not be confirmed; restart is blocked".to_string(),
            ));
            Err(RuntimeError::RestartBlocked)
        }
    }
}

fn reap_and_mark_failed(
    current: &mut Option<ActiveChild>,
    owner_guard: &mut Option<OwnerLockGuard>,
    config: &RuntimeConfig,
    shared: &Arc<Mutex<SharedState>>,
    events_tx: &Sender<RuntimeEvent>,
) {
    let mut active = match current.take() {
        Some(a) => a,
        None => return,
    };

    let generation = active.generation;
    {
        let mut state = shared.lock().unwrap();
        state.ready_transport = None;
    }

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
            set_status(shared, RuntimeState::Failed, false, generation);
            let _ = events_tx.send(RuntimeEvent::Diagnostic(
                "runtime exited unexpectedly".to_string(),
            ));
        }
        StopOutcome::RestartBlocked => {
            *current = Some(active);
            set_status(shared, RuntimeState::Failed, true, generation);
            let _ = events_tx.send(RuntimeEvent::Diagnostic(
                "runtime exited unexpectedly and cleanup could not be confirmed".to_string(),
            ));
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
}
