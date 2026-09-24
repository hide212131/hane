//! Forward Explorer's file argument to a running Hane process on Windows.
//! The named pipe belongs to the first instance; it never executes commands,
//! only passes a bounded UTF-16 path to that instance's editor.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

const MAX_UNITS: usize = 16_384;

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

fn write_path(pipe: &mut File, path: &Path) -> io::Result<()> {
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
    if ack[0] != 1 {
        return Err(io::Error::other("Hane did not accept the open request"));
    }
    Ok(())
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

pub fn forward(path: &Path) -> io::Result<bool> {
    let name = pipe_name();
    for attempt in 0..10 {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(mut pipe) => return write_path(&mut pipe, path).map(|()| true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error)
                if attempt < 9
                    && (error.kind() == io::ErrorKind::PermissionDenied
                        || error.kind() == io::ErrorKind::WouldBlock) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub fn listen() -> io::Result<Receiver<PathBuf>> {
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
                let accepted = sender.send(path).is_ok();
                let _ = pipe.write_all(&[u8::from(accepted)]);
                if !accepted {
                    break;
                }
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
    fn forwards_a_unicode_file_path_to_the_running_instance() {
        let receiver = listen().expect("start Hane's named pipe");
        let path = Path::new(r"C:\Temp\Hane 日本語.md");
        assert!(forward(path).expect("send the open request"));
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), path);
    }
}
