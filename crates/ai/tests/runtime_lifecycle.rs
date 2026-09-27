//! Process-boundary integration tests: spawns the `fake_app_server` test
//! fixture as a real child process (via `AiRuntime`) to exercise the
//! initialize/initialized barrier and its request schema, finite lifecycle
//! timeouts, generation bumps across restarts, single-flight start/restart
//! coalescing (including a coalesced *failing* start wave), forced-kill
//! escalation, immediate release of pending calls on stop, stderr drain
//! under load, the runtime owner lock (including across real OS processes),
//! and "no automatic resend after a crash".

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use hane_ai::{
    AiRuntime, OwnerLock, RejectAllServerRequests, RuntimeConfig, RuntimeError, RuntimeEvent, RuntimeState,
};

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

#[test]
fn concurrent_failing_start_calls_share_one_spawn_and_the_same_failure() {
    // `never_respond` plus a short start timeout makes the one underlying
    // attempt reliably fail its `initialize` handshake, without depending on
    // scheduling. Without single-flight coalescing of `Start`, the
    // coordinator would process every queued `Start` command in turn once
    // the leader's attempt leaves `current` empty again, spawning (and
    // timing out) once per concurrent caller instead of once per wave.
    let mut config = base_config("failing_single_flight");
    config.start_timeout = Duration::from_millis(300);
    config
        .extra_env
        .push(("FAKE_SERVER_MODE".to_string(), "never_respond".to_string()));
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
    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.join().unwrap());
    }

    let mut debug_reprs = Vec::new();
    for outcome in &outcomes {
        assert!(
            matches!(outcome, Err(RuntimeError::Handshake(_))),
            "expected every coalesced caller in the failing wave to see a Handshake failure, got {outcome:?}"
        );
        debug_reprs.push(format!("{outcome:?}"));
    }
    assert!(
        debug_reprs.iter().all(|d| *d == debug_reprs[0]),
        "every coalesced start call must observe the identical failure, got {debug_reprs:?}"
    );

    let spawned = std::fs::read_to_string(&marker).unwrap_or_default();
    let spawn_count = spawned.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        spawn_count, 1,
        "every concurrent start call in the same failing wave must share one spawn/initialize attempt"
    );

    // A later call is a new wave and is still free to try again.
    let retried = runtime.start();
    assert!(matches!(retried, Err(RuntimeError::Handshake(_))));
    let spawned_after_retry = std::fs::read_to_string(&marker).unwrap_or_default();
    let spawn_count_after_retry = spawned_after_retry.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(spawn_count_after_retry, 2, "a later explicit retry must still be free to spawn again");
}

#[test]
fn concurrent_restart_calls_do_not_respawn_the_child_repeatedly() {
    let mut config = base_config("restart_single_flight");
    let dir = config.owner_lock_path.parent().unwrap().to_path_buf();
    let marker = dir.join("spawned.log");
    config.extra_env.push((
        "FAKE_SERVER_SPAWN_MARKER_FILE".to_string(),
        marker.display().to_string(),
    ));

    let (events_tx, _events_rx) = mpsc::channel();
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = Arc::new(AiRuntime::spawn(config, handler, events_tx));
    runtime.start().expect("start should succeed");

    let mut handles = Vec::new();
    for _ in 0..8 {
        let runtime = runtime.clone();
        handles.push(std::thread::spawn(move || runtime.restart()));
    }
    let mut generations = Vec::new();
    for handle in handles {
        let status = handle.join().unwrap().expect("every restart call should succeed");
        generations.push(status.generation);
    }
    assert!(
        generations.iter().all(|g| *g == generations[0]),
        "every coalesced restart call must observe the same single outcome, got {generations:?}"
    );

    let spawned = std::fs::read_to_string(&marker).unwrap_or_default();
    let spawn_count = spawned.lines().filter(|l| !l.trim().is_empty()).count();
    // One spawn for the initial `start`, exactly one more for the single
    // coalesced restart cycle shared by all 8 concurrent callers.
    assert_eq!(spawn_count, 2);

    let _ = runtime.stop();
}

#[test]
fn initialize_sends_schema_compliant_params_and_the_initialized_notification() {
    let mut config = base_config("initialize_schema");
    let dir = config.owner_lock_path.parent().unwrap().to_path_buf();
    let record_file = dir.join("init_record.log");
    config.extra_env.push((
        "FAKE_SERVER_RECORD_INIT_FILE".to_string(),
        record_file.display().to_string(),
    ));
    let (runtime, _events) = spawn_runtime(config);

    let status = runtime.start().expect("start should succeed");
    assert_eq!(status.state, RuntimeState::Ready);

    let recorded = std::fs::read_to_string(&record_file).unwrap();
    let mut lines = recorded.lines();

    let params_line = lines.next().expect("init params should have been recorded");
    let params_json = params_line
        .strip_prefix("INIT_PARAMS:")
        .expect("recorded line should carry the INIT_PARAMS prefix");
    let params: serde_json::Value = serde_json::from_str(params_json).unwrap();
    assert_eq!(params["clientInfo"]["name"], "hane");
    assert_eq!(params["clientInfo"]["title"], "Hane");
    assert!(params["clientInfo"]["version"].is_string());
    assert_eq!(params["capabilities"]["experimentalApi"], false);
    assert_eq!(params["capabilities"]["requestAttestation"], false);

    assert_eq!(lines.next(), Some("INITIALIZED"));

    let _ = runtime.stop();
}

#[test]
fn stderr_spam_does_not_block_stdio_communication() {
    let mut config = base_config("stderr_spam");
    config
        .extra_env
        .push(("FAKE_SERVER_STDERR_SPAM".to_string(), "1".to_string()));
    let (runtime, _events) = spawn_runtime(config);

    let status = runtime.start().expect("start should succeed even while the child spams stderr");
    assert_eq!(status.state, RuntimeState::Ready);

    // The fake server keeps writing to stderr on its own thread for several
    // seconds; the client must keep draining it and still be able to
    // exchange normal stdio traffic instead of stalling.
    let result = runtime
        .call("test/echo", Some(serde_json::json!({"x": 1})), Duration::from_secs(5))
        .expect("echo call should succeed while stderr is being spammed");
    assert_eq!(result["x"], 1);

    let _ = runtime.stop();
}

#[test]
fn stop_releases_pending_calls_immediately_instead_of_waiting_for_the_child_to_exit() {
    // A long grace timeout combined with a server that ignores stdin EOF
    // means the *old* behavior (releasing pending calls only once the
    // reader thread observes real EOF) would keep the call blocked for
    // close to the whole grace period. Asserting the call fails in well
    // under that window is what actually distinguishes "released as soon
    // as stop begins" from "released once the child eventually exits".
    let grace_timeout = Duration::from_secs(3);
    let mut config = base_config("stop_releases_pending");
    config.stop_grace_timeout = grace_timeout;
    config.stop_force_timeout = Duration::from_secs(3);
    config
        .extra_env
        .push(("FAKE_SERVER_IGNORE_STOP".to_string(), "1".to_string()));
    let (runtime, _events) = spawn_runtime(config);

    let status = runtime.start().expect("start should succeed");
    assert_eq!(status.state, RuntimeState::Ready);

    let runtime = Arc::new(runtime);
    let call_runtime = runtime.clone();
    let call_thread = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = call_runtime.call("test/echo", None, Duration::from_secs(30));
        (started.elapsed(), result)
    });

    // Give the call a moment to actually register as pending before we ask
    // the runtime to stop.
    std::thread::sleep(Duration::from_millis(100));

    let stopped = runtime.stop().expect("stop should still succeed via a forced kill");
    assert_eq!(stopped.state, RuntimeState::Stopped);

    let (elapsed, result) = call_thread.join().unwrap();
    assert!(matches!(result, Err(RuntimeError::Rpc(_))));
    assert!(
        elapsed < grace_timeout / 2,
        "pending call took {elapsed:?} to fail; it should be released as soon as stop begins, \
         well before the {grace_timeout:?} grace timeout elapses"
    );
}

#[test]
fn owner_lock_fails_immediately_from_a_separate_os_process() {
    let dir = unique_dir("owner_lock_cross_process");
    let lock_path = dir.join("runtime.lock");
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_fake_app_server"));

    let lock = OwnerLock::new(&lock_path);
    let guard = lock.try_acquire().unwrap().expect("first acquire should succeed");

    let busy_output = std::process::Command::new(&binary)
        .env("FAKE_SERVER_OWNER_LOCK_TRY_PATH", &lock_path)
        .env_remove("FAKE_SERVER_MODE")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run fake_app_server");
    let busy_stdout = String::from_utf8_lossy(&busy_output.stdout);
    assert!(
        busy_stdout.contains("OWNER_LOCK_BUSY"),
        "expected the lock to be reported busy from a separate process, stdout was: {busy_stdout}"
    );

    drop(guard);

    let acquired_output = std::process::Command::new(&binary)
        .env("FAKE_SERVER_OWNER_LOCK_TRY_PATH", &lock_path)
        .env_remove("FAKE_SERVER_MODE")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run fake_app_server");
    let acquired_stdout = String::from_utf8_lossy(&acquired_output.stdout);
    assert!(
        acquired_stdout.contains("OWNER_LOCK_ACQUIRED"),
        "expected a separate process to acquire the now-free lock, stdout was: {acquired_stdout}"
    );
}

#[test]
fn cleanup_that_cannot_be_confirmed_within_the_timeout_blocks_restart() {
    // An artificially zero-length grace/force timeout means `stop`'s single
    // post-kill `try_wait` check has no realistic chance to observe the
    // exit that `kill()` only just asked the OS to perform, so cleanup is
    // reported as unconfirmed (`RestartBlocked`) even though the process
    // itself is not adversarial. `FAKE_SERVER_IGNORE_STOP` guarantees the
    // grace phase cannot exit voluntarily either, so a forced kill is
    // always attempted.
    let mut config = base_config("cleanup_unconfirmed");
    config.stop_grace_timeout = Duration::ZERO;
    config.stop_force_timeout = Duration::ZERO;
    config
        .extra_env
        .push(("FAKE_SERVER_IGNORE_STOP".to_string(), "1".to_string()));
    let (runtime, _events) = spawn_runtime(config);

    runtime.start().expect("start should succeed");

    let stop_result = runtime.stop();
    assert!(matches!(stop_result, Err(RuntimeError::RestartBlocked)));
    assert!(runtime.snapshot().restart_blocked);
    assert_eq!(runtime.snapshot().state, RuntimeState::Failed);
}
