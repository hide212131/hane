//! Process-boundary integration tests: spawns the `fake_app_server` test
//! fixture as a real child process (via `AiRuntime`) to exercise the
//! initialize/initialized barrier, finite lifecycle timeouts, generation
//! bumps across restarts, forced-kill escalation, the runtime owner lock,
//! and "no automatic resend after a crash".

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use hane_ai::{AiRuntime, OwnerLock, RejectAllServerRequests, RuntimeConfig, RuntimeError, RuntimeEvent, RuntimeState};

fn unique_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("hane-ai-runtime-it-{pid}-{name}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn base_config(name: &str) -> RuntimeConfig {
    let dir = unique_dir(name);
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server"));
    let mut config = RuntimeConfig::new(binary, dir.join("runtime.lock"));
    config.args = Vec::new();
    config.start_timeout = Duration::from_secs(5);
    config.stop_grace_timeout = Duration::from_millis(500);
    config.stop_force_timeout = Duration::from_secs(5);
    config
}

fn spawn_runtime(config: RuntimeConfig) -> (AiRuntime, mpsc::Receiver<RuntimeEvent>) {
    let (events_tx, events_rx) = mpsc::channel();
    let handler = Arc::new(RejectAllServerRequests);
    (AiRuntime::spawn(config, handler, events_tx), events_rx)
}

#[test]
fn initialize_barrier_reaches_ready_then_calls_and_stops_cleanly() {
    let config = base_config("initialize_barrier");
    let (runtime, _events) = spawn_runtime(config);

    let status = runtime.start().expect("start should succeed");
    assert_eq!(status.state, RuntimeState::Ready);
    assert!(!status.restart_blocked);

    let result = runtime
        .call("test/echo", Some(serde_json::json!({"x": 1})), Duration::from_secs(5))
        .expect("echo call should succeed");
    assert_eq!(result["x"], 1);

    let stopped = runtime.stop().expect("stop should succeed");
    assert_eq!(stopped.state, RuntimeState::Stopped);
    assert!(!stopped.restart_blocked);
}

#[test]
fn start_times_out_and_transitions_to_failed_when_server_never_responds() {
    let mut config = base_config("never_respond");
    config.start_timeout = Duration::from_millis(300);
    config
        .extra_env
        .push(("FAKE_SERVER_MODE".to_string(), "never_respond".to_string()));
    let (runtime, _events) = spawn_runtime(config);

    let result = runtime.start();
    assert!(matches!(result, Err(RuntimeError::Handshake(_))));

    let status = runtime.snapshot();
    assert_eq!(status.state, RuntimeState::Failed);
    assert!(!status.restart_blocked);
}

#[test]
fn crash_mid_request_fails_the_call_and_does_not_auto_restart() {
    let mut config = base_config("crash_mid_request");
    config
        .extra_env
        .push(("FAKE_SERVER_MODE".to_string(), "crash_mid_request".to_string()));
    let (runtime, _events) = spawn_runtime(config);

    let status = runtime.start().expect("start should succeed");
    assert_eq!(status.state, RuntimeState::Ready);

    let result = runtime.call("test/echo", None, Duration::from_secs(5));
    assert!(matches!(result, Err(RuntimeError::Rpc(_))));

    // The coordinator must land on Failed on its own once it observes the
    // closed transport, not silently keep serving the dead generation.
    let mut status = runtime.snapshot();
    for _ in 0..50 {
        if status.state == RuntimeState::Failed {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        status = runtime.snapshot();
    }
    assert_eq!(status.state, RuntimeState::Failed);

    // A later call is rejected outright, never silently retried against a
    // freshly (and implicitly) spawned replacement process.
    let retried = runtime.call("test/echo", None, Duration::from_millis(200));
    assert!(matches!(retried, Err(RuntimeError::NotReady)));
}

#[test]
fn owner_lock_blocks_a_second_owner_and_releases_after_drop() {
    let config = base_config("owner_lock");
    let lock_path = config.owner_lock_path.clone();
    let external_lock = OwnerLock::new(&lock_path);
    let external_guard = external_lock
        .try_acquire()
        .unwrap()
        .expect("first acquire should succeed");

    let (runtime, _events) = spawn_runtime(config);
    let result = runtime.start();
    assert!(matches!(result, Err(RuntimeError::OwnerLockUnavailable)));
    assert_eq!(runtime.snapshot().state, RuntimeState::Stopped);

    drop(external_guard);

    let status = runtime.start().expect("start should succeed once the lock is free");
    assert_eq!(status.state, RuntimeState::Ready);
    let _ = runtime.stop();
}

#[test]
fn restart_advances_the_generation() {
    let config = base_config("restart_generation");
    let (runtime, _events) = spawn_runtime(config);

    let first = runtime.start().expect("start should succeed");
    let restarted = runtime.restart().expect("restart should succeed");
    assert!(restarted.generation > first.generation);
    assert_eq!(restarted.state, RuntimeState::Ready);
    let _ = runtime.stop();
}

#[test]
fn stop_escalates_to_a_forced_kill_when_the_server_ignores_graceful_shutdown() {
    let mut config = base_config("ignore_stop");
    config.stop_grace_timeout = Duration::from_millis(150);
    config.stop_force_timeout = Duration::from_secs(10);
    config
        .extra_env
        .push(("FAKE_SERVER_IGNORE_STOP".to_string(), "1".to_string()));
    let (runtime, _events) = spawn_runtime(config);

    let status = runtime.start().expect("start should succeed");
    assert_eq!(status.state, RuntimeState::Ready);

    let stopped = runtime.stop().expect("stop should still succeed via a forced kill");
    assert_eq!(stopped.state, RuntimeState::Stopped);
    assert!(!stopped.restart_blocked);
}

#[test]
fn concurrent_start_calls_spawn_exactly_one_process() {
    let mut config = base_config("single_flight");
    let dir = config.owner_lock_path.parent().unwrap().to_path_buf();
    let marker = dir.join("spawned.log");
    config.extra_env.push((
        "FAKE_SERVER_SPAWN_MARKER_FILE".to_string(),
        marker.display().to_string(),
    ));

    let (events_tx, _events_rx) = mpsc::channel();
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = Arc::new(AiRuntime::spawn(config, handler, events_tx));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let runtime = runtime.clone();
        handles.push(std::thread::spawn(move || runtime.start()));
    }
    let mut generations = Vec::new();
    for handle in handles {
        let status = handle.join().unwrap().expect("every start call should succeed");
        generations.push(status.generation);
    }
    assert!(generations.iter().all(|g| *g == generations[0]));

    let spawned = std::fs::read_to_string(&marker).unwrap_or_default();
    let spawn_count = spawned.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(spawn_count, 1);

    let _ = runtime.stop();
}
