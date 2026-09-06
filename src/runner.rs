//! Child process management: spawn (with Job Object + stdio redirection) and
//! the nssm-style multi-level stop sequence.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, LPARAM,
    WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleWindow, SetConsoleCtrlHandler,
    ATTACH_PARENT_PROCESS, CTRL_BREAK_EVENT,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, ResumeThread, SetPriorityClass, TerminateProcess,
    WaitForMultipleObjects, WaitForSingleObject, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_CREATION_FLAGS, STARTF_USESTDHANDLES, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, PostThreadMessageW,
    WM_CLOSE, WM_QUIT,
};

use crate::config::Config;
use crate::logger::{spawn_pipe_reader, Logger};
use crate::util;

// Stop-method skip bits (nssm compatible).
pub const SKIP_CONSOLE: u32 = 1;
pub const SKIP_WINDOW: u32 = 2;
pub const SKIP_THREADS: u32 = 4;
pub const SKIP_TERMINATE: u32 = 8;

use windows::Win32::System::Threading::REALTIME_PRIORITY_CLASS;

pub struct Child {
    pub process: HANDLE,
    pub job: HANDLE,
    pub thread: HANDLE,
    pub pid: u32,
    pub start: Instant,
    pub readers: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            // KILL_ON_JOB_CLOSE: closing the job handle terminates any
            // processes still left in the job (orphaned children etc.).
            let _ = CloseHandle(self.process);
            let _ = CloseHandle(self.job);
            let _ = CloseHandle(self.thread);
        }
    }
}

fn priority_class(p: u32) -> PROCESS_CREATION_FLAGS {
    match p {
        crate::config::PRIORITY_REALTIME => REALTIME_PRIORITY_CLASS,
        crate::config::PRIORITY_HIGH => HIGH_PRIORITY_CLASS,
        crate::config::PRIORITY_ABOVE_NORMAL => ABOVE_NORMAL_PRIORITY_CLASS,
        crate::config::PRIORITY_BELOW_NORMAL => BELOW_NORMAL_PRIORITY_CLASS,
        crate::config::PRIORITY_IDLE => IDLE_PRIORITY_CLASS,
        _ => NORMAL_PRIORITY_CLASS,
    }
}

/// Build the UTF-16 environment block: current process environment plus the
/// configured overrides (KEY=VALUE entries, case-insensitive replacement).
///
/// Uses `vars_os` + lossy conversion: `env::vars()` panics on non-UTF-8
/// entries, which would kill the whole service (panic = "abort" in release).
/// A non-Unicode variable keeps its lossy-decoded name/value instead.
fn build_env_block(extra: &[String]) -> Vec<u16> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        map.insert(
            k.to_string_lossy().to_lowercase(),
            v.to_string_lossy().to_string(),
        );
    }
    for e in extra {
        if let Some(eq) = e.find('=') {
            let key = e[..eq].trim().to_string();
            if key.is_empty() {
                continue;
            }
            let val = e[eq + 1..].to_string();
            map.insert(key.to_lowercase(), format!("{key}={val}"));
        }
    }
    let mut block: Vec<u16> = Vec::new();
    for v in map.values() {
        block.extend(v.encode_utf16());
        block.push(0);
    }
    // Double-NUL terminator; a fully empty environment block is still two
    // NUL units wide.
    block.push(0);
    if map.is_empty() {
        block.push(0);
    }
    block
}

/// Spawn the wrapped application.
pub fn spawn(cfg: &Config, logger: &Logger) -> Result<Child, String> {
    let app = cfg.application.trim().trim_matches('"').to_string();
    if app.is_empty() {
        return Err("Application 未配置".into());
    }
    if !Path::new(&app).is_file() {
        return Err(format!("应用程序不存在: {app}"));
    }

    // Working directory: explicit, or the application's directory.
    let workdir = if cfg.app_directory.trim().is_empty() {
        Path::new(&app)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
    } else {
        std::path::PathBuf::from(cfg.app_directory.trim())
    };
    if !workdir.as_os_str().is_empty() && !workdir.is_dir() {
        logger.service_line(&format!(
            "warning: 工作目录不存在, 使用系统默认: {}",
            workdir.display()
        ));
    }

    unsafe {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };

        // NUL device for unused stdio slots.
        let nul: HANDLE = CreateFileW(
            w!("NUL"),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&sa),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .unwrap_or_default();

        // stdout / stderr targets.
        let mut out_r = HANDLE::default();
        let mut out_w = HANDLE::default();
        let mut err_r = HANDLE::default();
        let mut err_w = HANDLE::default();

        if !cfg.stdout.trim().is_empty() {
            if CreatePipe(&mut out_r, &mut out_w, Some(&sa), 0).is_err() {
                return Err("创建 stdout 管道失败".into());
            }
            let _ = windows::Win32::Foundation::SetHandleInformation(
                out_r,
                HANDLE_FLAG_INHERIT.0,
                HANDLE_FLAGS(0),
            );
        }
        if !cfg.stderr.trim().is_empty() {
            if CreatePipe(&mut err_r, &mut err_w, Some(&sa), 0).is_err() {
                return Err("创建 stderr 管道失败".into());
            }
            let _ = windows::Win32::Foundation::SetHandleInformation(
                err_r,
                HANDLE_FLAG_INHERIT.0,
                HANDLE_FLAGS(0),
            );
        }

        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: nul,
            hStdOutput: if !out_w.is_invalid() { out_w } else { nul },
            hStdError: if !err_w.is_invalid() { err_w } else { nul },
            ..Default::default()
        };

        // Command line.
        let cmdline = format!("{} {}", util::quote_path(&app), cfg.app_parameters.trim());
        let mut cmd_w = util::to_wide(&cmdline);
        let dir_w = if workdir.as_os_str().is_empty() {
            None
        } else {
            Some(util::to_wide(&workdir.to_string_lossy()))
        };

        // Environment block.
        // Both env lists apply on top of the inherited environment
        // (AppEnvironment first, then AppEnvironmentExtra).
        let mut env: Vec<String> = cfg.environment.clone();
        env.extend(cfg.environment_extra.iter().cloned());
        let env_block = build_env_block(&env);

        let flags: PROCESS_CREATION_FLAGS = CREATE_SUSPENDED
            | CREATE_NEW_PROCESS_GROUP
            | CREATE_NO_WINDOW
            | CREATE_UNICODE_ENVIRONMENT;

        let mut pi = windows::Win32::System::Threading::PROCESS_INFORMATION::default();
        let created = CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR::from_raw(cmd_w.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            Some(env_block.as_ptr() as *const std::ffi::c_void),
            dir_w
                .as_ref()
                .map(|d| PCWSTR::from_raw(d.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            &si,
            &mut pi,
        );

        // Parent copies of the write ends must be closed, otherwise the
        // reader threads never see EOF after the child exits.
        if !out_w.is_invalid() {
            let _ = CloseHandle(out_w);
        }
        if !err_w.is_invalid() {
            let _ = CloseHandle(err_w);
        }
        if !nul.is_invalid() {
            let _ = CloseHandle(nul);
        }

        created.map_err(|e| format!("CreateProcessW 失败: {}", util::last_error(&e)))?;

        // Job object: keeps the whole process tree contained.
        let job = CreateJobObjectW(None, PCWSTR::null())
            .map_err(|e| format!("CreateJobObjectW 失败: {}", util::last_error(&e)))?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| format!("SetInformationJobObject 失败: {}", util::last_error(&e)))?;
        if let Err(e) = AssignProcessToJobObject(pi.hProcess, job) {
            logger.service_line(&format!(
                "warning: AssignProcessToJobObject 失败: {}",
                util::last_error(&e)
            ));
        }

        let _ = SetPriorityClass(pi.hProcess, priority_class(cfg.priority));
        ResumeThread(pi.hThread);

        // Reader threads (they own the read ends via std::fs::File).
        let mut readers = Vec::new();
        if !out_r.is_invalid() {
            if let Some(w) = logger.clone_out_writer() {
                match spawn_pipe_reader(crate::util::SharedHandle(out_r), w) {
                    Some(h) => readers.push(h),
                    None => {
                        let _ = CloseHandle(out_r);
                        logger.service_line("warning: stdout 读取线程创建失败, 该流日志将被丢弃");
                    }
                }
            } else {
                let _ = CloseHandle(out_r);
            }
        }
        if !err_r.is_invalid() {
            if let Some(w) = logger.clone_err_writer() {
                match spawn_pipe_reader(crate::util::SharedHandle(err_r), w) {
                    Some(h) => readers.push(h),
                    None => {
                        let _ = CloseHandle(err_r);
                        logger.service_line("warning: stderr 读取线程创建失败, 该流日志将被丢弃");
                    }
                }
            } else {
                let _ = CloseHandle(err_r);
            }
        }

        Ok(Child {
            process: pi.hProcess,
            job,
            thread: pi.hThread,
            pid: pi.dwProcessId,
            start: Instant::now(),
            readers,
        })
    }
}

/// Wait until any of the handles is signaled; returns the 0-based index.
/// Returns `None` when the wait itself fails (e.g. an invalid handle) — the
/// caller is expected to fail in a controlled way instead of spinning.
pub fn wait_any(handles: &[HANDLE]) -> Option<usize> {
    unsafe {
        let r = WaitForMultipleObjects(handles, false, u32::MAX);
        let base = WAIT_OBJECT_0.0;
        if r.0 >= base && r.0 < base + handles.len() as u32 {
            Some((r.0 - base) as usize)
        } else {
            // WAIT_FAILED (WAIT_TIMEOUT is impossible with INFINITE).
            None
        }
    }
}

/// Wait for a single handle with timeout. true = signaled.
pub fn wait_signal(handle: HANDLE, timeout_ms: u32) -> bool {
    unsafe { WaitForSingleObject(handle, timeout_ms) == WAIT_OBJECT_0 }
}

/// true if the process has exited.
pub fn has_exited(handle: HANDLE) -> bool {
    unsafe { WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 }
}

/// Exit code of an exited process. `None` when GetExitCodeProcess fails —
/// callers must log this instead of silently treating it as success.
pub fn exit_code(handle: HANDLE) -> Option<u32> {
    unsafe {
        let mut code: u32 = 0;
        GetExitCodeProcess(handle, &mut code).ok().map(|_| code)
    }
}

/// Enumerate the process tree below (and including) `root`.
fn descendant_pids(root: u32) -> Vec<u32> {
    unsafe {
        let mut pids = vec![root];
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return pids,
        };
        let mut all: Vec<(u32, u32)> = Vec::new(); // (pid, ppid)
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                all.push((entry.th32ProcessID, entry.th32ParentProcessID));
                entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        // BFS over the parent -> children relation.
        let mut i = 0;
        while i < pids.len() {
            let cur = pids[i];
            for &(pid, ppid) in &all {
                if ppid == cur && pid != 0 && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
            i += 1;
        }
        pids
    }
}

/// All thread IDs owned by the given processes.
fn thread_ids_of(pids: &[u32]) -> Vec<u32> {
    unsafe {
        let mut tids = Vec::new();
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
            Ok(h) => h,
            Err(_) => return tids,
        };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        if Thread32First(snap, &mut entry).is_ok() {
            loop {
                if pids.contains(&entry.th32OwnerProcessID) {
                    tids.push(entry.th32ThreadID);
                }
                entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        tids
    }
}

/// Attach to the child's console and send CTRL_BREAK to its process group.
///
/// Best effort about restoring the original console state: if we owned a
/// console before (debugging context), re-attach to the parent afterwards.
/// A service process normally has no console at all, so both FreeConsole
/// calls are no-ops there.
fn send_ctrl_break(pid: u32) -> bool {
    unsafe {
        let had_console = !GetConsoleWindow().0.is_null();
        // A service process has no console; attaching to the child's own
        // console lets us target its process group.
        if had_console {
            let _ = FreeConsole();
        }
        if AttachConsole(pid).is_err() {
            // Could not attach (child has no console / exited). If we came
            // with our own console, get it back.
            if had_console {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
            return false;
        }
        // Ignore the event ourselves so it does not kill this process.
        let _ = SetConsoleCtrlHandler(None, true);
        let ok = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid).is_ok();
        let _ = FreeConsole();
        if had_console {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        ok
    }
}

unsafe extern "system" fn enum_close_wnd(
    hwnd: windows::Win32::Foundation::HWND,
    lparam: LPARAM,
) -> windows::core::BOOL {
    let pids = &*(lparam.0 as *const Vec<u32>);
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != 0 && pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    true.into()
}

/// nssm-style multi-level stop:
///   1. CTRL_BREAK to the process group          (timeout: stop_timeout_console)
///   2. WM_CLOSE to visible windows of the tree  (timeout: stop_timeout_window)
///   3. WM_QUIT to threads of the tree           (timeout: stop_timeout_threads)
///   4. TerminateJobObject - kills the whole tree instantly
///
/// Levels can be skipped via cfg.stop_method_skip (bit 1/2/4/8).
pub fn stop_child(child: &mut Child, cfg: &Config, logger: &Logger, shutdown: bool) {
    if has_exited(child.process) {
        logger.service_line("application already exited");
        // Even on the early-exit path the pipe reader threads must be
        // drained: they may still hold the tail of the application's output
        // (EOF arrives once every write end is closed by Child::drop).
        for h in child.readers.drain(..) {
            let _ = h.join();
        }
        return;
    }
    let scale = |t: u32| if shutdown { t.min(2000) } else { t };
    let pids = descendant_pids(child.pid);
    let skip = cfg.stop_method_skip;
    logger.service_line(&format!(
        "stopping process tree (root pid {}, {} process{})",
        child.pid,
        pids.len(),
        if pids.len() > 1 { "es" } else { "" }
    ));

    let mut exited = false;

    if skip & SKIP_CONSOLE == 0 {
        if send_ctrl_break(child.pid) {
            logger.service_line("sent CTRL_BREAK to process group");
            exited = wait_signal(child.process, scale(cfg.stop_timeout_console));
        } else {
            logger.service_line("CTRL_BREAK unavailable (no console), skipping");
        }
    }
    if !exited && skip & SKIP_WINDOW == 0 && !pids.is_empty() {
        unsafe {
            let lparam = LPARAM(&pids as *const Vec<u32> as isize);
            let _ = EnumWindows(Some(enum_close_wnd), lparam);
        }
        logger.service_line("posted WM_CLOSE to windows of the process tree");
        exited = wait_signal(child.process, scale(cfg.stop_timeout_window));
    }
    if !exited && skip & SKIP_THREADS == 0 {
        let tids = thread_ids_of(&pids);
        unsafe {
            for tid in tids {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        logger.service_line("posted WM_QUIT to threads of the process tree");
        exited = wait_signal(child.process, scale(cfg.stop_timeout_threads));
    }
    if !exited && skip & SKIP_TERMINATE == 0 {
        unsafe {
            let _ = TerminateJobObject(child.job, 1);
        }
        logger.service_line("terminated the process tree (force kill)");
        // Should be near-instant; give it a bounded wait.
        let _ = wait_signal(child.process, 5000);
    } else if !exited {
        // Termination skipped by configuration: wait, then hard-terminate the
        // root process as a last resort so the service can still stop.
        if wait_signal(child.process, 30_000) {
            // exited in time
        } else {
            unsafe {
                let _ = TerminateProcess(child.process, 1);
            }
        }
    }

    // Reader threads see EOF once every process holding the pipe write end
    // is gone (job close in Child::drop guarantees this).
    for h in child.readers.drain(..) {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_block_applies_overrides_case_insensitively() {
        std::env::set_var("RSSVC_TEST_VAR", "base");
        let block = build_env_block(&["RSSVC_TEST_VAR=override".to_string()]);
        let text = String::from_utf16_lossy(&block);
        assert!(text.contains("RSSVC_TEST_VAR=override"));
        // The inherited entry is replaced (case-insensitive key match).
        assert!(!text.contains("rssvc_test_var=base"));
    }

    #[test]
    fn env_block_always_double_nul_terminated() {
        let block = build_env_block(&[]);
        assert!(block.len() >= 2);
        assert_eq!(block.len() % 2, 0);
        assert_eq!(&block[block.len() - 2..], &[0u16, 0u16]);
    }
}
