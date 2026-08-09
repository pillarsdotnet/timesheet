//! Minimal hand-written FFI to kernel32.dll for the pieces of Windows process/daemon management
//! `std` doesn't expose: querying/terminating a process by PID, a console control handler (the
//! Windows analog of the Unix SIGTERM handler used for the logoff/shutdown STOP guarantee), and
//! named Job Objects (the Windows analog of a Unix process group, letting one call kill both the
//! daemon and any chooser dialog window it currently has open). No external crate: these are the
//! same handful of kernel32 exports every Windows Rust binary already links against implicitly.
#![cfg(target_os = "windows")]

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;

pub type RawHandle = *mut c_void;

pub const PROCESS_TERMINATE: u32 = 0x0001;
pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

pub const CTRL_CLOSE_EVENT: u32 = 2;
pub const CTRL_LOGOFF_EVENT: u32 = 5;
pub const CTRL_SHUTDOWN_EVENT: u32 = 6;

pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12
const HANDLE_FLAG_INHERIT: u32 = 0x0001;

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const JOB_OBJECT_TERMINATE: u32 = 0x0008;
// JOBOBJECTINFOCLASS::JobObjectExtendedLimitInformation
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;

#[repr(C)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> RawHandle;
    fn CloseHandle(hObject: RawHandle) -> i32;
    fn TerminateProcess(hProcess: RawHandle, uExitCode: u32) -> i32;
    fn GetCurrentProcess() -> RawHandle;
    fn SetConsoleCtrlHandler(
        handler_routine: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
    fn CreateJobObjectW(lp_job_attributes: *const c_void, lp_name: *const u16) -> RawHandle;
    fn OpenJobObjectW(desired_access: u32, inherit_handle: i32, lp_name: *const u16) -> RawHandle;
    fn AssignProcessToJobObject(h_job: RawHandle, h_process: RawHandle) -> i32;
    fn SetInformationJobObject(
        h_job: RawHandle,
        job_object_information_class: i32,
        lp_job_object_information: *const c_void,
        cb_job_object_information_length: u32,
    ) -> i32;
    fn TerminateJobObject(h_job: RawHandle, u_exit_code: u32) -> i32;
    fn GetStdHandle(std_handle: u32) -> RawHandle;
    fn SetHandleInformation(h_object: RawHandle, dw_mask: u32, dw_flags: u32) -> i32;
}

fn wide_null(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Strip the inheritable flag from this process's own stdin/stdout/stderr handles. Without this,
/// spawning a long-lived detached child (the reminder daemon) on Windows can silently duplicate
/// these handles into it even when the child's own stdio is explicitly redirected elsewhere
/// (`Stdio::null()`): CreateProcess's handle inheritance is all-or-nothing by default, so any
/// inheritable handle the parent happens to have open -- including its own stdout pipe -- rides
/// along. If that pipe is being read as a bounded stream (a script capturing `timesheet start`'s
/// output, `$out = & timesheet.exe start`, etc.), the reader then blocks forever waiting for EOF
/// that never comes, since the daemon holds the write end open indefinitely. Call this once,
/// before spawning the daemon.
pub fn make_own_std_handles_noninheritable() {
    unsafe {
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(which);
            if !h.is_null() {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// True if a process with this PID exists and is alive.
pub fn is_pid_running(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
        true
    }
}

/// Hard-terminate a process by PID. Windows has no SIGTERM equivalent for an arbitrary process, so
/// this is only used for the "intentional kill" path (the caller already removed the PID file
/// first, same as on Unix, so the daemon is not expected to run cleanup here); the logoff/shutdown
/// STOP guarantee is handled separately by the console control handler below.
pub fn terminate_process(pid: u32) {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 1);
            CloseHandle(h);
        }
    }
}

/// The deterministic Job Object name for a daemon PID, so a later, unrelated process (e.g.
/// `timesheet stop`) can reopen and terminate the same job by name.
pub fn reminder_job_name(pid: u32) -> String {
    format!("Local\\timesheet-reminder-{}", pid)
}

/// Create a named, kill-on-close Job Object and assign the *current* process to it. Meant to be
/// called by the daemon itself, early in its own startup (mirroring `setsid()` on Unix): the
/// returned handle must be kept open for the daemon's entire lifetime (store it; do not close it),
/// since the job's member processes -- the daemon and anything it later spawns, e.g. a chooser
/// dialog -- die when the job's last handle closes. Letting the *spawning* process create and then
/// close this handle instead would kill the daemon the moment that spawning process exits, which
/// is exactly the failure mode this ordering avoids.
pub fn create_and_join_kill_on_close_job(name: &str) -> Option<RawHandle> {
    unsafe {
        let wname = wide_null(name);
        let job = CreateJobObjectW(std::ptr::null(), wname.as_ptr());
        if job.is_null() {
            return None;
        }
        let mut info: JobObjectExtendedLimitInformation = std::mem::zeroed();
        info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &info as *const _ as *const c_void,
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
        );
        if configured == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            CloseHandle(job);
            return None;
        }
        Some(job)
    }
}

/// Terminate the named Job Object for a daemon PID: kills the daemon and any process it spawned
/// (e.g. an open chooser window) in one call. No-op if the job cannot be opened (e.g. the daemon
/// never created one, or has already exited and the job was destroyed with it).
pub fn terminate_reminder_job(pid: u32) {
    unsafe {
        let wname = wide_null(&reminder_job_name(pid));
        let job = OpenJobObjectW(JOB_OBJECT_TERMINATE, 0, wname.as_ptr());
        if !job.is_null() {
            TerminateJobObject(job, 1);
            CloseHandle(job);
        }
    }
}

/// Register a console control handler. The handler receives `CTRL_LOGOFF_EVENT` /
/// `CTRL_SHUTDOWN_EVENT` / `CTRL_CLOSE_EVENT` (among others) and should return 1 (handled) once
/// done; returning promptly matters; Windows does not guarantee it waits for the handler the way
/// launchd/systemd wait for their equivalents, so this is a best-effort guarantee, same as the
/// per-platform caveats already documented for the Unix daemon paths.
pub fn set_console_ctrl_handler(handler: unsafe extern "system" fn(u32) -> i32) -> bool {
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}
