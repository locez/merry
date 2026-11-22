//! Windows Job Object wrapper for deterministic process-tree termination.
//!
//! On Windows, killing only a shell child (`pwsh` / `powershell.exe` /
//! `cmd.exe`) leaves descendant build tools or dev servers running. Assigning
//! each spawned child to an anonymous Job Object configured with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ensures cancellation, explicit stop,
//! and session teardown terminate the entire descendant tree.
//!
//! Reviewed FFI boundary: `windows_sys` Win32 calls are confined to this
//! module and wrapped in an RAII handle.

#![allow(unsafe_code)]

use std::io;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
    },
};

/// Owns a Windows Job Object handle whose processes terminate when explicitly
/// killed or when the job handle is dropped.
pub(crate) struct WindowsProcessJob {
    handle: HANDLE,
}

// SAFETY: A Windows Job Object kernel HANDLE can be sent between threads and
// closed or terminated from any thread in the owning process.
unsafe impl Send for WindowsProcessJob {}
// SAFETY: Operations on `handle` are kernel-synchronized Win32 handle calls.
unsafe impl Sync for WindowsProcessJob {}

impl WindowsProcessJob {
    /// Creates an anonymous Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: Passing null security attributes and null name creates a
        // private unnamed Job Object owned by this process.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain C POD
        // struct where all-zeroes is a valid initialized value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `handle` is a valid job handle created above and `info` lives
        // for the duration of the call with exact byte size.
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: `handle` is owned exclusively here and not used after.
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }

        Ok(Self { handle })
    }

    /// Creates a Job Object and attaches `pid` to it.
    pub(crate) fn for_pid(pid: u32) -> io::Result<Self> {
        let job = Self::new()?;
        job.assign_pid(pid)?;
        Ok(job)
    }

    /// Attaches an existing child process by PID to this Job Object.
    pub(crate) fn assign_pid(&self, pid: u32) -> io::Result<()> {
        // SAFETY: `OpenProcess` takes plain integer arguments and returns a
        // nullable process handle closed before this method returns.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: Both `self.handle` and `process` are valid OS handles.
        let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
        let error = (ok == 0).then(io::Error::last_os_error);

        // SAFETY: `process` was opened above and is closed exactly once.
        unsafe {
            CloseHandle(process);
        }

        if let Some(error) = error {
            return Err(error);
        }
        Ok(())
    }

    /// Terminates every process currently belonging to this Job Object.
    pub(crate) fn terminate(&self) {
        // SAFETY: `self.handle` is a valid Job Object handle until `Drop`.
        let _ = unsafe { TerminateJobObject(self.handle, 1) };
    }
}

impl Drop for WindowsProcessJob {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is owned solely by this struct and closed once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
