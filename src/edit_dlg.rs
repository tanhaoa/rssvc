//! 编辑已有服务配置的图形对话框。
//!
//! 从注册表加载当前配置（`Config::load`），表单预填后允许修改：
//! 显示名称 / 描述 / 应用程序 / 启动参数 / 工作目录 / stdout / stderr /
//! 启动类型 / 进程优先级 / 日志轮转 / 重启策略 / 停止超时 / 环境变量 /
//! 依赖服务 / 停止级别跳过。
//!
//! 保存流程与 CLI `import` 一致：
//!   1. `Config::save_parameters`（Parameters 注册表键）
//!   2. `save_delayed_flag`（延迟自动启动标志）
//!   3. `scm::change_config`（ChangeServiceConfigW + 描述 + 依赖）
//!   4. 尽力发送 SERVICE_CONTROL_PARAMCHANGE（轮转参数即时生效）
//!   5. 询问是否立即重启服务（后台线程执行，结果经 WM_APP 回传主窗口）
//!
//! 服务名不可修改（改名等价于删除重建，为避免误操作不提供）。
//! 运行账户修改需要交互式密码输入，暂不在表单中提供（用 CLI install）。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetWindowLongPtrW, LoadCursorW, PostMessageW, RegisterClassExW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, IDC_ARROW,
    MB_ICONERROR, MB_OK, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_SETFONT, WNDCLASSEXW, WS_CAPTION, WS_SYSMENU,
};

use crate::config::{
    Config, PRIORITY_ABOVE_NORMAL, PRIORITY_BELOW_NORMAL, PRIORITY_HIGH, PRIORITY_IDLE,
    PRIORITY_NORMAL, PRIORITY_REALTIME, START_AUTO, START_MANUAL,
};
use crate::ctl::{self, Form};
use crate::gui::{msg_box, request_op, OP_RESTART, WM_APP_REBUILD};
use crate::util;

// Control IDs.
const ID_DISPLAY: u32 = 1;
const ID_DESC: u32 = 2;
const ID_APP: u32 = 3;
const ID_ARGS: u32 = 4;
const ID_DIR: u32 = 5;
const ID_STDOUT: u32 = 6;
const ID_STDERR: u32 = 7;
const ID_STARTUP: u32 = 8;
const ID_PRIORITY: u32 = 9;
const ID_ROTATE: u32 = 10;
const ID_KEEP: u32 = 11;
const ID_RDELAY: u32 = 12;
const ID_THROTTLE: u32 = 13;
const ID_MAXRE: u32 = 14;
const ID_TMO_C: u32 = 15;
const ID_TMO_W: u32 = 16;
const ID_TMO_T: u32 = 17;
const ID_ENV: u32 = 18;
const ID_ENV_EXTRA: u32 = 19;
const ID_OK: u32 = 20;
const ID_CANCEL: u32 = 21;
const ID_SKIP_C: u32 = 22; // stop-method-skip checkboxes (bits of AppStopMethodSkip)
const ID_SKIP_W: u32 = 23;
const ID_SKIP_T: u32 = 24;
const ID_SKIP_K: u32 = 25;
const ID_DEPS: u32 = 26;
const ID_BROWSE_APP: u32 = 30;
const ID_BROWSE_DIR: u32 = 31;

// Desired CLIENT area size; the outer window size is computed from it via
// AdjustWindowRectEx (title bar + borders would otherwise crop the form).
const DLG_W: i32 = 720;
const DLG_H: i32 = 722;

struct EditState {
    owner: HWND,
    name: String,
    form: Form,
}

// ------------------------------------------------------------- entry ----

/// Open the edit dialog for service `name` with its current `cfg` preloaded.
pub fn open_edit_dialog(owner: HWND, name: String, cfg: Config) {
    unsafe {
        let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else {
            return;
        };
        let hinstance = HINSTANCE(hmodule.0);
        register_class(hinstance);

        let (outer_w, outer_h) = ctl::outer_size_for_client(DLG_W, DLG_H);
        let (x, y) = ctl::center_on(owner, outer_w, outer_h);
        let title = windows::core::HSTRING::from(format!("rssvc - 编辑服务配置 [{name}]"));
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("rssvcEditWnd"),
            PCWSTR::from_raw(title.as_ptr()),
            WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0),
            x,
            y,
            outer_w,
            outer_h,
            Some(owner),
            None,
            Some(hinstance),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };

        let mut st = Box::new(EditState {
            owner,
            name,
            form: Form::new(hwnd),
        });
        create_form(&mut st, hinstance);
        prefill(&st.form, &cfg);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);

        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn register_class(hinstance: HINSTANCE) {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| unsafe {
        let mut wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(edit_wndproc),
            ..Default::default()
        };
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        wc.hbrBackground = windows::Win32::Graphics::Gdi::HBRUSH(
            (windows::Win32::Graphics::Gdi::COLOR_WINDOW.0 + 1) as *mut core::ffi::c_void,
        );
        wc.lpszClassName = w!("rssvcEditWnd");
        RegisterClassExW(&wc);
    });
}

// -------------------------------------------------------------- form ----

unsafe fn create_form(st: &mut EditState, hinstance: HINSTANCE) {
    let f = &mut st.form;
    let full_w = DLG_W - 28; // client margins 14 + 14
    let half_w = (full_w - 20) / 2;
    let right_edge = DLG_W - 14; // right client margin

    f.label(hinstance, "服务名（不可修改）", 14, 8, 400);
    let name_h = util::to_wide(&st.name);
    let name_static = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR::from_raw(name_h.as_ptr()),
        WINDOW_STYLE(
            windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0
                | windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE.0,
        ),
        14,
        24,
        500,
        18,
        Some(f.hwnd),
        None,
        Some(hinstance),
        None,
    );
    if let Ok(h) = name_static {
        SendMessageW(
            h,
            WM_SETFONT,
            Some(WPARAM(f.hfont.0 as usize)),
            Some(LPARAM(1)),
        );
    }

    f.label(hinstance, "显示名称 (可选，留空保持不变)", 14, 50, 400);
    f.edit(hinstance, ID_DISPLAY, 14, 66, full_w);

    f.label(hinstance, "描述 (可选)", 14, 96, 400);
    f.edit(hinstance, ID_DESC, 14, 112, full_w);

    f.label(hinstance, "应用程序 *", 14, 142, 400);
    f.edit(hinstance, ID_APP, 14, 158, full_w - 118);
    f.button(
        hinstance,
        ID_BROWSE_APP,
        "浏览...",
        right_edge - 104,
        157,
        104,
        26,
        false,
    );

    f.label(hinstance, "启动参数 (可选)", 14, 192, 400);
    f.edit(hinstance, ID_ARGS, 14, 208, full_w);

    f.label(
        hinstance,
        "工作目录 (可选, 默认: 程序所在目录)",
        14,
        238,
        400,
    );
    f.edit(hinstance, ID_DIR, 14, 254, full_w - 118);
    f.button(
        hinstance,
        ID_BROWSE_DIR,
        "浏览...",
        right_edge - 104,
        253,
        104,
        26,
        false,
    );

    f.label(hinstance, "stdout 日志 (可选)", 14, 288, 280);
    f.label(hinstance, "stderr 日志 (可选)", 14 + half_w + 20, 288, 280);
    f.edit(hinstance, ID_STDOUT, 14, 304, half_w);
    f.edit(hinstance, ID_STDERR, 14 + half_w + 20, 304, half_w);

    f.label(hinstance, "启动类型", 14, 338, 60);
    f.combo(
        hinstance,
        ID_STARTUP,
        76,
        334,
        170,
        &["自动", "自动（延迟启动）", "手动"],
        0,
    );
    f.label(hinstance, "进程优先级", 270, 338, 76);
    f.combo(
        hinstance,
        ID_PRIORITY,
        348,
        334,
        170,
        &["实时", "高", "高于标准", "标准", "低于标准", "空闲"],
        3,
    );

    f.label(hinstance, "轮转大小(字节, 0=仅按天)", 14, 372, 170);
    f.edit(hinstance, ID_ROTATE, 188, 368, 100);
    f.label(hinstance, "保留份数", 306, 372, 60);
    f.edit(hinstance, ID_KEEP, 368, 368, 70);

    f.label(hinstance, "重启延迟(ms)", 14, 402, 90);
    f.edit(hinstance, ID_RDELAY, 106, 398, 90);
    f.label(hinstance, "节流阈值(ms)", 214, 402, 90);
    f.edit(hinstance, ID_THROTTLE, 306, 398, 90);
    f.label(hinstance, "最大连续重启", 414, 402, 100);
    f.edit(hinstance, ID_MAXRE, 516, 398, 70);

    f.label(hinstance, "停止超时(ms): 控制台", 14, 432, 140);
    f.edit(hinstance, ID_TMO_C, 156, 428, 80);
    f.label(hinstance, "窗口", 254, 432, 40);
    f.edit(hinstance, ID_TMO_W, 296, 428, 80);
    f.label(hinstance, "线程", 394, 432, 40);
    f.edit(hinstance, ID_TMO_T, 436, 428, 80);

    f.label(
        hinstance,
        "环境变量 AppEnvironment (替换式, 通常留空; 每行 KEY=VALUE, # 注释)",
        14,
        462,
        half_w,
    );
    f.edit_ml(hinstance, ID_ENV, 14, 480, half_w, 110, false);
    f.label(
        hinstance,
        "追加环境变量 AppEnvironmentExtra (推荐; 每行 KEY=VALUE, # 注释)",
        14 + half_w + 20,
        462,
        half_w,
    );
    f.edit_ml(
        hinstance,
        ID_ENV_EXTRA,
        14 + half_w + 20,
        480,
        half_w,
        110,
        false,
    );

    // Dependencies + stop-method-skip (previously CLI-only fields).
    f.label(
        hinstance,
        "依赖服务 (逗号分隔, 可选; 导入时非空才会更新)",
        14,
        600,
        half_w,
    );
    f.edit(hinstance, ID_DEPS, 14, 618, half_w);
    f.label(
        hinstance,
        "跳过停止级别 (高级; 默认全部执行)",
        14 + half_w + 20,
        600,
        half_w,
    );
    let cb_x0 = 14 + half_w + 20;
    let cb_w = (half_w - 14) / 2;
    f.checkbox(hinstance, ID_SKIP_C, "1 控制台事件", cb_x0, 618, cb_w);
    f.checkbox(
        hinstance,
        ID_SKIP_W,
        "2 窗口消息",
        cb_x0 + cb_w + 14,
        618,
        cb_w,
    );
    f.checkbox(hinstance, ID_SKIP_T, "4 线程消息", cb_x0, 642, cb_w);
    f.checkbox(
        hinstance,
        ID_SKIP_K,
        "8 强制终止",
        cb_x0 + cb_w + 14,
        642,
        cb_w,
    );

    f.button(
        hinstance,
        ID_OK,
        "保存",
        right_edge - 224,
        678,
        104,
        30,
        true,
    );
    f.button(
        hinstance,
        ID_CANCEL,
        "取消",
        right_edge - 104,
        678,
        104,
        30,
        false,
    );
}

fn prefill(form: &Form, cfg: &Config) {
    form.set_text(ID_DISPLAY, &cfg.display_name);
    form.set_text(ID_DESC, &cfg.description);
    form.set_text(ID_APP, &cfg.application);
    form.set_text(ID_ARGS, &cfg.app_parameters);
    form.set_text(ID_DIR, &cfg.app_directory);
    form.set_text(ID_STDOUT, &cfg.stdout);
    form.set_text(ID_STDERR, &cfg.stderr);
    form.set_combo_sel(
        ID_STARTUP,
        if cfg.startup == START_MANUAL {
            2
        } else if cfg.delayed_autostart {
            1
        } else {
            0
        },
    );
    let prio = [
        PRIORITY_REALTIME,
        PRIORITY_HIGH,
        PRIORITY_ABOVE_NORMAL,
        PRIORITY_NORMAL,
        PRIORITY_BELOW_NORMAL,
        PRIORITY_IDLE,
    ];
    form.set_combo_sel(
        ID_PRIORITY,
        prio.iter().position(|p| *p == cfg.priority).unwrap_or(3) as i32,
    );
    form.set_text(ID_ROTATE, &cfg.rotate_bytes.to_string());
    form.set_text(ID_KEEP, &cfg.rotate_keep.to_string());
    form.set_text(ID_RDELAY, &cfg.restart_delay_ms.to_string());
    form.set_text(ID_THROTTLE, &cfg.throttle_ms.to_string());
    form.set_text(ID_MAXRE, &cfg.max_restarts.to_string());
    form.set_text(ID_TMO_C, &cfg.stop_timeout_console.to_string());
    form.set_text(ID_TMO_W, &cfg.stop_timeout_window.to_string());
    form.set_text(ID_TMO_T, &cfg.stop_timeout_threads.to_string());
    form.set_text(ID_ENV, &cfg.environment.join("\r\n"));
    form.set_text(ID_ENV_EXTRA, &cfg.environment_extra.join("\r\n"));
    form.set_text(ID_DEPS, &cfg.dependencies.join(", "));
    form.set_checked(
        ID_SKIP_C,
        cfg.stop_method_skip & crate::runner::SKIP_CONSOLE != 0,
    );
    form.set_checked(
        ID_SKIP_W,
        cfg.stop_method_skip & crate::runner::SKIP_WINDOW != 0,
    );
    form.set_checked(
        ID_SKIP_T,
        cfg.stop_method_skip & crate::runner::SKIP_THREADS != 0,
    );
    form.set_checked(
        ID_SKIP_K,
        cfg.stop_method_skip & crate::runner::SKIP_TERMINATE != 0,
    );
}

// ------------------------------------------------------------ wndproc ----

unsafe extern "system" fn edit_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EditState;
    if p.is_null() {
        return windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *p;
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u32;
            match id {
                ID_OK => on_save(st),
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                ID_BROWSE_APP => {
                    if let Some(path) =
                        ctl::pick_open(st.owner, "选择应用程序可执行文件", ctl::PickMode::Exe)
                    {
                        st.form.set_text(ID_APP, &path);
                    }
                }
                ID_BROWSE_DIR => {
                    if let Some(path) =
                        ctl::pick_open(st.owner, "选择工作目录", ctl::PickMode::Folder)
                    {
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
        _ => windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------- save ----

fn parse_num(field: &str, s: &str, current: u32) -> Result<u32, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(current); // keep the loaded value
    }
    t.parse::<u32>()
        .map_err(|_| format!("{field} 必须是非负整数，当前输入: \"{t}\""))
}

fn on_save(st: &mut EditState) {
    let form = &st.form;
    let name = st.name.clone();

    let app = form.text(ID_APP);
    if app.is_empty() {
        msg_box(
            st.hwnd(),
            "应用程序不能为空。",
            "rssvc",
            MB_OK | MB_ICONERROR,
        );
        return;
    }
    if !std::path::Path::new(&app).is_file() {
        msg_box(
            st.hwnd(),
            "应用程序路径无效或文件不存在。",
            "rssvc",
            MB_OK | MB_ICONERROR,
        );
        return;
    }

    // Fresh load preserves untouched advanced fields
    // (stop_method_skip, dependencies, account ...).
    let mut cfg = crate::config::Config::load(&name).unwrap_or_default();
    let num = |field: &str, id: u32, cur: u32| -> Result<u32, String> {
        parse_num(field, &form.text(id), cur)
    };
    let rotate_bytes = match num("轮转大小", ID_ROTATE, cfg.rotate_bytes) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let rotate_keep = match num("保留份数", ID_KEEP, cfg.rotate_keep) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let restart_delay_ms = match num("重启延迟", ID_RDELAY, cfg.restart_delay_ms) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let throttle_ms = match num("节流阈值", ID_THROTTLE, cfg.throttle_ms) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let max_restarts = match num("最大连续重启", ID_MAXRE, cfg.max_restarts) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let stop_timeout_console = match num("停止超时(控制台)", ID_TMO_C, cfg.stop_timeout_console)
    {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let stop_timeout_window = match num("停止超时(窗口)", ID_TMO_W, cfg.stop_timeout_window) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let stop_timeout_threads = match num("停止超时(线程)", ID_TMO_T, cfg.stop_timeout_threads)
    {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let environment = match ctl::parse_env_text(&form.text(ID_ENV)) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let environment_extra = match ctl::parse_env_text(&form.text(ID_ENV_EXTRA)) {
        Ok(v) => v,
        Err(e) => return msg_box(st.hwnd(), &e, "rssvc", MB_OK | MB_ICONERROR),
    };
    let mut skip = 0u32;
    if form.checked(ID_SKIP_C) {
        skip |= crate::runner::SKIP_CONSOLE;
    }
    if form.checked(ID_SKIP_W) {
        skip |= crate::runner::SKIP_WINDOW;
    }
    if form.checked(ID_SKIP_T) {
        skip |= crate::runner::SKIP_THREADS;
    }
    if form.checked(ID_SKIP_K) {
        skip |= crate::runner::SKIP_TERMINATE;
    }
    let dependencies: Vec<String> = form
        .text(ID_DEPS)
        .split(&[',', ';'][..])
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect();

    // Apply to the freshly loaded config so untouched advanced fields
    // (account ...) are preserved as-is.
    cfg.application = util::canonicalize_plain(&app);
    let dir = form.text(ID_DIR);
    cfg.app_directory = if dir.is_empty() {
        std::path::Path::new(&cfg.application)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        dir
    };
    cfg.app_parameters = form.text(ID_ARGS);
    cfg.stdout = form.text(ID_STDOUT);
    cfg.stderr = form.text(ID_STDERR);
    cfg.display_name = form.text(ID_DISPLAY);
    cfg.description = form.text(ID_DESC);
    cfg.rotate_bytes = rotate_bytes;
    cfg.rotate_keep = rotate_keep;
    cfg.restart_delay_ms = restart_delay_ms;
    cfg.throttle_ms = throttle_ms;
    cfg.max_restarts = max_restarts;
    cfg.stop_timeout_console = stop_timeout_console;
    cfg.stop_timeout_window = stop_timeout_window;
    cfg.stop_timeout_threads = stop_timeout_threads;
    cfg.environment = environment;
    cfg.environment_extra = environment_extra;
    cfg.stop_method_skip = skip;
    cfg.dependencies = dependencies;
    let su = form.combo_sel(ID_STARTUP, 0);
    cfg.startup = if su == 2 { START_MANUAL } else { START_AUTO };
    cfg.delayed_autostart = su == 1;
    let pr = form.combo_sel(ID_PRIORITY, 3) as usize;
    cfg.priority = [
        PRIORITY_REALTIME,
        PRIORITY_HIGH,
        PRIORITY_ABOVE_NORMAL,
        PRIORITY_NORMAL,
        PRIORITY_BELOW_NORMAL,
        PRIORITY_IDLE,
    ][pr.min(5)];

    // ---- persist ----------------------------------------------------------
    if let Err(e) = cfg.save_parameters(&name) {
        msg_box(
            st.hwnd(),
            &format!("写入注册表配置失败: {e}"),
            "rssvc 保存失败",
            MB_OK | MB_ICONERROR,
        );
        return;
    }
    if let Err(e) = cfg.save_delayed_flag(&name) {
        msg_box(
            st.hwnd(),
            &format!("写入延迟启动标志失败: {e}"),
            "rssvc 保存失败",
            MB_OK | MB_ICONERROR,
        );
        return;
    }

    // Start type / display name / description via the SCM API.
    let scm_write = || -> Result<(), String> {
        let scm = crate::scm::open_manager(true)?;
        let res = (|| {
            let svc = crate::scm::open_service(scm, &name, crate::scm::ACCESS_CONFIG)?;
            crate::scm::change_config(&svc, &cfg)
        })();
        unsafe {
            let _ = windows::Win32::System::Services::CloseServiceHandle(scm);
        }
        res
    };
    if let Err(e) = scm_write() {
        msg_box(st.hwnd(), &e, "rssvc 保存失败", MB_OK | MB_ICONERROR);
        return;
    }

    // Best-effort: notify the running service so rotation settings apply now.
    notify_param_change(&name);

    // Ask whether to restart now (MB_YESNO + IDYES via the shared helper:
    // the old MB_OKCANCEL + IDYES mismatch never triggered the restart).
    let text = "配置已保存。\n\n日志轮转参数已实时下发到运行中的服务；\n其余参数将在服务重启后生效。\n\n是否立即重启该服务？";
    if ctl::ask_yes_no(st.hwnd(), text, "rssvc 保存成功") {
        // Restart via the main window's background operation queue.
        request_op(st.owner, name.clone(), OP_RESTART);
    }
    unsafe {
        let _ = PostMessageW(Some(st.owner), WM_APP_REBUILD, WPARAM(0), LPARAM(0));
        let _ = DestroyWindow(st.form.hwnd);
    }
}

fn notify_param_change(name: &str) {
    use windows::Win32::System::Services::CloseServiceHandle;
    let Ok(scm) = crate::scm::open_manager(true) else {
        return;
    };
    let sent = (|| -> Result<(), String> {
        let svc = crate::scm::open_service(
            scm,
            name,
            crate::scm::ACCESS_PAUSE | crate::scm::ACCESS_QUERY,
        )?;
        let _ = crate::scm::control(&svc, crate::scm::CONTROL_PARAMCHANGE);
        Ok(())
    })();
    let _ = sent;
    unsafe {
        let _ = CloseServiceHandle(scm);
    }
}

impl EditState {
    fn hwnd(&self) -> HWND {
        self.form.hwnd
    }
}
