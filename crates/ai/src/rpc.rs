//! stdio transport: framing, request/response correlation, notifications and
//! server-originated requests.
//!
//! - Reads are line-buffered so partial reads, UTF-8 characters split across
//!   OS-level reads, and multiple messages delivered in one read all decode
//!   correctly (`BufRead::lines` only yields once a full line has
//!   accumulated).
//! - Writes are serialized through a single writer thread so callers never
//!   interleave partial lines.
//! - Server-originated requests are never auto-approved: unless the caller
//!   supplies a handler, every server request is answered with an explicit
//!   "not supported" error so the server is not left waiting and no
//!   automatic approval is ever sent.
//! - stderr is drained continuously on its own thread so a slow or absent
//!   consumer can never cause the child to block on a full pipe.
//! - The outgoing write queue and the number of in-flight requests are
//!   bounded. Callers get an explicit [`RpcError::Backpressure`] instead of
//!   blocking, and the reader/stderr threads only ever *try* to hand events
//!   or server-request replies to a bounded queue: a full queue is reported
//!   as a diagnostic (or a backpressure error to the caller) and never stalls
//!   the thread that is draining the child's stdout/stderr.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use crate::protocol::{self, ErrorObject, IncomingMessage, RequestId};

/// Hard cap on concurrently in-flight requests. Once reached, `call` fails
/// fast with [`RpcError::Backpressure`] instead of growing the pending map
/// without bound.
const MAX_PENDING_REQUESTS: usize = 256;

/// Capacity of the bounded outgoing write queue. `call`/`notify` use a
/// non-blocking `try_send` against this queue so a stalled peer can never
/// block the caller indefinitely.
const WRITER_QUEUE_CAPACITY: usize = 256;

/// Handles a request the server sent to us. Implementations must respond
/// promptly (success or a definite decline); they must never silently
/// approve something they don't understand.
pub trait ServerRequestHandler: Send + Sync + 'static {
    fn handle(&self, method: &str, params: Option<Value>) -> Result<Value, ErrorObject>;
}

/// Default handler used when the caller does not provide one. It declines
/// every server request, which also guarantees approval requests are never
/// auto-approved.
pub struct RejectAllServerRequests;

impl ServerRequestHandler for RejectAllServerRequests {
    fn handle(&self, method: &str, _params: Option<Value>) -> Result<Value, ErrorObject> {
        Err(ErrorObject::method_not_found(method))
    }
}

#[derive(Debug, Clone)]
pub enum RpcError {
    Timeout,
    Disconnected,
    /// The outgoing queue is full or the in-flight request limit was
    /// reached. The caller must back off and retry later; this is never
    /// silently turned into blocking.
    Backpressure,
    Remote(ErrorObject),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Timeout => write!(f, "rpc call timed out"),
            RpcError::Disconnected => write!(f, "transport disconnected"),
            RpcError::Backpressure => {
                write!(f, "rpc request rejected: too many pending requests or a full outgoing queue")
            }
            RpcError::Remote(err) => write!(f, "remote error {}: {}", err.code, err.message),
        }
    }
}

impl std::error::Error for RpcError {}

/// Events surfaced to the owner of a transport: notifications from the
/// server and non-fatal diagnostics (stderr lines, malformed input, late
/// responses).
#[derive(Debug, Clone)]
pub enum RpcEvent {
    Notification { method: String, params: Option<Value> },
    Diagnostic(String),
}

enum WriterCommand {
    Line(String),
    Close,
}

/// The thread-safe, cloneable half of a transport used to send requests and
/// notifications. Kept separate from [`RpcTransport`] so callers can hold a
/// handle without owning the reader/writer/stderr threads.
pub struct RpcCore {
    writer_tx: SyncSender<WriterCommand>,
    pending: Mutex<HashMap<RequestId, Sender<Result<Value, RpcError>>>>,
    next_id: Mutex<i64>,
}

impl RpcCore {
    pub fn call(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value, RpcError> {
        let (id, reply_rx) = {
            // Bound the in-flight request count and reserve the pending slot
            // under a single lock so concurrent callers can never race past
            // the cap.
            let mut pending = self.pending.lock().unwrap();
            if pending.len() >= MAX_PENDING_REQUESTS {
                return Err(RpcError::Backpressure);
            }
            let id = {
                let mut guard = self.next_id.lock().unwrap();
                let current = *guard;
                *guard += 1;
                RequestId::Number(current)
            };
            let (reply_tx, reply_rx) = mpsc::channel();
            pending.insert(id.clone(), reply_tx);
            (id, reply_rx)
        };

        let line = protocol::encode_request(&id, method, params);
        match self.writer_tx.try_send(WriterCommand::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(RpcError::Backpressure);
            }
            Err(TrySendError::Disconnected(_)) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(RpcError::Disconnected);
            }
        }

        match reply_rx.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap().remove(&id);
                Err(RpcError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(RpcError::Disconnected),
        }
    }

    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), RpcError> {
        let line = protocol::encode_notification(method, params);
        match self.writer_tx.try_send(WriterCommand::Line(line)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(RpcError::Backpressure),
            Err(TrySendError::Disconnected(_)) => Err(RpcError::Disconnected),
        }
    }

    /// Closes the write side (e.g. the child's stdin), which is the polite
    /// way of asking a well-behaved stdio server to exit. This is only ever
    /// called from lifecycle/drop paths, never from the reader/stderr drain
    /// threads, so a blocking send here cannot stall draining.
    pub fn close_writer(&self) {
        let _ = self.writer_tx.send(WriterCommand::Close);
    }

    /// Immediately fails every currently pending request. Called both when
    /// the reader thread observes real EOF and proactively when a lifecycle
    /// shutdown is requested, so pending requests are released as soon as
    /// stop begins instead of waiting for the child to actually exit.
    fn fail_all_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(RpcError::Disconnected));
        }
    }
}

fn spawn_writer(mut writer: Box<dyn Write + Send>, rx: Receiver<WriterCommand>) -> JoinHandle<()> {
    thread::spawn(move || {
        for cmd in rx {
            match cmd {
                WriterCommand::Line(line) => {
                    if writer.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                    if writer.write_all(b"\n").is_err() {
                        break;
                    }
                    if writer.flush().is_err() {
                        break;
                    }
                }
                WriterCommand::Close => break,
            }
        }
        // `writer` is dropped here, closing the underlying handle (e.g. the
        // child's stdin), which signals EOF to a well-behaved peer.
    })
}

fn spawn_reader(
    reader: Box<dyn Read + Send>,
    core: Arc<RpcCore>,
    handler: Arc<dyn ServerRequestHandler>,
    events_tx: SyncSender<RpcEvent>,
    on_closed: Box<dyn FnOnce() + Send>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line_result in buffered.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            match protocol::parse_incoming(&line) {
                Ok(IncomingMessage::Response { id, outcome }) => {
                    let sender = core.pending.lock().unwrap().remove(&id);
                    match sender {
                        Some(tx) => {
                            let mapped = outcome.map_err(RpcError::Remote);
                            let _ = tx.send(mapped);
                        }
                        None => {
                            // Best-effort: never block the reader (which
                            // drains the child's stdout) on a full events
                            // queue.
                            let _ = events_tx.try_send(RpcEvent::Diagnostic(format!(
                                "late or unknown response id={id:?}"
                            )));
                        }
                    }
                }
                Ok(IncomingMessage::Notification { method, params }) => {
                    let _ = events_tx.try_send(RpcEvent::Notification { method, params });
                }
                Ok(IncomingMessage::ServerRequest { id, method, params }) => {
                    let outcome = handler.handle(&method, params);
                    let response_line = match outcome {
                        Ok(result) => protocol::encode_response_ok(&id, result),
                        Err(err) => protocol::encode_response_err(&id, err),
                    };
                    // Non-blocking: a full outgoing queue must not stall the
                    // reader thread that is draining the child's stdout.
                    if core.writer_tx.try_send(WriterCommand::Line(response_line)).is_err() {
                        let _ = events_tx.try_send(RpcEvent::Diagnostic(format!(
                            "dropped reply to server request id={id:?}: outgoing queue full or closed"
                        )));
                    }
                }
                Err(err) => {
                    let _ = events_tx.try_send(RpcEvent::Diagnostic(format!(
                        "malformed message ignored: {err}"
                    )));
                }
            }
        }
        core.fail_all_pending();
        on_closed();
    })
}

fn spawn_stderr(reader: Box<dyn Read + Send>, events_tx: SyncSender<RpcEvent>) -> JoinHandle<()> {
    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line_result in buffered.lines() {
            match line_result {
                Ok(line) => {
                    // Non-blocking: a slow/absent events consumer must never
                    // stop this thread from draining stderr, or a chatty
                    // child could block on a full stderr pipe.
                    let _ = events_tx.try_send(RpcEvent::Diagnostic(format!("stderr: {line}")));
                }
                Err(_) => break,
            }
        }
    })
}

/// Owns the reader/writer/stderr threads for one child process's stdio.
/// Dropping it closes the writer and joins the threads; by the time this is
/// dropped in `runtime`, the process has already been confirmed exited, so
/// the reader/stderr threads have already observed EOF or are about to.
pub struct RpcTransport {
    core: Arc<RpcCore>,
    reader_thread: Option<JoinHandle<()>>,
    writer_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
}

impl RpcTransport {
    pub fn spawn(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        stderr: Option<Box<dyn Read + Send>>,
        handler: Arc<dyn ServerRequestHandler>,
        events_tx: SyncSender<RpcEvent>,
        on_closed: Box<dyn FnOnce() + Send>,
    ) -> RpcTransport {
        let (writer_tx, writer_rx) = mpsc::sync_channel(WRITER_QUEUE_CAPACITY);
        let core = Arc::new(RpcCore {
            writer_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
        });

        let writer_thread = spawn_writer(writer, writer_rx);
        let reader_thread = spawn_reader(reader, core.clone(), handler, events_tx.clone(), on_closed);
        let stderr_thread = stderr.map(|s| spawn_stderr(s, events_tx));

        RpcTransport {
            core,
            reader_thread: Some(reader_thread),
            writer_thread: Some(writer_thread),
            stderr_thread,
        }
    }

    pub fn handle(&self) -> Arc<RpcCore> {
        self.core.clone()
    }

    /// Begins a graceful shutdown: closes our write side (stdin EOF for the
    /// child) and immediately releases every pending request instead of
    /// waiting for the child to actually exit or for each call's own
    /// timeout to elapse.
    pub fn request_shutdown(&self) {
        self.core.close_writer();
        self.core.fail_all_pending();
    }
}

impl Drop for RpcTransport {
    fn drop(&mut self) {
        self.core.close_writer();
        if let Some(t) = self.writer_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.reader_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.stderr_thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    struct RecordingHandler {
        seen: Mutex<Vec<String>>,
    }

    impl ServerRequestHandler for RecordingHandler {
        fn handle(&self, method: &str, _params: Option<Value>) -> Result<Value, ErrorObject> {
            self.seen.lock().unwrap().push(method.to_string());
            Err(ErrorObject::method_not_found(method))
        }
    }

    fn spawn_transport_over_pipes(
        handler: Arc<dyn ServerRequestHandler>,
    ) -> (
        RpcTransport,
        std::io::PipeWriter,
        std::io::PipeReader,
        Receiver<RpcEvent>,
        Receiver<()>,
    ) {
        let (to_client_r, to_client_w) = std::io::pipe().unwrap();
        let (from_client_r, from_client_w) = std::io::pipe().unwrap();
        let (events_tx, events_rx) = mpsc::sync_channel(1024);
        let (closed_tx, closed_rx) = mpsc::channel();
        let on_closed = Box::new(move || {
            let _ = closed_tx.send(());
        });
        let transport = RpcTransport::spawn(
            Box::new(to_client_r),
            Box::new(from_client_w),
            None,
            handler,
            events_tx,
            on_closed,
        );
        (transport, to_client_w, from_client_r, events_rx, closed_rx)
    }

    #[test]
    fn call_correlates_response_by_integer_id() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, mut server_read, _events, _closed) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        let responder = thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let n = server_read.read(&mut buf).unwrap();
            let request: Value = serde_json::from_slice(&buf[..n]).unwrap();
            let id = request["id"].clone();
            let resp = serde_json::json!({"id": id, "result": {"echoed": true}});
            writeln!(server_write, "{}", resp).unwrap();
        });

        let result = core
            .call("test/echo", Some(serde_json::json!({"a": 1})), Duration::from_secs(5))
            .unwrap();
        assert_eq!(result["echoed"], Value::Bool(true));
        responder.join().unwrap();
    }

    #[test]
    fn call_correlates_response_by_string_id_without_confusing_notifications() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, mut server_read, events_rx, _closed) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        let responder = thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let n = server_read.read(&mut buf).unwrap();
            let request: Value = serde_json::from_slice(&buf[..n]).unwrap();
            let id = request["id"].clone();
            // A notification arrives first, then the response.
            writeln!(server_write, "{}", serde_json::json!({"method": "session/event", "params": {"n": 1}})).unwrap();
            writeln!(server_write, "{}", serde_json::json!({"id": id, "result": "ok"})).unwrap();
        });

        let result = core.call("initialize", None, Duration::from_secs(5)).unwrap();
        assert_eq!(result, Value::String("ok".to_string()));
        responder.join().unwrap();

        let event = events_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match event {
            RpcEvent::Notification { method, .. } => assert_eq!(method, "session/event"),
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn multiple_messages_in_one_write_are_each_decoded() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, _server_read, events_rx, _closed) =
            spawn_transport_over_pipes(handler);
        let _core = transport.handle();

        let batch = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"method": "a", "params": 1}),
            serde_json::json!({"method": "b", "params": 2}),
            serde_json::json!({"method": "c", "params": 3}),
        );
        server_write.write_all(batch.as_bytes()).unwrap();
        server_write.flush().unwrap();

        let mut methods = Vec::new();
        for _ in 0..3 {
            match events_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                RpcEvent::Notification { method, .. } => methods.push(method),
                other => panic!("expected notification, got {other:?}"),
            }
        }
        assert_eq!(methods, vec!["a", "b", "c"]);
    }

    #[test]
    fn utf8_multibyte_character_split_across_writes_decodes_correctly() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, _server_read, events_rx, _closed) =
            spawn_transport_over_pipes(handler);
        let _core = transport.handle();

        // "日" is E6 97 A5 in UTF-8; split the write in the middle of the
        // character to exercise the buffered reader's boundary handling.
        let full = serde_json::json!({"method": "greet", "params": "日本語"}).to_string();
        let bytes = full.as_bytes();
        let split_at = bytes
            .windows(3)
            .position(|w| w == [0xE6, 0x97, 0xA5])
            .unwrap()
            + 1;

        server_write.write_all(&bytes[..split_at]).unwrap();
        server_write.flush().unwrap();
        thread::sleep(Duration::from_millis(20));
        server_write.write_all(&bytes[split_at..]).unwrap();
        server_write.write_all(b"\n").unwrap();
        server_write.flush().unwrap();

        match events_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            RpcEvent::Notification { method, params } => {
                assert_eq!(method, "greet");
                assert_eq!(params.unwrap(), Value::String("日本語".to_string()));
            }
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn server_request_is_never_auto_approved() {
        let handler = Arc::new(RecordingHandler {
            seen: Mutex::new(Vec::new()),
        });
        let (transport, mut server_write, mut server_read, _events, _closed) =
            spawn_transport_over_pipes(handler.clone());
        let _core = transport.handle();

        writeln!(
            server_write,
            "{}",
            serde_json::json!({"id": "srv-1", "method": "approveCommand", "params": {}})
        )
        .unwrap();
        server_write.flush().unwrap();

        let mut buf = [0u8; 4096];
        let n = server_read.read(&mut buf).unwrap();
        let response: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(response["id"], "srv-1");
        assert!(response.get("error").is_some());
        assert!(response.get("result").is_none());
        let seen = handler.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], "approveCommand");
    }

    #[test]
    fn malformed_line_does_not_stop_the_reader() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, _server_read, events_rx, _closed) =
            spawn_transport_over_pipes(handler);
        let _core = transport.handle();

        writeln!(server_write, "not json").unwrap();
        writeln!(server_write, "{}", serde_json::json!({"method": "after", "params": null})).unwrap();
        server_write.flush().unwrap();

        // First event is a diagnostic about the malformed line.
        match events_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            RpcEvent::Diagnostic(_) => {}
            other => panic!("expected diagnostic, got {other:?}"),
        }
        // The reader kept going and delivered the following notification.
        match events_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            RpcEvent::Notification { method, .. } => assert_eq!(method, "after"),
            other => panic!("expected notification, got {other:?}"),
        }
    }

    #[test]
    fn disconnect_fails_an_in_flight_call_instead_of_hanging_for_the_full_timeout() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, server_write, _server_read, _events, closed_rx) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        let call_thread = thread::spawn(move || core.call("test/echo", None, Duration::from_secs(30)));

        // Give the call time to register itself as pending before the peer
        // disappears, then simulate the child crashing mid-request.
        thread::sleep(Duration::from_millis(50));
        drop(server_write);

        assert_eq!(closed_rx.recv_timeout(Duration::from_secs(5)), Ok(()));
        let result = call_thread.join().unwrap();
        assert!(matches!(result, Err(RpcError::Disconnected)));
    }

    #[test]
    fn call_times_out_and_a_late_response_is_dropped_not_delivered_to_a_new_call() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, mut server_read, events_rx, _closed) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        let responder = thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let n = server_read.read(&mut buf).unwrap();
            let request: Value = serde_json::from_slice(&buf[..n]).unwrap();
            let id = request["id"].clone();
            // Reply only after the caller has already timed out.
            thread::sleep(Duration::from_millis(150));
            let resp = serde_json::json!({"id": id, "result": "late"});
            writeln!(server_write, "{}", resp).unwrap();
        });

        let result = core.call("slow", None, Duration::from_millis(20));
        assert!(matches!(result, Err(RpcError::Timeout)));
        responder.join().unwrap();

        match events_rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            RpcEvent::Diagnostic(msg) => assert!(msg.contains("late or unknown response")),
            other => panic!("expected diagnostic about late response, got {other:?}"),
        }
    }

    #[test]
    fn responses_delivered_out_of_order_are_correlated_to_the_right_caller() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, mut server_write, mut server_read, _events, _closed) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        let responder = thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut ids = Vec::new();
            while ids.len() < 2 {
                let n = server_read.read(&mut buf).unwrap();
                for line in std::str::from_utf8(&buf[..n]).unwrap().lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let request: Value = serde_json::from_str(line).unwrap();
                    ids.push(request["id"].clone());
                }
            }
            // Reply in the reverse order the requests were received in.
            for id in ids.into_iter().rev() {
                let resp = serde_json::json!({"id": id, "result": {"echo_id": id}});
                writeln!(server_write, "{}", resp).unwrap();
            }
        });

        let core_a = core.clone();
        let call_a = thread::spawn(move || core_a.call("first", None, Duration::from_secs(5)));
        thread::sleep(Duration::from_millis(20));
        let core_b = core.clone();
        let call_b = thread::spawn(move || core_b.call("second", None, Duration::from_secs(5)));

        let result_a = call_a.join().unwrap().unwrap();
        let result_b = call_b.join().unwrap().unwrap();
        responder.join().unwrap();

        // Each caller must see the reply addressed to its own request id,
        // regardless of the order the two responses arrived on the wire.
        assert_ne!(result_a["echo_id"], result_b["echo_id"]);
    }

    #[test]
    fn request_shutdown_fails_pending_calls_immediately_without_waiting_for_eof() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, _server_write, mut server_read, _events, _closed) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        let call_thread = thread::spawn(move || core.call("slow", None, Duration::from_secs(30)));

        // Wait for the request to actually be written before shutting down.
        let mut buf = [0u8; 4096];
        let _ = server_read.read(&mut buf).unwrap();

        // `_server_write` is still open (no EOF has been sent to our
        // reader), so if this still unblocks the call quickly it can only
        // be because `request_shutdown` proactively released pending
        // requests instead of waiting for the reader thread to observe EOF.
        transport.request_shutdown();

        let result = call_thread.join().unwrap();
        assert!(matches!(result, Err(RpcError::Disconnected)));
    }

    #[test]
    fn call_reports_backpressure_when_the_pending_request_limit_is_reached() {
        let handler = Arc::new(RejectAllServerRequests);
        let (transport, _server_write, _server_read, _events, _closed) =
            spawn_transport_over_pipes(handler);
        let core = transport.handle();

        // Fill every pending slot with calls that will never receive a
        // response, then confirm a new call fails fast instead of growing
        // the pending map without bound.
        let mut in_flight = Vec::new();
        for _ in 0..MAX_PENDING_REQUESTS {
            let core = core.clone();
            in_flight.push(thread::spawn(move || core.call("never_replied", None, Duration::from_secs(30))));
        }

        // Poll instead of sleeping a fixed amount: each probe that lands
        // before every spawned thread has registered itself simply times
        // out quickly (and cleans up after itself), so this converges
        // without being sensitive to how fast threads get scheduled.
        let mut observed_backpressure = false;
        for _ in 0..100 {
            match core.call("one_too_many", None, Duration::from_millis(20)) {
                Err(RpcError::Backpressure) => {
                    observed_backpressure = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(
            observed_backpressure,
            "expected backpressure once MAX_PENDING_REQUESTS in-flight calls were pending"
        );

        transport.request_shutdown();
        for handle in in_flight {
            let _ = handle.join();
        }
    }
}
