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
    file: File,
}

impl OwnerLockGuard {
    /// The path of the runtime owner lock this guard proves ownership of.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnerLockGuard {
    /// Explicitly releases the OS-level lock before the file handle itself is
    /// closed. On Unix this matters because `spawn_child` (`crate::runtime`)
    /// spawns the App Server via `fork`+`exec`: between those two steps the
    /// freshly forked child holds its own inherited copy of this file
    /// descriptor, referring to the very same open file description as ours
    /// — `FD_CLOEXEC` (set by `std::fs::File` by default) only closes that
    /// copy at the child's `exec`, not at `fork`. `flock` locks are owned by
    /// the open file description, not by any single file descriptor or
    /// process, and are released either by an explicit `LOCK_UN` on *any* fd
    /// referring to that description, or once *every* such fd has been
    /// closed. Relying on plain `close` here (i.e. just dropping `file`)
    /// would therefore leave the lock held until the child's own copy is
    /// also closed — which does not happen until it execs (or exits) —
    /// blocking this process from reacquiring its own lock in the meantime.
    /// An explicit `LOCK_UN` releases it immediately regardless of who else
    /// still holds the description open. See `platform::unlock_before_close`.
    fn drop(&mut self) {
        platform::unlock_before_close(&self.file);
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
            Ok(Some(OwnerLockGuard {
                path: self.path.clone(),
                file,
            }))
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

    /// Explicitly unlocks before `file` is closed. See
    /// [`super::OwnerLockGuard`]'s `Drop` impl for why this is necessary:
    /// a plain `close` (dropping `file` with no explicit unlock) only
    /// releases an `flock` once every fd referring to the same open file
    /// description is closed, which a forked-but-not-yet-exec'd child's own
    /// inherited copy can delay well past this guard's drop. Best-effort:
    /// there is nothing useful to do with an error here, and the fd is
    /// closed right after regardless.
    pub fn unlock_before_close(file: &File) {
        let fd = file.as_raw_fd();
        unsafe {
            let _ = libc::flock(fd, libc::LOCK_UN);
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

        fn UnlockFileEx(
            h_file: *mut core::ffi::c_void,
            dw_reserved: u32,
            n_number_of_bytes_to_unlock_low: u32,
            n_number_of_bytes_to_unlock_high: u32,
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

    /// Explicitly releases the byte range locked by `try_lock_exclusive`
    /// before `file` is closed. Unlike Unix `flock`, `LockFileEx`'s lock
    /// belongs to the specific handle it was taken on, not to some shared,
    /// fork-inherited open file description, so there is no equivalent to
    /// Unix's fork-before-exec window here: `std::fs::File`'s underlying
    /// `HANDLE` is created non-inheritable (`bInheritHandle == FALSE`), and
    /// `std::process::Command` only ever makes the explicit stdio handles it
    /// wires up inheritable, so the App Server child this process spawns
    /// never receives a duplicate of this handle. Still, releasing the lock
    /// explicitly here (rather than relying on `CloseHandle` when `file` is
    /// dropped) matches the same offset/length `try_lock_exclusive` used to
    /// acquire it. Best-effort: there is nothing useful to do with an error
    /// here, and the handle is closed right after regardless.
    pub fn unlock_before_close(file: &File) {
        let handle = file.as_raw_handle();
        let mut overlapped: Overlapped = unsafe { std::mem::zeroed() };
        unsafe {
            let _ = UnlockFileEx(handle, 0, 1, 0, &mut overlapped);
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

    /// Regression coverage for the root cause behind a parallel
    /// `runtime_lifecycle` restart occasionally failing to reacquire its own
    /// just-dropped runtime owner lock: `spawn_child`
    /// (`crate::runtime::RuntimeConfig`) spawns the App Server via
    /// `fork`+`exec`, and between those two steps the child holds its own
    /// inherited copy of the lock file's fd, referring to the very same
    /// `flock`-tracked open file description as the parent's — `FD_CLOEXEC`
    /// only closes that copy at `exec`, not at `fork`. Reproduces exactly
    /// that pre-exec window deterministically (no timing-dependent sleeps):
    /// a real forked child holds its inherited copy of the lock fd open and
    /// signals so over a pipe before the parent drops its own guard and
    /// immediately tries to reacquire.
    #[cfg(unix)]
    #[test]
    fn parent_can_reacquire_immediately_after_dropping_the_guard_even_while_a_forked_child_still_holds_the_inherited_pre_exec_fd()
     {
        let dir = std::env::temp_dir().join(format!(
            "hane-ai-owner-lock-test-{}-{}",
            std::process::id(),
            "reacquire_across_fork_pre_exec"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("runtime.lock");

        let lock = OwnerLock::new(&lock_path);
        let guard = lock
            .try_acquire()
            .unwrap()
            .expect("first acquire must succeed");

        // Two pipes synchronize with the forked child without relying on
        // sleeps: `ready` lets the child tell the parent it is alive and
        // still holding its inherited copy of the lock fd (standing in for
        // the fork-but-not-yet-exec'd App Server child `spawn_child`
        // produces); `go` lets the parent tell the child it may now exit.
        let mut ready_fds = [0i32; 2];
        let mut go_fds = [0i32; 2];
        assert_eq!(
            unsafe { libc::pipe(ready_fds.as_mut_ptr()) },
            0,
            "pipe() for readiness signal failed"
        );
        assert_eq!(
            unsafe { libc::pipe(go_fds.as_mut_ptr()) },
            0,
            "pipe() for exit signal failed"
        );
        let [ready_r, ready_w] = ready_fds;
        let [go_r, go_w] = go_fds;

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork() failed");

        if pid == 0 {
            // Child: still holds its own inherited copy of the lock file's
            // fd. Only raw syscalls from here on (no Rust allocation, no
            // std locks) since this is a forked copy of a multi-threaded
            // test process. Signal alive, block until told to exit, then
            // `_exit` directly rather than unwinding back through the test
            // harness.
            unsafe {
                libc::close(ready_r);
                libc::close(go_w);
                let byte: u8 = 1;
                libc::write(ready_w, &byte as *const u8 as *const libc::c_void, 1);
                let mut buf: u8 = 0;
                libc::read(go_r, &mut buf as *mut u8 as *mut libc::c_void, 1);
                libc::_exit(0);
            }
        }

        // Parent.
        unsafe {
            libc::close(ready_w);
            libc::close(go_r);
        }
        let mut buf: u8 = 0;
        let n = unsafe { libc::read(ready_r, &mut buf as *mut u8 as *mut libc::c_void, 1) };
        assert_eq!(
            n, 1,
            "child must signal it is alive, still holding its inherited fd, before the parent proceeds"
        );
        unsafe { libc::close(ready_r) };

        // The child now holds its own inherited fd referring to the same
        // open file description as `guard`'s, without ever having exec'd.
        // Dropping `guard` here must release the lock immediately via the
        // explicit `LOCK_UN` in its `Drop` impl, rather than only once every
        // fd sharing that description (including the child's) is closed.
        drop(guard);

        let reacquired = lock.try_acquire().unwrap();

        // Let the child exit and reap it regardless of the assertion below,
        // so a failure does not leak a zombie process.
        unsafe {
            let byte: u8 = 1;
            libc::write(go_w, &byte as *const u8 as *const libc::c_void, 1);
            libc::close(go_w);
            let mut status: i32 = 0;
            libc::waitpid(pid, &mut status, 0);
        }

        assert!(
            reacquired.is_some(),
            "parent must be able to reacquire its own runtime owner lock immediately after dropping the guard, \
             even while a forked child still holds an inherited pre-exec copy of the lock fd"
        );

        drop(reacquired);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
