//! One connection's life: authenticate, then answer requests one at a time.
//! Nothing here touches a document: edits and reads are handed to the UI
//! thread through [`Host`] and only their outcome comes back.

use crate::auth::Credentials;
use crate::frame::{self, FrameError, MAX_FRAME_BYTES, MAX_PREAUTH_FRAME_BYTES};
use crate::host::{Host, HostCall, SubmitError, TickClock, Ticket};
use crate::ledger::{self, Begin, Ledger};
use crate::message::{
    self, ErrorCode, HelloParams, HostRequest, LinkError, LinkResult, ParseError, Request,
};
use crate::transport::{Connection, Deadlined};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Concurrent connections an instance serves.
pub const MAX_CONNECTIONS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// From connecting to a verified `hello`.
    pub handshake: Duration,
    /// For the body of a frame whose length has arrived, and for a write.
    pub frame: Duration,
    /// How long an authenticated connection may stay silent.
    pub idle: Duration,
    /// How long a read of the document may wait for the UI thread.
    pub read_wait: Duration,
    /// How long to keep waiting for a request the UI thread already started.
    pub started_grace: Duration,
    /// The longest a mutation waits for the UI thread, whatever its deadline.
    pub max_mutation_wait: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(3),
            frame: Duration::from_secs(5),
            idle: Duration::from_secs(10 * 60),
            read_wait: Duration::from_secs(3),
            started_grace: Duration::from_secs(5),
            max_mutation_wait: Duration::from_secs(30),
        }
    }
}

/// State every connection of one service instance shares.
pub struct Shared {
    pub credentials: Credentials,
    pub host: Arc<dyn Host>,
    pub clock: Arc<dyn TickClock>,
    pub ledger: Mutex<Ledger>,
    pub stopping: AtomicBool,
    pub connections: AtomicUsize,
    pub timeouts: Timeouts,
}

impl Shared {
    pub fn new(
        credentials: Credentials,
        host: Arc<dyn Host>,
        clock: Arc<dyn TickClock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            credentials,
            host,
            clock,
            ledger: Mutex::new(Ledger::default()),
            stopping: AtomicBool::new(false),
            connections: AtomicUsize::new(0),
            timeouts: Timeouts::default(),
        })
    }
}

/// Why a connection ended, for tests and counters. Never carries data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ended {
    PeerClosed,
    Unauthenticated,
    Malformed,
    TimedOut,
    Stopped,
    WriteFailed,
}

fn write_response<C: Connection>(conn: &mut C, shared: &Shared, body: &[u8]) -> Result<(), Ended> {
    let mut writer = Deadlined::new(
        conn,
        Instant::now() + shared.timeouts.frame,
        &shared.stopping,
    );
    frame::write_frame(&mut writer, body).map_err(|_| Ended::WriteFailed)
}

/// Reads one frame with an idle wait for its first byte and a bounded wait for
/// the rest.
fn read_request_frame<C: Connection>(
    conn: &mut C,
    shared: &Shared,
    first_wait: Duration,
    limit: usize,
) -> Result<Vec<u8>, Ended> {
    let classify = |error: FrameError, stopping: bool| match error {
        FrameError::Closed => Ended::PeerClosed,
        FrameError::Io(ref io)
            if stopping || io.kind() == std::io::ErrorKind::ConnectionAborted =>
        {
            Ended::Stopped
        }
        FrameError::Io(ref io) if io.kind() == std::io::ErrorKind::TimedOut => Ended::TimedOut,
        _ => Ended::Malformed,
    };
    let length = {
        let mut reader = Deadlined::new(conn, Instant::now() + first_wait, &shared.stopping);
        frame::read_length(&mut reader, limit)
            .map_err(|error| classify(error, shared.stopping.load(Ordering::Acquire)))?
    };
    let mut reader = Deadlined::new(
        conn,
        Instant::now() + shared.timeouts.frame,
        &shared.stopping,
    );
    frame::read_body(&mut reader, length)
        .map_err(|error| classify(error, shared.stopping.load(Ordering::Acquire)))
}

/// Verifies the first frame. Every refusal is the same fixed answer and
/// closes the connection; nothing about the document is ever said before this
/// returns `Ok`.
fn handshake<C: Connection>(conn: &mut C, shared: &Shared) -> Result<(), Ended> {
    let refuse = |conn: &mut C, id: &serde_json::Value, code: ErrorCode| {
        let body = message::error_response(id, LinkError::not_applied(code));
        let _ = write_response(conn, shared, &body);
    };
    let body = read_request_frame(
        conn,
        shared,
        shared.timeouts.handshake,
        MAX_PREAUTH_FRAME_BYTES,
    )?;
    let request = match message::parse_request(&body) {
        Ok(request) if request.method == "hello" => request,
        Ok(request) => {
            refuse(conn, &request.id, ErrorCode::Unauthorized);
            return Err(Ended::Unauthenticated);
        }
        Err(_) => {
            refuse(conn, &serde_json::Value::Null, ErrorCode::Unauthorized);
            return Err(Ended::Unauthenticated);
        }
    };
    let Ok(hello) = serde_json::from_value::<HelloParams>(request.params.clone()) else {
        refuse(conn, &request.id, ErrorCode::Unauthorized);
        return Err(Ended::Unauthenticated);
    };
    if !shared.credentials.verify(&hello.token) {
        refuse(conn, &request.id, ErrorCode::Unauthorized);
        return Err(Ended::Unauthenticated);
    }
    let Some(agreed) = message::negotiate(hello.protocol.min, hello.protocol.max) else {
        // Authenticated, so the reason may be given. Still closed.
        refuse(conn, &request.id, ErrorCode::UnsupportedVersion);
        return Err(Ended::Unauthenticated);
    };
    let result = message::hello_result(agreed, shared.credentials.instance_id());
    write_response(conn, shared, &message::ok_response(&request.id, result))
}

/// Serves `conn` until it ends. The connection is dropped (closed) by the caller.
pub fn serve_connection<C: Connection>(conn: &mut C, shared: &Arc<Shared>) -> Ended {
    if let Err(ended) = handshake(conn, shared) {
        return ended;
    }
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            return Ended::Stopped;
        }
        let body = match read_request_frame(conn, shared, shared.timeouts.idle, MAX_FRAME_BYTES) {
            Ok(body) => body,
            Err(ended) => return ended,
        };
        let request = match message::parse_request(&body) {
            Ok(request) => request,
            Err(ParseError::Syntax) => {
                let body = message::error_response(
                    &serde_json::Value::Null,
                    LinkError::not_applied(ErrorCode::InvalidRequest),
                );
                let _ = write_response(conn, shared, &body);
                return Ended::Malformed;
            }
            Err(ParseError::Shape { id }) => {
                let body =
                    message::error_response(&id, LinkError::not_applied(ErrorCode::InvalidRequest));
                if let Err(ended) = write_response(conn, shared, &body) {
                    return ended;
                }
                continue;
            }
        };
        let id = request.id.clone();
        let outcome = handle_request(shared, request);
        let body = match outcome {
            Ok(result) => message::ok_response(&id, result),
            Err(error) => message::error_response(&id, error),
        };
        if let Err(ended) = write_response(conn, shared, &body) {
            return ended;
        }
    }
}

fn handle_request(shared: &Arc<Shared>, request: Request) -> LinkResult {
    let Some(parsed) = HostRequest::parse(&request.method, request.params) else {
        // `hello` again, or a method this service does not have.
        return Err(LinkError::not_applied(ErrorCode::InvalidRequest));
    };
    let request = parsed?;
    match request.operation_id().map(str::to_owned) {
        None => run_read(shared, request),
        Some(operation_id) => run_mutation(shared, request, &operation_id),
    }
}

fn submit(
    shared: &Arc<Shared>,
    request: HostRequest,
) -> Result<(Arc<Ticket>, mpsc::Receiver<LinkResult>), LinkError> {
    if shared.stopping.load(Ordering::Acquire) {
        return Err(LinkError::not_applied(ErrorCode::Busy));
    }
    let ticket = Ticket::new();
    let (reply, outcome) = mpsc::channel();
    let call = HostCall {
        request,
        instance_id: shared.credentials.instance_id().to_owned(),
        ticket: Arc::clone(&ticket),
        reply,
    };
    match shared.host.submit(call) {
        Ok(()) => Ok((ticket, outcome)),
        Err(SubmitError::Unavailable) => Err(LinkError::not_applied(ErrorCode::Busy)),
    }
}

/// Waits for the UI thread. `Err(None)` means the wait ended and the UI thread
/// never took the request; `Err(Some(_))` is a request it took but did not
/// answer in time, whose effect is unknown.
fn wait(
    shared: &Shared,
    ticket: &Ticket,
    outcome: &mpsc::Receiver<LinkResult>,
    first: Duration,
) -> Result<LinkResult, WaitFailed> {
    match outcome.recv_timeout(first) {
        Ok(result) => return Ok(result),
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(if ticket.started() {
                WaitFailed::Started
            } else {
                WaitFailed::NeverStarted
            });
        }
    }
    if ticket.abandon() {
        return Err(WaitFailed::NeverStarted);
    }
    // The UI thread already claimed it: its turn is short, so wait for it.
    outcome
        .recv_timeout(shared.timeouts.started_grace)
        .map_err(|_| WaitFailed::Started)
}

#[derive(Debug)]
enum WaitFailed {
    NeverStarted,
    Started,
}

fn run_read(shared: &Arc<Shared>, request: HostRequest) -> LinkResult {
    let (ticket, outcome) = submit(shared, request)?;
    match wait(shared, &ticket, &outcome, shared.timeouts.read_wait) {
        Ok(result) => result,
        Err(WaitFailed::NeverStarted) => Err(LinkError::not_applied(ErrorCode::Busy)),
        Err(WaitFailed::Started) => Err(LinkError::unknown(ErrorCode::Internal)),
    }
}

fn run_mutation(shared: &Arc<Shared>, request: HostRequest, operation_id: &str) -> LinkResult {
    let fingerprint = request.fingerprint();
    let begin = shared.ledger.lock().expect("ledger lock").begin(
        operation_id,
        &fingerprint,
        Instant::now(),
    );
    match begin {
        Begin::Fresh => {}
        Begin::InProgress => return Err(LinkError::unknown(ErrorCode::InProgress)),
        Begin::Replay(result) => return result,
        Begin::Conflict => return Err(LinkError::not_applied(ErrorCode::OperationConflict)),
        Begin::Full => return Err(ledger::FULL_ERROR),
    }
    let finish = |result: LinkResult| {
        shared
            .ledger
            .lock()
            .expect("ledger lock")
            .finish(operation_id, result.clone());
        result
    };
    let deadline = request.deadline_tick().unwrap_or(0);
    let now = shared.clock.now_ms();
    if now > deadline {
        return finish(Err(LinkError::not_applied(ErrorCode::DeadlineExceeded)));
    }
    let wait_for = Duration::from_millis(deadline - now).min(shared.timeouts.max_mutation_wait);
    let (ticket, outcome) = match submit(shared, request) {
        Ok(submitted) => submitted,
        Err(error) => {
            // Nothing reached the editor: a resend may still be applied.
            shared
                .ledger
                .lock()
                .expect("ledger lock")
                .forget(operation_id);
            return Err(error);
        }
    };
    match wait(shared, &ticket, &outcome, wait_for) {
        Ok(result) => finish(result),
        Err(WaitFailed::NeverStarted) => {
            finish(Err(LinkError::not_applied(ErrorCode::DeadlineExceeded)))
        }
        Err(WaitFailed::Started) => {
            // The edit may have happened. Keep the record open and let the
            // late answer, if one comes, settle it for a resend.
            let shared = Arc::clone(shared);
            let operation_id = operation_id.to_owned();
            std::thread::spawn(move || {
                if let Ok(result) = outcome.recv_timeout(Duration::from_secs(60)) {
                    shared
                        .ledger
                        .lock()
                        .expect("ledger lock")
                        .finish(&operation_id, result);
                }
            });
            Err(LinkError::unknown(ErrorCode::DeadlineExceeded))
        }
    }
}
