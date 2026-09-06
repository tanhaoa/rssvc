//! Small helpers: UTF-16 conversion, local time formatting.

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// A handle wrapper that is Send + Sync. Win32 HANDLE values are plain
/// integers / pointers and are safe to move between threads as long as the
/// owner closes them exactly once; our usage (events, status handle, pipe
/// read ends) satisfies that.
pub struct SharedHandle(pub HANDLE);
unsafe impl Send for SharedHandle {}
unsafe impl Sync for SharedHandle {}

use windows::Win32::Foundation::HANDLE;

/// Encode a string as a NUL-terminated UTF-16 buffer.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Current local time (Win32 GetLocalTime).
pub fn now() -> SYSTEMTIME {
    unsafe { GetLocalTime() }
}

/// "20260906"
pub fn date_code(st: &SYSTEMTIME) -> (u16, u16, u16) {
    (st.wYear, st.wMonth, st.wDay)
}

/// "20260906_153045"
pub fn datetime_code(st: &SYSTEMTIME) -> String {
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// "2026-09-06 15:30:45"
pub fn log_ts(st: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// Quote a path for a command line, unless already quoted.
pub fn quote_path(p: &str) -> String {
    let p = p.trim();
    if p.starts_with('"') {
        p.to_string()
    } else {
        format!("\"{}\"", p)
    }
}

/// Last Win32 error as a human readable string.
pub fn last_error(e: &windows::core::Error) -> String {
    let code = e.code().0 as u32;
    let msg = e.message();
    format!("(0x{code:08X} / {code}) {msg}")
}

/// Common admin-right hint for access-denied style errors.
pub fn admin_hint(e: &windows::core::Error) -> &'static str {
    const ERROR_ACCESS_DENIED: u32 = 5;
    if e.code().0 as u32 == ERROR_ACCESS_DENIED {
        "\n提示: 请以管理员身份运行命令提示符 / PowerShell 后重试。"
    } else {
        ""
    }
}
