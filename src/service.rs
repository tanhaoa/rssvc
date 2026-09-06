//! Service runtime: dispatcher, control handler, and the monitor loop that
//! keeps the wrapped application alive (crash restart with throttling).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, HANDLE,
};
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_PARAMCHANGE, SERVICE_ACCEPT_PAUSE_CONTINUE, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_CONTINUE, SERVICE_CONTROL_INTERROGATE,
    SERVICE_CONTROL_PARAMCHANGE, SERVICE_CONTROL_PAUSE, SERVICE_CONTROL_SHUTDOWN,
    SERVICE_CONTROL_STOP, SERVICE_PAUSED, SERVICE_PAUSE_PENDING, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE,
    SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};

use crate::config::Config;
use crate::logger::Logger;
use crate::runner;
use crate::{util, VERSION};

// Exit codes reported to the SCM.
pub const EXIT_CONFIG_ERROR: u32 = 2001;
pub const EXIT_SPAWN_ERROR: u32 = 2002;
pub const EXIT_WAIT_ERROR: u32 = 2003;

// Custom exit codes (and the wrapped application's own exit codes) do not
// belong in dwWin32ExitCode, which per Win32 contract carries system error
// codes only. They are reported as ERROR_SERVICE_SPECIFIC_ERROR +
// dwServiceSpecificExitCode instead, so services.msc / sc query / Event
// Viewer display them correctly.
const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

static STATUS_HANDLE: OnceLock<StatusHandle> = OnceLock::new();
static CURRENT_STATE: AtomicU32 = AtomicU32::new(SERVICE_STOPPED.0);
static STOP_EVENT: OnceLock<util::SharedHandle> = OnceLock::new();
static PAUSE_EVENT: OnceLock<util::SharedHandle> = OnceLock::new();
static RESUME_EVENT: OnceLock<util::SharedHandle> = OnceLock::new();
static RELOAD_EVENT: OnceLock<util::SharedHandle> = OnceLock::new();
static SHUTDOWN_MODE: AtomicBool = AtomicBool::new(false);

fn signal(h: &OnceLock<util::SharedHandle>) {
    if let Some(handle) = h.get() {
        unsafe {
            let _ = SetEvent(handle.0);
        }
    }
}

fn unsignal(h: &OnceLock<util::SharedHandle>) {
    if let Some(handle) = h.get() {
        unsafe {
            let _ = ResetEvent(handle.0);
        }
    }
}

// SERVICE_STATUS_HANDLE is a raw pointer wrapper without Send/Sync; we move
// it across threads exactly once (set in run_service, used from the SCM
// callback thread and the worker thread).
struct StatusHandle(SERVICE_STATUS_HANDLE);
unsafe impl Send for StatusHandle {}
unsafe impl Sync for StatusHandle {}

fn report(
    state: SERVICE_STATUS_CURRENT_STATE,
    controls: u32,
    exit_code: u32,
    checkpoint: u32,
    wait_hint_ms: u32,
) {
    CURRENT_STATE.store(state.0, Ordering::SeqCst);
    let Some(h) = STATUS_HANDLE.get() else { return };
    let (win32, specific) = if exit_code == 0 {
        (0, 0)
    } else {
        (ERROR_SERVICE_SPECIFIC_ERROR, exit_code)
    };
    let ss = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: controls,
        dwWin32ExitCode: win32,
        dwServiceSpecificExitCode: specific,
        dwCheckPoint: checkpoint,
        dwWaitHint: wait_hint_ms,
    };
    unsafe {
        let _ = SetServiceStatus(h.0, &ss);
    }
}

fn controls_running() -> u32 {
    SERVICE_ACCEPT_STOP
        | SERVICE_ACCEPT_SHUTDOWN
        | SERVICE_ACCEPT_PAUSE_CONTINUE
        | SERVICE_ACCEPT_PARAMCHANGE
}

/// Controls accepted for a given state, so INTERROGATE replies always match
/// what the service would report proactively in that state (paused services
/// do not accept PARAMCHANGE; pending states accept nothing).
fn controls_for_state(state: SERVICE_STATUS_CURRENT_STATE) -> u32 {
    match state {
        SERVICE_RUNNING => controls_running(),
        SERVICE_PAUSED => {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_PAUSE_CONTINUE
        }
        _ => 0,
    }
}

unsafe extern "system" fn control_handler(
    ctrl: u32,
    _event_type: u32,
    _event_data: *mut std::ffi::c_void,
    _context: *mut std::ffi::c_void,
) -> u32 {
    match ctrl {
        SERVICE_CONTROL_STOP => {
            signal(&STOP_EVENT);
            0
        }
        SERVICE_CONTROL_SHUTDOWN => {
            SHUTDOWN_MODE.store(true, Ordering::SeqCst);
            signal(&STOP_EVENT);
            0
        }
        SERVICE_CONTROL_PAUSE => {
            signal(&PAUSE_EVENT);
            0
        }
        SERVICE_CONTROL_CONTINUE => {
            signal(&RESUME_EVENT);
            0
        }
        SERVICE_CONTROL_PARAMCHANGE => {
            signal(&RELOAD_EVENT);
            0
        }
        SERVICE_CONTROL_INTERROGATE => {
            let state = CURRENT_STATE.load(Ordering::SeqCst);
            report(
                SERVICE_STATUS_CURRENT_STATE(state),
                controls_for_state(SERVICE_STATUS_CURRENT_STATE(state)),
                0,
                0,
                0,
            );
            0
        }
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

/// Try to run as a Windows service. Returns false if we are not running
/// under the SCM (console context), so the caller can fall back to CLI mode.
pub fn try_dispatch(name: String) -> bool {
    // Record the service identity for service_main. Without this the worker
    // would read an empty name and fail to load its registry configuration
    // (every installed service would exit immediately with code 2001).
    // SCM guarantees argv[0] carries the same name; service_main falls back
    // to parsing argv if this OnceLock was somehow not set.
    let _ = crate::SERVICE_NAME.set(name.clone());
    let name_w = util::to_wide(&name);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: windows::core::PWSTR::from_raw(name_w.as_ptr() as *mut u16),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    match unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } {
        Ok(()) => true, // service ran to completion
        Err(e) => {
            if e.code() == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.to_hresult() {
                false // console context
            } else {
                eprintln!("rssvc: 服务调度失败: {}", util::last_error(&e));
                true
            }
        }
    }
}

unsafe extern "system" fn service_main(argc: u32, argv: *mut windows::core::PWSTR) {
    let mut name = crate::SERVICE_NAME.get().cloned().unwrap_or_default();
    if name.is_empty() && argc > 0 && !argv.is_null() {
        // Fallback: SCM passes the service name as argv[0].
        let first = *argv;
        name = util::pwstr_to_string(first.0);
    }
    let code = run_service(&name);
    // Ensure the process exits with the service exit code.
    std::process::exit(code as i32);
}

fn run_service(name: &str) -> u32 {
    // Events used by the control handler must exist before registration.
    let ev = |once: &OnceLock<util::SharedHandle>| -> HANDLE {
        let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.unwrap_or_default();
        let _ = once.set(util::SharedHandle(h));
        h
    };
    ev(&STOP_EVENT);
    ev(&PAUSE_EVENT);
    ev(&RESUME_EVENT);
    ev(&RELOAD_EVENT);

    let name_w = util::to_wide(name);
    let registered = unsafe {
        RegisterServiceCtrlHandlerExW(
            PCWSTR::from_raw(name_w.as_ptr()),
            Some(control_handler),
            None,
        )
    };
    let status_handle = match registered {
        Ok(h) if !h.is_invalid() => h,
        Ok(_) => return EXIT_CONFIG_ERROR,
        Err(e) => {
            eprintln!(
                "rssvc: RegisterServiceCtrlHandlerExW 失败: {}",
                util::last_error(&e)
            );
            return EXIT_CONFIG_ERROR;
        }
    };
    let _ = STATUS_HANDLE.set(StatusHandle(status_handle));

    // START_PENDING while we load config and spawn the first process.
    report(SERVICE_START_PENDING, 0, 0, 1, 5000);
    let code = worker(name);
    report(SERVICE_STOPPED, 0, code, 0, 0);
    code
}

fn worker(name: &str) -> u32 {
    let mut cfg = match Config::load(name) {
        Ok(c) if !c.application.trim().is_empty() => c,
        Ok(_) => return EXIT_CONFIG_ERROR, // nothing to run
        Err(_) => return EXIT_CONFIG_ERROR,
    };

    let logger = Logger::new(&cfg.stdout, &cfg.stderr, cfg.rotate_bytes, cfg.rotate_keep);
    logger.service_line(&format!(
        "rssvc v{VERSION}: starting service '{name}' (\"{}\" {})",
        cfg.application, cfg.app_parameters
    ));

    let stop_ev = STOP_EVENT.get().unwrap().0;
    let pause_ev = PAUSE_EVENT.get().unwrap().0;
    let resume_ev = RESUME_EVENT.get().unwrap().0;
    let reload_ev = RELOAD_EVENT.get().unwrap().0;

    let mut thrash: u32 = 0;

    'outer: loop {
        // START_PENDING until the first process of this round is up.
        report(SERVICE_START_PENDING, 0, 0, 2, 5000);
        let mut child = match runner::spawn(&cfg, &logger) {
            Ok(c) => c,
            Err(e) => {
                logger.service_line(&format!("error: failed to start application: {e}"));
                return EXIT_SPAWN_ERROR;
            }
        };
        logger.service_line(&format!(
            "application started (pid {}, ran \"{}\" {})",
            child.pid, cfg.application, cfg.app_parameters
        ));
        report(
            SERVICE_RUNNING,
            controls_for_state(SERVICE_RUNNING),
            0,
            0,
            0,
        );

        'inner: loop {
            let handles = [child.process, stop_ev, pause_ev, reload_ev];
            let Some(idx) = runner::wait_any(&handles) else {
                // WaitForMultipleObjects failed (e.g. an invalid handle).
                // Fail in a controlled way instead of spinning forever.
                logger.service_line("error: WaitForMultipleObjects 失败, 服务将停止");
                return EXIT_WAIT_ERROR;
            };
            match idx {
                0 => {
                    // Application exited on its own.
                    let code = runner::exit_code(child.process).unwrap_or_else(|| {
                        logger.service_line("warning: 读取应用退出码失败 (GetExitCodeProcess)");
                        0
                    });
                    let ran = child.start.elapsed();
                    // Closing the job handle (in Child::drop) kills any leftover
                    // processes in the tree, which in turn closes the pipe write
                    // ends so the reader threads see EOF and can be joined.
                    let readers = std::mem::take(&mut child.readers);
                    drop(child);
                    for h in readers {
                        let _ = h.join();
                    }
                    logger.service_line(&format!(
                        "application exited (code {code:#x}, ran {:.1}s)",
                        ran.as_secs_f32()
                    ));

                    if SHUTDOWN_MODE.load(Ordering::SeqCst) || is_stopping(stop_ev) {
                        // Stopping while the app died: finish the service.
                        break 'outer;
                    }

                    // Throttling / backoff.
                    if ran < Duration::from_millis(cfg.throttle_ms as u64) {
                        thrash = thrash.saturating_add(1);
                    } else {
                        thrash = 0;
                    }
                    if cfg.max_restarts > 0 && thrash > cfg.max_restarts {
                        logger.service_line(&format!(
                            "error: giving up after {thrash} rapid restarts; stopping service"
                        ));
                        return code;
                    }
                    let delay = if thrash > 0 {
                        let mult = 1u32 << (thrash - 1).min(6);
                        cfg.restart_delay_ms
                            .saturating_mul(mult)
                            .clamp(cfg.restart_delay_ms, 60_000)
                    } else {
                        cfg.restart_delay_ms
                    };
                    if delay > 0 {
                        logger.service_line(&format!("restarting in {delay} ms"));
                        if runner::wait_signal(stop_ev, delay) {
                            break 'outer; // stop requested during the delay
                        }
                    }
                    break 'inner; // respawn
                }
                1 => {
                    // STOP or SHUTDOWN.
                    let shutdown = SHUTDOWN_MODE.load(Ordering::SeqCst);
                    let hint = stop_hint(&cfg);
                    report(SERVICE_STOP_PENDING, 0, 0, 1, hint);
                    runner::stop_child(&mut child, &cfg, &logger, shutdown);
                    logger.service_line("service stopped");
                    break 'outer;
                }
                2 => {
                    // PAUSE: stop the application, report paused, wait.
                    unsignal(&PAUSE_EVENT);
                    let hint = stop_hint(&cfg);
                    report(SERVICE_PAUSE_PENDING, 0, 0, 1, hint);
                    runner::stop_child(&mut child, &cfg, &logger, false);
                    logger.service_line("service paused (application stopped)");
                    report(SERVICE_PAUSED, controls_for_state(SERVICE_PAUSED), 0, 0, 0);
                    // Wait for RESUME or STOP.
                    let wait_handles = [stop_ev, resume_ev];
                    match runner::wait_any(&wait_handles) {
                        Some(0) => break 'outer,
                        Some(_) => {}
                        None => {
                            logger.service_line("error: 等待恢复/停止事件失败, 服务将停止");
                            break 'outer;
                        }
                    }
                    unsignal(&RESUME_EVENT);
                    logger.service_line("service resumed");
                    break 'inner; // respawn the application
                }
                3 => {
                    // PARAMCHANGE: reload configuration (rotation settings
                    // apply immediately; the rest at next restart).
                    unsignal(&RELOAD_EVENT);
                    if let Ok(nc) = Config::load(name) {
                        logger.update(nc.rotate_bytes, nc.rotate_keep);
                        cfg = nc;
                        logger.service_line("configuration reloaded");
                    } else {
                        logger.service_line("warning: configuration reload failed");
                    }
                    continue 'inner;
                }
                _ => unreachable!(),
            }
        }
    }

    // NOTE: do not CloseHandle the STOP/PAUSE/RESUME/RELOAD events here.
    // The control handler can still fire after the worker returns (SCM is
    // asynchronous), and closing the handle leaves a stale value in the
    // OnceLock that SetEvent could then apply to an unrelated object whose
    // handle value got recycled. The kernel reclaims event handles when the
    // process exits right after this function returns.
    0
}

fn stop_hint(cfg: &Config) -> u32 {
    cfg.stop_timeout_console + cfg.stop_timeout_window + cfg.stop_timeout_threads + 5000
}

/// true when a stop has been requested (manual-reset stop event signaled).
fn is_stopping(stop_ev: HANDLE) -> bool {
    runner::wait_signal(stop_ev, 0)
}
