//! Windows-only parts: private files, the named-pipe listener and process
//! facts. Everything unsafe in the crate lives here.
//!
//! The pipe is created with an explicit DACL, rejects remote clients, and every
//! read, write and accept is overlapped so it can be bounded by a timeout.

use crate::transport::{Connection, Listener};
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, FILETIME, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_GROUPS, TOKEN_QUERY,
    TOKEN_USER, TokenLogonSid, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};

const GENERIC_WRITE_ACCESS: u32 = 0x4000_0000;
/// FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE: what
/// a client needs to talk over the pipe. Not FILE_CREATE_PIPE_INSTANCE.
const PIPE_CLIENT_RIGHTS: &str = "0x00100083";

pub fn tick_count_ms() -> u64 {
    // SAFETY: no arguments, no preconditions.
    unsafe { GetTickCount64() }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// The creation time of `pid` as a FILETIME in 100 ns units, or `None` when the
/// process is gone or cannot be queried.
pub fn process_creation_time(pid: u32) -> Option<u64> {
    // SAFETY: plain handle calls; the handle is closed on every path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        (ok != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

/// An owned process token, closed on drop.
struct Token(HANDLE);

impl Token {
    fn open() -> io::Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: the pseudo handle of the current process is always valid.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(token))
    }

    /// The raw bytes of a token information class.
    fn information(&self, class: i32) -> io::Result<Vec<u8>> {
        let mut needed = 0u32;
        // SAFETY: a size query with a null buffer.
        unsafe { GetTokenInformation(self.0, class, null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        // Align for the structures read from it.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: the buffer holds at least `needed` bytes.
        let ok = unsafe {
            GetTokenInformation(
                self.0,
                class,
                buffer.as_mut_ptr().cast::<c_void>(),
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), buffer.len() * 8) };
        Ok(bytes.to_vec())
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcessToken.
        unsafe { CloseHandle(self.0) };
    }
}

fn sid_string(sid: *mut c_void) -> io::Result<String> {
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` points at a valid SID inside the token buffer.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    // SAFETY: the API returns a NUL-terminated string that LocalFree releases.
    let result = unsafe {
        while *text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    unsafe { LocalFree(text.cast()) };
    Ok(result)
}

fn current_user_sid() -> io::Result<String> {
    let token = Token::open()?;
    let bytes = token.information(TokenUser)?;
    // SAFETY: the buffer is 8-aligned and holds a TOKEN_USER with its SID.
    let user = unsafe { &*bytes.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

/// The SID of this logon session, when the token has one.
fn logon_sid() -> Option<String> {
    let token = Token::open().ok()?;
    let bytes = token.information(TokenLogonSid).ok()?;
    // SAFETY: as above, for a TOKEN_GROUPS.
    let groups = unsafe { &*bytes.as_ptr().cast::<TOKEN_GROUPS>() };
    if groups.GroupCount == 0 {
        return None;
    }
    sid_string(groups.Groups[0].Sid).ok()
}

/// A security descriptor built from SDDL, freed on drop.
struct Descriptor(*mut c_void);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut descriptor: *mut c_void = null_mut();
        // SAFETY: `text` is NUL-terminated; the descriptor is LocalAlloc'd.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by the conversion call.
        unsafe { LocalFree(self.0) };
    }
}

/// A DACL that grants `FA` to the current user only, with inheritance cut.
fn private_sddl(container: bool) -> io::Result<String> {
    let sid = current_user_sid()?;
    let flags = if container { "OICI" } else { "" };
    Ok(format!("D:P(A;{flags};FA;;;{sid})"))
}

/// Creates a directory that only the current user can use, from the moment it
/// exists.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    let descriptor = Descriptor::from_sddl(&private_sddl(true)?)?;
    let attributes = descriptor.attributes();
    let name = wide_path(path);
    // SAFETY: `name` is NUL-terminated and `attributes` outlives the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Replaces the DACL of an existing directory with the private one.
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    let descriptor = Descriptor::from_sddl(&private_sddl(true)?)?;
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl = null_mut();
    // SAFETY: the descriptor is valid; the ACL pointer stays inside it.
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
    {
        return Err(io::Error::other("no DACL in descriptor"));
    }
    let name = wide_path(path);
    // SAFETY: `name` is NUL-terminated and the ACL belongs to `descriptor`.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// Creates a new file, failing if it exists, usable by the current user only.
/// A reparse point at the path is not followed.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let descriptor = Descriptor::from_sddl(&private_sddl(false)?)?;
    let attributes = descriptor.attributes();
    let name = wide_path(path);
    // SAFETY: `name` is NUL-terminated and `attributes` outlives the call.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE_ACCESS,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh, owned file handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

// ---- the pipe ------------------------------------------------------------

struct Event(HANDLE);

impl Event {
    fn new() -> io::Result<Self> {
        // SAFETY: an unnamed manual-reset event.
        let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: created by CreateEventW.
        unsafe { CloseHandle(self.0) };
    }
}

fn overlapped(event: &Event) -> OVERLAPPED {
    // SAFETY: an all-zero OVERLAPPED is its documented initial state.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.0;
    overlapped
}

fn millis(timeout: Duration) -> u32 {
    timeout.as_millis().clamp(1, u128::from(u32::MAX - 1)) as u32
}

/// One connected pipe instance.
pub struct PipeConnection {
    handle: HANDLE,
    event: Event,
}

// SAFETY: the handle is used by one thread at a time (the connection's).
unsafe impl Send for PipeConnection {}

impl Drop for PipeConnection {
    fn drop(&mut self) {
        // SAFETY: the pipe instance is owned. Closing (not disconnecting) lets
        // the client still read what was written before.
        unsafe { CloseHandle(self.handle) };
    }
}

impl PipeConnection {
    /// Finishes an overlapped operation that returned pending: waits up to
    /// `timeout`, and cancels it on a timeout. `Ok(None)` means it timed out
    /// and nothing was transferred.
    fn finish(&self, overlapped: &mut OVERLAPPED, timeout: Duration) -> io::Result<Option<u32>> {
        // SAFETY: `overlapped` is the one the pending operation uses and stays
        // alive until the operation has completed or been cancelled.
        unsafe {
            let waited = WaitForSingleObject(self.event.0, millis(timeout));
            let mut transferred = 0u32;
            if waited == WAIT_OBJECT_0 {
                return if GetOverlappedResult(self.handle, overlapped, &mut transferred, 0) != 0 {
                    Ok(Some(transferred))
                } else {
                    Err(io::Error::from_raw_os_error(GetLastError() as i32))
                };
            }
            debug_assert_eq!(waited, WAIT_TIMEOUT);
            CancelIoEx(self.handle, overlapped);
            // Wait for the cancellation to land; the operation may still have
            // completed just before it.
            if GetOverlappedResult(self.handle, overlapped, &mut transferred, 1) != 0 {
                return Ok(Some(transferred));
            }
            let error = GetLastError();
            if error == ERROR_OPERATION_ABORTED {
                Ok(None)
            } else {
                Err(io::Error::from_raw_os_error(error as i32))
            }
        }
    }
}

fn map_end(error: u32) -> io::Error {
    match error {
        ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA => {
            io::Error::from(io::ErrorKind::BrokenPipe)
        }
        other => io::Error::from_raw_os_error(other as i32),
    }
}

impl Connection for PipeConnection {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut overlapped = overlapped(&self.event);
        let length = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: `buf` and `overlapped` outlive the operation (see `finish`).
        let ok = unsafe {
            ReadFile(
                self.handle,
                buf.as_mut_ptr(),
                length,
                null_mut(),
                &mut overlapped,
            )
        };
        let pending = ok == 0 && unsafe { GetLastError() } == ERROR_IO_PENDING;
        if ok == 0 && !pending {
            let error = unsafe { GetLastError() };
            // The client closing its end is an end of stream, not a failure.
            return if error == ERROR_BROKEN_PIPE || error == ERROR_PIPE_NOT_CONNECTED {
                Ok(0)
            } else {
                Err(map_end(error))
            };
        }
        match self.finish(&mut overlapped, timeout) {
            Ok(Some(read)) => Ok(read as usize),
            Ok(None) => Err(io::ErrorKind::TimedOut.into()),
            Err(error)
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32)
                    || error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED as i32) =>
            {
                Ok(0)
            }
            Err(error) => Err(error),
        }
    }

    fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let mut overlapped = overlapped(&self.event);
        let length = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: as in `read_timeout`.
        let ok = unsafe {
            WriteFile(
                self.handle,
                buf.as_ptr(),
                length,
                null_mut(),
                &mut overlapped,
            )
        };
        let pending = ok == 0 && unsafe { GetLastError() } == ERROR_IO_PENDING;
        if ok == 0 && !pending {
            return Err(map_end(unsafe { GetLastError() }));
        }
        match self.finish(&mut overlapped, timeout)? {
            Some(written) => Ok(written as usize),
            None => Err(io::ErrorKind::TimedOut.into()),
        }
    }
}

/// A pipe instance waiting for a client.
struct Waiting {
    handle: HANDLE,
    event: Event,
    /// Boxed so its address is stable while the connect is pending.
    overlapped: Box<OVERLAPPED>,
    connected: bool,
}

impl Waiting {
    /// Hands the connected pipe to a [`PipeConnection`] without closing it.
    fn into_connection(self) -> PipeConnection {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: each field is read out of the ManuallyDrop value exactly once
        // and `Waiting::drop` never runs, so nothing is released twice.
        unsafe {
            let event = std::ptr::read(&this.event);
            drop(std::ptr::read(&this.overlapped));
            PipeConnection {
                handle: this.handle,
                event,
            }
        }
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        // SAFETY: cancel and wait out a pending connect before the overlapped
        // structure and the handle go away.
        unsafe {
            if !self.connected {
                CancelIoEx(self.handle, &*self.overlapped);
                let mut transferred = 0;
                GetOverlappedResult(self.handle, &*self.overlapped, &mut transferred, 1);
            }
            CloseHandle(self.handle);
        }
    }
}

/// Accepts clients on `\\.\pipe\<name>`.
pub struct PipeListener {
    name: Vec<u16>,
    descriptor: Descriptor,
    waiting: Option<Waiting>,
    first: bool,
}

// SAFETY: the listener is used by the accept thread only.
unsafe impl Send for PipeListener {}

impl PipeListener {
    /// Creates the first instance of the pipe, so a name already in use by
    /// another process fails here. `name` is the short name of the descriptor.
    pub fn create(name: &str) -> io::Result<Self> {
        // The current user may create more instances; the logon session may
        // only read and write.
        let mut sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        if let Some(logon) = logon_sid() {
            sddl.push_str(&format!("(A;;{PIPE_CLIENT_RIGHTS};;;{logon})"));
        }
        let mut listener = Self {
            name: wide(&format!(r"\\.\pipe\{name}")),
            descriptor: Descriptor::from_sddl(&sddl)?,
            waiting: None,
            first: true,
        };
        listener.waiting = Some(listener.new_instance()?);
        Ok(listener)
    }

    fn new_instance(&mut self) -> io::Result<Waiting> {
        let attributes = self.descriptor.attributes();
        let first = if self.first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
        // SAFETY: `name` is NUL-terminated and `attributes` outlives the call.
        let handle = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        self.first = false;
        let event = Event::new().inspect_err(|_| unsafe {
            CloseHandle(handle);
        })?;
        let mut overlapped = Box::new(overlapped(&event));
        // SAFETY: the boxed overlapped lives as long as the pending connect.
        let connected = unsafe { ConnectNamedPipe(handle, &mut *overlapped) } != 0;
        let mut waiting = Waiting {
            handle,
            event,
            overlapped,
            connected,
        };
        if !connected {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => waiting.connected = true,
                ERROR_IO_PENDING => {}
                error => {
                    // Nothing is pending: dropping must not wait for a cancel.
                    waiting.connected = true;
                    return Err(io::Error::from_raw_os_error(error as i32));
                }
            }
        }
        Ok(waiting)
    }
}

impl Listener for PipeListener {
    type Conn = PipeConnection;

    fn accept(&mut self, timeout: Duration) -> io::Result<Option<PipeConnection>> {
        if self.waiting.is_none() {
            self.waiting = Some(self.new_instance()?);
        }
        let waiting = self.waiting.as_mut().expect("just created");
        if !waiting.connected {
            // SAFETY: the event belongs to the pending connect.
            let waited = unsafe { WaitForSingleObject(waiting.event.0, millis(timeout)) };
            if waited == WAIT_TIMEOUT {
                return Ok(None);
            }
            let mut transferred = 0;
            // SAFETY: the overlapped is the pending connect's.
            if unsafe {
                GetOverlappedResult(waiting.handle, &*waiting.overlapped, &mut transferred, 0)
            } == 0
            {
                self.waiting = None;
                return Err(io::Error::last_os_error());
            }
            waiting.connected = true;
        }
        // Take the connected instance and prepare the next one at once, so the
        // name is never without a free instance for long.
        let done = self.waiting.take().expect("connected");
        let connection = done.into_connection();
        self.waiting = self.new_instance().ok();
        Ok(Some(connection))
    }
}
