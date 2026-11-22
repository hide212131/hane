//! Scenario tests of the connection protocol over the in-memory transport:
//! authentication, version negotiation, framing limits, resend handling and
//! deadlines, with a scripted stand-in for the editor side.

use hane_link::host::{Host, HostCall, SubmitError, TickClock};
use hane_link::message::{HostRequest, LinkError};
use hane_link::server::Shared;
use hane_link::service::RunningService;
use hane_link::testing::{ManualClock, MemoryConnection, MemoryConnector, listener};
use hane_link::{Credentials, ErrorCode};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(3);

/// What the scripted editor side does with a call.
type Script = Box<dyn FnMut(&HostCall) -> Option<hane_link::LinkResult> + Send>;

struct ScriptedHost {
    calls: Sender<HostCall>,
    submitted: AtomicUsize,
    refuse: Mutex<bool>,
}

impl Host for ScriptedHost {
    fn submit(&self, call: HostCall) -> Result<(), SubmitError> {
        if *self.refuse.lock().unwrap() {
            return Err(SubmitError::Unavailable);
        }
        self.submitted.fetch_add(1, Ordering::SeqCst);
        self.calls.send(call).map_err(|_| SubmitError::Unavailable)
    }
}

struct Fixture {
    connector: MemoryConnector,
    service: Option<RunningService>,
    clock: Arc<ManualClock>,
    host: Arc<ScriptedHost>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    /// The pump plays the UI thread: it claims each ticket like the real one
    /// and answers as `script` says (`None` = never answer).
    fn new(mut script: Script) -> Self {
        let (calls, incoming): (Sender<HostCall>, Receiver<HostCall>) = mpsc::channel();
        let host = Arc::new(ScriptedHost {
            calls,
            submitted: AtomicUsize::new(0),
            refuse: Mutex::new(false),
        });
        let clock = ManualClock::new(1_000);
        let (listener, connector) = listener();
        let shared = Shared::new(
            Credentials::from_parts("instance-1", TOKEN),
            host.clone() as Arc<dyn Host>,
            clock.clone() as Arc<dyn TickClock>,
        );
        let pump_clock = clock.clone();
        let pump = std::thread::spawn(move || {
            while let Ok(call) = incoming.recv() {
                let deadline = call.request.deadline_tick();
                if call.ticket.begin(pump_clock.now_ms(), deadline).is_err() {
                    continue;
                }
                if let Some(result) = script(&call) {
                    let _ = call.reply.send(result);
                }
                // `None`: hold the reply sender so the caller times out.
                else {
                    std::mem::forget(call.reply);
                }
            }
        });
        Self {
            connector,
            service: Some(RunningService::start(listener, shared)),
            clock,
            host,
            pump: Some(pump),
        }
    }

    fn echo() -> Self {
        Self::new(Box::new(|call| {
            Some(Ok(match &call.request {
                HostRequest::ContextGet(_) => json!({"kind": "context"}),
                HostRequest::Insert(params) => json!({"kind": "insert", "text": params.text}),
                HostRequest::Replace(_) => json!({"kind": "replace"}),
            }))
        }))
    }

    fn connect(&self) -> MemoryConnection {
        self.connector.connect()
    }

    fn authenticated(&self) -> MemoryConnection {
        let mut conn = self.connect();
        let reply = call(&mut conn, 1, "hello", hello(TOKEN));
        assert!(reply.get("result").is_some(), "{reply}");
        conn
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.take().map(RunningService::stop);
        drop(self.pump.take());
    }
}

fn hello(token: &str) -> Value {
    json!({
        "token": token,
        "minVersion": {"major": 1, "minor": 0},
        "maxVersion": {"major": 1, "minor": 0},
    })
}

fn send(conn: &mut MemoryConnection, id: u64, method: &str, params: Value) {
    let body = serde_json::to_vec(&json!({"id": id, "method": method, "params": params})).unwrap();
    conn.send_frame(&body).unwrap();
}

fn read(conn: &mut MemoryConnection) -> Value {
    let body = conn.receive_frame(WAIT).unwrap().expect("a response frame");
    serde_json::from_slice(&body).unwrap()
}

fn call(conn: &mut MemoryConnection, id: u64, method: &str, params: Value) -> Value {
    send(conn, id, method, params);
    read(conn)
}

fn insert_params(operation: &str, deadline: u64, text: &str) -> Value {
    json!({
        "operationId": operation, "deadlineTick": deadline.to_string(), "hwnd": 7,
        "documentId": "d", "expectedRevision": "1",
        "expectedSelection": {"anchorByte": 0, "activeByte": 0}, "text": text,
    })
}

fn error_of(reply: &Value) -> (String, String) {
    let error = &reply["error"];
    (
        error["code"].as_str().unwrap().to_owned(),
        error["effect"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn hello_with_the_right_token_negotiates_and_reports_the_instance() {
    let fixture = Fixture::echo();
    let mut conn = fixture.connect();
    let reply = call(&mut conn, 1, "hello", hello(TOKEN));
    assert_eq!(reply["id"], json!(1));
    assert_eq!(reply["result"]["protocol"], json!({"major": 1, "minor": 0}));
    assert_eq!(reply["result"]["instanceId"], json!("instance-1"));
    let features = reply["result"]["features"].as_array().unwrap();
    assert!(features.contains(&json!("edit.insert")));
    assert_eq!(
        reply["result"]["limits"]["maxEditTextBytes"],
        json!(32 * 1024)
    );
}

#[test]
fn a_wrong_token_gets_a_fixed_refusal_and_the_connection_closes() {
    let fixture = Fixture::echo();
    for token in ["", "wrong", &TOKEN.replace('0', "1")] {
        let mut conn = fixture.connect();
        let reply = call(&mut conn, 5, "hello", hello(token));
        assert_eq!(
            error_of(&reply),
            ("UNAUTHORIZED".into(), "not_applied".into())
        );
        assert!(reply.get("result").is_none());
        assert!(conn.receive_frame(WAIT).unwrap().is_none(), "closed");
    }
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 0);
}

#[test]
fn nothing_but_hello_is_served_before_authentication() {
    let fixture = Fixture::echo();
    let mut conn = fixture.connect();
    let reply = call(&mut conn, 1, "context.get", json!({"hwnd": 7}));
    assert_eq!(error_of(&reply).0, "UNAUTHORIZED");
    assert!(conn.receive_frame(WAIT).unwrap().is_none());
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 0);
}

#[test]
fn garbage_and_oversized_first_frames_close_without_an_allocation_or_a_leak() {
    let fixture = Fixture::echo();
    let mut conn = fixture.connect();
    conn.send_frame(b"not json").unwrap();
    assert_eq!(error_of(&read(&mut conn)).0, "UNAUTHORIZED");
    assert!(conn.receive_frame(WAIT).unwrap().is_none());

    // A length above the pre-authentication limit is refused on the header.
    let mut conn = fixture.connect();
    conn.send(&(64 * 1024u32).to_le_bytes()).unwrap();
    assert!(
        conn.receive_frame(WAIT).unwrap().is_none(),
        "closed without a reply"
    );
}

#[test]
fn a_silent_peer_is_dropped_at_the_handshake_timeout() {
    // Time is real here: the wait is the production 3 s handshake window.
    let fixture = Fixture::echo();
    let mut conn = fixture.connect();
    let started = std::time::Instant::now();
    assert!(
        conn.receive_frame(Duration::from_secs(6))
            .unwrap()
            .is_none()
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
}

#[test]
fn a_major_mismatch_is_reported_after_authentication_and_closes() {
    let fixture = Fixture::echo();
    let mut conn = fixture.connect();
    let params = json!({
        "token": TOKEN,
        "minVersion": {"major": 2, "minor": 0},
        "maxVersion": {"major": 2, "minor": 1},
    });
    let reply = call(&mut conn, 1, "hello", params);
    assert_eq!(
        error_of(&reply),
        ("UNSUPPORTED_VERSION".into(), "not_applied".into())
    );
    assert!(conn.receive_frame(WAIT).unwrap().is_none());
}

#[test]
fn a_newer_minor_is_spoken_at_the_servers_minor() {
    let fixture = Fixture::echo();
    let mut conn = fixture.connect();
    let params = json!({
        "token": TOKEN,
        "minVersion": {"major": 1, "minor": 0},
        "maxVersion": {"major": 1, "minor": 9},
    });
    let reply = call(&mut conn, 1, "hello", params);
    assert_eq!(reply["result"]["protocol"], json!({"major": 1, "minor": 0}));
}

#[test]
fn requests_after_hello_are_answered_with_their_own_id() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    let reply = call(&mut conn, 42, "context.get", json!({"hwnd": 7}));
    assert_eq!(reply["id"], json!(42));
    assert_eq!(reply["result"], json!({"kind": "context"}));
}

#[test]
fn unknown_methods_bad_params_and_a_second_hello_fail_without_closing() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    for (method, params) in [
        ("edit.nope", json!({})),
        ("hello", hello(TOKEN)),
        ("context.get", json!({"beforeChars": 3})),
        ("edit.insert", json!({"operationId": "x"})),
    ] {
        let reply = call(&mut conn, 9, method, params);
        assert_eq!(
            error_of(&reply),
            ("INVALID_REQUEST".into(), "not_applied".into()),
            "{method}"
        );
    }
    // Still usable.
    assert!(
        call(&mut conn, 10, "context.get", json!({"hwnd": 7}))
            .get("result")
            .is_some()
    );
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 1);
}

#[test]
fn a_syntax_error_after_hello_closes_the_connection() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    conn.send_frame(b"{broken").unwrap();
    assert_eq!(error_of(&read(&mut conn)).0, "INVALID_REQUEST");
    assert!(conn.receive_frame(WAIT).unwrap().is_none());
}

#[test]
fn a_frame_over_the_limit_closes_without_reading_the_body() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    conn.send(&((256 * 1024 + 1) as u32).to_le_bytes()).unwrap();
    assert!(conn.receive_frame(WAIT).unwrap().is_none());
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 0);
}

#[test]
fn an_edit_text_over_32_kib_is_refused_before_reaching_the_editor() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    let big = "a".repeat(32 * 1024 + 1);
    let reply = call(
        &mut conn,
        2,
        "edit.insert",
        insert_params("op-big", 99_999, &big),
    );
    assert_eq!(
        error_of(&reply),
        ("INVALID_REQUEST".into(), "not_applied".into())
    );
    let ok = "a".repeat(32 * 1024);
    let reply = call(
        &mut conn,
        3,
        "edit.insert",
        insert_params("op-ok", 99_999, &ok),
    );
    assert!(reply.get("result").is_some());
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 1);
}

#[test]
fn a_resend_of_a_finished_insert_is_answered_from_the_record_not_applied_again() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    let params = insert_params("op-1", 99_999, "こんにちは");
    let first = call(&mut conn, 2, "edit.insert", params.clone());
    // Even from another connection of the same instance, with a new request id.
    let mut other = fixture.authenticated();
    let again = call(&mut other, 77, "edit.insert", params);
    assert_eq!(first["result"], again["result"]);
    assert_eq!(again["id"], json!(77));
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 1);
}

#[test]
fn the_same_operation_id_with_other_content_conflicts() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    call(
        &mut conn,
        2,
        "edit.insert",
        insert_params("op-1", 99_999, "a"),
    );
    let reply = call(
        &mut conn,
        3,
        "edit.insert",
        insert_params("op-1", 99_999, "b"),
    );
    assert_eq!(
        error_of(&reply),
        ("OPERATION_CONFLICT".into(), "not_applied".into())
    );
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 1);
}

#[test]
fn an_expired_deadline_is_refused_without_reaching_the_editor() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    fixture.clock.set(5_000);
    let reply = call(
        &mut conn,
        2,
        "edit.insert",
        insert_params("op-late", 4_999, "a"),
    );
    assert_eq!(
        error_of(&reply),
        ("DEADLINE_EXCEEDED".into(), "not_applied".into())
    );
    assert_eq!(fixture.host.submitted.load(Ordering::SeqCst), 0);
    // The refusal is recorded: the resend gets the same answer.
    let again = call(
        &mut conn,
        3,
        "edit.insert",
        insert_params("op-late", 4_999, "a"),
    );
    assert_eq!(error_of(&again).0, "DEADLINE_EXCEEDED");
}

#[test]
fn an_unanswered_request_times_out_as_not_applied_when_it_never_started() {
    // The UI side never claims the ticket, so the deadline wins the race.
    let (calls, incoming) = mpsc::channel::<HostCall>();
    let host = Arc::new(ScriptedHost {
        calls,
        submitted: AtomicUsize::new(0),
        refuse: Mutex::new(false),
    });
    let clock = ManualClock::new(1_000);
    let (listener, connector) = listener();
    let shared = Shared::new(
        Credentials::from_parts("i", TOKEN),
        host.clone() as Arc<dyn Host>,
        clock as Arc<dyn TickClock>,
    );
    let service = RunningService::start(listener, shared);
    let mut conn = connector.connect();
    call(&mut conn, 1, "hello", hello(TOKEN));
    let reply = call(
        &mut conn,
        2,
        "edit.insert",
        insert_params("op-1", 1_300, "a"),
    );
    assert_eq!(
        error_of(&reply),
        ("DEADLINE_EXCEEDED".into(), "not_applied".into())
    );
    // The abandoned ticket can no longer be claimed late.
    let queued = incoming.try_recv().unwrap();
    assert!(queued.ticket.begin(1_000, Some(1_300)).is_err());
    service.stop();
}

#[test]
fn a_started_but_unanswered_edit_reports_unknown_and_a_resend_is_in_progress() {
    let fixture = Fixture::new(Box::new(|_| None));
    let mut conn = fixture.authenticated();
    // A short window: the pump claims the ticket at once, then stays silent.
    fixture.clock.set(10_000);
    let started = std::time::Instant::now();
    send(
        &mut conn,
        2,
        "edit.insert",
        insert_params("op-1", 10_200, "a"),
    );
    let reply = conn
        .receive_frame(Duration::from_secs(10))
        .unwrap()
        .map(|body| serde_json::from_slice::<Value>(&body).unwrap())
        .unwrap();
    assert_eq!(
        error_of(&reply),
        ("DEADLINE_EXCEEDED".into(), "unknown".into())
    );
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "waited out the grace"
    );
    let again = call(
        &mut conn,
        3,
        "edit.insert",
        insert_params("op-1", 10_200, "a"),
    );
    assert_eq!(error_of(&again), ("IN_PROGRESS".into(), "unknown".into()));
}

#[test]
fn an_overloaded_editor_side_refuses_with_busy_and_does_not_record_the_operation() {
    let fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    *fixture.host.refuse.lock().unwrap() = true;
    let reply = call(
        &mut conn,
        2,
        "edit.insert",
        insert_params("op-1", 99_999, "a"),
    );
    assert_eq!(error_of(&reply), ("BUSY".into(), "not_applied".into()));
    // Once the editor side is back, the same operation is applied.
    *fixture.host.refuse.lock().unwrap() = false;
    let reply = call(
        &mut conn,
        3,
        "edit.insert",
        insert_params("op-1", 99_999, "a"),
    );
    assert!(reply.get("result").is_some());
}

#[test]
fn a_rejection_from_the_editor_is_recorded_and_replayed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let fixture = Fixture::new(Box::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        Some(Err(LinkError::not_applied(ErrorCode::StaleVersion)))
    }));
    let mut conn = fixture.authenticated();
    let params = insert_params("op-1", 99_999, "a");
    let first = call(&mut conn, 2, "edit.insert", params.clone());
    let second = call(&mut conn, 3, "edit.insert", params);
    assert_eq!(
        error_of(&first),
        ("STALE_VERSION".into(), "not_applied".into())
    );
    assert_eq!(
        error_of(&second),
        ("STALE_VERSION".into(), "not_applied".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stopping_the_service_ends_open_connections_and_refuses_new_ones() {
    let mut fixture = Fixture::echo();
    let mut conn = fixture.authenticated();
    fixture.service.take().unwrap().stop();
    assert!(
        conn.receive_frame(Duration::from_secs(3))
            .unwrap()
            .is_none()
    );
    // Nothing accepts any more: a new connection is never served.
    let mut late = fixture.connect();
    late.send_frame(b"{}").unwrap();
    assert!(late.receive_frame(Duration::from_millis(600)).is_err());
}

#[test]
fn more_than_the_allowed_connections_are_refused_with_busy() {
    let fixture = Fixture::echo();
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(fixture.authenticated());
    }
    let mut extra = fixture.connect();
    let reply = read(&mut extra);
    assert_eq!(error_of(&reply), ("BUSY".into(), "not_applied".into()));
    drop(held);
}
