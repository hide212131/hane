//! Development and test stand-in for the Windows pipe and process handling:
//! a Unix socket in a private directory, a pipe inherited by the child only,
//! and a process group killed as a whole. The product engine is Windows-only;
//! this exists so the protocol and the process lifecycle can be exercised on
//! the development machine.

use crate::transport::{
    ChildProcess, EngineSpec, Handoff, Platform, VoiceConnection, VoiceListener,
};
use hane_link::frame;
use hane_link::transport::Connection;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Where the socket of `name` lives: a short name in a directory only this
/// user can enter (socket paths are short on some systems).
pub fn socket_path(name: &str) -> PathBuf {
    // SAFETY: no arguments.
    let uid = unsafe { libc::geteuid() };
    let tail = &name[name.len().saturating_sub(16)..];
    std::env::temp_dir()
        .join(format!("hv-{uid}"))
        .join(format!("{tail}.s"))
}

fn private_dir(path: &std::path::Path) -> io::Result<()> {
    let parent = path.parent().expect("socket path has a parent");
    match std::fs::DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = std::fs::metadata(parent)?;
    // SAFETY: no arguments.
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket directory is not private",
        ));
    }
    Ok(())
}

pub struct UnixConnection(UnixStream);

impl UnixConnection {
    pub fn new(stream: UnixStream) -> Self {
        Self(stream)
    }
}

fn map_timeout(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::WouldBlock => io::ErrorKind::TimedOut.into(),
        _ => error,
    }
}

impl Connection for UnixConnection {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.0.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        self.0.read(buf).map_err(map_timeout)
    }

    fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        self.0.set_write_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        self.0.write(buf).map_err(map_timeout)
    }
}

impl VoiceConnection for UnixConnection {
    #[cfg(target_os = "macos")]
    fn peer_pid(&self) -> Option<u32> {
        let mut pid: libc::pid_t = 0;
        let mut length = size_of::<libc::pid_t>() as libc::socklen_t;
        // SAFETY: the socket is open; the buffer holds one pid_t.
        let status = unsafe {
            libc::getsockopt(
                self.0.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut length,
            )
        };
        (status == 0).then_some(pid as u32)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn peer_pid(&self) -> Option<u32> {
        let mut credentials = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the socket is open; the buffer holds one ucred.
        let status = unsafe {
            libc::getsockopt(
                self.0.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        (status == 0).then_some(credentials.pid as u32)
    }
}

pub struct UnixVoiceListener {
    listener: UnixListener,
    path: PathBuf,
}

impl UnixVoiceListener {
    pub fn bind(name: &str) -> io::Result<Self> {
        let path = socket_path(name);
        private_dir(&path)?;
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path })
    }
}

impl Drop for UnixVoiceListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl VoiceListener for UnixVoiceListener {
    fn accept(&mut self, timeout: Duration) -> io::Result<Option<Box<dyn VoiceConnection>>> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    return Ok(Some(Box::new(UnixConnection(stream))));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

/// Connects to the socket of `name`. For the stand-in engine.
pub fn connect(name: &str) -> io::Result<UnixConnection> {
    UnixStream::connect(socket_path(name)).map(UnixConnection)
}

struct UnixChild {
    child: Child,
    done: Option<i32>,
}

impl ChildProcess for UnixChild {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn exit_code(&mut self) -> Option<i32> {
        if self.done.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.done = Some(status.code().unwrap_or(-1));
        }
        self.done
    }

    fn kill(&mut self) {
        // The child leads its own process group: this ends its workers too.
        // SAFETY: signalling a group this process created.
        unsafe { libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL) };
        if self.done.is_none() {
            let _ = self.child.kill();
            self.done = self.child.wait().ok().map(|status| status.code().unwrap_or(-1));
        }
    }
}

impl Drop for UnixChild {
    fn drop(&mut self) {
        if self.done.is_none() && self.exit_code().is_none() {
            self.kill();
        }
    }
}

pub struct UnixPlatform;

impl Platform for UnixPlatform {
    fn listen(&self, name: &str) -> io::Result<Box<dyn VoiceListener>> {
        Ok(Box::new(UnixVoiceListener::bind(name)?))
    }

    fn launch(&self, spec: &EngineSpec, handoff: &Handoff) -> io::Result<Box<dyn ChildProcess>> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors are fresh and owned here.
        let (read, mut write) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
        for fd in [read.as_raw_fd(), write.as_raw_fd()] {
            // Neither end leaks into unrelated children; the read end is made
            // inheritable in the child only.
            // SAFETY: the descriptor is open.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let inherited = read.as_raw_fd();
        let mut command = Command::new(&spec.exe);
        command
            .arg("--voice-engine")
            .arg("--voice-handoff")
            .arg(inherited.to_string())
            .args(&spec.extra_args)
            .stdin(Stdio::null())
            .process_group(0);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                libc::fcntl(inherited, libc::F_SETFD, 0);
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(read);
        // A frame this small fits the pipe buffer: the write does not block.
        // A child that already ended cannot read it; that is reported by its exit
        // code during the connect wait, not as a failure to start.
        let _ = frame::write_frame(&mut write, &handoff.to_json());
        drop(write);
        Ok(Box::new(UnixChild { child, done: None }))
    }
}
