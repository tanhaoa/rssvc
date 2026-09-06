//! Shared Win32 control-building helpers for the rssvc GUI dialogs/forms.
//!
//! `Form` bundles a parent HWND, a default UI font and the list of created
//! child controls so that dialogs (install / edit / prompt / log viewer) can
//! declare controls tersely and read them back by ID.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FONT_CLIP_PRECISION,
    FONT_CHARSET, FONT_OUTPUT_PRECISION, FONT_QUALITY, FW_NORMAL, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, FOS_FORCEFILESYSTEM, FOS_FILEMUSTEXIST, FOS_OVERWRITEPROMPT,
    FOS_PICKFOLDERS, IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, GetWindowRect, SendMessageW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
    CBS_DROPDOWNLIST, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY,
    ES_WANTRETURN, WM_SETFONT, WM_SETTEXT,
};

use crate::util;

// --------------------------------------------------------------- font ----

/// The shared UI font ("Microsoft YaHei UI", 9pt-ish at default DPI).
pub fn ui_font() -> HFONT {
    unsafe {
        CreateFontW(
            -12, 0, 0, 0,
            FW_NORMAL.0 as i32, 0, 0, 0,
            FONT_CHARSET(DEFAULT_CHARSET.0),
            FONT_OUTPUT_PRECISION(OUT_DEFAULT_PRECIS.0),
            FONT_CLIP_PRECISION(CLIP_DEFAULT_PRECIS.0),
            FONT_QUALITY(CLEARTYPE_QUALITY.0),
            0,
            w!("Microsoft YaHei UI"),
        )
    }
}

/// Monospace font for log text ("Consolas").
pub fn mono_font() -> HFONT {
    unsafe {
        CreateFontW(
            -13, 0, 0, 0,
            FW_NORMAL.0 as i32, 0, 0, 0,
            FONT_CHARSET(DEFAULT_CHARSET.0),
            FONT_OUTPUT_PRECISION(OUT_DEFAULT_PRECIS.0),
            FONT_CLIP_PRECISION(CLIP_DEFAULT_PRECIS.0),
            FONT_QUALITY(CLEARTYPE_QUALITY.0),
            0,
            w!("Consolas"),
        )
    }
}

// --------------------------------------------------------------- form ----

/// A small builder over a parent window: creates labelled controls with a
/// common font and lets the dialog read them back by control ID.
pub struct Form {
    pub hwnd: HWND,
    pub ctrls: Vec<(u32, HWND)>,
    pub hfont: HFONT,
}

impl Form {
    pub fn new(hwnd: HWND) -> Form {
        Form {
            hwnd,
            ctrls: Vec::new(),
            hfont: ui_font(),
        }
    }

    /// Apply the shared UI font to an externally created control.
    pub fn apply_font(&self, h: HWND) {
        unsafe {
            SendMessageW(h, WM_SETFONT, Some(WPARAM(self.hfont.0 as usize)), Some(LPARAM(1)));
        }
    }

    fn push(&mut self, id: u32, h: HWND) {
        self.ctrls.push((id, h));
    }

    pub unsafe fn label(&mut self, hinstance: HINSTANCE, text: &str, x: i32, y: i32, w: i32) {
        let t = util::to_wide(text);
        if let Ok(h) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            PCWSTR::from_raw(t.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
            x, y, w, 16,
            Some(self.hwnd), None, Some(hinstance), None,
        ) {
            self.apply_font(h);
        }
    }

    /// Single-line edit with a sunken border.
    pub unsafe fn edit(&mut self, hinstance: HINSTANCE, id: u32, x: i32, y: i32, w: i32) {
        self.edit_ex(hinstance, id, x, y, w, 24, false);
    }

    /// Multiline edit; `readonly` adds ES_READONLY.
    pub unsafe fn edit_ml(
        &mut self,
        hinstance: HINSTANCE,
        id: u32,
        x: i32, y: i32, w: i32, h: i32,
        readonly: bool,
    ) {
        let ro = if readonly { ES_READONLY as u32 } else { 0 };
        if let Ok(h) = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0
                    | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32 | ro,
            ),
            x, y, w, h,
            Some(self.hwnd), Some(HMENU_ID(id)), Some(hinstance), None,
        ) {
            self.apply_font(h);
            self.push(id, h);
        }
    }

    fn edit_ex(
        &mut self,
        hinstance: HINSTANCE,
        id: u32,
        x: i32, y: i32, w: i32, h: i32,
        _multiline: bool,
    ) {
        unsafe {
            if let Ok(h) = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("EDIT"),
                w!(""),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32),
                x, y, w, h,
                Some(self.hwnd), Some(HMENU_ID(id)), Some(hinstance), None,
            ) {
                self.apply_font(h);
                self.push(id, h);
            }
        }
    }

    pub unsafe fn button(
        &mut self,
        hinstance: HINSTANCE,
        id: u32,
        text: &str,
        x: i32, y: i32, w: i32, h: i32,
        def: bool,
    ) {
        let t = util::to_wide(text);
        let bs = if def { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
        if let Ok(h) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR::from_raw(t.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | bs as u32),
            x, y, w, h,
            Some(self.hwnd), Some(HMENU_ID(id)), Some(hinstance), None,
        ) {
            self.apply_font(h);
            self.push(id, h);
        }
    }

    pub unsafe fn checkbox(
        &mut self,
        hinstance: HINSTANCE,
        id: u32,
        text: &str,
        x: i32, y: i32, w: i32,
    ) -> HWND {
        let t = util::to_wide(text);
        match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR::from_raw(t.as_ptr()),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32),
            x, y, w, 22,
            Some(self.hwnd), Some(HMENU_ID(id)), Some(hinstance), None,
        ) {
            Ok(h) => {
                self.apply_font(h);
                self.push(id, h);
                h
            }
            Err(_) => HWND::default(),
        }
    }

    /// Dropdown list combo (`items` pre-filled, `sel` pre-selected).
    ///
    /// NOTE: `CBS_DROPDOWNLIST` is mandatory. Without any CBS_* style Win32
    /// falls back to CBS_SIMPLE (list permanently expanded) which overlays
    /// every control placed below the combo.
    pub unsafe fn combo(
        &mut self,
        hinstance: HINSTANCE,
        id: u32,
        x: i32, y: i32, w: i32,
        items: &[&str],
        sel: i32,
    ) {
        if let Ok(h) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("COMBOBOX"),
            w!(""),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0
                    | CBS_DROPDOWNLIST as u32,
            ),
            x, y, w, 160,
            Some(self.hwnd), Some(HMENU_ID(id)), Some(hinstance), None,
        ) {
            self.apply_font(h);
            for it in items {
                let t = util::to_wide(it);
                SendMessageW(h, CB_ADDSTRING, Some(WPARAM(0)), Some(LPARAM(t.as_ptr() as isize)));
            }
            SendMessageW(h, CB_SETCURSEL, Some(WPARAM(sel.max(0) as usize)), Some(LPARAM(0)));
            self.push(id, h);
        }
    }

    // ---------------------------------------------------------- accessors --

    pub fn ctl(&self, id: u32) -> HWND {
        self.ctrls
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, h)| *h)
            .unwrap_or_default()
    }

    pub fn text(&self, id: u32) -> String {
        let h = self.ctl(id);
        if h.is_invalid() {
            return String::new();
        }
        unsafe {
            let mut buf = [0u16; 4096];
            let n = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(h, &mut buf);
            let n = if n < 0 { 0 } else { n as usize };
            String::from_utf16_lossy(&buf[..n]).trim().to_string()
        }
    }

    pub fn set_text(&self, id: u32, s: &str) {
        let h = self.ctl(id);
        if h.is_invalid() {
            return;
        }
        let t = util::to_wide(s);
        unsafe {
            SendMessageW(h, WM_SETTEXT, Some(WPARAM(0)), Some(LPARAM(t.as_ptr() as isize)));
        }
    }

    pub fn combo_sel(&self, id: u32, default: i32) -> i32 {
        let h = self.ctl(id);
        if h.is_invalid() {
            return default;
        }
        let r = unsafe { SendMessageW(h, CB_GETCURSEL, None, None).0 };
        if r < 0 {
            default
        } else {
            r as i32
        }
    }

    pub fn set_combo_sel(&self, id: u32, idx: i32) {
        let h = self.ctl(id);
        if h.is_invalid() {
            return;
        }
        unsafe {
            SendMessageW(h, CB_SETCURSEL, Some(WPARAM(idx.max(0) as usize)), Some(LPARAM(0)));
        }
    }
}

/// Control ID as HMENU (Win32 passes control IDs through this field).
#[inline]
#[allow(non_snake_case)]
pub fn HMENU_ID(id: u32) -> windows::Win32::UI::WindowsAndMessaging::HMENU {
    windows::Win32::UI::WindowsAndMessaging::HMENU(id as usize as *mut core::ffi::c_void)
}

// -------------------------------------------------------------- layout ----

/// Convert a desired **client area** size into the outer window size for a
/// `WS_CAPTION | WS_SYSMENU` dialog. `CreateWindowExW` takes the *outer*
/// size; passing the client size directly crops the bottom of the form
/// (title bar + borders eat roughly 40 px).
pub fn outer_size_for_client(cw: i32, ch: i32) -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
    unsafe {
        let _ = AdjustWindowRectEx(
            &mut r,
            WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0),
            false,
            WINDOW_EX_STYLE(0),
        );
    }
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    (w.max(cw), h.max(ch))
}

/// Center a `w x h` window rectangle on `owner`.
pub fn center_on(owner: HWND, w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(owner, &mut r).is_err() {
            return (CW_USEDEFAULT, 0);
        }
        let cx = (r.left + r.right) / 2;
        let cy = (r.top + r.bottom) / 2;
        ((cx - w / 2).max(0), (cy - h / 2).max(0))
    }
}

// -------------------------------------------------------------- pickers ----

/// What kind of item a file-open dialog should accept.
pub enum PickMode {
    /// Executable files (must exist).
    Exe,
    /// Folders only.
    Folder,
    /// Any existing file.
    Any,
}

/// Common `IFileOpenDialog` flow; returns the selected file-system path.
pub fn pick_open(owner: HWND, title: &str, mode: PickMode) -> Option<String> {
    unsafe {
        let dlg: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg
            .GetOptions()
            .unwrap_or(windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS(0))
            .0;
        let extra: u32 = match mode {
            PickMode::Exe => FOS_FILEMUSTEXIST.0 | FOS_FORCEFILESYSTEM.0,
            PickMode::Folder => FOS_PICKFOLDERS.0 | FOS_FORCEFILESYSTEM.0,
            PickMode::Any => FOS_FILEMUSTEXIST.0 | FOS_FORCEFILESYSTEM.0,
        };
        let _ = dlg.SetOptions(windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS(opts | extra));
        let t = util::to_wide(title);
        let _ = dlg.SetTitle(PCWSTR::from_raw(t.as_ptr()));
        if dlg.Show(Some(owner)).is_err() {
            return None; // user cancelled
        }
        let item = dlg.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const core::ffi::c_void));
        s
    }
}

/// Common `IFileSaveDialog` flow with a default file name suggestion.
pub fn pick_save(owner: HWND, title: &str, default_name: &str) -> Option<String> {
    unsafe {
        let dlg: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg
            .GetOptions()
            .unwrap_or(windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS(0))
            .0;
        let _ = dlg.SetOptions(windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS(
            opts | FOS_OVERWRITEPROMPT.0 | FOS_FORCEFILESYSTEM.0,
        ));
        let t = util::to_wide(title);
        let _ = dlg.SetTitle(PCWSTR::from_raw(t.as_ptr()));
        if !default_name.trim().is_empty() {
            let n = util::to_wide(default_name.trim());
            let _ = dlg.SetFileName(PCWSTR::from_raw(n.as_ptr()));
        }
        if dlg.Show(Some(owner)).is_err() {
            return None; // user cancelled
        }
        let item = dlg.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const core::ffi::c_void));
        s
    }
}

// -------------------------------------------------------------- parsing --

/// Parse a multi-line KEY=VALUE environment text. Blank lines and lines
/// starting with `#` are ignored. Returns an error message on malformed lines.
pub fn parse_env_text(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in s.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let key = l.split('=').next().unwrap_or("").trim();
        if !l.contains('=') || key.is_empty() {
            return Err(format!("环境变量格式错误（应为 KEY=VALUE）: {l}"));
        }
        out.push(l.to_string());
    }
    Ok(out)
}

/// Set the text of an arbitrary HWND (helper for non-Form controls).
pub unsafe fn set_window_text(h: HWND, s: &str) {
    let t = util::to_wide(s);
    SendMessageW(h, WM_SETTEXT, Some(WPARAM(0)), Some(LPARAM(t.as_ptr() as isize)));
}
