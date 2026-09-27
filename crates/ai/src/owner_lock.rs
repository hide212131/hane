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
pub struct OwnerLockGuard {
    _file: File,
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
            Ok(Some(OwnerLockGuard { _file: file }))
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

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LockFile(
            h_file: *mut core::ffi::c_void,
            dw_file_offset_low: u32,
            dw_file_offset_high: u32,
            n_number_of_bytes_to_lock_low: u32,
            n_number_of_bytes_to_lock_high: u32,
        ) -> i32;
    }

    const ERROR_LOCK_VIOLATION: i32 = 33;

    pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
        let handle = file.as_raw_handle();
        // Lock a single fixed byte range; only exclusivity matters here, not
        // the file's actual contents or size.
        let ok = unsafe { LockFile(handle, 0, 0, 1, 0) };
        if ok != 0 {
            Ok(true)
        } else {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
                Ok(false)
            } else {
                Err(err)
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
