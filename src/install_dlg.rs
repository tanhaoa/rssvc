//! 安装新服务的图形对话框 + TOML 导入安装入口。
//!
//! `open_install_dialog`: 轻量原生 Win32 表单（服务名 / 显示名称 / 应用程序 /
//! 启动参数 / 工作目录 / stdout / stderr / 启动类型 / 进程优先级），点击"安装"
//! 后调用共享的 `install_service`，与 CLI `rssvc install` 行为完全一致。
//!
//! `prompt_name_and_install`: "导入 TOML" 的后半段——用户已在主窗口选定 TOML
//! 文件并解析为 `Config`，此处询问服务名后直接创建服务（完整保留 TOML 中的
//! 高级字段：环境变量 / 轮转 / 重启策略 / 停止超时等）。
//!
//! 父窗口在对话框存在期间被禁用（模态行为），安装成功后向父窗口发送
//! `WM_APP_REBUILD` 以刷新服务列表。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Services::CloseServiceHandle;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DefWindowProcW, GetWindowLongPtrW, LoadCursorW,
    PostMessageW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW,
    ShowWindow, GWLP_USERDATA, IDC_ARROW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, SW_SHOW,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW, WS_CAPTION,
    WS_SYSMENU,
};

use crate::config::{
    Config, PRIORITY_ABOVE_NORMAL, PRIORITY_BELOW_NORMAL, PRIORITY_HIGH, PRIORITY_IDLE,
    PRIORITY_NORMAL, PRIORITY_REALTIME, START_AUTO, START_MANUAL,
};
use crate::ctl::{self, Form, PickMode};
use crate::gui::{msg_box, WM_APP_REBUILD};

// Control IDs (passed as HMENU).
const ID_NAME: u32 = 1;
const ID_DISPLAY: u32 = 2;
const ID_APP: u32 = 3;
const ID_ARGS: u32 = 4;
const ID_DIR: u32 = 5;
const ID_STDOUT: u32 = 6;
const ID_STDERR: u32 = 7;
const ID_STARTUP: u32 = 8;
const ID_PRIORITY: u32 = 9;
const ID_OK: u32 = 10;
const ID_CANCEL: u32 = 11;
const ID_BROWSE_APP: u32 = 30;
const ID_BROWSE_DIR: u32 = 31;

struct DlgState {
    owner: HWND,
    form: Form,
}

struct PromptState {
    owner: HWND,
    form: Form,
    cfg: Config,
}

// ------------------------------------------------------------- entry ----

/// Open the install dialog (modeless; disables `owner` until closed).
pub fn open_install_dialog(owner: HWND) {
    unsafe {
        let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else { return };
        let hinstance = HINSTANCE(hmodule.0);
        register_class(hinstance);

        // Center on the owner window.
        let (x, y) = ctl::center_on(owner, 660, 470);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("rssvcInstallWnd"),
            w!("rssvc - 安装新服务"),
            WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0),
            x,
            y,
            660,
            470,
            Some(owner),
            None,
            Some(hinstance),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };

        let mut st = Box::new(DlgState {
            owner,
            form: Form::new(hwnd),
        });
        create_form(&mut st, hinstance);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);

        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Ask for a service name, then install `cfg` under it (TOML import flow).
pub fn prompt_name_and_install(owner: HWND, cfg: Config) {
    unsafe {
        let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else { return };
        let hinstance = HINSTANCE(hmodule.0);
        register_prompt_class(hinstance);

        let (x, y) = ctl::center_on(owner, 480, 180);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("rssvcPromptWnd"),
            w!("导入 TOML 配置 - rssvc"),
            WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0),
            x,
            y,
            480,
            180,
            Some(owner),
            None,
            Some(hinstance),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };

        let mut st = Box::new(PromptState {
            owner,
            form: Form::new(hwnd),
            cfg,
        });
        unsafe fn build(st: &mut PromptState, hinstance: HINSTANCE) {
            let f = &mut st.form;
            f.label(hinstance, "服务名 *（将创建一个新服务）", 16, 12, 420);
            f.edit(hinstance, ID_NAME, 16, 32, 440);
            f.button(hinstance, ID_OK, "安装", 244, 96, 104, 30, true);
            f.button(hinstance, ID_CANCEL, "取消", 356, 96, 104, 30, false);
        }
        build(&mut st, hinstance);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);

        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn register_class(hinstance: HINSTANCE) {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| unsafe {
        let mut wc = WNDCLASSEXW::default();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(dlg_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        wc.hbrBackground = color_window_brush();
        wc.lpszClassName = w!("rssvcInstallWnd");
        RegisterClassExW(&wc);
    });
}

fn register_prompt_class(hinstance: HINSTANCE) {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| unsafe {
        let mut wc = WNDCLASSEXW::default();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(prompt_wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        wc.hbrBackground = color_window_brush();
        wc.lpszClassName = w!("rssvcPromptWnd");
        RegisterClassExW(&wc);
    });
}

fn color_window_brush() -> windows::Win32::Graphics::Gdi::HBRUSH {
    windows::Win32::Graphics::Gdi::HBRUSH(
        (windows::Win32::Graphics::Gdi::COLOR_WINDOW.0 + 1) as *mut core::ffi::c_void,
    )
}

// -------------------------------------------------------------- form ----

unsafe fn create_form(st: &mut DlgState, hinstance: HINSTANCE) {
    let f = &mut st.form;
    // Full-width rows: label + edit (+ optional browse button).
    let rows: [(&str, u32, Option<u32>, i32); 5] = [
        ("服务名 *", ID_NAME, None, 12),
        ("显示名称 (可选)", ID_DISPLAY, None, 66),
        ("应用程序 *", ID_APP, Some(ID_BROWSE_APP), 120),
        ("启动参数 (可选)", ID_ARGS, None, 174),
        ("工作目录 (可选, 默认: 程序所在目录)", ID_DIR, Some(ID_BROWSE_DIR), 228),
    ];
    for (text, id, browse, y) in rows {
        f.label(hinstance, text, 14, y, 400);
        match browse {
            Some(bid) => {
                f.edit(hinstance, id, 14, y + 18, 500);
                f.button(hinstance, bid, "浏览...", 526, y + 17, 104, 26, false);
            }
            None => f.edit(hinstance, id, 14, y + 18, 616),
        }
    }
    // Log files (two columns).
    f.label(hinstance, "stdout 日志 (可选)", 14, 282, 280);
    f.label(hinstance, "stderr 日志 (可选)", 344, 282, 280);
    f.edit(hinstance, ID_STDOUT, 14, 300, 316);
    f.edit(hinstance, ID_STDERR, 344, 300, 286);
    // Combos.
    f.label(hinstance, "启动类型", 14, 342, 64);
    f.combo(hinstance, ID_STARTUP, 82, 338, 170, &["自动", "自动（延迟启动）", "手动"], 0);
    f.label(hinstance, "进程优先级", 300, 342, 76);
    f.combo(
        hinstance,
        ID_PRIORITY,
        380,
        338,
        170,
        &["实时", "高", "高于标准", "标准", "低于标准", "空闲"],
        3,
    );
    // Buttons.
    f.button(hinstance, ID_OK, "安装", 424, 384, 104, 30, true);
    f.button(hinstance, ID_CANCEL, "取消", 536, 384, 104, 30, false);
}

// ---------------------------------------------------------------- wndproc ----

unsafe extern "system" fn dlg_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgState;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *p;
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u32;
            match id {
                ID_OK => on_ok(st),
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                ID_BROWSE_APP => {
                    if let Some(path) = pick(st.owner, "选择应用程序可执行文件", PickMode::Exe) {
                        st.form.set_text(ID_APP, &path);
                    }
                }
                ID_BROWSE_DIR => {
                    if let Some(path) = pick(st.owner, "选择工作目录", PickMode::Folder) {
                        st.form.set_text(ID_DIR, &path);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = EnableWindow(st.owner, true);
            let _ = SetForegroundWindow(st.owner);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(p));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn prompt_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptState;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *p;
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u32;
            match id {
                ID_OK => prompt_ok(st),
                ID_CANCEL | 0x2 /* IDCANCEL */ => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = EnableWindow(st.owner, true);
            let _ = SetForegroundWindow(st.owner);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(p));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------- pickers ----

fn pick(owner: HWND, title: &str, mode: PickMode) -> Option<String> {
    ctl::pick_open(owner, title, mode)
}

// ---------------------------------------------------------------- install ----

/// Create the service (`name`) for `cfg`, shared by GUI install / TOML import.
/// Mirrors the CLI `install` behavior: CreateServiceW + Parameters registry.
pub fn install_service(name: &str, cfg: &Config) -> Result<(), String> {
    let Ok(self_exe) = std::env::current_exe() else {
        return Err("无法确定 rssvc.exe 自身路径。".to_string());
    };
    let scm = crate::scm::open_manager(true)?;
    let res = crate::scm::create_rssvc_service(scm, name, cfg, &self_exe.to_string_lossy());
    unsafe {
        let _ = CloseServiceHandle(scm);
    }
    res?;
    cfg.save_parameters(name)
        .map_err(|e| format!("服务已创建，但写入注册表配置失败: {e}"))?;
    let _ = cfg.save_delayed_flag(name); // best-effort
    Ok(())
}

fn valid_name(name: &str) -> Option<String> {
    const BAD_CHARS: &str = "\\/:*?\"<>|";
    if name.is_empty() {
        return Some("请填写服务名。".to_string());
    }
    if name.chars().any(|c| BAD_CHARS.contains(c)) {
        return Some("服务名不能包含 \\ / : * ? \" < > | 等字符。".to_string());
    }
    None
}

fn on_ok(st: &mut DlgState) {
    let name = st.form.text(ID_NAME);
    let app = st.form.text(ID_APP);

    if let Some(err) = valid_name(&name) {
        msg_box(st.form.hwnd, &err, "rssvc", MB_OK | MB_ICONINFORMATION);
        return;
    }
    if app.is_empty() || !std::path::Path::new(&app).is_file() {
        msg_box(st.form.hwnd, "应用程序路径无效或文件不存在。", "rssvc", MB_OK | MB_ICONERROR);
        return;
    }

    let mut cfg = Config::default();
    cfg.application = std::fs::canonicalize(&app)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| app.clone());
    cfg.app_parameters = st.form.text(ID_ARGS);
    let dir = st.form.text(ID_DIR);
    cfg.app_directory = if dir.is_empty() {
        std::path::Path::new(&cfg.application)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        dir
    };
    cfg.stdout = st.form.text(ID_STDOUT);
    cfg.stderr = st.form.text(ID_STDERR);
    let su = st.form.combo_sel(ID_STARTUP, 0);
    cfg.startup = if su == 2 { START_MANUAL } else { START_AUTO };
    cfg.delayed_autostart = su == 1;
    let pr = st.form.combo_sel(ID_PRIORITY, 3) as usize;
    cfg.priority = [
        PRIORITY_REALTIME,
        PRIORITY_HIGH,
        PRIORITY_ABOVE_NORMAL,
        PRIORITY_NORMAL,
        PRIORITY_BELOW_NORMAL,
        PRIORITY_IDLE,
    ][pr.min(5)];
    let display = st.form.text(ID_DISPLAY);
    cfg.display_name = if display.is_empty() { name.clone() } else { display };

    match install_service(&name, &cfg) {
        Ok(()) => {
            msg_box(
                st.form.hwnd,
                &format!(
                    "服务 \"{name}\" 安装成功。\n\n在主窗口选中该服务后点击\"启动\"即可运行；\n也可以在 services.msc 中管理它。"
                ),
                "rssvc",
                MB_OK | MB_ICONINFORMATION,
            );
            unsafe {
                let _ = PostMessageW(Some(st.owner), WM_APP_REBUILD, WPARAM(0), LPARAM(0));
                let _ = DestroyWindow(st.form.hwnd);
            }
        }
        Err(e) => msg_box(st.form.hwnd, &e, "rssvc 安装失败", MB_OK | MB_ICONERROR),
    }
}

fn prompt_ok(st: &mut PromptState) {
    let name = st.form.text(ID_NAME);
    if let Some(err) = valid_name(&name) {
        msg_box(st.form.hwnd, &err, "rssvc", MB_OK | MB_ICONINFORMATION);
        return;
    }
    // Refuse to overwrite an existing service.
    if let Ok(scm) = crate::scm::open_manager_read() {
        let exists = crate::scm::open_service(scm, &name, crate::scm::ACCESS_QUERY).is_ok();
        unsafe {
            let _ = CloseServiceHandle(scm);
        }
        if exists {
            msg_box(
                st.form.hwnd,
                &format!("服务 \"{name}\" 已存在，请换一个名称。"),
                "rssvc",
                MB_OK | MB_ICONINFORMATION,
            );
            return;
        }
    }

    let mut cfg = st.cfg.clone();
    if cfg.display_name.trim().is_empty() {
        cfg.display_name = name.clone();
    }
    match install_service(&name, &cfg) {
        Ok(()) => {
            msg_box(
                st.form.hwnd,
                &format!(
                    "服务 \"{name}\" 导入成功（完整保留 TOML 中的全部配置）。\n\n在主窗口选中该服务后点击\"启动\"即可运行。"
                ),
                "rssvc",
                MB_OK | MB_ICONINFORMATION,
            );
            unsafe {
                let _ = PostMessageW(Some(st.owner), WM_APP_REBUILD, WPARAM(0), LPARAM(0));
                let _ = DestroyWindow(st.form.hwnd);
            }
        }
        Err(e) => msg_box(st.form.hwnd, &e, "rssvc 导入失败", MB_OK | MB_ICONERROR),
    }
}
