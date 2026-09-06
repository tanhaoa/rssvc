//! Thin wrappers around the Service Control Manager (SCM) API.

use windows::core::PCWSTR;
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    QueryServiceStatusEx, StartServiceW, ENUM_SERVICE_TYPE, SC_MANAGER_ALL_ACCESS,
    SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_CONTINUE, SERVICE_CONTROL_PARAMCHANGE,
    SERVICE_CONTROL_PAUSE, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR_NORMAL, SERVICE_NO_CHANGE, SERVICE_PAUSED, SERVICE_PAUSE_PENDING,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_START_TYPE, SERVICE_STATUS,
    SERVICE_STATUS_PROCESS, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_WIN32_OWN_PROCESS,
};

use crate::config::Config;
use crate::util;

// Access masks are taken from the windows crate's typed constants instead
// of hand-written magic numbers (a missing SC_MANAGER_CONNECT bit once broke
// every query command). In windows 0.62 these constants are plain u32 aliases.
pub const ACCESS_ALL: u32 = SERVICE_ALL_ACCESS;
pub const ACCESS_START: u32 = 0x0010; // SERVICE_START
pub const ACCESS_STOP: u32 = 0x0020; // SERVICE_STOP
pub const ACCESS_PAUSE: u32 = 0x0040; // SERVICE_PAUSE_CONTINUE
pub const ACCESS_QUERY: u32 = 0x0004; // SERVICE_QUERY_STATUS
pub const ACCESS_CONFIG: u32 = 0x0002; // SERVICE_CHANGE_CONFIG
pub const SC_MANAGER_ALL: u32 = SC_MANAGER_ALL_ACCESS;
pub const SC_MANAGER_CONNECT_MASK: u32 = SC_MANAGER_CONNECT;

// State / control constants exposed as plain u32 for the CLI layer.
pub const CONTROL_STOP: u32 = SERVICE_CONTROL_STOP;
pub const CONTROL_PAUSE: u32 = SERVICE_CONTROL_PAUSE;
pub const CONTROL_CONTINUE: u32 = SERVICE_CONTROL_CONTINUE;
pub const CONTROL_PARAMCHANGE: u32 = SERVICE_CONTROL_PARAMCHANGE;
pub const STATE_STOPPED: u32 = SERVICE_STOPPED.0;
pub const STATE_START_PENDING: u32 = SERVICE_START_PENDING.0;
pub const STATE_STOP_PENDING: u32 = SERVICE_STOP_PENDING.0;
pub const STATE_RUNNING: u32 = SERVICE_RUNNING.0;
pub const STATE_PAUSE_PENDING: u32 = SERVICE_PAUSE_PENDING.0;
pub const STATE_PAUSED: u32 = SERVICE_PAUSED.0;

fn wide(s: &str) -> Vec<u16> {
    util::to_wide(s)
}

/// An open handle to a service.
pub struct Service {
    pub handle: windows::Win32::System::Services::SC_HANDLE,
}

impl Drop for Service {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.handle);
        }
    }
}

fn to_err(prefix: &str, e: &windows::core::Error) -> String {
    format!("{prefix}: {}{}", util::last_error(e), util::admin_hint(e))
}

/// Open the SCM database. `all = true` requests full access; otherwise only
/// SC_MANAGER_CONNECT is requested — OpenServiceW requires the manager handle
/// to carry that right even for pure queries (status / list).
pub fn open_manager(all: bool) -> Result<windows::Win32::System::Services::SC_HANDLE, String> {
    let access = if all {
        SC_MANAGER_ALL
    } else {
        SC_MANAGER_CONNECT_MASK
    };
    unsafe {
        OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), access)
            .map_err(|e| to_err("打开服务控制管理器失败", &e))
    }
}

/// Open the SCM database with connect-only access (no admin required).
/// Enough to open services for QUERY_STATUS; used by the GUI list.
pub fn open_manager_read() -> Result<windows::Win32::System::Services::SC_HANDLE, String> {
    unsafe {
        OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| to_err("打开服务控制管理器失败", &e))
    }
}

/// Create a new service whose ImagePath points back at rssvc.exe.
pub fn create_rssvc_service(
    scm: windows::Win32::System::Services::SC_HANDLE,
    name: &str,
    cfg: &Config,
    rssvc_exe: &str,
) -> Result<Service, String> {
    let name_w = wide(name);
    let display = if cfg.display_name.trim().is_empty() {
        name.to_string()
    } else {
        cfg.display_name.trim().to_string()
    };
    let display_w = wide(&display);
    // Quote the service name too: SCM parses everything after the quoted exe
    // path as arguments, and an unquoted name containing a space would split
    // into two arguments (making the service permanently unstartable).
    let image = format!("{} {}", util::quote_path(rssvc_exe), util::quote_path(name));
    let image_w = wide(&image);

    // Dependencies: double-NUL terminated MULTI_SZ.
    let mut deps_w: Vec<u16> = Vec::new();
    for d in &cfg.dependencies {
        let d = d.trim();
        if !d.is_empty() {
            deps_w.extend(d.encode_utf16());
            deps_w.push(0);
        }
    }
    deps_w.push(0);
    let has_deps = deps_w.len() > 1;

    let account_w = if cfg.account.trim().is_empty() {
        None
    } else {
        Some(wide(cfg.account.trim()))
    };
    let password_w = if cfg.password.is_empty() {
        None
    } else {
        Some(wide(&cfg.password))
    };

    let start_type: SERVICE_START_TYPE = if cfg.startup == crate::config::START_MANUAL {
        SERVICE_DEMAND_START
    } else {
        SERVICE_AUTO_START
    };

    unsafe {
        let handle = CreateServiceW(
            scm,
            PCWSTR::from_raw(name_w.as_ptr()),
            PCWSTR::from_raw(display_w.as_ptr()),
            ACCESS_ALL,
            SERVICE_WIN32_OWN_PROCESS,
            start_type,
            SERVICE_ERROR_NORMAL,
            PCWSTR::from_raw(image_w.as_ptr()),
            PCWSTR::null(), // load order group
            None,           // tag id
            if has_deps {
                PCWSTR::from_raw(deps_w.as_ptr())
            } else {
                PCWSTR::null()
            },
            account_w
                .as_ref()
                .map(|a| PCWSTR::from_raw(a.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            password_w
                .as_ref()
                .map(|p| PCWSTR::from_raw(p.as_ptr()))
                .unwrap_or(PCWSTR::null()),
        )
        .map_err(|e| to_err("创建服务失败", &e))?;

        // Description (separate API). An empty string deletes the description,
        // so edited configs can also clear it.
        {
            let mut desc_w = wide(cfg.description.trim());
            let sd = SERVICE_DESCRIPTIONW {
                lpDescription: windows::core::PWSTR::from_raw(desc_w.as_mut_ptr()),
            };
            if let Err(e) = ChangeServiceConfig2W(
                handle,
                SERVICE_CONFIG_DESCRIPTION,
                Some(&sd as *const _ as *const std::ffi::c_void),
            ) {
                eprintln!("warning: 设置描述失败: {}", util::last_error(&e));
            }
        }

        Ok(Service { handle })
    }
}

/// Open an existing service by name.
pub fn open_service(
    scm: windows::Win32::System::Services::SC_HANDLE,
    name: &str,
    access: u32,
) -> Result<Service, String> {
    let name_w = wide(name);
    unsafe {
        OpenServiceW(scm, PCWSTR::from_raw(name_w.as_ptr()), access)
            .map(|h| Service { handle: h })
            .map_err(|e| to_err(&format!("打开服务 {name} 失败"), &e))
    }
}

/// Query the current SERVICE_STATUS.
pub fn query_status(svc: &Service) -> Result<SERVICE_STATUS, String> {
    unsafe {
        let mut st = SERVICE_STATUS::default();
        QueryServiceStatus(svc.handle, &mut st).map_err(|e| to_err("查询服务状态失败", &e))?;
        Ok(st)
    }
}

/// Extended status (SERVICE_STATUS_PROCESS): current state + PID.
pub struct StatusEx {
    pub state: u32,
    pub pid: u32,
}

pub fn query_status_ex(svc: &Service) -> Result<StatusEx, String> {
    unsafe {
        let mut ssp = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        let size = std::mem::size_of::<SERVICE_STATUS_PROCESS>();
        let buf = std::slice::from_raw_parts_mut(
            &mut ssp as *mut SERVICE_STATUS_PROCESS as *mut u8,
            size,
        );
        QueryServiceStatusEx(svc.handle, SC_STATUS_PROCESS_INFO, Some(buf), &mut needed)
            .map_err(|e| to_err("查询服务状态失败", &e))?;
        Ok(StatusEx {
            state: ssp.dwCurrentState.0,
            pid: ssp.dwProcessId,
        })
    }
}

/// Start the service. Tolerates "already running".
pub fn start(svc: &Service) -> Result<(), String> {
    unsafe {
        match StartServiceW(svc.handle, None) {
            Ok(()) => Ok(()),
            Err(e) if e.code().0 as u32 == 1056 => Ok(()), // ERROR_SERVICE_ALREADY_RUNNING
            Err(e) => Err(to_err("启动服务失败", &e)),
        }
    }
}

/// Send a control code (STOP / PAUSE / CONTINUE).
pub fn control(svc: &Service, code: u32) -> Result<SERVICE_STATUS, String> {
    unsafe {
        let mut st = SERVICE_STATUS::default();
        ControlService(svc.handle, code, &mut st)
            .map(|_| st)
            .map_err(|e| {
                if e.code().0 as u32 == 1062 {
                    "服务未在运行".to_string()
                } else {
                    to_err("发送服务控制命令失败", &e)
                }
            })
    }
}

/// Poll until the service reaches `want_state` or the timeout expires.
pub fn wait_for_state(svc: &Service, want_state: u32, timeout_ms: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    while std::time::Instant::now() < deadline {
        if let Ok(st) = query_status(svc) {
            if st.dwCurrentState.0 == want_state {
                return true;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    query_status(svc)
        .map(|st| st.dwCurrentState.0 == want_state)
        .unwrap_or(false)
}

/// Delete the service.
pub fn delete(svc: &Service) -> Result<(), String> {
    unsafe { DeleteService(svc.handle).map_err(|e| to_err("删除服务失败", &e)) }
}

/// Update start type / display name / description / dependencies of an
/// existing service (used by `import`).
///
/// Dependency semantics: an empty `cfg.dependencies` leaves the current
/// dependencies untouched (passing NULL to ChangeServiceConfigW); a
/// non-empty list replaces them. The CLI import surfaces this contract in
/// its output so nothing is silently dropped.
pub fn change_config(svc: &Service, cfg: &Config) -> Result<(), String> {
    let display = if cfg.display_name.trim().is_empty() {
        None
    } else {
        Some(wide(cfg.display_name.trim()))
    };
    let start_type: SERVICE_START_TYPE = if cfg.startup == crate::config::START_MANUAL {
        SERVICE_DEMAND_START
    } else {
        SERVICE_AUTO_START
    };
    // Dependencies: double-NUL terminated MULTI_SZ, only when provided.
    let mut deps_w: Vec<u16> = Vec::new();
    for d in &cfg.dependencies {
        let d = d.trim();
        if !d.is_empty() {
            deps_w.extend(d.encode_utf16());
            deps_w.push(0);
        }
    }
    deps_w.push(0);
    let deps_ptr = if cfg.dependencies.is_empty() {
        PCWSTR::null() // keep existing dependencies
    } else {
        PCWSTR::from_raw(deps_w.as_ptr())
    };
    unsafe {
        ChangeServiceConfigW(
            svc.handle,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            start_type,
            SERVICE_ERROR_NORMAL,
            PCWSTR::null(), // binary path
            PCWSTR::null(), // load order group
            None,           // tag id
            deps_ptr,       // dependencies (null = unchanged)
            PCWSTR::null(), // start name
            PCWSTR::null(), // password
            display
                .as_ref()
                .map(|d| PCWSTR::from_raw(d.as_ptr()))
                .unwrap_or(PCWSTR::null()),
        )
        .map_err(|e| to_err("修改服务配置失败", &e))?;

        // Description (separate API). An empty string deletes the description.
        {
            let mut desc_w = wide(cfg.description.trim());
            let sd = SERVICE_DESCRIPTIONW {
                lpDescription: windows::core::PWSTR::from_raw(desc_w.as_mut_ptr()),
            };
            ChangeServiceConfig2W(
                svc.handle,
                SERVICE_CONFIG_DESCRIPTION,
                Some(&sd as *const _ as *const std::ffi::c_void),
            )
            .map_err(|e| to_err("修改服务描述失败", &e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Sanity: our re-exposed masks must equal the canonical Win32 values.
    #[test]
    fn access_masks_match_win32_constants() {
        use windows::Win32::System::Services::{
            SC_MANAGER_ALL_ACCESS, SC_MANAGER_CONNECT, SERVICE_ALL_ACCESS, SERVICE_START,
            SERVICE_STOP,
        };
        assert_eq!(super::SC_MANAGER_ALL, SC_MANAGER_ALL_ACCESS);
        assert_eq!(super::SC_MANAGER_CONNECT_MASK, SC_MANAGER_CONNECT);
        assert_eq!(super::ACCESS_ALL, SERVICE_ALL_ACCESS);
        assert_eq!(super::ACCESS_START, SERVICE_START);
        assert_eq!(super::ACCESS_STOP, SERVICE_STOP);
        // 0x0004 SERVICE_QUERY_STATUS, 0x0002 SERVICE_CHANGE_CONFIG.
        assert_eq!(super::ACCESS_QUERY, 0x0004);
        assert_eq!(super::ACCESS_CONFIG, 0x0002);
    }
}
