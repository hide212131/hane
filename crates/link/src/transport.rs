//! The byte stream a connection runs over, abstracted so the same protocol
//! code serves a Windows named pipe in the product and an in-memory pair in
//! tests.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a blocked read wakes to look at the stop flag.
pub const SLICE: Duration = Duration::from_millis(250);

/// A bidirectional byte stream with bounded waits. A wait that ends without
/// data or room reports [`io::ErrorKind::TimedOut`] and leaves the stream
/// usable; end of stream is `Ok(0)` from a read.
pub trait Connection: Send {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
    fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

/// Hands out accepted connections. `accept` waits at most `timeout` so the
/// accept loop can notice a stop request.
pub trait Listener: Send {
    type Conn: Connection + 'static;
    fn accept(&mut self, timeout: Duration) -> io::Result<Option<Self::Conn>>;
}

/// A [`Connection`] viewed as `Read` + `Write` that gives up at a deadline or
/// when the service stops, so the frame code never waits unboundedly.
pub struct Deadlined<'a, C: Connection> {
    conn: &'a mut C,
    deadline: Instant,
    stopping: &'a AtomicBool,
}

impl<'a, C: Connection> Deadlined<'a, C> {
    pub fn new(conn: &'a mut C, deadline: Instant, stopping: &'a AtomicBool) -> Self {
        Self {
            conn,
            deadline,
            stopping,
        }
    }

    fn remaining(&self) -> io::Result<Duration> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "stopping"));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "deadline"));
        }
        Ok(remaining.min(SLICE))
    }
}

impl<C: Connection> Read for Deadlined<'_, C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let slice = self.remaining()?;
            match self.conn.read_timeout(buf, slice) {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                other => return other,
            }
        }
    }
}

impl<C: Connection> Write for Deadlined<'_, C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            let slice = self.remaining()?;
            match self.conn.write_timeout(buf, slice) {
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                other => return other,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Connection for Box<dyn Connection> {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        (**self).read_timeout(buf, timeout)
    }

    fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        (**self).write_timeout(buf, timeout)
    }
}

trait ErasedListener: Send {
    fn accept_boxed(&mut self, timeout: Duration) -> io::Result<Option<Box<dyn Connection>>>;
}

impl<L: Listener> ErasedListener for L {
    fn accept_boxed(&mut self, timeout: Duration) -> io::Result<Option<Box<dyn Connection>>> {
        Ok(self
            .accept(timeout)?
            .map(|conn| Box::new(conn) as Box<dyn Connection>))
    }
}

/// Any [`Listener`], type-erased, so the app can pick the platform's listener
/// at run time and tests can substitute an in-memory one.
pub struct BoxListener(Box<dyn ErasedListener>);

impl BoxListener {
    pub fn new<L: Listener + 'static>(listener: L) -> Self {
        Self(Box::new(listener))
    }
}

impl Listener for BoxListener {
    type Conn = Box<dyn Connection>;

    fn accept(&mut self, timeout: Duration) -> io::Result<Option<Self::Conn>> {
        self.0.accept_boxed(timeout)
    }
}
