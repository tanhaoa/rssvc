//! 实时日志尾随窗口（`tail -f` 的 Win32 原生实现）。
//!
//! - 双流：服务配置了 stdout / stderr 时可下拉切换，各自独立记录读取位置
//! - 尾随：定时器（600ms）从上次偏移增量读取新内容追加到只读 EDIT 控件
//! - 首次打开只载入文件末尾 64KB，避免一次性灌入整个历史日志
//! - 轮转感知：文件长度小于已读偏移（被改名重建）时自动从头重新尾随；
//!   单次增长超过 1MB 时直接跳到末尾前 256KB，防止 UI 被灌爆
//! - 显示缓冲上限 280KB（超出从头裁剪，按 UTF-8 字符边界对齐）
//! - "暂停"复选框冻结读取；"外部打开"用系统默认程序打开完整日志文件
//!
//! 该窗口为主窗口线程上的无模态窗口（共用消息循环），不调用 PostQuitMessage。

use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, EM_REPLACESEL, EM_SCROLLCARET, EM_SETLIMITTEXT, EM_SETSEL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer, LoadCursorW,
    MoveWindow, RegisterClassExW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    ShowWindow, BM_GETCHECK, CBN_SELCHANGE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_SETCURSEL, GWLP_USERDATA, IDC_ARROW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_GETTEXTLENGTH, WM_SETFONT,
    WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};

use crate::config::Config;
use crate::ctl::{self, Form, HMENU_ID};
use crate::gui::{msg_box, shell_open};
use crate::util;

const ID_FILE: u32 = 100; // stream combo (CBN_SELCHANGE)
const ID_PAUSE: u32 = 101; // 暂停 checkbox
const ID_OPEN: u32 = 102; // 外部打开 button

const MAX_TEXT: usize = 280 * 1024; // display buffer cap (bytes of UTF-8)
const INITIAL_TAIL: u64 = 64 * 1024; // first-open window
const JUMP_THRESHOLD: u64 = 1024 * 1024; // snap-to-tail threshold
const JUMP_TAIL: u64 = 256 * 1024;

const VIEW_W: i32 = 880; // desired CLIENT area width
const VIEW_H: i32 = 580; // desired CLIENT area height

struct Stream {
    path: PathBuf,
    file: Option<std::fs::File>,
    offset: u64,
    pending: Vec<u8>, // partial UTF-8 sequence carry-over
    text: String,     // display buffer for this stream
    hinted: bool,     // "waiting for file" placeholder shown
}

struct LogView {
    owner: HWND,
    form: Form,
    combo: HWND,
    chk: HWND,
    edit: HWND,
    streams: Vec<Stream>,
    cur: usize,
}

enum Upd {
    None,
    Reset,
    Append(String),
}

// ------------------------------------------------------------- entry ----

/// Open the realtime log viewer for a service.
pub fn open_log_viewer(owner: HWND, name: &str, cfg: &Config) {
    let mut streams: Vec<Stream> = Vec::new();
    let out = cfg.stdout.trim().to_string();
    let err = cfg.stderr.trim().to_string();
    if !out.is_empty() {
        streams.push(new_stream(&out));
    }
    if !err.is_empty() && !err.eq_ignore_ascii_case(&out) {
        streams.push(new_stream(&err));
    }
    if streams.is_empty() {
        msg_box(
            owner,
            &format!(
                "服务 {name} 未配置日志文件。\n提示: 可用 \"编辑配置\" 为服务设置 stdout / stderr 日志路径。"
            ),
            "rssvc",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }

    unsafe {
        let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else {
            return;
        };
        let hinstance = HINSTANCE(hmodule.0);
        register_class(hinstance);

        let (outer_w, outer_h) = ctl::outer_size_for_client(VIEW_W, VIEW_H);
        let (x, y) = ctl::center_on(owner, outer_w, outer_h);
        let title = windows::core::HSTRING::from(format!("实时日志 - {name} - rssvc"));
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("rssvcLogWnd"),
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

        let mut lv = Box::new(LogView {
            owner,
            form: Form::new(hwnd),
            combo: HWND::default(),
            chk: HWND::default(),
            edit: HWND::default(),
            streams,
            cur: 0,
        });
        create_controls(&mut lv, hinstance);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(lv) as isize);
        let _ = SetTimer(Some(hwnd), 1, 600, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn new_stream(path: &str) -> Stream {
    Stream {
        path: PathBuf::from(path),
        file: None,
        offset: 0,
        pending: Vec::new(),
        text: String::new(),
        hinted: false,
    }
}

fn register_class(hinstance: HINSTANCE) {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(log_wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
                (windows::Win32::Graphics::Gdi::COLOR_WINDOW.0 + 1) as *mut core::ffi::c_void,
            ),
            lpszClassName: w!("rssvcLogWnd"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    });
}

unsafe fn create_controls(lv: &mut LogView, hinstance: HINSTANCE) {
    let f = &mut lv.form;

    f.label(hinstance, "文件:", 12, 12, 40);
    lv.combo = match CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("COMBOBOX"),
        w!(""),
        // CBS_DROPDOWNLIST is mandatory: without it Win32 falls back to
        // CBS_SIMPLE (list always expanded) which covers the log area.
        WINDOW_STYLE(
            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
        ),
        56,
        8,
        560,
        200,
        Some(f.hwnd),
        Some(HMENU_ID(ID_FILE)),
        Some(hinstance),
        None,
    ) {
        Ok(h) => {
            f.apply_font(h);
            for (i, s) in lv.streams.iter().enumerate() {
                let label = if i == 0 { "stdout" } else { "stderr" };
                let item = format!("{label} — {}", s.path.display());
                let t = util::to_wide(&item);
                SendMessageW(
                    h,
                    CB_ADDSTRING,
                    Some(WPARAM(0)),
                    Some(LPARAM(t.as_ptr() as isize)),
                );
            }
            SendMessageW(h, CB_SETCURSEL, Some(WPARAM(0)), Some(LPARAM(0)));
            h
        }
        Err(_) => HWND::default(),
    };

    lv.chk = f.checkbox(hinstance, ID_PAUSE, "暂停", 640, 9, 64);
    f.button(hinstance, ID_OPEN, "外部打开", 716, 6, 90, 26, false);

    // Read-only log text with monospace font.
    let mono = ctl::mono_font();
    let edit = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w!("EDIT"),
        w!(""),
        WINDOW_STYLE(
            WS_CHILD.0
                | WS_VISIBLE.0
                | WS_TABSTOP.0
                | WS_VSCROLL.0
                | windows::Win32::UI::WindowsAndMessaging::ES_MULTILINE as u32
                | windows::Win32::UI::WindowsAndMessaging::ES_AUTOVSCROLL as u32
                | windows::Win32::UI::WindowsAndMessaging::ES_READONLY as u32,
        ),
        12,
        40,
        VIEW_W - 24,
        VIEW_H - 120,
        Some(f.hwnd),
        None,
        Some(hinstance),
        None,
    );
    lv.edit = edit.unwrap_or_default();
    SendMessageW(
        lv.edit,
        WM_SETFONT,
        Some(WPARAM(mono.0 as usize)),
        Some(LPARAM(1)),
    );
    SendMessageW(
        lv.edit,
        EM_SETLIMITTEXT,
        Some(WPARAM(0x7FFFFFFE)),
        Some(LPARAM(0)),
    );
}

// ------------------------------------------------------------ reading ----

fn decode(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    // Cut at the last UTF-8 char boundary (walk back over continuation bytes
    // 0b10xxxxxx) so multi-byte characters are not split.
    let mut k = pending.len();
    while k > 0 && (pending[k - 1] & 0xC0) == 0x80 {
        k -= 1;
    }
    let out = String::from_utf8_lossy(&pending[..k]).into_owned();
    pending.drain(..k);
    if pending.len() >= 4 {
        // A broken sequence is stuck at the head; flush it so we never stall.
        let junk = String::from_utf8_lossy(pending).into_owned();
        pending.clear();
        return format!("{out}{junk}");
    }
    out
}

fn trim_tail(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut cut = s.len() - max;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    s.drain(..cut);
}

fn read_more(s: &mut Stream) -> Upd {
    use std::io::{Read, Seek, SeekFrom};

    // (Re)open the file when needed.
    if s.file.is_none() {
        match std::fs::File::open(&s.path) {
            Ok(f) => {
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                s.offset = len.saturating_sub(INITIAL_TAIL);
                s.file = Some(f);
                s.hinted = false;
            }
            Err(_) => return Upd::None, // file may not exist yet
        }
    }

    let f = s.file.as_mut().unwrap();
    let len = match f.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Upd::None,
    };

    let mut reset = false;
    if len < s.offset {
        // Rotated / truncated: start following the fresh file from zero.
        match std::fs::File::open(&s.path) {
            Ok(nf) => *f = nf,
            Err(_) => {
                s.file = None;
                return Upd::None;
            }
        }
        s.offset = 0;
        s.text.clear();
        s.pending.clear();
        reset = true;
    } else if len - s.offset > JUMP_THRESHOLD {
        // Huge growth in one tick: snap near the tail instead of dumping it all.
        s.offset = len.saturating_sub(JUMP_TAIL);
        s.text.clear();
        s.pending.clear();
        reset = true;
    }

    if f.seek(SeekFrom::Start(s.offset)).is_err() {
        return Upd::None;
    }
    let mut chunk: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                chunk.extend_from_slice(&buf[..n]);
                if chunk.len() > 512 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if chunk.is_empty() {
        return if reset { Upd::Reset } else { Upd::None };
    }
    s.offset += chunk.len() as u64;
    let piece = decode(&mut s.pending, &chunk);
    s.text.push_str(&piece);
    trim_tail(&mut s.text, MAX_TEXT);
    if reset {
        Upd::Reset
    } else {
        Upd::Append(piece)
    }
}

// -------------------------------------------------------------- paint ----

unsafe fn scroll_end(edit: HWND) {
    let len = SendMessageW(edit, WM_GETTEXTLENGTH, None, None).0;
    SendMessageW(
        edit,
        EM_SETSEL,
        Some(WPARAM(len as usize)),
        Some(LPARAM(len as isize)),
    );
    SendMessageW(edit, EM_SCROLLCARET, None, None);
}

unsafe fn apply(lv: &mut LogView, upd: Upd) {
    match upd {
        Upd::None => {}
        Upd::Reset => {
            let text = lv.streams[lv.cur].text.clone();
            ctl::set_window_text(lv.edit, &text);
            scroll_end(lv.edit);
        }
        Upd::Append(piece) => {
            let w = util::to_wide(&piece);
            let len = SendMessageW(lv.edit, WM_GETTEXTLENGTH, None, None).0;
            SendMessageW(
                lv.edit,
                EM_SETSEL,
                Some(WPARAM(len as usize)),
                Some(LPARAM(len as isize)),
            );
            SendMessageW(
                lv.edit,
                EM_REPLACESEL,
                Some(WPARAM(1)),
                Some(LPARAM(w.as_ptr() as isize)),
            );
            SendMessageW(lv.edit, EM_SCROLLCARET, None, None);
        }
    }
}

unsafe fn show_placeholder(lv: &mut LogView) {
    let needs = lv.streams[lv.cur].file.is_none() && !lv.streams[lv.cur].hinted;
    if !needs {
        return;
    }
    let path = lv.streams[lv.cur].path.display().to_string();
    lv.streams[lv.cur].hinted = true;
    ctl::set_window_text(lv.edit, &format!("(等待日志文件出现: {path})"));
}

unsafe fn tick(lv: &mut LogView) {
    let checked = SendMessageW(lv.chk, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0;
    if checked == BST_CHECKED.0 as isize {
        return; // 暂停
    }
    show_placeholder(lv);
    let upd = read_more(&mut lv.streams[lv.cur]);
    apply(lv, upd);
}

// ------------------------------------------------------------- wndproc ----

unsafe extern "system" fn log_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LogView;
    if p.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let lv = &mut *p;
    match msg {
        WM_TIMER => {
            tick(lv);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xffff) as u32;
            let code = ((wparam.0 >> 16) & 0xffff) as u32;
            match id {
                ID_FILE if code == CBN_SELCHANGE => {
                    let sel = SendMessageW(lv.combo, CB_GETCURSEL, None, None).0;
                    if sel >= 0 {
                        lv.cur = sel as usize;
                        let text = lv.streams[lv.cur].text.clone();
                        ctl::set_window_text(lv.edit, &text);
                        scroll_end(lv.edit);
                    }
                    LRESULT(0)
                }
                ID_PAUSE => LRESULT(0),
                ID_OPEN => {
                    let path = lv.streams[lv.cur].path.display().to_string();
                    if !shell_open(lv.owner, &path) {
                        msg_box(
                            lv.owner,
                            &format!("无法打开文件: {path}"),
                            "rssvc",
                            MB_OK | MB_ICONERROR,
                        );
                    }
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_SIZE => {
            let cw = (lparam.0 & 0xffff) as i32;
            let ch = ((lparam.0 >> 16) & 0xffff) as i32;
            // Guard both dimensions: extreme shrink must not compute a
            // negative width/height for MoveWindow.
            if cw > 40 && ch > 60 {
                let _ = MoveWindow(lv.edit, 12, 40, cw - 24, ch - 52, true);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), 1);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(p));
            // NOTE: deliberately NOT PostQuitMessage - the owner window's
            // message loop must keep running.
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
