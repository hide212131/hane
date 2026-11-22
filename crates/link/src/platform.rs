//! The clock and process facts the link needs from the operating system.

use crate::descriptor::ProcessProbe;
use crate::host::TickClock;

/// The system millisecond clock both processes share: `GetTickCount64` on
/// Windows. Elsewhere (tests, development) the time since this process began.
pub struct SystemTick;

impl TickClock for SystemTick {
    #[cfg(windows)]
    fn now_ms(&self) -> u64 {
        crate::windows::tick_count_ms()
    }

    #[cfg(not(windows))]
    fn now_ms(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64
    }
}

/// Real process lookups.
pub struct SystemProcesses;

impl ProcessProbe for SystemProcesses {
    #[cfg(windows)]
    fn creation_time(&self, pid: u32) -> Option<u64> {
        crate::windows::process_creation_time(pid)
    }

    /// Only liveness: the platform has no creation time worth comparing, so
    /// a live process reports a constant. (The product link runs on Windows.)
    #[cfg(not(windows))]
    fn creation_time(&self, pid: u32) -> Option<u64> {
        #[cfg(unix)]
        {
            // SAFETY: signal 0 only checks that the process exists.
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            alive.then_some(0)
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }
}

/// This process's own creation time, as the descriptor records it.
pub fn current_process_creation_time() -> Option<u64> {
    SystemProcesses.creation_time(std::process::id())
}
