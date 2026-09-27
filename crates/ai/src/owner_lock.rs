//! Cross-process exclusive "runtime owner" lock.
//!
//! Only one Hane process may own the embedded App Server runtime for a
//! given app-data directory at a time. The lock is backed by an OS file
//! lock (not a PID file), so it is automatically released by the OS when
//! the owning process exits or the guard is dropped, and it never blocks
//! indefinitely: acquisition is always a non-blocking try-lock.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

pub struct OwnerLock {
    path: PathBuf,
}

/// Holding this guard means the current process owns the runtime lock.
/// Dropping it (or the process exiting) releases the OS-level lock.
///
/// Carries the exact path it was acquired against so a caller that receives
/// this guard from elsewhere (e.g. [`crate::runtime::AiRuntime::start_with_owner_lock`]
/// or [`crate::settings::AiSettingsStore::save`]/`write_while_locked`) can
/// confirm it actually proves ownership of *its own* runtime owner lock,
/// rather than trusting that any `OwnerLockGuard` value proves ownership of
/// whichever lock file that caller cares about.
pub struct OwnerLockGuard {
    path: PathBuf,
    _file: File,
}

impl OwnerLockGuard {
    /// The path of the runtime owner lock this guard proves ownership of.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl OwnerLock {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        OwnerLock { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Attempts to acquire the lock without blocking. Returns `Ok(None)`
    /// when another process already owns it.
    pub fn try_acquire(&self) -> io::Result<Option<OwnerLockGuard>> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.path)?;
        if platform::try_lock_exclusive(&file)? {
            Ok(Some(OwnerLockGuard { path: self.path.clone(), _file: file }))
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

    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
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

    /// Mirrors the layout of the Win32 `OVERLAPPED` struct closely enough
    /// for `LockFileEx`'s purposes: the `Offset`/`OffsetHigh` fields we set
    /// occupy the same first 8 bytes as the union's `Pointer` field on every
    /// supported target, and we never touch `hEvent` (synchronous handle).
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

    /// Non-blocking exclusive try-lock. `LOCKFILE_FAIL_IMMEDIATELY` is what
    /// makes this a true try-lock: without it `LockFileEx` would wait for
    /// the region to become available instead of failing right away.
    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        let handle = file.as_raw_handle();
        let mut overlapped: Overlapped = unsafe { std::mem::zeroed() };
        // Lock a single fixed byte range; only exclusivity matters here, not
        // the file's actual contents or size.
        let ok = unsafe {
            LockFileEx(
                handle,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
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

    #[test]
    fn second_acquire_fails_while_first_guard_is_held() {
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-owner-lock-test-{}-{}",
            std::process::id(),
            "second_acquire_fails_while_first_guard_is_held"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("runtime.lock");

        let lock_a = OwnerLock::new(&lock_path);
        let lock_b = OwnerLock::new(&lock_path);

        let guard_a = lock_a.try_acquire().unwrap();
        assert!(guard_a.is_some());

        let guard_b = lock_b.try_acquire().unwrap();
        assert!(guard_b.is_none());

        drop(guard_a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_becomes_available_again_after_the_guard_is_dropped() {
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-owner-lock-test-{}-{}",
            std::process::id(),
            "lock_becomes_available_again_after_the_guard_is_dropped"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("runtime.lock");

        let lock = OwnerLock::new(&lock_path);
        let guard = lock.try_acquire().unwrap();
        assert!(guard.is_some());
        drop(guard);

        let guard_again = lock.try_acquire().unwrap();
        assert!(guard_again.is_some());

        drop(guard_again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
