//! Cross-process shared/exclusive lock guarding every `AiSettings`
//! read-modify-write cycle and credential operation, distinct from
//! [`crate::owner_lock::OwnerLock`]'s runtime-ownership lock.
//!
//! Per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` section
//! 7.3: settings writes (including credential journal transactions) take the
//! **exclusive** mode; an inference turn or a fixed connectivity probe takes
//! the **shared** mode for the duration of that turn/probe so it can safely
//! read `settings_generation` and compare it against the running App
//! Server's generation without a concurrent settings write racing it
//! (TOCTOU). A settings save never waits for an in-progress turn/probe to
//! finish: it takes a non-blocking try-lock and is rejected immediately
//! ("AI processing in progress") if a shared holder already exists, rather
//! than blocking or implicitly cancelling that turn/probe.
//!
//! When a caller needs both this lock and the runtime owner lock, section 8
//! requires acquiring them in a fixed order: **runtime owner lock, then AI
//! settings lock** (never the reverse), so two callers taking both locks can
//! never deadlock against each other.
//!
//! Like [`crate::owner_lock::OwnerLock`], this is backed by an OS file lock,
//! not a PID file: acquisition is always a non-blocking try-lock, and the
//! lock is automatically released by the OS when the owning process exits or
//! every guard referencing it is dropped, without depending on that process
//! having run any cleanup code.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub struct AiSettingsLock {
    path: PathBuf,
}

/// Holding this guard means the current process holds the **exclusive** AI
/// settings lock: no other process holds it in either shared or exclusive
/// mode. Dropping it (or the process exiting) releases the OS-level lock.
pub struct AiSettingsExclusiveGuard {
    _file: File,
}

/// Holding this guard means the current process holds the **shared** AI
/// settings lock: no other process holds the lock in exclusive mode, though
/// other processes may concurrently hold it in shared mode too. Dropping it
/// (or the process exiting) releases the OS-level lock.
pub struct AiSettingsSharedGuard {
    _file: File,
}

impl AiSettingsLock {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        AiSettingsLock { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open(&self) -> io::Result<File> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).write(true).truncate(false).open(&self.path)
    }

    /// Attempts to acquire the lock in exclusive mode without blocking.
    /// Returns `Ok(None)` when any other process already holds it, in either
    /// mode. Callers must treat `Ok(None)` as an immediate rejection (e.g.
    /// "AI is currently in use"), never as a reason to poll or block.
    pub fn try_acquire_exclusive(&self) -> io::Result<Option<AiSettingsExclusiveGuard>> {
        let file = self.open()?;
        if platform::try_lock(&file, true)? {
            Ok(Some(AiSettingsExclusiveGuard { _file: file }))
        } else {
            Ok(None)
        }
    }

    /// Attempts to acquire the lock in shared mode without blocking. Returns
    /// `Ok(None)` only when another process currently holds the lock in
    /// exclusive mode (i.e. a settings save is in progress); coexists freely
    /// with other shared holders.
    pub fn try_acquire_shared(&self) -> io::Result<Option<AiSettingsSharedGuard>> {
        let file = self.open()?;
        if platform::try_lock(&file, false)? {
            Ok(Some(AiSettingsSharedGuard { _file: file }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::unix::io::AsRawFd;

    pub fn try_lock(file: &File, exclusive: bool) -> io::Result<bool> {
        let fd = file.as_raw_fd();
        let mode = if exclusive { libc::LOCK_EX } else { libc::LOCK_SH };
        let ret = unsafe { libc::flock(fd, mode | libc::LOCK_NB) };
        if ret == 0 {
            Ok(true)
        } else {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                Ok(false)
            } else {
                Err(err)
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;

    /// See `owner_lock::platform::Overlapped` for why this layout is safe to
    /// use with `LockFileEx`.
    #[repr(C)]
    #[allow(non_snake_case, dead_code)]
    struct Overlapped {
        Internal: usize,
        InternalHigh: usize,
        Offset: u32,
        OffsetHigh: u32,
        hEvent: *mut core::ffi::c_void,
    }

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LockFileEx(
            h_file: *mut core::ffi::c_void,
            dw_flags: u32,
            dw_reserved: u32,
            n_number_of_bytes_to_lock_low: u32,
            n_number_of_bytes_to_lock_high: u32,
            lp_overlapped: *mut Overlapped,
        ) -> i32;
    }

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x1;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x2;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    const ERROR_IO_PENDING: i32 = 997;

    /// Non-blocking try-lock. Shared mode omits `LOCKFILE_EXCLUSIVE_LOCK` so
    /// multiple shared holders can coexist; exclusive mode sets it so any
    /// other holder (shared or exclusive) blocks this from succeeding.
    pub fn try_lock(file: &File, exclusive: bool) -> io::Result<bool> {
        let handle = file.as_raw_handle();
        let mut overlapped: Overlapped = unsafe { std::mem::zeroed() };
        let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
        if exclusive {
            flags |= LOCKFILE_EXCLUSIVE_LOCK;
        }
        let ok = unsafe { LockFileEx(handle, flags, 0, 1, 0, &mut overlapped) };
        if ok != 0 {
            Ok(true)
        } else {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(ERROR_LOCK_VIOLATION) | Some(ERROR_IO_PENDING) => Ok(false),
                _ => Err(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_lock_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-settings-lock-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ai-settings.lock")
    }

    #[test]
    fn exclusive_acquire_blocks_a_second_exclusive_acquire() {
        let path = unique_lock_path("excl_vs_excl");
        let lock_a = AiSettingsLock::new(&path);
        let lock_b = AiSettingsLock::new(&path);

        let guard_a = lock_a.try_acquire_exclusive().unwrap();
        assert!(guard_a.is_some());
        assert!(lock_b.try_acquire_exclusive().unwrap().is_none());

        drop(guard_a);
        assert!(lock_b.try_acquire_exclusive().unwrap().is_some());
    }

    #[test]
    fn exclusive_acquire_is_rejected_while_a_shared_holder_exists() {
        let path = unique_lock_path("excl_vs_shared");
        let lock_a = AiSettingsLock::new(&path);
        let lock_b = AiSettingsLock::new(&path);

        let shared = lock_a.try_acquire_shared().unwrap();
        assert!(shared.is_some());
        // A settings save must be rejected immediately while a turn/probe
        // holds the shared lock, never wait for it to finish.
        assert!(lock_b.try_acquire_exclusive().unwrap().is_none());

        drop(shared);
        assert!(lock_b.try_acquire_exclusive().unwrap().is_some());
    }

    #[test]
    fn multiple_shared_holders_coexist() {
        let path = unique_lock_path("shared_vs_shared");
        let lock_a = AiSettingsLock::new(&path);
        let lock_b = AiSettingsLock::new(&path);

        let shared_a = lock_a.try_acquire_shared().unwrap();
        let shared_b = lock_b.try_acquire_shared().unwrap();
        assert!(shared_a.is_some());
        assert!(shared_b.is_some());
    }

    #[test]
    fn shared_acquire_is_rejected_while_exclusive_holder_exists() {
        let path = unique_lock_path("shared_vs_excl");
        let lock_a = AiSettingsLock::new(&path);
        let lock_b = AiSettingsLock::new(&path);

        let exclusive = lock_a.try_acquire_exclusive().unwrap();
        assert!(exclusive.is_some());
        assert!(lock_b.try_acquire_shared().unwrap().is_none());

        drop(exclusive);
        assert!(lock_b.try_acquire_shared().unwrap().is_some());
    }
}
