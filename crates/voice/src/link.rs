//! One launch of the engine and its single authenticated connection.
//!
//! A dedicated thread owns everything slow: creating the channel, starting the
//! engine, waiting for it to connect, checking `hello`, then reading frames and
//! writing the recording controls. Nothing here touches the editor: validated
//! events leave through a bounded queue in arrival order ([`Inbound`]) and the
//! UI applies them in its own turn.
//!
//! One launch authenticates exactly one connection. Whatever ends it (the
//! engine exits, the pipe drops, the contract is broken, the engine stops
//! answering, Hane shuts down) the engine process and the workers it started
//! are ended, and the UI hears it once as [`Inbound::Lost`].

use crate::transport::{ChildProcess, EngineSpec, Handoff, Platform, VoiceConnection};
use crate::wire::{self, Context};
use hane_editor::{VoiceEnvelope, VoiceEvent};
use hane_link::Credentials;
use hane_link::frame::{self, MAX_FRAME_BYTES, MAX_PREAUTH_FRAME_BYTES};
use hane_link::transport::{Connection, Deadlined};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Events and the loss notice wait here for the UI. Past this the engine is
/// ahead of Hane and the recording is failed instead of buffered without bound.
pub const INBOUND_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// From starting the engine to its verified `hello`.
    pub connect: Duration,
    /// For one frame body to arrive, or one frame to be written.
    pub frame: Duration,
    /// For the first event (`session.start` or an early `session.end`) after
    /// `capture.begin`.
    pub begin: Duration,
    /// For `session.end` after `capture.stop`: the drain limit plus margin.
    pub drain: Duration,
    /// For `session.end` after `capture.cancel`.
    pub cancel: Duration,
    /// How long a closed connection gets before the engine is ended by force.
    pub exit_grace: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            frame: Duration::from_secs(5),
            begin: Duration::from_secs(10),
            drain: Duration::from_secs(35),
            cancel: Duration::from_secs(5),
            exit_grace: Duration::from_secs(2),
        }
    }
}

/// Why a launch ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The engine could not be started.
    Launch,
    /// The engine ended before it authenticated (code 2: it refused us).
    ExitedEarly(i32),
    ConnectTimeout,
    /// `hello` was refused: wrong token, process or protocol.
    Auth(&'static str),
    /// The connection closed (or the engine ended with this code).
    Disconnected(Option<i32>),
    /// A message broke the contract.
    Violation(&'static str),
    /// The engine did not answer in time.
    Unresponsive,
    /// More events arrived than Hane could take in.
    Overflow,
}

impl Failure {
    /// Shown in the status line. Never carries recognized text or a token.
    pub fn message(&self) -> String {
        match self {
            Self::Launch => "音声エンジンを起動できませんでした".to_owned(),
            Self::ExitedEarly(2) => "音声エンジンが接続を拒否して終了しました（認証・版の不一致）".to_owned(),
            Self::ExitedEarly(code) => format!("音声エンジンが接続前に終了しました（終了コード {code}）"),
            Self::ConnectTimeout => "音声エンジンが時間内に接続しませんでした".to_owned(),
            Self::Auth(why) => format!("音声エンジンの認証に失敗しました（{why}）"),
            Self::Disconnected(Some(code)) => format!("音声エンジンが終了しました（終了コード {code}）"),
            Self::Disconnected(None) => "音声エンジンとの接続が切れました".to_owned(),
            Self::Violation(why) => format!("音声エンジンが契約に違反しました（{why}）"),
            Self::Unresponsive => "音声エンジンが応答しなくなったため停止しました".to_owned(),
            Self::Overflow => "音声エンジンからの受信が追いつかず停止しました".to_owned(),
        }
    }
}

/// What the UI receives, in arrival order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inbound {
    Event(VoiceEnvelope),
    /// The launch is over. Whatever recording was running is cancelled.
    Lost(Failure),
}

/// Recording controls Hane sends.
pub(crate) enum Control {
    Begin { session_id: String, context: Option<Context> },
    Stop(String),
    Cancel(String),
}

/// Delivers to the UI queue, dropping everything once this launch is no longer
/// the current one.
#[derive(Clone)]
pub(crate) struct Sink {
    pub tx: async_channel::Sender<Inbound>,
    pub launch: u64,
    pub current: Arc<AtomicU64>,
}

impl Sink {
    fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire) == self.launch
    }

    /// `false` when the queue is full.
    fn event(&self, envelope: VoiceEnvelope) -> bool {
        if !self.is_current() {
            return true;
        }
        !matches!(
            self.tx.try_send(Inbound::Event(envelope)),
            Err(async_channel::TrySendError::Full(_))
        )
    }

    /// The final notice is not dropped for a full queue: it waits for room
    /// while Hane is still listening.
    fn lost(&self, failure: Failure, stop: &AtomicBool) {
        if !self.is_current() {
            return;
        }
        let mut message = Inbound::Lost(failure);
        while !stop.load(Ordering::Acquire) {
            match self.tx.try_send(message) {
                Err(async_channel::TrySendError::Full(back)) => {
                    message = back;
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return,
            }
        }
    }
}

/// Reads one frame if one is arriving, waiting up to `poll` for its first
/// byte. `Ok(None)` is "nothing yet"; the stream stays in sync.
fn poll_frame<C: Connection>(
    conn: &mut C,
    poll: Duration,
    frame_timeout: Duration,
    limit: usize,
    stop: &AtomicBool,
) -> Result<Option<Vec<u8>>, Failure> {
    let mut first = [0u8; 1];
    match conn.read_timeout(&mut first, poll) {
        Ok(0) => return Err(Failure::Disconnected(None)),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(None),
        Err(_) => return Err(Failure::Disconnected(None)),
    }
    let mut reader = Deadlined::new(conn, Instant::now() + frame_timeout, stop);
    let mut header = [first[0], 0, 0, 0];
    io::Read::read_exact(&mut reader, &mut header[1..])
        .map_err(|_| Failure::Violation("truncated frame header"))?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > limit {
        return Err(Failure::Violation("frame length out of range"));
    }
    frame::read_body(&mut reader, length)
        .map(Some)
        .map_err(|_| Failure::Violation("truncated frame"))
}

fn send_frame<C: Connection>(
    conn: &mut C,
    body: &[u8],
    timeout: Duration,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut writer = Deadlined::new(conn, Instant::now() + timeout, stop);
    frame::write_frame(&mut writer, body)
}

/// Waits for the engine to connect and authenticate. A connection whose
/// process is not the child is closed and ignored; the child's own `hello` is
/// checked for the token, the process id and `voice/1`.
fn authenticate(
    listener: &mut dyn crate::transport::VoiceListener,
    child: &mut dyn ChildProcess,
    credentials: &Credentials,
    timeouts: &Timeouts,
    stop: &AtomicBool,
) -> Result<Box<dyn VoiceConnection>, Failure> {
    let deadline = Instant::now() + timeouts.connect;
    loop {
        if stop.load(Ordering::Acquire) {
            return Err(Failure::Disconnected(None));
        }
        if let Some(code) = child.exit_code() {
            return Err(Failure::ExitedEarly(code));
        }
        if Instant::now() >= deadline {
            return Err(Failure::ConnectTimeout);
        }
        let mut conn = match listener.accept(Duration::from_millis(50)) {
            Ok(Some(conn)) => conn,
            Ok(None) => continue,
            Err(_) => return Err(Failure::Launch),
        };
        if conn.peer_pid() != Some(child.pid()) {
            // Not the process we started: close it and keep waiting.
            continue;
        }
        let body = {
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(timeouts.frame);
            poll_frame(&mut conn, wait, timeouts.frame, MAX_PREAUTH_FRAME_BYTES, stop)
                .ok()
                .flatten()
                .ok_or(Failure::Auth("no hello"))?
        };
        let hello = wire::parse_hello(&body).map_err(|error| Failure::Auth(error.0))?;
        if !credentials.verify(&hello.token) {
            return Err(Failure::Auth("token"));
        }
        if hello.pid != child.pid() {
            return Err(Failure::Auth("process id"));
        }
        send_frame(&mut conn, &wire::hello_ok(), timeouts.frame, stop)
            .map_err(|_| Failure::Auth("no reply"))?;
        return Ok(conn);
    }
}

/// How long one read waits before the controls are looked at again.
const POLL: Duration = Duration::from_millis(20);

/// The state of the session the loop is watching.
#[derive(Default)]
struct Watch {
    /// The session whose events Hane takes in. Cleared when Hane cancels or
    /// the engine ends it, so late events are dropped.
    accepting: Option<String>,
    /// The next event number expected from the accepted session. A jump
    /// ahead is a contract violation; a repeat is left to the editor, which
    /// ignores an identical one and refuses a different one.
    next_event_no: u64,
    /// Waiting for `session.start` (or an early end) after `capture.begin`.
    start_by: Option<(String, Instant)>,
    /// Waiting for `session.end` after stop or cancel.
    end_by: Option<(String, Instant)>,
}

impl Watch {
    fn expired(&self, now: Instant) -> bool {
        [&self.start_by, &self.end_by]
            .into_iter()
            .flatten()
            .any(|(_, by)| now >= *by)
    }
}

fn run_connected(
    conn: &mut Box<dyn VoiceConnection>,
    child: &mut dyn ChildProcess,
    controls: &mpsc::Receiver<Control>,
    timeouts: &Timeouts,
    sink: &Sink,
    stop: &AtomicBool,
) -> Failure {
    let mut watch = Watch::default();
    loop {
        if stop.load(Ordering::Acquire) {
            return Failure::Disconnected(None);
        }
        while let Ok(control) = controls.try_recv() {
            let now = Instant::now();
            let body = match control {
                Control::Begin { session_id, context } => {
                    watch.accepting = Some(session_id.clone());
                    watch.next_event_no = 1;
                    watch.start_by = Some((session_id.clone(), now + timeouts.begin));
                    watch.end_by = None;
                    wire::capture_begin(&session_id, context.as_ref())
                }
                Control::Stop(id) => {
                    watch.end_by = Some((id.clone(), now + timeouts.drain));
                    wire::capture_stop(&id)
                }
                Control::Cancel(id) => {
                    // The local side stops listening at once; only the end is
                    // still awaited so a silent engine is noticed.
                    if watch.accepting.as_deref() == Some(id.as_str()) {
                        watch.accepting = None;
                    }
                    watch.start_by = None;
                    watch.end_by = Some((id.clone(), now + timeouts.cancel));
                    wire::capture_cancel(&id)
                }
            };
            if send_frame(conn, &body, timeouts.frame, stop).is_err() {
                return Failure::Disconnected(child.exit_code());
            }
        }
        match poll_frame(conn, POLL, timeouts.frame, MAX_FRAME_BYTES, stop) {
            Err(Failure::Disconnected(_)) => {
                // Give the exit code a moment to appear for the message.
                let until = Instant::now() + Duration::from_millis(200);
                let mut code = child.exit_code();
                while code.is_none() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                    code = child.exit_code();
                }
                return Failure::Disconnected(code);
            }
            Err(failure) => return failure,
            Ok(None) => {
                if watch.expired(Instant::now()) {
                    return Failure::Unresponsive;
                }
            }
            Ok(Some(body)) => {
                let envelope = match wire::parse_event(&body) {
                    Ok(envelope) => envelope,
                    Err(error) => return Failure::Violation(error.0),
                };
                let id = envelope.session_id.clone();
                if matches!(envelope.event, VoiceEvent::SessionStart)
                    && watch.start_by.as_ref().is_some_and(|(s, _)| *s == id)
                {
                    watch.start_by = None;
                }
                let ends = matches!(envelope.event, VoiceEvent::SessionEnd { .. });
                if ends {
                    for slot in [&mut watch.start_by, &mut watch.end_by] {
                        if slot.as_ref().is_some_and(|(s, _)| *s == id) {
                            *slot = None;
                        }
                    }
                }
                // Another id, an id Hane already cancelled, or anything after
                // the end: not taken in.
                if watch.accepting.as_deref() != Some(id.as_str()) {
                    continue;
                }
                if envelope.event_no > watch.next_event_no {
                    return Failure::Violation("event number gap");
                }
                if envelope.event_no == watch.next_event_no {
                    watch.next_event_no += 1;
                }
                if ends {
                    watch.accepting = None;
                }
                if !sink.event(envelope) {
                    return Failure::Overflow;
                }
            }
        }
    }
}

/// The thread body of one launch.
fn run(
    platform: Arc<dyn Platform>,
    spec: EngineSpec,
    timeouts: Timeouts,
    controls: mpsc::Receiver<Control>,
    sink: Sink,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
) {
    let outcome: Result<(), Failure> = (|| {
        let credentials = Credentials::generate().ok_or(Failure::Launch)?;
        let name = format!("hane-voice-{}", credentials.instance_id());
        let mut listener = platform.listen(&name).map_err(|_| Failure::Launch)?;
        let handoff = Handoff {
            pipe_name: name,
            token: credentials.token().to_owned(),
            parent_pid: std::process::id(),
        };
        let mut child = platform.launch(&spec, &handoff).map_err(|_| Failure::Launch)?;
        drop(handoff);
        let mut conn = match authenticate(&mut *listener, &mut *child, &credentials, &timeouts, &stop) {
            Ok(conn) => conn,
            Err(failure) => {
                child.kill();
                return Err(failure);
            }
        };
        // One connection per launch: nothing else may connect.
        drop(listener);
        let failure = run_connected(&mut conn, &mut *child, &controls, &timeouts, &sink, &stop);
        // Closing the connection is what tells a healthy engine to release the
        // microphone and exit; the grace ends there, then the tree is ended.
        drop(conn);
        let until = Instant::now() + timeouts.exit_grace;
        while child.exit_code().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill();
        Err(failure)
    })();
    alive.store(false, Ordering::Release);
    if let Err(failure) = outcome
        && !stop.load(Ordering::Acquire)
    {
        sink.lost(failure, &stop);
    }
}

/// A running launch. Dropping it ends the engine.
pub struct EngineLink {
    controls: mpsc::Sender<Control>,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl EngineLink {
    pub(crate) fn start(
        platform: Arc<dyn Platform>,
        spec: EngineSpec,
        timeouts: Timeouts,
        sink: Sink,
    ) -> io::Result<Self> {
        let (controls, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let thread = std::thread::Builder::new().name("hane-voice-link".to_owned()).spawn({
            let (stop, alive) = (Arc::clone(&stop), Arc::clone(&alive));
            move || run(platform, spec, timeouts, receiver, sink, stop, alive)
        })?;
        Ok(Self {
            controls,
            stop,
            alive,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Whether the launch is still running (not yet lost or shut down).
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub(crate) fn send(&self, control: Control) -> bool {
        self.controls.send(control).is_ok()
    }

    /// Ends the launch and waits until the engine process is gone.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().expect("link thread").take() {
            let _ = thread.join();
        }
    }
}

impl Drop for EngineLink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    //! The protocol against an in-memory connection and a pretend process: the
    //! test plays the engine, frame by frame.

    use super::*;
    use crate::engine::{EngineLocation, VoiceControl, VoiceEngine};
    use crate::transport::{Platform, VoiceListener};
    use hane_link::testing::{MemoryConnection, duplex};
    use serde_json::{Value, json};
    use std::collections::VecDeque;

    const CHILD_PID: u32 = 4242;
    const SESSION: &str = "a162d4cb5747474d9cf7fcaf1833dabb";

    struct Conn {
        inner: MemoryConnection,
        pid: Option<u32>,
    }

    impl Connection for Conn {
        fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
            self.inner.read_timeout(buf, timeout)
        }

        fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
            self.inner.write_timeout(buf, timeout)
        }
    }

    impl VoiceConnection for Conn {
        fn peer_pid(&self) -> Option<u32> {
            self.pid
        }
    }

    #[derive(Default)]
    struct World {
        handoff: Mutex<Option<(String, String, u32)>>,
        queue: Mutex<VecDeque<Box<dyn VoiceConnection>>>,
        listener_alive: AtomicBool,
        exited: Mutex<Option<i32>>,
        killed: AtomicBool,
        launches: AtomicU64,
    }

    struct FakeListener(Arc<World>);

    impl VoiceListener for FakeListener {
        fn accept(&mut self, timeout: Duration) -> io::Result<Option<Box<dyn VoiceConnection>>> {
            if let Some(conn) = self.0.queue.lock().unwrap().pop_front() {
                return Ok(Some(conn));
            }
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            Ok(None)
        }
    }

    impl Drop for FakeListener {
        fn drop(&mut self) {
            self.0.listener_alive.store(false, Ordering::SeqCst);
        }
    }

    struct FakeChild(Arc<World>);

    impl ChildProcess for FakeChild {
        fn pid(&self) -> u32 {
            CHILD_PID
        }

        fn exit_code(&mut self) -> Option<i32> {
            *self.0.exited.lock().unwrap()
        }

        fn kill(&mut self) {
            self.0.killed.store(true, Ordering::SeqCst);
            *self.0.exited.lock().unwrap() = Some(-9);
        }
    }

    struct FakePlatform(Arc<World>);

    impl Platform for FakePlatform {
        fn listen(&self, _name: &str) -> io::Result<Box<dyn VoiceListener>> {
            self.0.listener_alive.store(true, Ordering::SeqCst);
            Ok(Box::new(FakeListener(Arc::clone(&self.0))))
        }

        fn launch(&self, _spec: &EngineSpec, handoff: &Handoff) -> io::Result<Box<dyn ChildProcess>> {
            self.0.launches.fetch_add(1, Ordering::SeqCst);
            *self.0.exited.lock().unwrap() = None;
            *self.0.handoff.lock().unwrap() = Some((
                handoff.pipe_name.clone(),
                handoff.token.clone(),
                handoff.parent_pid,
            ));
            Ok(Box::new(FakeChild(Arc::clone(&self.0))))
        }
    }

    struct Rig {
        world: Arc<World>,
        engine: VoiceEngine,
        inbound: async_channel::Receiver<Inbound>,
        scratch: std::path::PathBuf,
    }

    fn quick() -> Timeouts {
        Timeouts {
            connect: Duration::from_millis(600),
            frame: Duration::from_millis(400),
            begin: Duration::from_millis(400),
            drain: Duration::from_millis(600),
            cancel: Duration::from_millis(300),
            exit_grace: Duration::from_millis(100),
        }
    }

    fn new_rig(timeouts: Timeouts) -> Rig {
        let scratch = std::env::temp_dir().join(format!(
            "hane-voice-link-{}",
            hane_link::auth::random_hex(8).unwrap()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let exe = scratch.join("engine");
        std::fs::write(&exe, "").unwrap();
        let world = Arc::new(World::default());
        let (engine, inbound) = VoiceEngine::with_platform(
            Arc::new(FakePlatform(Arc::clone(&world))),
            EngineLocation {
                registration: None,
                explicit: Some(exe),
                extra_args: Vec::new(),
            },
            timeouts,
        );
        Rig {
            world,
            engine,
            inbound,
            scratch,
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.scratch);
        }
    }

    fn until<T>(mut check: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(found) = check() {
                return found;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn send(conn: &mut MemoryConnection, value: Value) {
        conn.send_frame(value.to_string().as_bytes()).unwrap();
    }

    fn recv(conn: &mut MemoryConnection) -> Option<Value> {
        let body = conn.receive_frame(Duration::from_secs(5)).unwrap()?;
        Some(serde_json::from_slice(&body).unwrap())
    }

    fn event(no: u64, mut body: Value) -> Value {
        body["sessionId"] = json!(SESSION);
        body["eventNo"] = json!(no);
        body
    }

    impl Rig {
        /// Begins recording and connects as the engine; returns the engine's end.
        fn connect(&self, pid: u32, token: Option<&str>, hello_pid: u32) -> MemoryConnection {
            let (_, handoff_token, parent) = until(|| self.world.handoff.lock().unwrap().clone());
            assert_eq!(parent, std::process::id());
            let (server, mut client) = duplex();
            self.world.queue.lock().unwrap().push_back(Box::new(Conn {
                inner: server,
                pid: Some(pid),
            }));
            send(
                &mut client,
                json!({"type":"hello","protocol":"voice/1","token":token.unwrap_or(&handoff_token),"pid":hello_pid}),
            );
            client
        }

        fn handshaken(&self) -> MemoryConnection {
            let mut client = self.connect(CHILD_PID, None, CHILD_PID);
            assert_eq!(recv(&mut client).unwrap(), json!({"type":"hello.ok","protocol":"voice/1"}));
            client
        }

        fn next(&self) -> Inbound {
            until(|| self.inbound.try_recv().ok())
        }
    }

    #[test]
    fn a_normal_run_delivers_the_events_in_order_and_the_controls_reach_the_engine() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        let begin = recv(&mut client).unwrap();
        assert_eq!(begin["type"], "capture.begin");
        assert_eq!(begin["sessionId"], SESSION);
        send(&mut client, event(1, json!({"type":"session.start"})));
        send(&mut client, event(2, json!({"type":"segment.raw","seq":1,"status":"ok","text":"一つ目","polish":"off"})));
        rig.engine.stop(SESSION);
        assert_eq!(recv(&mut client).unwrap()["type"], "capture.stop");
        send(&mut client, event(3, json!({"type":"session.end","status":"completed","lastSeq":1})));
        let numbers: Vec<u64> = (0..3)
            .map(|_| match rig.next() {
                Inbound::Event(envelope) => envelope.event_no,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(numbers, [1, 2, 3]);
        // One connection per launch: nothing may connect after this one.
        until(|| (!rig.world.listener_alive.load(Ordering::SeqCst)).then_some(()));
    }

    #[test]
    fn a_wrong_token_pid_or_protocol_closes_the_connection_and_ends_the_engine() {
        for (token, hello_pid, why) in [
            (Some("0".repeat(64)), CHILD_PID, "token"),
            (None, CHILD_PID + 1, "process id"),
        ] {
            let rig = new_rig(quick());
            rig.engine.begin(SESSION, None).unwrap();
            let mut client = rig.connect(CHILD_PID, token.as_deref(), hello_pid);
            assert_eq!(rig.next(), Inbound::Lost(Failure::Auth(why)));
            // The engine got no hello.ok: the connection simply ends.
            assert!(recv(&mut client).is_none());
            until(|| rig.world.killed.load(Ordering::SeqCst).then_some(()));
        }
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let (_, token, _) = until(|| rig.world.handoff.lock().unwrap().clone());
        let (server, mut client) = duplex();
        rig.world.queue.lock().unwrap().push_back(Box::new(Conn {
            inner: server,
            pid: Some(CHILD_PID),
        }));
        send(&mut client, json!({"type":"hello","protocol":"voice/2","token":token,"pid":CHILD_PID}));
        assert!(matches!(rig.next(), Inbound::Lost(Failure::Auth(_))));
        assert!(rig.world.killed.load(Ordering::SeqCst));
    }

    #[test]
    fn a_connection_from_another_process_is_closed_and_the_real_engine_still_gets_in() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let (_, token, _) = until(|| rig.world.handoff.lock().unwrap().clone());
        // A different process (or one whose id cannot be read) connects first
        // and presents the real token: it is dropped without an answer.
        for pid in [Some(CHILD_PID + 7), None] {
            let (server, mut rogue) = duplex();
            rig.world.queue.lock().unwrap().push_back(Box::new(Conn { inner: server, pid }));
            // The link may already have closed it: the write can fail.
            let hello = json!({"type":"hello","protocol":"voice/1","token":token,"pid":CHILD_PID});
            let _ = rogue.send_frame(hello.to_string().as_bytes());
            assert!(recv(&mut rogue).is_none());
        }
        let mut client = rig.handshaken();
        assert_eq!(recv(&mut client).unwrap()["type"], "capture.begin");
    }

    #[test]
    fn an_engine_that_never_connects_is_ended_after_the_deadline() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        assert_eq!(rig.next(), Inbound::Lost(Failure::ConnectTimeout));
        assert!(rig.world.killed.load(Ordering::SeqCst));
        // The next recording starts a fresh launch.
        rig.engine.begin(SESSION, None).unwrap();
        until(|| (rig.world.launches.load(Ordering::SeqCst) == 2).then_some(()));
    }

    #[test]
    fn an_engine_that_exits_before_connecting_is_reported_with_its_code() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        until(|| rig.world.handoff.lock().unwrap().clone());
        *rig.world.exited.lock().unwrap() = Some(2);
        assert_eq!(rig.next(), Inbound::Lost(Failure::ExitedEarly(2)));
    }

    #[test]
    fn a_jump_in_event_numbers_closes_the_connection() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        send(&mut client, event(1, json!({"type":"session.start"})));
        send(&mut client, event(3, json!({"type":"segment.raw","seq":1,"status":"silent"})));
        assert!(matches!(rig.next(), Inbound::Event(_)));
        assert_eq!(rig.next(), Inbound::Lost(Failure::Violation("event number gap")));
        assert!(recv(&mut client).is_none(), "the connection is closed");
        until(|| rig.world.killed.load(Ordering::SeqCst).then_some(()));
    }

    #[test]
    fn malformed_unknown_and_oversized_frames_are_violations() {
        for bad in [
            br#"{"type":"mystery","sessionId":"a162d4cb5747474d9cf7fcaf1833dabb","eventNo":1}"#.as_slice(),
            b"not json",
        ] {
            let rig = new_rig(quick());
            rig.engine.begin(SESSION, None).unwrap();
            let mut client = rig.handshaken();
            recv(&mut client).unwrap();
            client.send_frame(bad).unwrap();
            assert!(matches!(rig.next(), Inbound::Lost(Failure::Violation(_))));
        }
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        // Only the announced length is sent: nothing is allocated for it.
        client.send(&((MAX_FRAME_BYTES + 1) as u32).to_le_bytes()).unwrap();
        assert_eq!(rig.next(), Inbound::Lost(Failure::Violation("frame length out of range")));
    }

    #[test]
    fn a_frame_cut_in_the_middle_ends_the_recording_without_replaying_text() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        send(&mut client, event(1, json!({"type":"session.start"})));
        client.send(&200u32.to_le_bytes()).unwrap();
        client.send(b"{\"type\":\"segm").unwrap();
        drop(client);
        assert!(matches!(rig.next(), Inbound::Event(_)));
        assert!(matches!(rig.next(), Inbound::Lost(Failure::Violation(_))));
        assert!(rig.inbound.try_recv().is_err());
    }

    #[test]
    fn a_closed_connection_is_reported_once_and_the_engine_is_ended() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        drop(client);
        assert!(matches!(rig.next(), Inbound::Lost(Failure::Disconnected(_))));
        until(|| rig.world.killed.load(Ordering::SeqCst).then_some(()));
        std::thread::sleep(Duration::from_millis(60));
        assert!(rig.inbound.try_recv().is_err());
    }

    #[test]
    fn a_silent_engine_is_noticed_after_begin_stop_and_cancel() {
        // No session.start after capture.begin.
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        assert_eq!(rig.next(), Inbound::Lost(Failure::Unresponsive));
        // No session.end after capture.stop.
        let rig2 = new_rig(quick());
        rig2.engine.begin(SESSION, None).unwrap();
        let mut client = rig2.handshaken();
        recv(&mut client).unwrap();
        send(&mut client, event(1, json!({"type":"session.start"})));
        assert!(matches!(rig2.next(), Inbound::Event(_)));
        rig2.engine.stop(SESSION);
        assert_eq!(rig2.next(), Inbound::Lost(Failure::Unresponsive));
        assert!(rig2.world.killed.load(Ordering::SeqCst));
        drop(client);
    }

    #[test]
    fn cancel_closes_the_local_side_at_once_and_late_events_are_not_taken_in() {
        let rig = new_rig(Timeouts {
            cancel: Duration::from_secs(3),
            ..quick()
        });
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        send(&mut client, event(1, json!({"type":"session.start"})));
        assert!(matches!(rig.next(), Inbound::Event(_)));
        rig.engine.cancel(SESSION);
        rig.engine.cancel(SESSION); // repeating has no further effect on Hane's side
        assert_eq!(recv(&mut client).unwrap()["type"], "capture.cancel");
        // Events that were already in flight, and the engine's own end, are
        // dropped: the editor has stopped listening.
        send(&mut client, event(2, json!({"type":"segment.raw","seq":1,"status":"ok","text":"遅れて届く","polish":"off"})));
        send(&mut client, event(3, json!({"type":"session.end","status":"cancelled","lastSeq":1})));
        std::thread::sleep(Duration::from_millis(150));
        assert!(rig.inbound.try_recv().is_err());
        // The link stays usable for the next recording.
        let other = "b162d4cb5747474d9cf7fcaf1833dabb";
        rig.engine.begin(other, None).unwrap();
        assert_eq!(recv(&mut client).unwrap()["type"], "capture.cancel");
        assert_eq!(recv(&mut client).unwrap()["sessionId"], other);
    }

    #[test]
    fn events_for_another_session_are_not_taken_in() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        let mut stray = event(1, json!({"type":"session.start"}));
        stray["sessionId"] = json!("c162d4cb5747474d9cf7fcaf1833dabb");
        send(&mut client, stray);
        send(&mut client, event(1, json!({"type":"session.start"})));
        match rig.next() {
            Inbound::Event(envelope) => assert_eq!(envelope.session_id, SESSION),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_flood_beyond_the_queue_fails_the_recording_instead_of_growing_it() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        send(&mut client, event(1, json!({"type":"session.start"})));
        for no in 2..(INBOUND_CAPACITY as u64 + 20) {
            // The link closes once the queue overflows; later writes may fail.
            let body = event(no, json!({"type":"segment.raw","seq":no - 1,"status":"silent"}));
            let _ = client.send_frame(body.to_string().as_bytes());
        }
        // The queue holds its capacity; the loss notice follows once the UI
        // takes some out.
        let mut events = 0;
        loop {
            match rig.next() {
                Inbound::Event(_) => events += 1,
                Inbound::Lost(failure) => {
                    assert_eq!(failure, Failure::Overflow);
                    break;
                }
            }
        }
        assert!(events <= INBOUND_CAPACITY);
    }

    #[test]
    fn shutdown_ends_the_engine_and_nothing_is_delivered_after_it() {
        let rig = new_rig(quick());
        rig.engine.begin(SESSION, None).unwrap();
        let mut client = rig.handshaken();
        recv(&mut client).unwrap();
        rig.engine.shutdown();
        assert!(rig.world.killed.load(Ordering::SeqCst));
        assert!(rig.inbound.try_recv().is_err());
        assert!(recv(&mut client).is_none());
        // A new recording after a shutdown launches again.
        rig.engine.begin(SESSION, None).unwrap();
        until(|| (rig.world.launches.load(Ordering::SeqCst) == 2).then_some(()));
    }

    #[test]
    fn an_unusable_installation_refuses_to_record_with_a_reason() {
        let world = Arc::new(World::default());
        let (engine, _inbound) = VoiceEngine::with_platform(
            Arc::new(FakePlatform(Arc::clone(&world))),
            EngineLocation::default(),
            quick(),
        );
        let reason = engine.begin(SESSION, None).unwrap_err();
        assert!(reason.contains("whisparrot"));
        assert_eq!(world.launches.load(Ordering::SeqCst), 0);
    }
}
