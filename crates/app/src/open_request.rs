//! Forward Explorer's file argument to a running Hane process on Windows.
//! The named pipe belongs to the first instance; it never executes commands,
//! only passes a bounded UTF-16 path to that instance's editor.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

const MAX_UNITS: usize = 16_384;
/// How long the pipe waits for the editor to take a request. Shorter than the
/// AI stop timeout, so a request during the final freeze is refused, not held.
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a sender keeps retrying refusals; covers the AI stop wait.
const FORWARD_DEADLINE: Duration = Duration::from_secs(30);

fn pipe_name() -> String {
    // Scope the pipe to this Windows profile and this Hane installation.
    // A different user or a second checkout must not consume the request.
    let profile = std::env::var_os("LOCALAPPDATA").unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_default();
    let units = profile
        .encode_wide()
        .chain(Some(0))
        .chain(exe.as_os_str().encode_wide());
    let hash = units.fold(0xcbf29ce484222325_u64, |hash, unit| {
        (hash ^ u64::from(unit)).wrapping_mul(0x100000001b3)
    });
    format!(r"\\.\pipe\Hane.Editor.OpenFile.v1.{hash:016x}")
}

/// Returns whether the running instance took responsibility for the request.
fn write_path(pipe: &mut File, path: &Path) -> io::Result<bool> {
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.is_empty() || units.len() > MAX_UNITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid Hane path length",
        ));
    }
    pipe.write_all(&(units.len() as u32).to_le_bytes())?;
    for unit in units {
        pipe.write_all(&unit.to_le_bytes())?;
    }
    pipe.flush()?;
    let mut ack = [0];
    pipe.read_exact(&mut ack)?;
    Ok(ack[0] == 1)
}

fn read_path(pipe: &mut File) -> io::Result<PathBuf> {
    let mut len_bytes = [0; 4];
    pipe.read_exact(&mut len_bytes)?;
    let count = u32::from_le_bytes(len_bytes) as usize;
    if count == 0 || count > MAX_UNITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid Hane path length",
        ));
    }
    let mut bytes = vec![0; count * 2];
    pipe.read_exact(&mut bytes)?;
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes(*chunk));
    Ok(std::ffi::OsString::from_wide(&units.collect::<Vec<_>>()).into())
}

/// A request received on the pipe. The sender is told "accepted" only when the
/// editor answers [`OpenRequest::accept`] after really taking the path; a
/// refused, dropped or unanswered request is refused on the pipe so the sender
/// retries instead of losing the file when this process exits.
pub struct OpenRequest {
    path: PathBuf,
    reply: SyncSender<bool>,
    abandoned: Arc<AtomicBool>,
}

impl OpenRequest {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True once the pipe gave up waiting and refused the sender; the editor
    /// must not open such a request.
    pub fn is_abandoned(&self) -> bool {
        self.abandoned.load(Ordering::SeqCst)
    }

    pub fn accept(self) {
        let _ = self.reply.send(true);
    }

    pub fn refuse(self) {
        let _ = self.reply.send(false);
    }
}

/// Forwards `path` to the running instance. Refusals (the instance is frozen
/// for an update restart or exiting) are retried until the instance either
/// accepts or is gone (`Ok(false)`, so the caller opens it itself).
pub fn forward(path: &Path) -> io::Result<bool> {
    let name = pipe_name();
    let deadline = Instant::now() + FORWARD_DEADLINE;
    loop {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(mut pipe) => match write_path(&mut pipe, path) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(error)
                    if error.kind() == io::ErrorKind::UnexpectedEof
                        || error.kind() == io::ErrorKind::BrokenPipe => {}
                Err(error) => return Err(error),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error)
                if error.kind() == io::ErrorKind::PermissionDenied
                    || error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("Hane did not accept the open request"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn listen() -> io::Result<Receiver<OpenRequest>> {
    let name: Vec<u16> = pipe_name().encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65_536,
            65_536,
            0,
            std::ptr::null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut pipe = unsafe { File::from_raw_handle(handle) };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let handle = pipe.as_raw_handle();
        loop {
            let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0;
            if !connected && unsafe { GetLastError() } != 535
            /* ERROR_PIPE_CONNECTED */
            {
                break;
            }
            if let Ok(path) = read_path(&mut pipe) {
                let (reply, answer) = mpsc::sync_channel(1);
                let abandoned = Arc::new(AtomicBool::new(false));
                let request = OpenRequest {
                    path,
                    reply,
                    abandoned: Arc::clone(&abandoned),
                };
                if sender.send(request).is_err() {
                    let _ = pipe.write_all(&[0]);
                    break;
                }
                // ACK only after the editor took the path. If it cannot
                // answer in time (frozen, stopping the AI service), refuse so
                // the sender retries rather than lose the request at exit.
                let accepted = matches!(answer.recv_timeout(REPLY_TIMEOUT), Ok(true));
                if !accepted {
                    abandoned.store(true, Ordering::SeqCst);
                }
                let _ = pipe.write_all(&[u8::from(accepted)]);
            }
            unsafe { DisconnectNamedPipe(handle) };
        }
    });
    Ok(receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_a_unicode_file_path_and_acks_only_after_the_editor_takes_it() {
        // One test: the pipe is a per-installation singleton.
        let receiver = listen().expect("start Hane's named pipe");
        let path = PathBuf::from(r"C:\Temp\Hane 日本語.md");
        let sent = path.clone();
        let sender = std::thread::spawn(move || forward(&sent));

        // A frozen/exiting editor refuses: the sender must not see success.
        let first = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(first.path(), path);
        first.refuse();
        // A request dropped unanswered (receiver torn down at quit) is refused too.
        let second = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(second);
        // The retry is finally taken, and only now does the sender succeed.
        let third = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(third.path(), path);
        assert!(!sender.is_finished(), "no ACK before the editor accepts");
        third.accept();
        assert!(sender.join().unwrap().expect("send the open request"));
    }
}
