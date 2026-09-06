//! rssvc gui - 原生 Win32 图形界面服务管理器。
//!
//! `rssvc gui`（或直接双击 exe）打开主窗口：
//! - 服务列表：状态 / PID / 内存 / 运行时长，每 1.5 秒自动刷新
//! - 详情面板：应用路径、参数、日志、轮转与重启策略等完整配置
//! - 操作按钮：启动 / 停止 / 重启 / 暂停 / 继续 / 删除 / 查看日志 / 安装新服务
//!
//! 全部使用 user32/comctl32 原生控件，不引入任何第三方 UI 库；
//! 同时兼容显示由 nssm 安装的服务（共享 Parameters 注册表布局）。

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, FILETIME, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, DEFAULT_CHARSET,
    FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, FW_NORMAL, HFONT,
    OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList, GetConsoleWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LIST_VIEW_ITEM_FLAGS,
    LVCOLUMNW, LVCOLUMNW_MASK, LVIF_PARAM, LVIF_STATE, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMN, LVM_INSERTITEM, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMSTATE, LVM_SETITEMTEXT, LVNI_SELECTED, LVN_ITEMCHANGED, LVS_EX_FULLROWSELECT,
    LVS_EX_GRIDLINES, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, MessageBoxW, MoveWindow, PostMessageW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetForegroundWindow, SetProcessDPIAware,
    SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, GWLP_USERDATA, HMENU, IDC_ARROW,
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MINMAXINFO, MSG, SW_HIDE, SW_SHOW,
    SW_SHOWNORMAL, WM_APP, WM_COMMAND, WM_DESTROY, WM_GETMINMAXINFO, WM_NOTIFY, WM_SETFONT,
    WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};

use crate::config::Config;
use crate::ctl;
use crate::util;

// ------------------------------------------------------------- constants --

pub const WM_APP_OP_DONE: u32 = WM_APP + 1;
pub const WM_APP_REBUILD: u32 = WM_APP + 2;

const ID_START: u32 = 101;
const ID_STOP: u32 = 102;
const ID_RESTART: u32 = 103;
const ID_PAUSE: u32 = 104;
const ID_CONTINUE: u32 = 105;
const ID_LOGVIEW: u32 = 106;
const ID_EDIT: u32 = 107;
const ID_EXPORT: u32 = 108;
const ID_IMPORT: u32 = 109;
const ID_INSTALL: u32 = 110;
const ID_REMOVE: u32 = 111;
const ID_REFRESH: u32 = 112;

pub const OP_START: u32 = 1;
pub const OP_STOP: u32 = 2;
pub const OP_RESTART: u32 = 3;
pub const OP_PAUSE: u32 = 4;
pub const OP_CONTINUE: u32 = 5;
pub const OP_DELETE: u32 = 6;

// ListView sub-item indexes.
const COL_NAME: i32 = 0;
const COL_STATE: i32 = 1;
const COL_PID: i32 = 2;
const COL_MEM: i32 = 3;
const COL_UPTIME: i32 = 4;
const COL_KIND: i32 = 5;
const COL_APP: i32 = 6;

// ------------------------------------------------------------- data model --

#[derive(Clone)]
struct SvcEntry {
    name: String,
    display: String,
    kind: &'static str,
    cfg: Option<Config>,
}

struct GuiState {
    main: HWND,
    list: HWND,
    details: HWND,
    banner: HWND,
    group: HWND,
    btns: Vec<(u32, HWND)>,
    hfont: HFONT,
    entries: Vec<SvcEntry>,
    busy: bool,
}

// ------------------------------------------------------------ entry point --

/// True when this process owns its console (i.e. double-clicked from Explorer).
pub fn owns_console() -> bool {
    unsafe {
        let mut buf = [0u32; 8];
        GetConsoleProcessList(&mut buf) == 1
    }
}

fn detach_console() {
    unsafe {
        let con = GetConsoleWindow();
        if con.is_invalid() {
            return;
        }
        let mut buf = [0u32; 8];
        if GetConsoleProcessList(&mut buf) == 1 {
            let _ = ShowWindow(con, SW_HIDE);
        }
        let _ = FreeConsole();
    }
}

/// Run the GUI manager. Never returns.
pub fn launch() -> ! {
    unsafe {
        detach_console();
        let _ = SetProcessDPIAware();
        // COM apartment for the file-open dialogs (install form).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);

        let hmodule = GetModuleHandleW(PCWSTR::null()).expect("GetModuleHandleW failed");
        let hinstance = HINSTANCE(hmodule.0);
        register_class(hinstance);
        let hwnd = create_main_window(hinstance);

        let hfont = CreateFontW(
            -12,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            FONT_CHARSET(DEFAULT_CHARSET.0),
            FONT_OUTPUT_PRECISION(OUT_DEFAULT_PRECIS.0),
            FONT_CLIP_PRECISION(CLIP_DEFAULT_PRECIS.0),
            FONT_QUALITY(CLEARTYPE_QUALITY.0),
            0,
            w!("Microsoft YaHei UI"),
        );

        let mut st = Box::new(GuiState {
            main: hwnd,
            list: HWND::default(),
            details: HWND::default(),
            banner: HWND::default(),
            group: HWND::default(),
            btns: Vec::new(),
            hfont,
            entries: Vec::new(),
            busy: false,
        });
        create_children(&mut st, hinstance);
        {
            let mut cr = RECT::default();
            let _ = GetClientRect(hwnd, &mut cr);
            layout(&st, cr.right, cr.bottom);
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);

        {
            let st = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState);
            rebuild_list(st);
        }
        let _ = SetTimer(Some(hwnd), 1, 1500, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        message_loop();
    }
    std::process::exit(0);
}

// -------------------------------------------------------------- creation --

fn register_class(hinstance: HINSTANCE) {
    let cls = w!("rssvcMainWnd");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(
            windows::Win32::UI::WindowsAndMessaging::CS_HREDRAW.0
                | windows::Win32::UI::WindowsAndMessaging::CS_VREDRAW.0,
        ),
        lpfnWndProc: Some(main_wndproc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
            (COLOR_WINDOW.0 + 1) as *mut core::ffi::c_void,
        ),
        lpszClassName: cls,
        ..Default::default()
    };
    unsafe {
        RegisterClassExW(&wc);
    }
}

fn create_main_window(hinstance: HINSTANCE) -> HWND {
    let cls = w!("rssvcMainWnd");
    let title = windows::core::HSTRING::from(format!("rssvc 服务管理器 v{}", crate::VERSION));
    unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            cls,
            PCWSTR::from_raw(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
            0,
            1000,
            720,
            None,
            None,
            Some(hinstance),
            None,
        )
        .expect("CreateWindowExW failed")
    }
}

fn create_children(st: &mut GuiState, hinstance: HINSTANCE) {
    // Banner: admin state hint.
    let admin = unsafe { IsUserAnAdmin().as_bool() };
    let banner_text = if admin {
        w!("以管理员身份运行 - 全部操作可用")
    } else {
        w!("[!] 未以管理员身份运行 - 启动/停止/安装/删除等操作可能被拒绝 (右键 rssvc.exe -> 以管理员身份运行)")
    };
    st.banner = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            w!("STATIC"),
            banner_text,
            window_style(WS_CHILD.0 | WS_VISIBLE.0),
            0,
            0,
            10,
            10,
            Some(st.main),
            None,
            Some(hinstance),
            None,
        )
        .expect("create banner")
    };

    // Service list.
    st.list = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("SysListView32"),
            w!("服务列表"),
            window_style(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | LVS_REPORT
                    | LVS_SINGLESEL
                    | LVS_SHOWSELALWAYS,
            ),
            0,
            0,
            10,
            10,
            Some(st.main),
            Some(HMENU(COL_NAME as usize as *mut _)),
            Some(hinstance),
            None,
        )
        .expect("create listview")
    };
    unsafe {
        SendMessageW(
            st.list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(0)),
            Some(LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_GRIDLINES) as isize)),
        );
        lv_insert_column(st.list, COL_NAME, 150, "服务名");
        lv_insert_column(st.list, COL_STATE, 72, "状态");
        lv_insert_column(st.list, COL_PID, 60, "PID");
        lv_insert_column(st.list, COL_MEM, 80, "内存");
        lv_insert_column(st.list, COL_UPTIME, 90, "运行时长");
        lv_insert_column(st.list, COL_KIND, 58, "类型");
        lv_insert_column(st.list, COL_APP, 340, "应用程序");
    }

    // Details group + read-only text.
    st.group = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("服务详情"),
            window_style(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | windows::Win32::UI::WindowsAndMessaging::BS_GROUPBOX as u32,
            ),
            0,
            0,
            10,
            10,
            Some(st.main),
            None,
            Some(hinstance),
            None,
        )
        .expect("create group")
    };
    st.details = unsafe {
        CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            window_style(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | WS_VSCROLL.0
                    | windows::Win32::UI::WindowsAndMessaging::ES_MULTILINE as u32
                    | windows::Win32::UI::WindowsAndMessaging::ES_READONLY as u32
                    | windows::Win32::UI::WindowsAndMessaging::ES_AUTOVSCROLL as u32,
            ),
            0,
            0,
            10,
            10,
            Some(st.main),
            None,
            Some(hinstance),
            None,
        )
        .expect("create details")
    };

    // Operation buttons.
    let defs: [(&str, u32); 12] = [
        ("启动", ID_START),
        ("停止", ID_STOP),
        ("重启", ID_RESTART),
        ("暂停", ID_PAUSE),
        ("继续", ID_CONTINUE),
        ("实时日志", ID_LOGVIEW),
        ("编辑配置", ID_EDIT),
        ("导出TOML", ID_EXPORT),
        ("导入TOML", ID_IMPORT),
        ("安装新服务", ID_INSTALL),
        ("删除服务", ID_REMOVE),
        ("刷新", ID_REFRESH),
    ];
    for (label, id) in defs {
        let wide_label = util::to_wide(label);
        let h = unsafe {
            CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                PCWSTR::from_raw(wide_label.as_ptr()),
                window_style(
                    WS_CHILD.0
                        | WS_VISIBLE.0
                        | WS_TABSTOP.0
                        | windows::Win32::UI::WindowsAndMessaging::BS_PUSHBUTTON as u32,
                ),
                0,
                0,
                10,
                10,
                Some(st.main),
                Some(HMENU(id as usize as *mut _)),
                Some(hinstance),
                None,
            )
            .expect("create button")
        };
        st.btns.push((id, h));
    }

    // Common font for every child control.
    for (_, h) in &st.btns {
        unsafe {
            SendMessageW(
                *h,
                WM_SETFONT,
                Some(WPARAM(st.hfont.0 as usize)),
                Some(LPARAM(1)),
            )
        };
    }
    unsafe {
        SendMessageW(
            st.banner,
            WM_SETFONT,
            Some(WPARAM(st.hfont.0 as usize)),
            Some(LPARAM(1)),
        );
        SendMessageW(
            st.list,
            WM_SETFONT,
            Some(WPARAM(st.hfont.0 as usize)),
            Some(LPARAM(1)),
        );
        SendMessageW(
            st.group,
            WM_SETFONT,
            Some(WPARAM(st.hfont.0 as usize)),
            Some(LPARAM(1)),
        );
        SendMessageW(
            st.details,
            WM_SETFONT,
            Some(WPARAM(st.hfont.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

fn window_style(bits: u32) -> windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE {
    windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(bits)
}

// ---------------------------------------------------------------- layout --

unsafe fn layout(st: &GuiState, cw: i32, ch: i32) {
    let m = 12;
    let bw = 118;
    let bx = cw - bw - m;
    let top = 34;
    let _ = MoveWindow(st.banner, m, 8, cw - bw - m - m, 20, true);
    let mut y = top;
    for (_, h) in &st.btns {
        let _ = MoveWindow(*h, bx, y, bw, 28, true);
        y += 33;
    }
    let list_h = (ch - top - 210).max(120);
    let _ = MoveWindow(st.list, m, top, cw - bw - m - m, list_h, true);
    let gy = ch - 200;
    let _ = MoveWindow(st.group, m, gy, cw - m - m, 190, true);
    let _ = MoveWindow(st.details, m + 8, gy + 18, cw - m - m - 16, 190 - 26, true);
}

// -------------------------------------------------------------- wndproc --

unsafe extern "system" fn main_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let st = &mut *p;
    let out = match msg {
        WM_SIZE => {
            layout(
                st,
                (lparam.0 & 0xffff) as i32,
                ((lparam.0 >> 16) & 0xffff) as i32,
            );
            LRESULT(0)
        }
        WM_COMMAND => {
            on_command(st, (wparam.0 & 0xffff) as u32);
            LRESULT(0)
        }
        WM_NOTIFY => on_notify(st, lparam),
        WM_TIMER => {
            if !st.busy {
                refresh_statuses(st);
            }
            LRESULT(0)
        }
        WM_APP_OP_DONE => {
            op_done(st, wparam, lparam);
            LRESULT(0)
        }
        WM_APP_REBUILD => {
            rebuild_list(st);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            mmi.ptMinTrackSize = POINT { x: 880, y: 640 };
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), 1);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    if msg == WM_DESTROY {
        // Reclaim the GuiState box after the match arm (whose borrow of it
        // has ended) and detach it from the window so no further message can
        // reach freed memory.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(p));
        }
    }
    out
}

unsafe fn on_notify(st: &mut GuiState, lparam: LPARAM) -> LRESULT {
    let nm = &*(lparam.0 as *const NMHDR);
    if nm.hwndFrom == st.list && nm.code == LVN_ITEMCHANGED {
        update_details(st);
        return LRESULT(0);
    }
    DefWindowProcW(st.main, WM_NOTIFY, WPARAM(0), lparam)
}

unsafe fn on_command(st: &mut GuiState, id: u32) {
    match id {
        ID_START => begin_op(st, OP_START),
        ID_STOP => begin_op(st, OP_STOP),
        ID_RESTART => begin_op(st, OP_RESTART),
        ID_PAUSE => begin_op(st, OP_PAUSE),
        ID_CONTINUE => begin_op(st, OP_CONTINUE),
        ID_REMOVE => confirm_remove(st),
        ID_LOGVIEW => open_logview(st),
        ID_EDIT => open_editor(st),
        ID_EXPORT => export_toml(st),
        ID_IMPORT => import_toml(st),
        ID_INSTALL => {
            crate::install_dlg::open_install_dialog(st.main);
        }
        ID_REFRESH => rebuild_list(st),
        _ => {}
    }
}

fn is_op_button(id: u32) -> bool {
    matches!(
        id,
        ID_START | ID_STOP | ID_RESTART | ID_PAUSE | ID_CONTINUE | ID_REMOVE
    )
}

// ------------------------------------------------------------- selection --

unsafe fn selected_index(list: HWND) -> i32 {
    SendMessageW(
        list,
        LVM_GETNEXTITEM,
        Some(WPARAM((-1isize) as usize)),
        Some(LPARAM(LVNI_SELECTED as isize)),
    )
    .0 as i32
}

fn selected_entry(st: &GuiState) -> Option<SvcEntry> {
    let idx = unsafe { selected_index(st.list) };
    if idx >= 0 && (idx as usize) < st.entries.len() {
        Some(st.entries[idx as usize].clone())
    } else {
        None
    }
}

// ----------------------------------------------------------- list build --

fn enumerate() -> Vec<SvcEntry> {
    use winreg::enums::*;
    use winreg::RegKey;
    let hk = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(services) = hk.open_subkey(r"SYSTEM\CurrentControlSet\Services") else {
        return Vec::new();
    };
    let mut out: Vec<SvcEntry> = Vec::new();
    for name in services.enum_keys().flatten() {
        let Ok(k) = services.open_subkey(&name) else {
            continue;
        };
        let image = k.get_value::<String, _>("ImagePath").unwrap_or_default();
        let img_lc = image.to_lowercase();
        let is_rssvc = img_lc.contains("rssvc.exe");
        let is_nssm = img_lc.contains("nssm.exe");
        if !(is_rssvc || is_nssm) {
            continue;
        }
        let has_app = k
            .open_subkey("Parameters")
            .and_then(|p| p.get_value::<String, _>("Application"))
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !has_app {
            continue;
        }
        let display = k.get_value::<String, _>("DisplayName").unwrap_or_default();
        let cfg = Config::load(&name).ok();
        out.push(SvcEntry {
            name,
            display,
            kind: if is_rssvc { "rssvc" } else { "nssm" },
            cfg,
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

unsafe fn rebuild_list(st: &mut GuiState) {
    let sel_idx = selected_index(st.list);
    let sel_name = if sel_idx >= 0 && (sel_idx as usize) < st.entries.len() {
        st.entries[sel_idx as usize].name.clone()
    } else {
        String::new()
    };

    st.entries = enumerate();
    SendMessageW(
        st.list,
        LVM_DELETEALLITEMS,
        Some(WPARAM(0)),
        Some(LPARAM(0)),
    );
    for (i, e) in st.entries.iter().enumerate() {
        lv_insert_row(st.list, i as i32, i, &e.name);
        lv_set_text(st.list, i as i32, COL_KIND, e.kind);
        let app = e
            .cfg
            .as_ref()
            .map(|c| c.application.clone())
            .unwrap_or_default();
        lv_set_text(st.list, i as i32, COL_APP, &app);
    }
    refresh_statuses(st);

    if !sel_name.is_empty() {
        if let Some(i) = st.entries.iter().position(|e| e.name == sel_name) {
            lv_select(st.list, i as i32);
            return;
        }
    }
    update_details(st);
}

unsafe fn refresh_statuses(st: &mut GuiState) {
    if st.entries.is_empty() {
        return;
    }
    let Ok(scm) = crate::scm::open_manager_read() else {
        return;
    };
    for (i, e) in st.entries.iter().enumerate() {
        let (state_s, pid) = match crate::scm::open_service(scm, &e.name, crate::scm::ACCESS_QUERY)
        {
            Ok(svc) => match crate::scm::query_status_ex(&svc) {
                Ok(s) => (state_text(s.state).to_string(), s.pid),
                Err(_) => ("?".to_string(), 0),
            },
            Err(_) => ("(已不存在)".to_string(), 0),
        };
        let (mem, up) = if pid != 0 {
            (proc_mem(pid), proc_uptime(pid))
        } else {
            ("-".to_string(), "-".to_string())
        };
        lv_set_text(st.list, i as i32, COL_STATE, &state_s);
        lv_set_text(
            st.list,
            i as i32,
            COL_PID,
            &if pid != 0 {
                pid.to_string()
            } else {
                "-".to_string()
            },
        );
        lv_set_text(st.list, i as i32, COL_MEM, &mem);
        lv_set_text(st.list, i as i32, COL_UPTIME, &up);
    }
    unsafe {
        let _ = windows::Win32::System::Services::CloseServiceHandle(scm);
    }
}

pub fn state_text(state: u32) -> &'static str {
    match state {
        crate::scm::STATE_STOPPED => "已停止",
        crate::scm::STATE_START_PENDING => "启动中",
        crate::scm::STATE_STOP_PENDING => "停止中",
        crate::scm::STATE_RUNNING => "运行中",
        crate::scm::STATE_PAUSE_PENDING => "暂停中",
        crate::scm::STATE_PAUSED => "已暂停",
        _ => "未知",
    }
}

// ---------------------------------------------------------------- details --

fn detail_text(e: &SvcEntry) -> String {
    let mut s = String::new();
    s.push_str(&format!("服务名称:  {}\r\n", e.name));
    if !e.display.is_empty() {
        s.push_str(&format!("显示名称:  {}\r\n", e.display));
    }
    s.push_str(&format!(
        "管理器:    {} (由 {} install 安装)\r\n",
        e.kind, e.kind
    ));
    if let Some(c) = &e.cfg {
        s.push_str(&format!("启动类型:  {}\r\n", c.startup_name()));
        if !c.account.is_empty() {
            s.push_str(&format!("运行账户:  {}\r\n", c.account));
        }
        s.push_str(&format!("应用程序:  {}\r\n", c.application));
        s.push_str(&format!("工作目录:  {}\r\n", c.app_directory));
        if !c.app_parameters.is_empty() {
            s.push_str(&format!("启动参数:  {}\r\n", c.app_parameters));
        }
        for (label, list) in [
            ("环境变量", &c.environment),
            ("追加环境变量", &c.environment_extra),
        ] {
            if !list.is_empty() {
                s.push_str(&format!("{label}:\r\n"));
                for e in list {
                    s.push_str(&format!("    {e}\r\n"));
                }
            }
        }
        if !c.stdout.is_empty() {
            s.push_str(&format!("stdout:    {}\r\n", c.stdout));
        }
        if !c.stderr.is_empty() {
            s.push_str(&format!("stderr:    {}\r\n", c.stderr));
        }
        s.push_str(&format!(
            "日志轮转:  {} 字节 / 保留 {} 份\r\n",
            c.rotate_bytes, c.rotate_keep
        ));
        s.push_str(&format!("进程优先级: {}\r\n", c.priority_name()));
        s.push_str(&format!(
            "重启策略:  延迟 {} ms / 节流阈值 {} ms / 最大连续重启 {} 次\r\n",
            c.restart_delay_ms, c.throttle_ms, c.max_restarts
        ));
        s.push_str(&format!(
            "停止策略:  跳过掩码 {} / 超时 {}-{}-{} ms (控制台-窗口-线程)\r\n",
            c.stop_method_skip,
            c.stop_timeout_console,
            c.stop_timeout_window,
            c.stop_timeout_threads
        ));
        if !c.dependencies.is_empty() {
            s.push_str(&format!("依赖服务:  {}\r\n", c.dependencies.join(", ")));
        }
        if !c.description.is_empty() {
            s.push_str(&format!("描述:      {}\r\n", c.description));
        }
        // Catch-all: NSSM-only parameters we do not model are shown verbatim
        // so nothing installed by nssm is ever invisible.
        let others = crate::config::raw_other_values(&e.name);
        if !others.is_empty() {
            s.push_str("其他参数 (未由 rssvc 管理, 原样保留):\r\n");
            for (k, v) in &others {
                s.push_str(&format!("    {k} = {v}\r\n"));
            }
        }
    }
    s
}

unsafe fn update_details(st: &mut GuiState) {
    let idx = selected_index(st.list);
    let text = if idx >= 0 && (idx as usize) < st.entries.len() {
        detail_text(&st.entries[idx as usize])
    } else {
        String::from("（未选中服务）在上方列表中选择一个服务查看详情；\r\n操作按钮（启动/停止/...）也依赖此处选中项。")
    };
    let w = util::to_wide(&text);
    SendMessageW(
        st.details,
        windows::Win32::UI::WindowsAndMessaging::WM_SETTEXT,
        Some(WPARAM(0)),
        Some(LPARAM(w.as_ptr() as isize)),
    );
    let has = idx >= 0 && !st.busy;
    for (id, h) in &st.btns {
        if is_op_button(*id) {
            let _ = EnableWindow(*h, has);
        }
    }
}

// ------------------------------------------------------------- operations --

fn begin_op(st: &mut GuiState, op: u32) {
    if st.busy {
        return;
    }
    let Some(entry) = selected_entry(st) else {
        return;
    };
    st.busy = true;
    for (id, h) in &st.btns {
        if is_op_button(*id) {
            unsafe {
                let _ = EnableWindow(*h, false);
            }
        }
    }
    spawn_op(st.main, entry.name, op);
}

fn confirm_remove(st: &mut GuiState) {
    if st.busy {
        return;
    }
    let Some(entry) = selected_entry(st) else {
        return;
    };
    let text = format!(
        "确认删除服务 \"{}\" ？\n\n正在运行的服务会先被停止（温和停止序列），然后从系统中移除。",
        entry.name
    );
    // MB_YESNO + IDYES via the shared helper (an MB_OKCANCEL box returns
    // IDOK/IDCANCEL and would never match an IDYES comparison).
    if ctl::ask_yes_no(st.main, &text, "rssvc 删除确认") {
        begin_op(st, OP_DELETE);
    }
}

fn spawn_op(main: HWND, name: String, op: u32) {
    // HWND is a raw pointer and not Send; ship it across the thread as isize.
    let main_addr = main.0 as isize;
    std::thread::spawn(move || {
        let main = HWND(main_addr as *mut core::ffi::c_void);
        let res = run_op(&name, op);
        let lp = Box::into_raw(Box::new(res));
        let sent = unsafe {
            PostMessageW(
                Some(main),
                WM_APP_OP_DONE,
                WPARAM(op as usize),
                LPARAM(lp as isize),
            )
        };
        if sent.is_err() {
            // The window is gone (or delivery failed): nobody will free the
            // payload through WM_APP_OP_DONE, so reclaim it here. Ownership
            // is transferred exactly once — no double-free is possible.
            drop(unsafe { Box::from_raw(lp) });
        }
    });
}

fn run_op(name: &str, op: u32) -> Result<String, String> {
    let scm = crate::scm::open_manager(true)?;
    let res = (|| -> Result<String, String> {
        match op {
            OP_START => {
                let svc = crate::scm::open_service(
                    scm,
                    name,
                    crate::scm::ACCESS_START | crate::scm::ACCESS_QUERY,
                )?;
                crate::scm::start(&svc)?;
                if crate::scm::wait_for_state(&svc, crate::scm::STATE_RUNNING, 8000) {
                    Ok(format!("服务 {name} 已启动。"))
                } else {
                    Ok(format!("启动命令已发送，服务 {name} 正在启动中…"))
                }
            }
            OP_STOP => {
                let svc = crate::scm::open_service(
                    scm,
                    name,
                    crate::scm::ACCESS_STOP | crate::scm::ACCESS_QUERY,
                )?;
                let cur = crate::scm::query_status(&svc)?;
                if cur.dwCurrentState.0 == crate::scm::STATE_STOPPED {
                    return Ok(format!("服务 {name} 已是停止状态。"));
                }
                crate::scm::control(&svc, crate::scm::CONTROL_STOP)?;
                if crate::scm::wait_for_state(&svc, crate::scm::STATE_STOPPED, 35_000) {
                    Ok(format!("服务 {name} 已停止。"))
                } else {
                    Err(format!(
                        "服务 {name} 停止超时(35 秒)，停止序列可能仍在进行，请稍后刷新查看。"
                    ))
                }
            }
            OP_RESTART => {
                let svc = crate::scm::open_service(
                    scm,
                    name,
                    crate::scm::ACCESS_START | crate::scm::ACCESS_STOP | crate::scm::ACCESS_QUERY,
                )?;
                let cur = crate::scm::query_status(&svc)?;
                if cur.dwCurrentState.0 != crate::scm::STATE_STOPPED {
                    crate::scm::control(&svc, crate::scm::CONTROL_STOP)?;
                    if !crate::scm::wait_for_state(&svc, crate::scm::STATE_STOPPED, 35_000) {
                        return Err(format!("服务 {name} 停止超时，无法重启。"));
                    }
                }
                crate::scm::start(&svc)?;
                if crate::scm::wait_for_state(&svc, crate::scm::STATE_RUNNING, 8000) {
                    Ok(format!("服务 {name} 已重启。"))
                } else {
                    Ok(format!("服务 {name} 正在启动中…"))
                }
            }
            OP_PAUSE => {
                let svc = crate::scm::open_service(
                    scm,
                    name,
                    crate::scm::ACCESS_PAUSE | crate::scm::ACCESS_QUERY,
                )?;
                crate::scm::control(&svc, crate::scm::CONTROL_PAUSE)?;
                if crate::scm::wait_for_state(&svc, crate::scm::STATE_PAUSED, 15_000) {
                    Ok(format!("服务 {name} 已暂停（应用已停止，服务保持挂起）。"))
                } else {
                    Ok(format!("暂停命令已发送，服务 {name} 暂停中…"))
                }
            }
            OP_CONTINUE => {
                let svc = crate::scm::open_service(
                    scm,
                    name,
                    crate::scm::ACCESS_PAUSE | crate::scm::ACCESS_QUERY,
                )?;
                crate::scm::control(&svc, crate::scm::CONTROL_CONTINUE)?;
                if crate::scm::wait_for_state(&svc, crate::scm::STATE_RUNNING, 15_000) {
                    Ok(format!("服务 {name} 已恢复运行。"))
                } else {
                    Ok(format!("恢复命令已发送，服务 {name} 恢复中…"))
                }
            }
            OP_DELETE => {
                let svc = crate::scm::open_service(scm, name, crate::scm::ACCESS_ALL)?;
                if let Ok(cur) = crate::scm::query_status(&svc) {
                    if cur.dwCurrentState.0 != crate::scm::STATE_STOPPED {
                        let _ = crate::scm::control(&svc, crate::scm::CONTROL_STOP);
                        if !crate::scm::wait_for_state(&svc, crate::scm::STATE_STOPPED, 30_000) {
                            return Err(format!("服务 {name} 停止超时，未删除。可稍后重试。"));
                        }
                    }
                }
                crate::scm::delete(&svc)?;
                Ok(format!("服务 {name} 已删除。"))
            }
            _ => Err("未知操作".to_string()),
        }
    })();
    unsafe {
        let _ = windows::Win32::System::Services::CloseServiceHandle(scm);
    }
    res
}

fn op_done(st: &mut GuiState, wparam: WPARAM, lparam: LPARAM) {
    let op = wparam.0 as u32;
    let res = unsafe { Box::from_raw(lparam.0 as *mut Result<String, String>) };
    st.busy = false;
    unsafe {
        refresh_statuses(st);
        update_details(st);
    }
    match *res {
        Ok(_) => {
            if op == OP_DELETE {
                unsafe { rebuild_list(st) };
            }
        }
        Err(e) => msg_box(st.main, &e, "rssvc 操作失败", MB_OK | MB_ICONERROR),
    }
}

// ------------------------------------------------------ viewer / editor / toml --

fn open_logview(st: &mut GuiState) {
    let Some(entry) = selected_entry(st) else {
        return;
    };
    let Some(cfg) = entry.cfg.clone() else {
        msg_box(
            st.main,
            "无法读取该服务的注册表配置。",
            "rssvc",
            MB_OK | MB_ICONERROR,
        );
        return;
    };
    crate::log_view::open_log_viewer(st.main, &entry.name, &cfg);
}

fn open_editor(st: &mut GuiState) {
    let Some(entry) = selected_entry(st) else {
        return;
    };
    let Some(cfg) = entry.cfg.clone() else {
        msg_box(
            st.main,
            "无法读取该服务的注册表配置。",
            "rssvc",
            MB_OK | MB_ICONERROR,
        );
        return;
    };
    crate::edit_dlg::open_edit_dialog(st.main, entry.name, cfg);
}

fn export_toml(st: &mut GuiState) {
    let Some(entry) = selected_entry(st) else {
        return;
    };
    let Some(cfg) = entry.cfg.clone() else {
        msg_box(
            st.main,
            "无法读取该服务的注册表配置。",
            "rssvc",
            MB_OK | MB_ICONERROR,
        );
        return;
    };
    let default_name = format!("{}.toml", entry.name);
    let Some(path) = crate::ctl::pick_save(st.main, "导出服务配置为 TOML", &default_name)
    else {
        return;
    };
    let dump = match toml::to_string_pretty(&cfg) {
        Ok(s) => s,
        Err(e) => {
            msg_box(
                st.main,
                &format!("TOML 序列化失败: {e}"),
                "rssvc 导出失败",
                MB_OK | MB_ICONERROR,
            );
            return;
        }
    };
    match std::fs::write(&path, dump) {
        Ok(()) => msg_box(
            st.main,
            &format!(
                "配置已导出到:\n{path}\n\n可用 \"rssvc import\" 或 \"导入TOML\" 在其他机器还原。"
            ),
            "rssvc 导出",
            MB_OK | MB_ICONINFORMATION,
        ),
        Err(e) => msg_box(
            st.main,
            &format!("写入文件失败: {e}"),
            "rssvc 导出失败",
            MB_OK | MB_ICONERROR,
        ),
    }
}

fn import_toml(st: &mut GuiState) {
    let Some(path) = crate::ctl::pick_open(
        st.main,
        "选择要导入的 TOML 配置文件",
        crate::ctl::PickMode::Any,
    ) else {
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            msg_box(
                st.main,
                &format!("读取文件失败: {e}"),
                "rssvc 导入失败",
                MB_OK | MB_ICONERROR,
            );
            return;
        }
    };
    let cfg: Config = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            msg_box(
                st.main,
                &format!("TOML 解析失败: {e}\n\n文件: {path}"),
                "rssvc 导入失败",
                MB_OK | MB_ICONERROR,
            );
            return;
        }
    };
    if cfg.application.trim().is_empty() {
        msg_box(
            st.main,
            "TOML 中缺少 application 字段，无法安装服务。",
            "rssvc 导入失败",
            MB_OK | MB_ICONERROR,
        );
        return;
    }
    // Ask for a service name, then create the service (full config preserved).
    crate::install_dlg::prompt_name_and_install(st.main, cfg);
}

/// Trigger a background service operation from another dialog (e.g. the edit
/// dialog's "restart now" prompt). Marks the window busy like a button op.
pub fn request_op(main: HWND, name: String, op: u32) {
    let p = unsafe { GetWindowLongPtrW(main, GWLP_USERDATA) } as *mut GuiState;
    if p.is_null() {
        return;
    }
    let st = unsafe { &mut *p };
    if st.busy {
        return;
    }
    st.busy = true;
    unsafe {
        for (id, h) in &st.btns {
            if is_op_button(*id) {
                let _ = EnableWindow(*h, false);
            }
        }
    }
    spawn_op(main, name, op);
}

// ---------------------------------------------------------------- log view --

/// Open a file with the system default program (shared with the log viewer).
pub fn shell_open(parent: HWND, path: &str) -> bool {
    let w = util::to_wide(path);
    unsafe {
        let h = ShellExecuteW(
            Some(parent),
            w!("open"),
            PCWSTR::from_raw(w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        (h.0 as isize) > 32
    }
}

// ---------------------------------------------------------------- helpers --

pub fn msg_box(parent: HWND, text: &str, title: &str, flags: MESSAGEBOX_STYLE) {
    let t = util::to_wide(text);
    let cap = util::to_wide(title);
    unsafe {
        MessageBoxW(
            Some(parent),
            PCWSTR::from_raw(t.as_ptr()),
            PCWSTR::from_raw(cap.as_ptr()),
            flags,
        );
    }
}

unsafe fn lv_insert_column(list: HWND, idx: i32, width: i32, text: &str) {
    const LVCF_WIDTH: i32 = 0x0002;
    const LVCF_TEXT: i32 = 0x0004;
    const LVCF_SUBITEM: i32 = 0x0008;
    let mut t = util::to_wide(text);
    let mut col = LVCOLUMNW {
        mask: LVCOLUMNW_MASK((LVCF_WIDTH | LVCF_TEXT | LVCF_SUBITEM) as u32),
        cx: width,
        pszText: PWSTR::from_raw(t.as_mut_ptr()),
        cchTextMax: t.len() as i32,
        iSubItem: idx,
        ..Default::default()
    };
    SendMessageW(
        list,
        LVM_INSERTCOLUMN,
        Some(WPARAM(idx as usize)),
        Some(LPARAM(&mut col as *mut LVCOLUMNW as isize)),
    );
}

unsafe fn lv_insert_row(list: HWND, idx: i32, param: usize, text: &str) {
    let mut t = util::to_wide(text);
    let mut it = LVITEMW {
        mask: LIST_VIEW_ITEM_FLAGS(LVIF_TEXT.0 | LVIF_PARAM.0),
        iItem: idx,
        lParam: LPARAM(param as isize),
        pszText: PWSTR::from_raw(t.as_mut_ptr()),
        cchTextMax: t.len() as i32,
        ..Default::default()
    };
    SendMessageW(
        list,
        LVM_INSERTITEM,
        Some(WPARAM(0)),
        Some(LPARAM(&mut it as *mut LVITEMW as isize)),
    );
}

unsafe fn lv_set_text(list: HWND, row: i32, sub: i32, text: &str) {
    let mut t = util::to_wide(text);
    let mut it = LVITEMW {
        mask: LVIF_TEXT,
        iItem: row,
        iSubItem: sub,
        pszText: PWSTR::from_raw(t.as_mut_ptr()),
        cchTextMax: t.len() as i32,
        ..Default::default()
    };
    SendMessageW(
        list,
        LVM_SETITEMTEXT,
        Some(WPARAM(row as usize)),
        Some(LPARAM(&mut it as *mut LVITEMW as isize)),
    );
}

unsafe fn lv_select(list: HWND, idx: i32) {
    use windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS;
    const LVIS_SELECTED_VAL: u32 = 0x0002;
    let mut it = LVITEMW {
        mask: LIST_VIEW_ITEM_FLAGS(LVIF_STATE.0),
        state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED_VAL),
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED_VAL),
        iItem: idx,
        ..Default::default()
    };
    SendMessageW(
        list,
        LVM_SETITEMSTATE,
        Some(WPARAM(idx as usize)),
        Some(LPARAM(&mut it as *mut LVITEMW as isize)),
    );
}

// -------------------------------------------------------------- proc info --

fn proc_mem(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return "-".to_string();
        };
        let mut pmc = PROCESS_MEMORY_COUNTERS::default();
        let mem = GetProcessMemoryInfo(
            h,
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .map(|_| pmc.WorkingSetSize)
        .unwrap_or(0);
        let _ = CloseHandle(h);
        if mem == 0 {
            "-".to_string()
        } else {
            format!("{:.1} MB", mem as f64 / 1048576.0)
        }
    }
}

fn proc_uptime(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return "-".to_string();
        };
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return "-".to_string();
        }
        let now = GetSystemTimeAsFileTime();
        let f = |ft: &FILETIME| ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
        let secs = (f(&now) - f(&c)) / 10_000_000;
        if secs < 0 {
            return "-".to_string();
        }
        if secs < 60 {
            format!("{secs} 秒")
        } else if secs < 3600 {
            format!("{} 分 {} 秒", secs / 60, secs % 60)
        } else if secs < 86400 {
            format!("{} 时 {} 分", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{} 天 {} 时", secs / 86400, (secs % 86400) / 3600)
        }
    }
}

// ----------------------------------------------------------- message loop --

fn message_loop() {
    let mut msg = MSG::default();
    unsafe {
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
