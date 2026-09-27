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

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::Value;

use crate::protocol::{self, ErrorObject, IncomingMessage, RequestId};

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
    Remote(ErrorObject),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Timeout => write!(f, "rpc call timed out"),
            RpcError::Disconnected => write!(f, "transport disconnected"),
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
    writer_tx: Sender<WriterCommand>,
    pending: Mutex<HashMap<RequestId, Sender<Result<Value, RpcError>>>>,
    next_id: Mutex<i64>,
}

impl RpcCore {
    pub fn call(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value, RpcError> {
        let id = {
            let mut guard = self.next_id.lock().unwrap();
            let current = *guard;
            *guard += 1;
            RequestId::Number(current)
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id.clone(), reply_tx);

        let line = protocol::encode_request(&id, method, params);
        if self.writer_tx.send(WriterCommand::Line(line)).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError::Disconnected);
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
        self.writer_tx
            .send(WriterCommand::Line(line))
            .map_err(|_| RpcError::Disconnected)
    }

    /// Closes the write side (e.g. the child's stdin), which is the polite
    /// way of asking a well-behaved stdio server to exit.
    pub fn close_writer(&self) {
        let _ = self.writer_tx.send(WriterCommand::Close);
    }

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
    events_tx: Sender<RpcEvent>,
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
                            let _ = events_tx.send(RpcEvent::Diagnostic(format!(
                                "late or unknown response id={id:?}"
                            )));
                        }
                    }
                }
                Ok(IncomingMessage::Notification { method, params }) => {
                    let _ = events_tx.send(RpcEvent::Notification { method, params });
                }
                Ok(IncomingMessage::ServerRequest { id, method, params }) => {
                    let outcome = handler.handle(&method, params);
                    let response_line = match outcome {
                        Ok(result) => protocol::encode_response_ok(&id, result),
                        Err(err) => protocol::encode_response_err(&id, err),
                    };
                    let _ = core.writer_tx.send(WriterCommand::Line(response_line));
                }
                Err(err) => {
                    let _ = events_tx.send(RpcEvent::Diagnostic(format!(
                        "malformed message ignored: {err}"
                    )));
                }
            }
        }
        core.fail_all_pending();
        on_closed();
    })
}

fn spawn_stderr(reader: Box<dyn Read + Send>, events_tx: Sender<RpcEvent>) -> JoinHandle<()> {
    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line_result in buffered.lines() {
            match line_result {
                Ok(line) => {
                    let _ = events_tx.send(RpcEvent::Diagnostic(format!("stderr: {line}")));
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
        events_tx: Sender<RpcEvent>,
        on_closed: Box<dyn FnOnce() + Send>,
    ) -> RpcTransport {
        let (writer_tx, writer_rx) = mpsc::channel();
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

    pub fn request_shutdown(&self) {
        self.core.close_writer();
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
        let (events_tx, events_rx) = mpsc::channel();
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
}
