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

    if let Ok(marker_path) = env::var("FAKE_SERVER_SPAWN_MARKER_FILE") {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&marker_path) {
            let _ = writeln!(file, "{}", std::process::id());
        }
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
            }
            (Some(id), Some(method)) if method == "test/echo" => {
                if mode == "crash_mid_request" {
                    std::process::exit(1);
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

    // stdin closed: this is the client's graceful-stop signal.
    if env::var("FAKE_SERVER_IGNORE_STOP").is_ok() {
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }
}

fn append_record(path: &str, line: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}
