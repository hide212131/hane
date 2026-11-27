//! Windows: the dedicated named pipe, and the engine process started with one
//! inherited anonymous pipe and placed in a job object that ends it (and the
//! recognition workers it starts) when Hane exits, however it exits.

use crate::transport::{
    ChildProcess, EngineSpec, Handoff, Platform, VoiceConnection, VoiceListener,
};
use hane_link::frame;
use hane_link::transport::{Connection, Listener};
use hane_link::windows::{PipeConnection, PipeListener};
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::ptr::{null, null_mut};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTUPINFOEXW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

struct Pipe(PipeConnection);

impl Connection for Pipe {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.0.read_timeout(buf, timeout)
    }

    fn write_timeout(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        self.0.write_timeout(buf, timeout)
    }
}

impl VoiceConnection for Pipe {
    fn peer_pid(&self) -> Option<u32> {
        self.0.client_pid()
    }
}

struct PipeServer(PipeListener);

impl VoiceListener for PipeServer {
    fn accept(&mut self, timeout: Duration) -> io::Result<Option<Box<dyn VoiceConnection>>> {
        Ok(self
            .0
            .accept(timeout)?
            .map(|connection| Box::new(Pipe(connection)) as Box<dyn VoiceConnection>))
    }
}

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// An owned handle, closed on drop.
struct Owned(HANDLE);

impl Owned {
    fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this process owns.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a handle is just a number for the kernel; it is used by one owner.
unsafe impl Send for Owned {}

struct WindowsChild {
    process: Owned,
    job: Owned,
    pid: u32,
}

impl ChildProcess for WindowsChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn exit_code(&mut self) -> Option<i32> {
        // SAFETY: the process handle is open.
        unsafe {
            if WaitForSingleObject(self.process.0, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            (GetExitCodeProcess(self.process.0, &mut code) != 0).then_some(code as i32)
        }
    }

    fn kill(&mut self) {
        // SAFETY: the job holds the engine and everything it started.
        unsafe { TerminateJobObject(self.job.0, 1) };
    }
}

impl Drop for WindowsChild {
    fn drop(&mut self) {
        // Closing the job handle (after this) also ends the tree; do it now
        // so a dropped link never leaves the engine running.
        self.kill();
    }
}

fn job_that_kills_on_close() -> io::Result<Owned> {
    // SAFETY: an unnamed job with default security.
    let job = Owned(unsafe { CreateJobObjectW(null(), null()) });
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: an all-zero limit structure is the documented initial state.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `limits` matches the information class and its size.
    let ok = unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// An attribute list naming the handles the child may inherit, freed on drop.
struct HandleList {
    buffer: Vec<u8>,
}

impl HandleList {
    fn new(handle: &HANDLE) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: a size query with a null list.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
        let mut buffer = vec![0u8; size];
        let list = buffer.as_mut_ptr().cast::<c_void>();
        // SAFETY: the buffer is as large as the query asked.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let this = Self { buffer };
        // SAFETY: `handle` outlives the list's use in CreateProcessW.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                this.list(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                (handle as *const HANDLE).cast::<c_void>(),
                size_of::<HANDLE>(),
                null_mut(),
                null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(this)
    }

    fn list(&self) -> *mut c_void {
        self.buffer.as_ptr().cast_mut().cast()
    }
}

impl Drop for HandleList {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`.
        unsafe { DeleteProcThreadAttributeList(self.list()) };
    }
}

fn quote(argument: &std::ffi::OsStr) -> String {
    let text = argument.to_string_lossy();
    format!("\"{}\"", text.replace('"', "\\\""))
}

pub struct WindowsPlatform;

impl Platform for WindowsPlatform {
    fn listen(&self, name: &str) -> io::Result<Box<dyn VoiceListener>> {
        Ok(Box::new(PipeServer(PipeListener::create_private(name)?)))
    }

    fn launch(&self, spec: &EngineSpec, handoff: &Handoff) -> io::Result<Box<dyn ChildProcess>> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let (mut read, mut write): (HANDLE, HANDLE) = (null_mut(), null_mut());
        // SAFETY: both out pointers are valid.
        if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let (read, write) = (Owned(read), Owned(write));
        // Only the read end goes to the child.
        // SAFETY: the handle is open.
        unsafe { SetHandleInformation(write.0, HANDLE_FLAG_INHERIT, 0) };

        let list = HandleList::new(&read.0)?;
        // SAFETY: an all-zero STARTUPINFOEXW is the documented initial state.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = list.list();

        let mut command = format!(
            "{} --voice-engine --voice-handoff {}",
            quote(spec.exe.as_os_str()),
            read.0 as usize
        );
        for extra in &spec.extra_args {
            command.push(' ');
            command.push_str(&quote(extra));
        }
        let mut command: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        let exe = wide(spec.exe.as_os_str());

        let job = job_that_kills_on_close()?;
        // SAFETY: an all-zero PROCESS_INFORMATION is filled by the call.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer is valid for the call; the child starts
        // suspended so it is in the job before it can start another process.
        let created = unsafe {
            CreateProcessW(
                exe.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        let (process, thread) = (Owned(info.hProcess), Owned(info.hThread));
        let child = WindowsChild {
            process,
            job,
            pid: info.dwProcessId,
        };
        // SAFETY: both handles are open.
        unsafe {
            if AssignProcessToJobObject(child.job.0, child.process.0) == 0 {
                let error = io::Error::last_os_error();
                // Not in the job yet: end the suspended process directly.
                TerminateProcess(child.process.0, 1);
                return Err(error);
            }
            ResumeThread(thread.0);
        }
        drop(thread);
        drop(read);
        // The frame is far below the pipe buffer: the write does not block.
        // SAFETY: the write end is an open pipe handle; the File takes it.
        let mut file = unsafe { File::from_raw_handle(write.0) };
        std::mem::forget(write);
        // A child that already ended cannot read it; that is reported by its exit
        // code during the connect wait, not as a failure to start.
        let _ = frame::write_frame(&mut file, &handoff.to_json());
        drop(file);
        Ok(Box::new(child))
    }
}
