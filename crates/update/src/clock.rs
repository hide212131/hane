use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Time seam so expiry, skew and download deadlines can be tested without
/// sleeping.
pub trait Clock: Send + Sync {
    /// Wall clock in seconds since the Unix epoch (may be wrong on the host).
    fn now_unix(&self) -> i64;
    /// Monotonic milliseconds from an arbitrary origin, for deadlines.
    fn monotonic_ms(&self) -> u64;
}

#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
        }
    }

    fn monotonic_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
