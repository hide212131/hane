//! Process-boundary integration tests: spawns the `fake_app_server` test
//! fixture as a real child process (via `AiRuntime`) to exercise the
//! initialize/initialized barrier and its request schema, finite lifecycle
//! timeouts, generation bumps across restarts, single-flight start/restart
//! coalescing (including a coalesced *failing* start wave), forced-kill
//! escalation, immediate release of pending calls on stop, stderr drain
//! under load, the runtime owner lock (including across real OS processes),
//! "no automatic resend after a crash", and invalidation of a late
//! completion from a timed-out/cancelled generation once a newer,
//! explicitly-started generation is already `Ready`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use hane_ai::{
    AiRuntime, OwnerLock, RejectAllServerRequests, RuntimeConfig, RuntimeError, RuntimeEvent, RuntimeEventKind,
    RuntimeState,
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

    // `runtime.start()` returns once the `initialized` notification has been
    // handed to the bounded writer queue, not once the writer thread has
    // actually flushed it to the child's stdin and the fake server has read
    // and recorded it. Wait for that record to actually appear (bounded)
    // instead of assuming it is already there by the time `start()` returns.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let recorded = loop {
        let contents = std::fs::read_to_string(&record_file).unwrap_or_default();
        if contents.lines().count() >= 2 {
            break contents;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fake server never recorded the INITIALIZED notification; recorded so far: {contents:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
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
    //
    // The normal fake server replies to `test/echo` immediately, which does
    // not reliably keep the call pending long enough to observe that
    // distinction (a fixed sleep before `stop()` is a guess about how fast
    // the reply round-trips). `hold_echo` mode instead lets `initialize`
    // succeed normally but never replies to `test/echo`, and records receipt
    // of the request to a marker file so the test can wait for confirmation
    // that the call is actually pending in the fake server before stopping.
    let grace_timeout = Duration::from_secs(3);
    let mut config = base_config("stop_releases_pending");
    config.stop_grace_timeout = grace_timeout;
    config.stop_force_timeout = Duration::from_secs(3);
    let dir = config.owner_lock_path.parent().unwrap().to_path_buf();
    let echo_received_file = dir.join("echo_received.log");
    config
        .extra_env
        .push(("FAKE_SERVER_MODE".to_string(), "hold_echo".to_string()));
    config.extra_env.push((
        "FAKE_SERVER_ECHO_RECEIVED_FILE".to_string(),
        echo_received_file.display().to_string(),
    ));
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

    // Wait (bounded) for the fake server to confirm it actually received the
    // `test/echo` request and is holding it, instead of assuming a fixed
    // sleep is long enough for the request to have registered as pending.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let received = std::fs::read_to_string(&echo_received_file)
            .map(|s| s.contains("ECHO_RECEIVED"))
            .unwrap_or(false);
        if received {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fake server never confirmed receiving the test/echo request"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

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
fn late_initialize_response_from_a_timed_out_generation_does_not_clobber_a_newer_ready_generation() {
    // `timeout_then_late_response` withholds its `initialize` reply from the
    // *first* spawned process until stdin closes, then sleeps briefly before
    // finally sending that (by then late) response and exiting. Every later
    // process spawned against the same marker file behaves normally. This
    // deterministically reproduces "an operation's completion arrives after
    // the coordinator already gave up on it and moved on" without depending
    // on incidental thread-scheduling timing.
    let mut config = base_config("timeout_then_late");
    config.start_timeout = Duration::from_millis(150);
    config.stop_grace_timeout = Duration::from_secs(2);
    config.stop_force_timeout = Duration::from_secs(2);
    let dir = config.owner_lock_path.parent().unwrap().to_path_buf();
    let marker = dir.join("timeout_once.marker");
    config
        .extra_env
        .push(("FAKE_SERVER_MODE".to_string(), "timeout_then_late_response".to_string()));
    config.extra_env.push((
        "FAKE_SERVER_TIMEOUT_ONCE_MARKER".to_string(),
        marker.display().to_string(),
    ));
    let (runtime, events_rx) = spawn_runtime(config);

    // First start: the fake server withholds its reply until stdin closes,
    // so the handshake must time out. Per the lifecycle-timeout contract,
    // the coordinator cancels this operation and lands on `Failed`
    // immediately, without waiting for `Child` cleanup to actually confirm
    // the (now cancelled) generation's process has exited; that cleanup
    // still runs synchronously on the coordinator thread afterward, so it is
    // the *next* explicit start below that ends up waiting for it.
    let first = runtime.start();
    assert!(matches!(first, Err(RuntimeError::Handshake(_))));
    let cancelled_status = runtime.snapshot();
    assert_eq!(cancelled_status.state, RuntimeState::Failed);

    // A second, explicit start spawns a fresh process (the marker file now
    // exists, so this one answers `initialize` immediately) and must reach
    // `Ready` on a newer generation of its own.
    let second = runtime.start().expect("second explicit start should succeed");
    assert_eq!(second.state, RuntimeState::Ready);
    assert!(second.generation > cancelled_status.generation);

    // The first generation's late `initialize` response is still read by its
    // own (now-closed) transport and only ever surfaces as a diagnostic
    // tagged with that stale generation, never as something that could be
    // mistaken for the second generation's own handshake.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut saw_stale_diagnostic = false;
    while std::time::Instant::now() < deadline {
        match events_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                if let RuntimeEventKind::Diagnostic(msg) = &event.kind {
                    if msg.contains("late or unknown response") {
                        assert!(
                            event.generation < second.generation,
                            "the stale generation's late response ({}) must predate the newer generation ({})",
                            event.generation,
                            second.generation
                        );
                        saw_stale_diagnostic = true;
                        break;
                    }
                }
            }
            Err(_) => continue,
        }
    }
    assert!(
        saw_stale_diagnostic,
        "expected a diagnostic about the timed-out generation's late `initialize` response"
    );

    // Crucially, observing that stale completion must not have clobbered the
    // newer, explicitly-started generation's `Ready` status.
    let status = runtime.snapshot();
    assert_eq!(status.state, RuntimeState::Ready);
    assert_eq!(status.generation, second.generation);

    let _ = runtime.stop();
}

#[test]
fn initialize_timeout_fails_every_coalesced_waiter_immediately_while_cleanup_continues_and_blocks_the_next_operation()
{
    // `timeout_then_late_response`'s first spawned process withholds its
    // `initialize` reply until stdin closes, then sleeps a fixed delay
    // before finally sending that (by then late) reply and exiting. That
    // fixed post-EOF delay gives a deterministic window during which the
    // timed-out operation's `Child` cleanup is still running, letting this
    // test assert that every coalesced `start()` caller already observes
    // that operation's own failure well before that window elapses, and
    // that only the *next* lifecycle operation (not this one) actually
    // waits for cleanup to confirm the old process is gone.
    let cleanup_delay = Duration::from_millis(300);
    let mut config = base_config("timeout_immediate_failed");
    config.start_timeout = Duration::from_millis(100);
    config.stop_grace_timeout = Duration::from_secs(5);
    config.stop_force_timeout = Duration::from_secs(5);
    let dir = config.owner_lock_path.parent().unwrap().to_path_buf();
    let marker = dir.join("timeout_once.marker");
    config
        .extra_env
        .push(("FAKE_SERVER_MODE".to_string(), "timeout_then_late_response".to_string()));
    config.extra_env.push((
        "FAKE_SERVER_TIMEOUT_ONCE_MARKER".to_string(),
        marker.display().to_string(),
    ));

    let (events_tx, _events_rx) = mpsc::channel();
    let handler = Arc::new(RejectAllServerRequests);
    let runtime = Arc::new(AiRuntime::spawn(config, handler, events_tx));

    // Several concurrent callers coalesce onto the one leader's attempt (see
    // `AiRuntime::start`); every one of them must observe the leader's own
    // Handshake failure, and must observe it fast -- well under the
    // fixture's post-EOF cleanup delay -- rather than being blocked until
    // `Child` cleanup actually confirms the old process has exited.
    let wave_started_at = std::time::Instant::now();
    let mut handles = Vec::new();
    for _ in 0..5 {
        let runtime = runtime.clone();
        handles.push(std::thread::spawn(move || runtime.start()));
    }
    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.join().unwrap());
    }
    let wave_elapsed = wave_started_at.elapsed();

    for outcome in &outcomes {
        assert!(
            matches!(outcome, Err(RuntimeError::Handshake(_))),
            "expected every coalesced caller to observe the timed-out operation's own failure, got {outcome:?}"
        );
    }
    assert!(
        wave_elapsed < cleanup_delay / 2,
        "every coalesced start() caller took {wave_elapsed:?} to fail, which is not comfortably under the \
         fixture's {cleanup_delay:?} post-EOF cleanup delay; no caller may be blocked waiting for cleanup \
         to actually confirm the process is gone"
    );

    // `Failed` must already be visible immediately, independent of the
    // cleanup that is still running in the background for the process that
    // timed out.
    assert_eq!(runtime.snapshot().state, RuntimeState::Failed);

    // The *next* lifecycle operation is a new wave and must still wait for
    // that cleanup to actually confirm the old process is gone before the
    // coordinator may spawn a new one.
    let next_started_at = std::time::Instant::now();
    let next = runtime.start().expect("next explicit start should succeed once cleanup completes");
    let next_elapsed = next_started_at.elapsed();
    assert_eq!(next.state, RuntimeState::Ready);
    assert!(
        next_elapsed >= cleanup_delay / 2,
        "the next lifecycle operation returned after only {next_elapsed:?}, which is not comfortably over \
         half of the fixture's {cleanup_delay:?} post-EOF cleanup delay; the coordinator must not start a \
         new operation before the previous one's cleanup is confirmed"
    );

    let _ = runtime.stop();
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
