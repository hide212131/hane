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
    file: File,
}

/// Holding this guard means the current process holds the **shared** AI
/// settings lock: no other process holds the lock in exclusive mode, though
/// other processes may concurrently hold it in shared mode too. Dropping it
/// (or the process exiting) releases the OS-level lock.
pub struct AiSettingsSharedGuard {
    file: File,
}

impl Drop for AiSettingsExclusiveGuard {
    /// Explicitly releases the OS-level lock before the file handle itself is
    /// closed. On Unix this matters because a caller may `fork`+`exec` a
    /// child process (e.g. the embedded App Server) while holding this
    /// guard: between those two steps the freshly forked child holds its own
    /// inherited copy of this file descriptor, referring to the very same
    /// open file description as ours -- `FD_CLOEXEC` (set by `std::fs::File`
    /// by default) only closes that copy at the child's `exec`, not at
    /// `fork`. `flock` locks are owned by the open file description, not by
    /// any single file descriptor or process, and are released either by an
    /// explicit `LOCK_UN` on *any* fd referring to that description, or once
    /// *every* such fd has been closed. Relying on plain `close` here (i.e.
    /// just dropping `file`) would therefore leave the lock held until the
    /// child's own copy is also closed -- which does not happen until it
    /// execs (or exits) -- blocking this lock's critical section from ending
    /// when it semantically should. An explicit `LOCK_UN` releases it
    /// immediately regardless of who else still holds the description open.
    /// See `platform::unlock_before_close`.
    fn drop(&mut self) {
        platform::unlock_before_close(&self.file);
    }
}

impl Drop for AiSettingsSharedGuard {
    /// See [`AiSettingsExclusiveGuard`]'s `Drop` impl: the same fork-inherited
    /// open file description hazard applies to the shared lock.
    fn drop(&mut self) {
        platform::unlock_before_close(&self.file);
    }
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
            Ok(Some(AiSettingsExclusiveGuard { file }))
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
            Ok(Some(AiSettingsSharedGuard { file }))
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

    /// Explicitly unlocks before `file` is closed. See
    /// [`super::AiSettingsExclusiveGuard`]'s `Drop` impl for why this is
    /// necessary: a plain `close` (dropping `file` with no explicit unlock)
    /// only releases an `flock` once every fd referring to the same open file
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

    /// No-op: unlike Unix `flock`, `LockFileEx`'s lock belongs to the
    /// specific handle it was taken on, not to some shared, fork-inherited
    /// open file description. `std::fs::File`'s underlying `HANDLE` is
    /// created non-inheritable (`bInheritHandle == FALSE`), and
    /// `std::process::Command` only ever makes the explicit stdio handles it
    /// wires up inheritable, so a spawned child process never receives a
    /// duplicate of this handle in the first place. A plain `CloseHandle`
    /// (via dropping `file`) therefore already releases the lock as soon as
    /// this guard is dropped, with no equivalent to Unix's fork-before-exec
    /// window to work around.
    pub fn unlock_before_close(_file: &File) {}
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

    /// Regression coverage for the root cause behind
    /// `custom_provider_runtime`'s generation-gate test occasionally seeing
    /// `SettingsBusy` instead of the expected `NotReady`: a caller can
    /// `fork`+`exec` a child process (e.g. the embedded App Server) while
    /// holding this exclusive guard, and between those two steps the child
    /// holds its own inherited copy of the lock file's fd, referring to the
    /// very same `flock`-tracked open file description as the parent's --
    /// `FD_CLOEXEC` only closes that copy at `exec`, not at `fork`.
    /// Reproduces exactly that pre-exec window deterministically (no
    /// timing-dependent sleeps): a real forked child holds its inherited
    /// copy of the lock fd open and signals so over a pipe before the parent
    /// drops its own guard and immediately tries to reacquire.
    #[cfg(unix)]
    #[test]
    fn exclusive_guard_can_be_reacquired_immediately_after_drop_even_while_a_forked_child_still_holds_the_inherited_pre_exec_fd()
    {
        let path = unique_lock_path("excl_reacquire_across_fork_pre_exec");
        let lock = AiSettingsLock::new(&path);
        let guard = lock.try_acquire_exclusive().unwrap().expect("first acquire must succeed");

        // Two pipes synchronize with the forked child without relying on
        // sleeps: `ready` lets the child tell the parent it is alive and
        // still holding its inherited copy of the lock fd; `go` lets the
        // parent tell the child it may now exit.
        let mut ready_fds = [0i32; 2];
        let mut go_fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(ready_fds.as_mut_ptr()) }, 0, "pipe() for readiness signal failed");
        assert_eq!(unsafe { libc::pipe(go_fds.as_mut_ptr()) }, 0, "pipe() for exit signal failed");
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
        assert_eq!(n, 1, "child must signal it is alive, still holding its inherited fd, before the parent proceeds");
        unsafe { libc::close(ready_r) };

        // The child now holds its own inherited fd referring to the same
        // open file description as `guard`'s, without ever having exec'd.
        // Dropping `guard` here must release the lock immediately via the
        // explicit `LOCK_UN` in its `Drop` impl, rather than only once every
        // fd sharing that description (including the child's) is closed.
        drop(guard);

        let reacquired = lock.try_acquire_exclusive().unwrap();

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
            "must be able to reacquire the exclusive AI settings lock immediately after dropping the guard, \
             even while a forked child still holds an inherited pre-exec copy of the lock fd"
        );
    }

    /// Same fork-inherited open file description hazard as above, but for
    /// the shared guard: confirms an exclusive acquire is still correctly
    /// rejected while the shared guard (and its forked child's inherited
    /// copy of the fd) are outstanding, and that dropping the shared guard
    /// releases the lock immediately -- letting an exclusive acquire succeed
    /// without waiting for the still-running child to exit.
    #[cfg(unix)]
    #[test]
    fn shared_guard_can_be_exclusively_reacquired_immediately_after_drop_even_while_a_forked_child_still_holds_the_inherited_pre_exec_fd()
    {
        let path = unique_lock_path("shared_reacquire_across_fork_pre_exec");
        let lock = AiSettingsLock::new(&path);
        let guard = lock.try_acquire_shared().unwrap().expect("first acquire must succeed");

        let mut ready_fds = [0i32; 2];
        let mut go_fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(ready_fds.as_mut_ptr()) }, 0, "pipe() for readiness signal failed");
        assert_eq!(unsafe { libc::pipe(go_fds.as_mut_ptr()) }, 0, "pipe() for exit signal failed");
        let [ready_r, ready_w] = ready_fds;
        let [go_r, go_w] = go_fds;

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork() failed");

        if pid == 0 {
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
        assert_eq!(n, 1, "child must signal it is alive, still holding its inherited fd, before the parent proceeds");
        unsafe { libc::close(ready_r) };

        // While the shared guard (and the forked child's inherited copy of
        // its fd) are both still outstanding, a settings save must still be
        // rejected immediately.
        let other = AiSettingsLock::new(&path);
        assert!(
            other.try_acquire_exclusive().unwrap().is_none(),
            "exclusive acquire must be rejected while the shared guard is still held"
        );

        // Dropping `guard` here must release the lock immediately via the
        // explicit `LOCK_UN` in its `Drop` impl, rather than only once every
        // fd sharing that description (including the still-running child's)
        // is closed.
        drop(guard);

        let reacquired = other.try_acquire_exclusive().unwrap();

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
            "must be able to exclusively acquire the AI settings lock immediately after dropping the shared guard, \
             without waiting for a forked child still holding an inherited pre-exec copy of the lock fd to exit"
        );
    }
}
