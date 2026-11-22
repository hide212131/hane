//! In-memory transport and clock for tests of the link and of its users.
//! Not used by the product.

use crate::host::TickClock;
use crate::transport::{Connection, Listener};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Pipe {
    state: Mutex<PipeState>,
    ready: Condvar,
}

#[derive(Default)]
struct PipeState {
    bytes: VecDeque<u8>,
    closed: bool,
}

/// One end of an in-memory duplex stream. Dropping an end closes it.
pub struct MemoryConnection {
    incoming: Arc<Pipe>,
    outgoing: Arc<Pipe>,
}

/// Two connected ends: bytes written to one are read from the other.
pub fn duplex() -> (MemoryConnection, MemoryConnection) {
    let a = Arc::new(Pipe::default());
    let b = Arc::new(Pipe::default());
    (
        MemoryConnection {
            incoming: Arc::clone(&a),
            outgoing: Arc::clone(&b),
        },
        MemoryConnection {
            incoming: b,
            outgoing: a,
        },
    )
}

impl Drop for MemoryConnection {
    fn drop(&mut self) {
        for pipe in [&self.incoming, &self.outgoing] {
            pipe.state.lock().unwrap().closed = true;
            pipe.ready.notify_all();
        }
    }
}

impl Connection for MemoryConnection {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut state = self.incoming.state.lock().unwrap();
        if state.bytes.is_empty() && !state.closed {
            state = self.incoming.ready.wait_timeout(state, timeout).unwrap().0;
        }
        if state.bytes.is_empty() {
            return if state.closed {
                Ok(0)
            } else {
                Err(io::ErrorKind::TimedOut.into())
            };
        }
        let count = buf.len().min(state.bytes.len());
        for slot in &mut buf[..count] {
            *slot = state.bytes.pop_front().expect("counted");
        }
        Ok(count)
    }

    fn write_timeout(&mut self, buf: &[u8], _timeout: Duration) -> io::Result<usize> {
        let mut state = self.outgoing.state.lock().unwrap();
        if state.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        state.bytes.extend(buf);
        self.outgoing.ready.notify_all();
        Ok(buf.len())
    }
}

impl MemoryConnection {
    /// Writes all of `bytes`.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_timeout(bytes, Duration::from_secs(1))
            .map(|_| ())
    }

    /// Reads exactly `count` bytes within `timeout` or fails.
    pub fn receive(&mut self, count: usize, timeout: Duration) -> io::Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + timeout;
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut chunk = vec![0u8; count - out.len()];
            match self.read_timeout(&mut chunk, left) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(read) => out.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error),
            }
        }
        Ok(out)
    }

    /// Sends one frame.
    pub fn send_frame(&mut self, body: &[u8]) -> io::Result<()> {
        let mut wire = (body.len() as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(body);
        self.send(&wire)
    }

    /// Receives one frame, or `None` when the peer closed the stream.
    pub fn receive_frame(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let header = match self.receive(4, timeout) {
            Ok(header) => header,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        };
        let length = u32::from_le_bytes(header.try_into().expect("four bytes")) as usize;
        self.receive(length, timeout).map(Some)
    }
}

/// A listener fed by the test through [`MemoryListener::connect`].
pub struct MemoryListener {
    incoming: Arc<(Mutex<VecDeque<MemoryConnection>>, Condvar)>,
}

#[derive(Clone)]
pub struct MemoryConnector {
    incoming: Arc<(Mutex<VecDeque<MemoryConnection>>, Condvar)>,
}

pub fn listener() -> (MemoryListener, MemoryConnector) {
    let incoming = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
    (
        MemoryListener {
            incoming: Arc::clone(&incoming),
        },
        MemoryConnector { incoming },
    )
}

impl MemoryConnector {
    /// A new client end whose server end the listener will accept.
    pub fn connect(&self) -> MemoryConnection {
        let (client, server) = duplex();
        self.incoming.0.lock().unwrap().push_back(server);
        self.incoming.1.notify_all();
        client
    }
}

impl Listener for MemoryListener {
    type Conn = MemoryConnection;

    fn accept(&mut self, timeout: Duration) -> io::Result<Option<MemoryConnection>> {
        let mut queue = self.incoming.0.lock().unwrap();
        if queue.is_empty() {
            queue = self.incoming.1.wait_timeout(queue, timeout).unwrap().0;
        }
        Ok(queue.pop_front())
    }
}

/// A clock the test moves by hand.
#[derive(Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub fn new(start: u64) -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(start)))
    }

    pub fn set(&self, tick: u64) {
        self.0.store(tick, Ordering::SeqCst);
    }

    pub fn advance(&self, by: u64) {
        self.0.fetch_add(by, Ordering::SeqCst);
    }
}

impl TickClock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
