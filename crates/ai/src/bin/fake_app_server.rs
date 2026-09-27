//! A minimal stdio JSONL fake Codex App Server used only by `hane-ai`'s
//! integration tests to exercise process-boundary behavior (initialize
//! barrier, timeouts, graceful/forced shutdown, unsolicited server
//! requests) without depending on a real Codex binary.
//!
//! This is a test fixture, not a product artifact: it is not wired into
//! `crates/app` and is not part of Hane's shipped binary.
//!
//! Behavior is controlled entirely through environment variables so tests
//! can select a scenario per spawned process:
//! - `FAKE_SERVER_MODE=never_respond`: reads `initialize` but never replies,
//!   to exercise the client's start timeout.
//! - `FAKE_SERVER_MODE=crash_after_initialize`: exits immediately after
//!   receiving `initialize`, before replying.
//! - `FAKE_SERVER_MODE=crash_mid_request`: exits immediately instead of
//!   answering a `test/echo` request, to exercise mid-request failure.
//! - `FAKE_SERVER_MODE=close_stdout_after_initialize`: replies to
//!   `initialize` normally, then, once
//!   `FAKE_SERVER_CLOSE_STDOUT_TRIGGER_FILE=<path>` appears on disk, closes
//!   its stdout (fd 1 on Unix, the stdout handle on Windows) so the client's
//!   reader observes EOF, but keeps running and blocking on its own stdin
//!   read loop instead of exiting, only doing so once stdin itself sees EOF.
//!   The trigger file lets a test defer this until it has already confirmed
//!   the client reached `Ready`, instead of racing the client's handshake
//!   against a close that happens immediately after the reply is written.
//!   This reproduces "the reader thread observed the transport close but the
//!   child process is still alive" (e.g. a crashed communication channel, or
//!   a server-request handler panic) independent of the child's own process
//!   exiting on its own.
//! - `FAKE_SERVER_MODE=hold_echo`: replies to `initialize` normally but never
//!   replies to a `test/echo` request, to exercise a call that is genuinely
//!   still pending (as opposed to one that already completed) when the
//!   client asks to stop. Paired with `FAKE_SERVER_ECHO_RECEIVED_FILE=<path>`,
//!   which is appended with `ECHO_RECEIVED` as soon as the request is read,
//!   so a test can wait for that instead of assuming a fixed delay.
//! - `FAKE_SERVER_MODE=timeout_then_late_response`: paired with
//!   `FAKE_SERVER_TIMEOUT_ONCE_MARKER=<path>`. The first process spawned
//!   against a given marker path withholds its `initialize` reply (like
//!   `never_respond`) until stdin closes, then sleeps
//!   `FAKE_SERVER_TIMEOUT_ONCE_DELAY_MS` (default 300ms) and only then sends
//!   the (by then late) `initialize` response before exiting, to exercise a
//!   stale completion arriving from an already timed-out/cancelled
//!   generation. Every later process spawned against the same marker path
//!   (the marker file now exists) instead behaves exactly like `normal`, so a
//!   subsequent explicit start can succeed on its own generation.
//! - `FAKE_SERVER_EMIT_SERVER_REQUEST=1`: after replying to `initialize`,
//!   sends an unsolicited server-to-client request to exercise the "never
//!   auto-approve" contract.
//! - `FAKE_SERVER_STDERR_SPAM=1`: writes continuously to stderr to exercise
//!   the always-on stderr drain.
//! - `FAKE_SERVER_IGNORE_STOP=1`: after stdin closes (the client's graceful
//!   stop signal), sleeps instead of exiting, so the client must escalate
//!   to a forced kill.
//! - `FAKE_SERVER_RECORD_INIT_FILE=<path>`: appends the raw `initialize`
//!   request params as `INIT_PARAMS:{json}` and, once the `initialized`
//!   notification is received, appends `INITIALIZED`, so a test can assert
//!   on the exact schema the client sent.
//! - `FAKE_SERVER_OWNER_LOCK_TRY_PATH=<path>`: instead of running the normal
//!   stdio protocol loop, tries to acquire `hane_ai::OwnerLock` at the given
//!   path in this (separate) OS process, prints `OWNER_LOCK_ACQUIRED` or
//!   `OWNER_LOCK_BUSY` to stdout, and exits immediately. Used to verify the
//!   runtime owner lock is exclusive across real processes, not just across
//!   handles within one process.

use std::env;
use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

fn main() {
    if let Ok(path) = env::var("FAKE_SERVER_OWNER_LOCK_TRY_PATH") {
        let lock = hane_ai::OwnerLock::new(std::path::PathBuf::from(path));
        match lock.try_acquire() {
            Ok(Some(_guard)) => println!("OWNER_LOCK_ACQUIRED"),
            Ok(None) => println!("OWNER_LOCK_BUSY"),
            Err(e) => println!("OWNER_LOCK_ERROR:{e}"),
        }
        let _ = io::stdout().flush();
        return;
    }

    let mode = env::var("FAKE_SERVER_MODE").unwrap_or_else(|_| "normal".to_string());
    let emit_server_request = env::var("FAKE_SERVER_EMIT_SERVER_REQUEST").is_ok();
    let record_init_file = env::var("FAKE_SERVER_RECORD_INIT_FILE").ok();
    let echo_received_file = env::var("FAKE_SERVER_ECHO_RECEIVED_FILE").ok();
    let timeout_once_marker = env::var("FAKE_SERVER_TIMEOUT_ONCE_MARKER").ok();
    let timeout_once_delay_ms: u64 = env::var("FAKE_SERVER_TIMEOUT_ONCE_DELAY_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    // Only the first process spawned against a given marker path withholds
    // its `initialize` reply; every later one (the marker already exists)
    // behaves normally so a subsequent explicit start can actually succeed.
    let is_timeout_once_first = mode == "timeout_then_late_response"
        && match &timeout_once_marker {
            Some(path) => {
                let first = !std::path::Path::new(path).exists();
                if first {
                    let _ = std::fs::write(path, b"");
                }
                first
            }
            None => true,
        };
    let mut pending_initialize_id: Option<serde_json::Value> = None;

    if let Some(mut file) = env::var("FAKE_SERVER_SPAWN_MARKER_FILE")
        .ok()
        .and_then(|marker_path| std::fs::OpenOptions::new().create(true).append(true).open(marker_path).ok())
    {
        let _ = writeln!(file, "{}", std::process::id());
    }

    if env::var("FAKE_SERVER_STDERR_SPAM").is_ok() {
        thread::spawn(|| {
            let mut stderr = io::stderr();
            for i in 0..2000 {
                if writeln!(stderr, "fake-app-server stderr line {i}").is_err() {
                    break;
                }
                if stderr.flush().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = value.get("id").cloned();
        let method = value
            .get("method")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());

        match (id, method) {
            (Some(id), Some(method)) if method == "initialize" => {
                if let Some(path) = &record_init_file {
                    let params = value.get("params").cloned().unwrap_or(serde_json::Value::Null);
                    append_record(path, &format!("INIT_PARAMS:{params}"));
                }
                if mode == "never_respond" {
                    continue;
                }
                if mode == "crash_after_initialize" {
                    std::process::exit(1);
                }
                if mode == "timeout_then_late_response" && is_timeout_once_first {
                    pending_initialize_id = Some(id);
                    continue;
                }
                let response = serde_json::json!({"id": id, "result": {"ok": true}});
                if writeln!(stdout, "{response}").is_err() || stdout.flush().is_err() {
                    break;
                }
                if emit_server_request {
                    let request =
                        serde_json::json!({"id": "srv-1", "method": "test/serverRequest", "params": {}});
                    if writeln!(stdout, "{request}").is_err() || stdout.flush().is_err() {
                        break;
                    }
                }
                if mode == "close_stdout_after_initialize"
                    && let Ok(path) = env::var("FAKE_SERVER_CLOSE_STDOUT_TRIGGER_FILE")
                {
                    // Deferred: only closes stdout once the trigger file
                    // appears, so a test can wait for its own `start()` call
                    // to actually observe `Ready` first, instead of racing
                    // that confirmation against a close that happens right
                    // after this reply is written.
                    thread::spawn(move || loop {
                        if std::path::Path::new(&path).exists() {
                            close_stdout();
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    });
                }
            }
            (Some(id), Some(method)) if method == "test/echo" => {
                if mode == "crash_mid_request" {
                    std::process::exit(1);
                }
                if mode == "hold_echo" {
                    if let Some(path) = &echo_received_file {
                        append_record(path, "ECHO_RECEIVED");
                    }
                    continue;
                }
                let params = value.get("params").cloned().unwrap_or(serde_json::Value::Null);
                let response = serde_json::json!({"id": id, "result": params});
                if writeln!(stdout, "{response}").is_err() || stdout.flush().is_err() {
                    break;
                }
            }
            (Some(id), Some(method)) => {
                let response = serde_json::json!({
                    "id": id,
                    "error": {"code": -32601, "message": format!("unknown method: {method}")}
                });
                if writeln!(stdout, "{response}").is_err() || stdout.flush().is_err() {
                    break;
                }
            }
            (None, Some(method)) if method == "initialized" => {
                if let Some(path) = &record_init_file {
                    append_record(path, "INITIALIZED");
                }
            }
            _ => {
                // Other notifications and malformed lines need no reply.
            }
        }
    }

    // stdin closed: this is the client's graceful-stop signal. A held-back
    // `initialize` reply is sent only now, deliberately late relative to
    // whatever timeout the client already gave up waiting under.
    if let Some(id) = pending_initialize_id {
        thread::sleep(Duration::from_millis(timeout_once_delay_ms));
        let response = serde_json::json!({"id": id, "result": {"ok": true}});
        let _ = writeln!(stdout, "{response}");
        let _ = stdout.flush();
    }

    if env::var("FAKE_SERVER_IGNORE_STOP").is_ok() {
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
}

/// Closes this process's own OS-level stdout (fd 1 on Unix, the stdout
/// handle on Windows), not merely this handle's buffering, so the client's
/// reader observes EOF right away while this process keeps running and
/// blocking on its own stdin read loop.
fn close_stdout() {
    #[cfg(unix)]
    {
        // SAFETY: fd 1 is this process's own stdout, a valid open file
        // descriptor at this point.
        unsafe {
            libc::close(1);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        // SAFETY: stdout's raw handle is valid at this point; wrapping it in
        // an `OwnedHandle` and dropping it closes the underlying OS handle.
        let handle = io::stdout().as_raw_handle();
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
    }
}

fn append_record(path: &str, line: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}
