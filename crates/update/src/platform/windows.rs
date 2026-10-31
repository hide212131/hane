//! Windows placement checks: portable x64 on a local fixed NTFS volume.
//!
//! The two volume queries are the only `unsafe` in the update crate. Network
//! shares, removable media, other file systems and non-`hane.exe` binaries
//! (for example a development `cargo` output) are not supported placements.

#![allow(unsafe_code, reason = "two read-only volume queries through windows-sys")]

use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, Prefix};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetLengthSid, GetTokenInformation, IsValidSid, RevertToSelf, SECURITY_ATTRIBUTES,
    TOKEN_QUERY,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, CreatePipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, ImpersonateNamedPipeClient, SetNamedPipeHandleState,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::UI::Shell::{SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessWithTokenW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThread,
    GetExitCodeProcess, GetProcessId, GetProcessTimes, OpenProcess, OpenProcessToken, OpenThreadToken,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, ResumeThread,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};

use super::{HELPER_FILE_NAME, HelperSource, Placement, owner_name, unsupported};
use crate::elevation::{
    AcceptedPeer, ElevatedListener, ElevatedProcess, ElevationOs, LaunchFailure, PeerFacts,
};
use crate::error::{ErrorCode, Result, UpdateError};
use crate::fsutil::{hash_regular_file, read_limited};
use crate::identity::{FileIdentity, InstallFacts, InstallId, SignedInventory, TransactionId};
use crate::limits::{MAX_INVENTORY_BYTES, MAX_SIGNATURE_BYTES};
use crate::model::{INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME, Target};
use crate::trust::TrustPolicy;

const EXE_NAME: &str = "hane.exe";

/// `GetDriveTypeW` result for a local fixed disk (winbase.h `DRIVE_FIXED`).
/// Defined here because windows-sys exposes it only behind the unrelated
/// `Win32_System_WindowsProgramming` feature.
const DRIVE_FIXED: u32 = 3;

/// The volume that really holds a path, including directory mount points.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Volume {
    /// Stable `\\?\Volume{GUID}\` name of the volume; the volume identity.
    guid: String,
    serial: u32,
    file_system: String,
    fixed: bool,
}

const VOLUME_BUFFER_UNITS: usize = 1024;

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn from_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Query the volume that actually contains `path` (not the drive-letter root),
/// resolving directory mount points. Any failure is an error, never a guess.
fn query_volume(path: &Path) -> Result<Volume> {
    let fail = |what: &str| UpdateError::new(ErrorCode::Io, what.to_owned());
    let path_w = wide_path(path);
    let mut mount_root = [0_u16; VOLUME_BUFFER_UNITS];
    // SAFETY: `path_w` is NUL-terminated and the out buffer length is passed.
    let ok = unsafe {
        GetVolumePathNameW(path_w.as_ptr(), mount_root.as_mut_ptr(), mount_root.len() as u32)
    };
    if ok == 0 {
        return Err(fail("the install volume root could not be resolved"));
    }
    let mut guid_buf = [0_u16; 64];
    // SAFETY: `mount_root` is NUL-terminated by the previous call and the out
    // buffer length is passed.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(
            mount_root.as_ptr(),
            guid_buf.as_mut_ptr(),
            guid_buf.len() as u32,
        )
    };
    if ok == 0 {
        return Err(fail("the install volume identity could not be resolved"));
    }
    let guid = from_wide(&guid_buf);
    if !guid.starts_with(r"\\?\Volume{") || !guid.ends_with('\\') {
        return Err(fail("the install volume identity is not recognised"));
    }
    query_volume_info(&guid)
}

fn query_volume_info(guid: &str) -> Result<Volume> {
    let root_w = std::ffi::OsStr::new(guid)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<u16>>();
    let mut serial = 0_u32;
    let mut max_component = 0_u32;
    let mut flags = 0_u32;
    let mut fs_name = [0_u16; 64];
    // SAFETY: `root_w` is NUL-terminated; every out-pointer refers to a live
    // local of the size the API documents; the volume-name buffer is omitted.
    let ok = unsafe {
        GetVolumeInformationW(
            root_w.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            &mut max_component,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(UpdateError::new(
            ErrorCode::Io,
            "the install volume could not be queried",
        ));
    }
    let len = fs_name.iter().position(|unit| *unit == 0).unwrap_or(fs_name.len());
    // SAFETY: `root_w` is a NUL-terminated wide string.
    let drive_type = unsafe { GetDriveTypeW(root_w.as_ptr()) };
    Ok(Volume {
        guid: guid.to_owned(),
        serial,
        file_system: String::from_utf16_lossy(&fs_name[..len]),
        fixed: drive_type == DRIVE_FIXED,
    })
}

/// Install root and coordination/stage area must share one fixed NTFS volume,
/// otherwise atomic replacement and recovery cannot be relied on.
fn ensure_supported_volumes(install: &Volume, coordination: &Volume) -> Result<()> {
    if install.guid != coordination.guid || install.serial != coordination.serial {
        return Err(unsupported(
            "the install root and its coordination area are on different volumes",
        ));
    }
    if !install.fixed {
        return Err(unsupported("only a local fixed drive is supported"));
    }
    if !install.file_system.eq_ignore_ascii_case("NTFS") {
        return Err(unsupported("only NTFS is supported"));
    }
    Ok(())
}

/// Bytes the calling user can still allocate on the volume holding `path`
/// (quota-aware). Any failure is an error, never a guess.
pub fn available_bytes(path: &Path) -> Result<u64> {
    let path_w = wide_path(path);
    let mut available = 0_u64;
    // SAFETY: `path_w` is NUL-terminated; only the first out-pointer refers to
    // a live local and the other two are documented as optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            path_w.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(UpdateError::new(
            ErrorCode::Io,
            "the free space of the install volume could not be queried",
        ));
    }
    Ok(available)
}

/// Creation time of a process as a 64-bit FILETIME, or `None` when unreadable.
fn creation_time(process: HANDLE) -> Option<u64> {
    let mut created = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    // SAFETY: `process` is a live process handle with query access and every
    // out-pointer refers to a live local FILETIME.
    let ok = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    (ok != 0).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// The id of the process that really started this one, as the OS records it.
/// A parent that has exited, cannot be opened, or whose id was reused by a
/// process created *after* this one is reported as `None`, never guessed.
pub fn parent_process_id() -> Option<u32> {
    // SAFETY: no arguments; returns the calling process id.
    let own = unsafe { GetCurrentProcessId() };
    // SAFETY: a plain process snapshot; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: PROCESSENTRY32W is plain data for which all-zero is valid.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut parent = None;
    // SAFETY: `snapshot` is valid and `entry.dwSize` is set as documented.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != 0 {
        if entry.th32ProcessID == own {
            parent = Some(entry.th32ParentProcessID);
            break;
        }
        // SAFETY: same handle and entry as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    // SAFETY: `snapshot` is a handle this function owns.
    unsafe { CloseHandle(snapshot) };
    let parent = parent?;
    // SAFETY: the current-process pseudo handle needs no closing.
    let own_created = creation_time(unsafe { GetCurrentProcess() })?;
    // SAFETY: plain open for limited query; null means failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent) };
    if handle.is_null() {
        return None;
    }
    let parent_created = creation_time(handle);
    // SAFETY: `handle` is owned by this function.
    unsafe { CloseHandle(handle) };
    (parent_created? <= own_created).then_some(parent)
}

/// The real consent launch: `ShellExecuteExW` with the `runas` verb, an absolute
/// executable path, the one fixed argument [`crate::elevation::ELEVATED_ARGUMENT`]
/// and no shell string. The real process handle is kept
/// (`SEE_MASK_NOCLOSEPROCESS`) so the application observes the very process it
/// started.
pub struct RunasLauncher {
    /// Owner window of the consent prompt; `0` for none.
    pub owner_hwnd: usize,
}

/// The elevated process, closed when dropped.
pub struct ShellProcess {
    handle: HANDLE,
    pid: u32,
}

impl Drop for ShellProcess {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by `ShellExecuteExW` with
        // `SEE_MASK_NOCLOSEPROCESS` and is owned by this value.
        unsafe { CloseHandle(self.handle) };
    }
}

// SAFETY: the process handle is used only through thread-safe Win32 calls
// (wait, query, terminate, close) and is owned by exactly one value.
unsafe impl Send for ShellProcess {}

impl crate::elevation::ElevatedProcess for ShellProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn terminate(&mut self) {
        // SAFETY: `handle` is a live process handle owned by this value. The
        // call may be refused for lack of access; the caller confirms the exit
        // separately and keeps the transaction when it cannot.
        unsafe { TerminateProcess(self.handle, 1) };
    }

    fn wait_exit(&mut self, timeout: std::time::Duration) -> Option<u32> {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: `handle` is a live process handle owned by this value.
        let waited = unsafe { WaitForSingleObject(self.handle, millis) };
        if waited != 0 {
            return None;
        }
        let mut code = 0_u32;
        // SAFETY: `handle` is live and `code` is a live local.
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        (ok != 0).then_some(code)
    }
}

impl crate::elevation::ElevatedLauncher for RunasLauncher {
    type Process = ShellProcess;

    fn launch(&mut self, helper: &Path) -> std::result::Result<ShellProcess, crate::elevation::LaunchFailure> {
        use crate::elevation::LaunchFailure;
        // SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC
        const MASK: u32 = 0x0000_0040 | 0x0000_0100;
        const SW_HIDE: i32 = 0;
        let failure = |stage: &'static str, os_error: Option<u32>| LaunchFailure {
            stage,
            api: "ShellExecuteExW",
            os_error,
            path: helper.display().to_string(),
        };
        if !helper.is_absolute() {
            return Err(failure("validate-path", None));
        }
        let verb: Vec<u16> = "runas".encode_utf16().chain(Some(0)).collect();
        let file = wide_path(helper);
        let parameters: Vec<u16> = crate::elevation::ELEVATED_ARGUMENT
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let directory = helper.parent().map(wide_path);
        // SAFETY: SHELLEXECUTEINFOW is plain data for which all-zero is valid.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = MASK;
        info.hwnd = self.owner_hwnd as HWND;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = parameters.as_ptr();
        info.lpDirectory = directory.as_ref().map_or(std::ptr::null(), |dir| dir.as_ptr());
        info.nShow = SW_HIDE;
        // SAFETY: `info` is initialised as documented and every pointer it holds
        // refers to a NUL-terminated buffer that outlives the call.
        let ok = unsafe { ShellExecuteExW(&mut info) };
        if ok == 0 {
            let code = std::io::Error::last_os_error().raw_os_error().map(|raw| raw as u32);
            return Err(failure("consent-launch", code));
        }
        if info.hProcess.is_null() {
            return Err(failure("no-process-handle", None));
        }
        // SAFETY: `hProcess` is the live handle just returned.
        let pid = unsafe { GetProcessId(info.hProcess) };
        let process = ShellProcess { handle: info.hProcess, pid };
        if pid == 0 {
            return Err(failure("process-id", None));
        }
        Ok(process)
    }
}

// ------------------------------------------------- the elevated local channel

// Values of the Win32 constants used below (winbase.h / winnt.h).
const PIPE_ACCESS_INBOUND: u32 = 0x1;
const PIPE_ACCESS_OUTBOUND: u32 = 0x2;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_TYPE_BYTE: u32 = 0;
const PIPE_READMODE_BYTE: u32 = 0;
const PIPE_WAIT: u32 = 0;
const PIPE_NOWAIT: u32 = 1;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 8;
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_NO_DATA: i32 = 232;
const ERROR_PIPE_CONNECTED: i32 = 535;
const ERROR_PIPE_LISTENING: i32 = 536;
/// The client lets the server read its identity but not act as it.
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
const SDDL_REVISION_1: u32 = 1;
const TOKEN_USER_CLASS: i32 = 1;
const TOKEN_ELEVATION_CLASS: i32 = 20;
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const PIPE_POLL: Duration = Duration::from_millis(25);

/// Only an elevated administrator token can open the endpoint: the pipe
/// grants read/write to BUILTIN\Administrators alone, and that group is
/// deny-only in a standard (filtered) token. The creating user owns the object.
const PIPE_DACL: &str = "D:P(A;;GRGW;;;BA)";

/// The first byte the elevated helper writes after it opened both pipes. It is
/// not part of the framed protocol: the server must read once before Windows
/// lets it ask for the client's token.
const PIPE_READY_MARKER: u8 = 0xE1;

fn wide_text(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn os_failure(what: &str) -> UpdateError {
    let code = std::io::Error::last_os_error().raw_os_error();
    io_failure(what, code)
}

fn io_failure(what: &str, code: Option<i32>) -> UpdateError {
    UpdateError::new(
        ErrorCode::Io,
        format!("{what} (os_error={})", code.map_or_else(|| "none".to_owned(), |c| c.to_string())),
    )
}

/// The names of the two one-way pipes of a transaction: application to helper
/// and helper to application. One pipe per direction, because a synchronous
/// read on a duplex handle would block the writes of the other thread.
fn pipe_names(transaction: &TransactionId) -> (String, String) {
    let base = format!(r"\\.\pipe\hane-update-{}", transaction.as_str());
    (format!("{base}-a2h"), format!("{base}-h2a"))
}

/// A handle closed when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by the OS to this value alone.
        unsafe { CloseHandle(self.0) };
    }
}

/// System-allocated memory released with `LocalFree`.
struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a call that documents `LocalFree`.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn token_information(token: HANDLE, class: i32) -> Result<Vec<u64>> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer of length 0 asks only for the required size.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(os_failure("the token could not be queried"));
    }
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer is `buffer.len() * 8` bytes and 8-byte aligned.
    let ok = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(os_failure("the token could not be queried"));
    }
    Ok(buffer)
}

/// The account of a token, in the same text form as the install owner
/// (`acl::principal_id`), so the two are compared as the same kind of value.
fn token_user_sid(token: HANDLE) -> Result<String> {
    let buffer = token_information(token, TOKEN_USER_CLASS)?;
    // SAFETY: on success the buffer begins with a SID pointer into itself.
    let sid = unsafe { *buffer.as_ptr().cast::<*mut c_void>() };
    // SAFETY: validity is checked before the length is read and the SID lives
    // inside `buffer`, which outlives this block.
    let bytes = unsafe {
        if sid.is_null() || IsValidSid(sid) == 0 {
            return Err(UpdateError::new(ErrorCode::Io, "the token has no valid account"));
        }
        std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize)
    };
    Ok(format!(
        "sid:{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

fn token_is_elevated(token: HANDLE) -> Result<bool> {
    // `TOKEN_ELEVATION` is a fixed-size structure of one `u32`, so it is read
    // directly rather than through the size probe of `token_information`,
    // which does not report a size for this class on every Windows version.
    // Any failure stays an error: it is never read as "not elevated".
    let mut elevation = 0_u32;
    let mut returned = 0_u32;
    // SAFETY: the buffer is exactly one `u32` and `returned` is a live out-pointer.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            std::ptr::addr_of_mut!(elevation).cast(),
            std::mem::size_of::<u32>() as u32,
            &mut returned,
        )
    };
    if ok == 0 {
        return Err(os_failure("the token elevation could not be queried"));
    }
    if returned != std::mem::size_of::<u32>() as u32 {
        return Err(UpdateError::new(
            ErrorCode::Io,
            "the token elevation had an unexpected size",
        ));
    }
    Ok(elevation != 0)
}

fn own_token() -> Result<Handle> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle needs no close; `token` is a live out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(os_failure("the process token could not be opened"));
    }
    Ok(Handle(token))
}

fn process_token(pid: u32) -> Result<Handle> {
    // SAFETY: plain open for limited query; null means failure.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(os_failure("the process could not be opened"));
    }
    let process = Handle(process);
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is live and `token` is a live out-pointer.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
        return Err(os_failure("the process token could not be opened"));
    }
    Ok(Handle(token))
}

/// The session of a process, as the OS records it.
fn process_session(pid: u32) -> Result<u32> {
    let mut session = 0_u32;
    // SAFETY: `session` is a live out-pointer.
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(os_failure("the session of the process could not be determined"));
    }
    Ok(session)
}

/// One one-way server pipe of the application. `inbound` is the direction as
/// the server sees it. The first instance flag makes a pre-existing pipe of the
/// same name a failure instead of a hijack, the DACL admits elevated
/// administrators only and remote clients are rejected.
fn create_server_pipe(name: &str, inbound: bool) -> Result<File> {
    let sddl = wide_text(PIPE_DACL);
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated and `descriptor` is a live out-pointer.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(os_failure("the access list of the update channel could not be built"));
    }
    let _descriptor = LocalMemory(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let direction = if inbound { PIPE_ACCESS_INBOUND } else { PIPE_ACCESS_OUTBOUND };
    let wide_name = wide_text(name);
    // SAFETY: `wide_name` is NUL-terminated; `attributes` and the descriptor it
    // points to outlive the call. The pipe is polled without blocking until the
    // peer is verified (`PIPE_NOWAIT`).
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            direction | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(os_failure("the update channel could not be created"));
    }
    // SAFETY: `handle` is a pipe handle this function owns; the `File` takes it.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Polls for a client without blocking: `true` once one is connected.
fn poll_connect(pipe: &File) -> Result<bool> {
    // SAFETY: `pipe` is a live server-side pipe handle; there is no OVERLAPPED.
    if unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0 {
        return Ok(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(ERROR_PIPE_CONNECTED) => Ok(true),
        Some(ERROR_PIPE_LISTENING) => Ok(false),
        other => Err(io_failure("the update channel could not accept a client", other)),
    }
}

fn set_wait_mode(pipe: &File) -> Result<()> {
    let mode = PIPE_WAIT | PIPE_READMODE_BYTE;
    // SAFETY: the handle is live and `mode` is a live local; the two limits
    // are documented as optional.
    let ok = unsafe {
        SetNamedPipeHandleState(pipe.as_raw_handle(), &mode, std::ptr::null(), std::ptr::null())
    };
    if ok == 0 {
        return Err(os_failure("the update channel mode could not be set"));
    }
    Ok(())
}

fn pipe_client_pid(pipe: &File) -> Result<u32> {
    let mut pid = 0_u32;
    // SAFETY: the handle is a live server-side pipe and `pid` a live local.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(os_failure("the process behind the update channel could not be identified"));
    }
    Ok(pid)
}

fn pipe_server_pid(pipe: &File) -> Result<u32> {
    let mut pid = 0_u32;
    // SAFETY: the handle is a live client-side pipe and `pid` a live local.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(os_failure("the owner of the update channel could not be identified"));
    }
    Ok(pid)
}

/// The account and elevation of the connected client, read from the token the
/// OS attaches to the connection (identification level only). The server must
/// have read from the pipe once.
fn pipe_client_token_facts(pipe: &File) -> Result<(String, bool)> {
    // SAFETY: the handle is a live server-side pipe.
    if unsafe { ImpersonateNamedPipeClient(pipe.as_raw_handle()) } == 0 {
        return Err(os_failure("the client of the update channel could not be identified"));
    }
    let facts = impersonated_token_facts();
    // SAFETY: ends the impersonation started above on this very thread.
    if unsafe { RevertToSelf() } == 0 {
        return Err(os_failure("the identification of the client could not be ended"));
    }
    facts
}

fn impersonated_token_facts() -> Result<(String, bool)> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the thread pseudo handle needs no close; the token is opened
    // against the process identity (`OpenAsSelf`) because an identification
    // level token cannot be used for access checks.
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        return Err(os_failure("the identity of the client could not be read"));
    }
    let token = Handle(token);
    Ok((token_user_sid(token.0)?, token_is_elevated(token.0)?))
}

/// The application's two one-way pipes, created before the helper is launched.
pub struct PipeListener {
    to_helper: Option<File>,
    from_helper: Option<File>,
}

impl PipeListener {
    pub fn create(transaction: &TransactionId) -> Result<Self> {
        let (to_helper, from_helper) = pipe_names(transaction);
        Ok(Self {
            to_helper: Some(create_server_pipe(&to_helper, false)?),
            from_helper: Some(create_server_pipe(&from_helper, true)?),
        })
    }
}

/// Reads the ready marker without blocking until `deadline`.
fn read_ready_marker(
    pipe: &mut File,
    deadline: Instant,
    helper_alive: &mut dyn FnMut() -> bool,
) -> Result<()> {
    let mut byte = [0_u8; 1];
    loop {
        match pipe.read(&mut byte) {
            Ok(1) if byte[0] == PIPE_READY_MARKER => return Ok(()),
            Ok(1) => {
                return Err(UpdateError::new(
                    ErrorCode::IdentityMismatch,
                    "the update channel peer did not start with the ready marker",
                ));
            }
            Ok(_) => {
                return Err(UpdateError::new(
                    ErrorCode::Truncated,
                    "the update channel was closed before the helper was ready",
                ));
            }
            Err(err) if err.raw_os_error() == Some(ERROR_NO_DATA) => {}
            Err(err) => return Err(io_failure("the update channel could not be read", err.raw_os_error())),
        }
        if !helper_alive() {
            return Err(UpdateError::new(
                ErrorCode::Truncated,
                "the elevated helper ended before it connected",
            ));
        }
        if Instant::now() >= deadline {
            return Err(UpdateError::new(ErrorCode::Timeout, "the elevated helper did not connect"));
        }
        std::thread::sleep(PIPE_POLL);
    }
}

impl ElevatedListener for PipeListener {
    fn accept(
        &mut self,
        timeout: Duration,
        helper_alive: &mut dyn FnMut() -> bool,
    ) -> Result<AcceptedPeer> {
        let (Some(to_helper), Some(mut from_helper)) = (self.to_helper.take(), self.from_helper.take())
        else {
            return Err(UpdateError::new(
                ErrorCode::TransitionInvalid,
                "the update channel accepts only one connection",
            ));
        };
        let deadline = Instant::now() + timeout;
        let (mut to_connected, mut from_connected) = (false, false);
        while !(to_connected && from_connected) {
            to_connected = to_connected || poll_connect(&to_helper)?;
            from_connected = from_connected || poll_connect(&from_helper)?;
            if to_connected && from_connected {
                break;
            }
            if !helper_alive() {
                return Err(UpdateError::new(
                    ErrorCode::Truncated,
                    "the elevated helper ended before it connected",
                ));
            }
            if Instant::now() >= deadline {
                return Err(UpdateError::new(ErrorCode::Timeout, "the elevated helper did not connect"));
            }
            std::thread::sleep(PIPE_POLL);
        }
        read_ready_marker(&mut from_helper, deadline, helper_alive)?;
        // Everything below is what the OS says about the connection.
        let pid = pipe_client_pid(&from_helper)?;
        if pipe_client_pid(&to_helper)? != pid {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the two update channels were opened by different processes",
            ));
        }
        let (token_sid, token_elevated) = pipe_client_token_facts(&from_helper)?;
        let session_id = process_session(pid)?;
        set_wait_mode(&to_helper)?;
        set_wait_mode(&from_helper)?;
        Ok(AcceptedPeer {
            facts: PeerFacts { pid, session_id, token_sid, token_elevated },
            reader: Box::new(from_helper),
            writer: Box::new(to_helper),
        })
    }
}

/// The elevated helper's ends of the two pipes, after the application behind
/// them was proven to be the requesting account's process in this session.
pub struct HelperPipes {
    pub reader: File,
    pub writer: File,
    /// The OS-reported owner of the pipes: the application.
    pub server_pid: u32,
}

fn open_client_pipe(name: &str, read: bool) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut last = None;
    for _ in 0..50 {
        let mut options = fs::OpenOptions::new();
        options
            .read(read)
            .write(!read)
            .security_qos_flags(SECURITY_IDENTIFICATION);
        match options.open(name) {
            Ok(file) => return Ok(file),
            Err(err) if matches!(err.raw_os_error(), Some(ERROR_PIPE_BUSY | ERROR_FILE_NOT_FOUND)) => {
                last = err.raw_os_error();
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(io_failure("the update channel could not be opened", err.raw_os_error())),
        }
    }
    Err(io_failure("the update channel could not be opened", last))
}

/// Helper side: refuses to start unless this process is elevated, then opens
/// the application's pipes and requires their owner to be a process of the same
/// account in the same session. Then it announces itself with the ready marker.
pub fn connect_helper_pipes(transaction: &TransactionId) -> Result<HelperPipes> {
    let own = own_token()?;
    if !token_is_elevated(own.0)? {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the elevated helper is not running elevated",
        ));
    }
    let own_sid = token_user_sid(own.0)?;
    let (to_helper, from_helper) = pipe_names(transaction);
    let reader = open_client_pipe(&to_helper, true)?;
    let mut writer = open_client_pipe(&from_helper, false)?;
    let server_pid = pipe_server_pid(&reader)?;
    if pipe_server_pid(&writer)? != server_pid {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the two update channels belong to different processes",
        ));
    }
    let server = process_token(server_pid)?;
    if token_user_sid(server.0)? != own_sid {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the application runs as another account",
        ));
    }
    // SAFETY: no arguments; returns the calling process id.
    if process_session(server_pid)? != process_session(unsafe { GetCurrentProcessId() })? {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the application runs in another session",
        ));
    }
    writer
        .write_all(&[PIPE_READY_MARKER])
        .and_then(|()| writer.flush())
        .map_err(|err| io_failure("the ready marker could not be sent", err.raw_os_error()))?;
    Ok(HelperPipes { reader, writer, server_pid })
}

// ------------------------------------------ the standard-token restart

const HANDLE_FLAG_INHERIT: u32 = 0x1;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TOKEN_DUPLICATE: u32 = 0x2;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
/// `SecurityImpersonation` and `TokenPrimary` of the `DuplicateTokenEx` enums.
const SECURITY_IMPERSONATION_LEVEL: i32 = 2;
const TOKEN_TYPE_PRIMARY: i32 = 1;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_FOREVER: u32 = u32::MAX;
const STOP_WAIT_MILLIS: u32 = 5_000;
const IMAGE_PATH_UNITS: usize = 32 * 1024;

impl Handle {
    /// Hands the handle over to a `File`, which closes it.
    fn into_file(self) -> File {
        let raw = self.0;
        std::mem::forget(self);
        // SAFETY: `raw` is an open handle this value owned; the `File` takes it.
        unsafe { File::from_raw_handle(raw) }
    }
}

/// Creation time of this process as a 64-bit FILETIME, or `None`.
pub fn own_process_creation_time() -> Option<u64> {
    // SAFETY: the current-process pseudo handle needs no closing.
    creation_time(unsafe { GetCurrentProcess() })
}

/// The unelevated primary token of the requesting user, taken from the desktop
/// shell of the same session. An elevated process cannot start an ordinary
/// process through its own (linked) token, and it must not inherit its own, so
/// the ordinary token is the shell's. It is accepted only when it is the
/// expected account, not elevated and in `expected_session`.
fn shell_standard_token(expected_sid: &str, expected_session: u32) -> Result<Handle> {
    // SAFETY: no arguments.
    let window = unsafe { GetShellWindow() };
    if window.is_null() {
        return Err(UpdateError::new(ErrorCode::Io, "the desktop shell is not running"));
    }
    let mut pid = 0_u32;
    // SAFETY: `window` was returned by the OS and `pid` is a live local.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == 0 {
        return Err(UpdateError::new(ErrorCode::Io, "the desktop shell process could not be identified"));
    }
    if process_session(pid)? != expected_session {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the desktop shell runs in another session",
        ));
    }
    // SAFETY: plain open for limited query; null means failure.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(os_failure("the desktop shell could not be opened"));
    }
    let process = Handle(process);
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is live and `token` is a live out-pointer.
    if unsafe { OpenProcessToken(process.0, TOKEN_DUPLICATE | TOKEN_QUERY, &mut token) } == 0 {
        return Err(os_failure("the token of the desktop shell could not be opened"));
    }
    let token = Handle(token);
    crate::elevation::require_standard_token(&token_user_sid(token.0)?, token_is_elevated(token.0)?, expected_sid)?;
    let mut primary: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` is live, the attributes are optional and `primary` is a
    // live out-pointer.
    let duplicated = unsafe {
        DuplicateTokenEx(
            token.0,
            MAXIMUM_ALLOWED,
            std::ptr::null(),
            SECURITY_IMPERSONATION_LEVEL,
            TOKEN_TYPE_PRIMARY,
            &mut primary,
        )
    };
    if duplicated == 0 {
        return Err(os_failure("the user's token could not be duplicated"));
    }
    let primary = Handle(primary);
    crate::elevation::require_standard_token(&token_user_sid(primary.0)?, token_is_elevated(primary.0)?, expected_sid)?;
    Ok(primary)
}

/// Whether an unelevated token of the process's own account can be obtained
/// here: the precondition of a restart that does not inherit an administrator
/// token. Nothing is started.
pub fn standard_token_available() -> Result<()> {
    let own = own_token()?;
    let sid = token_user_sid(own.0)?;
    // SAFETY: no arguments; returns the calling process id.
    let session = process_session(unsafe { GetCurrentProcessId() })?;
    shell_standard_token(&sid, session).map(|_| ())
}

/// One anonymous pipe for a child's standard stream: `(this process's end, the
/// child's end)`. Only the child's end is inheritable.
fn child_pipe(child_reads: bool) -> Result<(Handle, Handle)> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: both out-pointers are live locals; the attributes outlive the call.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(os_failure("a pipe to the new version could not be created"));
    }
    let (read, write) = (Handle(read), Handle(write));
    let (own, child) = if child_reads { (write, read) } else { (read, write) };
    // SAFETY: `own` is a live handle owned here.
    if unsafe { SetHandleInformation(own.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(os_failure("a pipe to the new version could not be protected"));
    }
    Ok((own, child))
}

/// A UTF-16 environment block (`name=value\0 ... \0`). A name or value that
/// cannot be represented is an error, never silently dropped.
fn environment_block(variables: &[(String, String)]) -> Result<Vec<u16>> {
    let mut block = Vec::new();
    for (name, value) in variables {
        if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "the environment of the new version holds an invalid variable",
            ));
        }
        block.extend(name.encode_utf16());
        block.push(u16::from(b'='));
        block.extend(value.encode_utf16());
        block.push(0);
    }
    // An empty block still needs its two terminators.
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn process_image_path(process: HANDLE) -> Result<String> {
    let mut buffer = vec![0_u16; IMAGE_PATH_UNITS];
    let mut length = buffer.len() as u32;
    // SAFETY: the buffer holds `length` units and both pointers are live.
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(os_failure("the executable of the new version could not be read"));
    }
    Ok(String::from_utf16_lossy(&buffer[..(length as usize).min(buffer.len())]))
}

/// The new version as a process the helper owns. Dropping the value closes the
/// handle without ending the process.
pub struct StandardTokenChild {
    handle: HANDLE,
    pid: u32,
}

impl Drop for StandardTokenChild {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateProcessWithTokenW` and is owned
        // by this value alone.
        unsafe { CloseHandle(self.handle) };
    }
}

// SAFETY: the process handle is used only through thread-safe Win32 calls and
// is owned by exactly one value.
unsafe impl Send for StandardTokenChild {}

impl crate::health::ChildProcess for StandardTokenChild {
    fn id(&self) -> u32 {
        self.pid
    }

    fn has_exited(&mut self) -> std::io::Result<bool> {
        // SAFETY: `handle` is a live process handle owned by this value.
        Ok(unsafe { WaitForSingleObject(self.handle, 0) } == WAIT_OBJECT_0)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        // SAFETY: `handle` is a live process handle owned by this value.
        if unsafe { TerminateProcess(self.handle, 1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn wait(&mut self) -> std::io::Result<()> {
        // SAFETY: `handle` is a live process handle owned by this value.
        if unsafe { WaitForSingleObject(self.handle, WAIT_FOREVER) } == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// A new version created with the user's standard token and not yet running.
/// Whatever the caller must record about it (process id, creation time) can be
/// published before [`SuspendedStandardChild::resume`]. Dropping it without a
/// resume ends the process.
pub struct SuspendedStandardChild {
    process: Option<StandardTokenChild>,
    thread: Handle,
    stdin: Option<File>,
    stdout: Option<File>,
    created: u64,
}

impl SuspendedStandardChild {
    pub fn pid(&self) -> u32 {
        self.process.as_ref().map_or(0, |process| process.pid)
    }

    /// Creation time of the process, as the OS reports it.
    pub fn created(&self) -> u64 {
        self.created
    }

    /// Lets the process run. The helper's ends of its stdin and stdout follow.
    pub fn resume(mut self) -> Result<(StandardTokenChild, File, File)> {
        // SAFETY: `thread` is the live primary thread handle of the process.
        if unsafe { ResumeThread(self.thread.0) } == u32::MAX {
            return Err(os_failure("the new version could not be resumed"));
        }
        match (self.process.take(), self.stdin.take(), self.stdout.take()) {
            (Some(process), Some(stdin), Some(stdout)) => Ok((process, stdin, stdout)),
            _ => Err(UpdateError::new(ErrorCode::TransitionInvalid, "the new version was already resumed")),
        }
    }
}

impl Drop for SuspendedStandardChild {
    fn drop(&mut self) {
        if let Some(process) = self.process.as_ref() {
            // SAFETY: `handle` is a live process handle owned by `process`.
            unsafe {
                TerminateProcess(process.handle, 1);
                WaitForSingleObject(process.handle, STOP_WAIT_MILLIS);
            }
        }
    }
}

/// Creates `exe` suspended with the unelevated token of the requesting user and
/// the given environment only, with piped stdin/stdout. The administrator token
/// of the calling helper is never inherited: the process is created from the
/// shell's token, and what the OS reports about the created process (id,
/// session, account, elevation, image) is verified before it is returned. Any
/// mismatch ends the process and is an error.
pub fn spawn_suspended_standard(exe: &Path, environment: &[(String, String)]) -> Result<SuspendedStandardChild> {
    if !exe.is_absolute() {
        return Err(UpdateError::new(ErrorCode::IdentityMismatch, "the new version has no absolute path"));
    }
    let own = own_token()?;
    let sid = token_user_sid(own.0)?;
    // SAFETY: no arguments; returns the calling process id.
    let session = process_session(unsafe { GetCurrentProcessId() })?;
    let token = shell_standard_token(&sid, session)?;
    let block = environment_block(environment)?;
    let (stdin_own, stdin_child) = child_pipe(true)?;
    let (stdout_own, stdout_child) = child_pipe(false)?;
    let null = fs::OpenOptions::new()
        .write(true)
        .open("NUL")
        .map_err(|err| io_failure("the null device could not be opened", err.raw_os_error()))?;
    // SAFETY: `null` is a live handle owned here.
    if unsafe { SetHandleInformation(null.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return Err(os_failure("the null device could not be made inheritable"));
    }
    let application = wide_path(exe);
    let mut command_line = wide_text(&format!("\"{}\"", exe.display()));
    let directory = exe.parent().map(wide_path);
    // SAFETY: STARTUPINFOW is plain data for which all-zero is valid.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = stdin_child.0;
    startup.hStdOutput = stdout_child.0;
    startup.hStdError = null.as_raw_handle();
    // SAFETY: PROCESS_INFORMATION is plain data for which all-zero is valid.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer refers to a NUL-terminated buffer or a live local
    // that outlives the call; the environment block is Unicode as flagged.
    let created = unsafe {
        CreateProcessWithTokenW(
            token.0,
            0,
            application.as_ptr(),
            command_line.as_mut_ptr(),
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            block.as_ptr().cast(),
            directory.as_ref().map_or(std::ptr::null(), |dir| dir.as_ptr()),
            &startup,
            &mut info,
        )
    };
    if created == 0 {
        return Err(os_failure("the new version could not be started with the user's token"));
    }
    // The child owns its ends now; keeping ours would hide its end of stream.
    drop((stdin_child, stdout_child, null, token));
    let mut suspended = SuspendedStandardChild {
        process: Some(StandardTokenChild { handle: info.hProcess, pid: info.dwProcessId }),
        thread: Handle(info.hThread),
        stdin: Some(stdin_own.into_file()),
        stdout: Some(stdout_own.into_file()),
        created: 0,
    };
    // Any error below drops `suspended`, which ends the not yet running process.
    suspended.created = creation_time(info.hProcess)
        .ok_or_else(|| UpdateError::new(ErrorCode::Io, "the creation time of the new version is unreadable"))?;
    let child_token = {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `hProcess` is live and `token` is a live out-pointer.
        if unsafe { OpenProcessToken(info.hProcess, TOKEN_QUERY, &mut token) } == 0 {
            return Err(os_failure("the token of the new version could not be opened"));
        }
        Handle(token)
    };
    // SAFETY: `hProcess` is the live handle just returned.
    let reported_pid = unsafe { GetProcessId(info.hProcess) };
    let facts = crate::elevation::StandardChildFacts {
        pid: reported_pid,
        session_id: process_session(reported_pid)?,
        token_sid: token_user_sid(child_token.0)?,
        token_elevated: token_is_elevated(child_token.0)?,
        image_path: process_image_path(info.hProcess)?,
    };
    let image = fs::canonicalize(exe)?;
    crate::elevation::verify_standard_child(
        &facts,
        &crate::elevation::ExpectedStandardChild {
            pid: info.dwProcessId,
            session_id: session,
            token_sid: sid,
            image_path: image.display().to_string(),
        },
    )?;
    Ok(suspended)
}

/// The Windows seam of the elevated route.
#[derive(Default)]
pub struct WindowsElevation {
    /// Owner window of the consent prompt; `0` for none.
    pub owner_hwnd: usize,
}

impl ElevationOs for WindowsElevation {
    fn standard_restart_available(&self) -> bool {
        // The application itself must be an ordinary process (otherwise there
        // is no prompt to give consent to) and the user's unelevated token must
        // be obtainable now: this is checked before any consent prompt.
        crate::elevation::STANDARD_RESTART_AVAILABLE
            && own_token()
                .and_then(|token| token_is_elevated(token.0))
                .is_ok_and(|elevated| !elevated)
            && standard_token_available().is_ok()
    }

    fn current_principals(&self, install_root: &Path) -> Result<(String, String)> {
        Ok((super::acl::principal_id()?, super::acl::directory_owner_id(install_root)?))
    }

    fn session_id(&self) -> Result<u32> {
        // SAFETY: no arguments; returns the calling process id.
        process_session(unsafe { GetCurrentProcessId() })
    }

    fn listen(&self, transaction: &TransactionId) -> Result<Box<dyn ElevatedListener>> {
        Ok(Box::new(PipeListener::create(transaction)?))
    }

    fn launch(&self, helper: &Path) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure> {
        let mut launcher = RunasLauncher { owner_hwnd: self.owner_hwnd };
        crate::elevation::ElevatedLauncher::launch(&mut launcher, helper)
            .map(|process| Box::new(process) as Box<dyn ElevatedProcess>)
    }
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
}

pub fn locate(exe: &Path) -> Result<Placement> {
    if Target::current()? != Target::WindowsX64 {
        return Err(unsupported("not a Windows x64 build"));
    }
    let exe = fs::canonicalize(exe)?;
    let name_ok = exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(EXE_NAME));
    if !name_ok {
        return Err(unsupported("the running binary is not hane.exe"));
    }
    let install_dir = exe
        .parent()
        .ok_or_else(|| unsupported("the install has no directory"))?
        .to_path_buf();
    let coordination_parent = install_dir
        .parent()
        .ok_or_else(|| unsupported("the install has no parent for coordination"))?
        .to_path_buf();
    let Some(Component::Prefix(prefix)) = install_dir.components().next() else {
        return Err(unsupported("the install path has no drive"));
    };
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
        return Err(unsupported("network and device paths are not supported"));
    }
    let volume = query_volume(&install_dir)?;
    let coordination_volume = query_volume(&coordination_parent)?;
    ensure_supported_volumes(&volume, &coordination_volume)?;
    let managed_files = [HELPER_FILE_NAME, INVENTORY_FILE_NAME, INVENTORY_SIGNATURE_FILE_NAME];
    let present = managed_files
        .iter()
        .filter(|name| is_regular_file(&install_dir.join(name)))
        .count();
    if present == 0 {
        // Nothing of the managed layout exists (for example a development build).
        return Err(unsupported("the install has no updater files"));
    }
    // Some but not all: a managed install that is incomplete or mid-update.
    // That is not an unsupported placement and must not start normal editing.
    if let Some(name) = managed_files
        .iter()
        .find(|name| !is_regular_file(&install_dir.join(name)))
    {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            format!("{name} is missing from a managed install"),
        ));
    }
    super::require_install_root_owner(&install_dir)?;
    let path_text = crate::identity::install_path_text(&install_dir)?;
    let path_text = path_text.strip_prefix(r"\\?\").unwrap_or(path_text).to_owned();
    let install_id = InstallId::derive(&InstallFacts {
        os: "windows",
        volume_id: &format!("{}:{:08x}", volume.guid, volume.serial),
        install_path: &path_text,
        owner: &owner_name()?,
    })?;
    Ok(Placement {
        target: Target::WindowsX64,
        install_root: install_dir,
        coordination_parent,
        install_id,
    })
}

/// The helper must be listed by the install's own signed inventory, and the
/// file on disk must match that entry.
pub fn installed_helper(placement: &Placement, policy: &TrustPolicy) -> Result<HelperSource> {
    let missing = |what: &str| UpdateError::new(ErrorCode::NotFound, format!("{what} is missing"));
    let root = &placement.install_root;
    let raw = read_limited(&root.join(INVENTORY_FILE_NAME), MAX_INVENTORY_BYTES)?
        .ok_or_else(|| missing("the install inventory"))?;
    let signature = read_limited(&root.join(INVENTORY_SIGNATURE_FILE_NAME), MAX_SIGNATURE_BYTES)?
        .ok_or_else(|| missing("the inventory signature"))?;
    let signed = SignedInventory::verify(&raw, &signature, policy)?;
    let entry = signed
        .inventory()
        .files
        .iter()
        .find(|file| file.path == HELPER_FILE_NAME)
        .ok_or_else(|| {
            UpdateError::new(ErrorCode::IdentityMismatch, "the inventory does not list the helper")
        })?;
    let identity = FileIdentity {
        size: entry.size,
        sha256: entry.sha256.clone(),
    };
    let path = root.join(HELPER_FILE_NAME);
    let on_disk = hash_regular_file(&path)?.map(|(size, sha256)| FileIdentity { size, sha256 });
    if on_disk.as_ref() != Some(&identity) {
        return Err(UpdateError::new(
            ErrorCode::InstallModified,
            "the installed helper differs from the signed inventory",
        ));
    }
    Ok(HelperSource { path, identity })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(guid: &str, serial: u32, file_system: &str, fixed: bool) -> Volume {
        Volume {
            guid: guid.to_owned(),
            serial,
            file_system: file_system.to_owned(),
            fixed,
        }
    }

    const SYSTEM: &str = r"\\?\Volume{11111111-1111-1111-1111-111111111111}\";
    const MOUNTED: &str = r"\\?\Volume{22222222-2222-2222-2222-222222222222}\";

    #[test]
    fn the_two_channels_are_one_way_and_belong_to_one_transaction() {
        let first = TransactionId::generate().unwrap();
        let second = TransactionId::generate().unwrap();
        let (to_helper, from_helper) = pipe_names(&first);
        assert_ne!(to_helper, from_helper);
        assert!(to_helper.starts_with(r"\\.\pipe\") && to_helper.contains(first.as_str()));
        assert!(from_helper.contains(first.as_str()));
        assert_ne!(pipe_names(&second).0, to_helper);
    }

    #[test]
    fn the_environment_of_the_new_version_is_exactly_what_was_given() {
        let block = environment_block(&[("A".to_owned(), "1".to_owned()), ("B".to_owned(), "x y".to_owned())]).unwrap();
        let expected: Vec<u16> = "A=1\0B=x y\0\0".encode_utf16().collect();
        assert_eq!(block, expected);
        assert_eq!(environment_block(&[]).unwrap(), vec![0, 0]);
        for bad in [("", "v"), ("A=B", "v"), ("A\0", "v"), ("A", "v\0")] {
            assert!(environment_block(&[(bad.0.to_owned(), bad.1.to_owned())]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_channel_admits_elevated_administrators_only() {
        // One protected ACE, for BUILTIN\Administrators, which a standard
        // (filtered) token carries as deny-only. No Everyone, user or
        // Authenticated Users entry.
        assert_eq!(PIPE_DACL.matches("(A;").count(), 1);
        assert!(PIPE_DACL.starts_with("D:P") && PIPE_DACL.ends_with(";BA)"));
    }

    #[test]
    fn the_own_account_and_session_are_readable_for_the_peer_comparison() {
        let token = own_token().unwrap();
        let sid = token_user_sid(token.0).unwrap();
        assert!(sid.starts_with("sid:"));
        assert_eq!(sid, super::super::acl::principal_id().unwrap());
        // SAFETY: no arguments; returns the calling process id.
        assert!(process_session(unsafe { GetCurrentProcessId() }).is_ok());
        // The elevation of the test process is whatever the runner has; it
        // must at least be answerable.
        assert!(token_is_elevated(token.0).is_ok());
    }

    #[test]
    fn same_fixed_ntfs_volume_is_supported() {
        let v = volume(SYSTEM, 1, "NTFS", true);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_ok());
    }

    #[test]
    fn mount_point_volume_that_is_fixed_ntfs_is_supported() {
        // Install root and coordination parent both live on the volume mounted
        // under a directory, regardless of the drive-letter volume's type.
        let v = volume(MOUNTED, 2, "ntfs", true);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_ok());
    }

    #[test]
    fn install_root_that_is_a_mount_point_splits_volumes() {
        let install = volume(MOUNTED, 2, "NTFS", true);
        let coordination = volume(SYSTEM, 1, "NTFS", true);
        assert!(ensure_supported_volumes(&install, &coordination).is_err());
    }

    #[test]
    fn same_serial_but_different_volume_is_rejected() {
        let install = volume(MOUNTED, 1, "NTFS", true);
        let coordination = volume(SYSTEM, 1, "NTFS", true);
        assert!(ensure_supported_volumes(&install, &coordination).is_err());
    }

    #[test]
    fn removable_mount_point_volume_is_rejected() {
        let v = volume(MOUNTED, 2, "NTFS", false);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_err());
    }

    #[test]
    fn non_ntfs_mount_point_volume_is_rejected() {
        let v = volume(MOUNTED, 2, "exFAT", true);
        assert!(ensure_supported_volumes(&v, &v.clone()).is_err());
    }
}
