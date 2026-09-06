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

/// Quote a single command-line argument following the Windows argv rules:
/// arguments containing whitespace (or quotes) are wrapped in double quotes,
/// embedded quotes are escaped as `\"` and backslash runs preceding a quote
/// are doubled. Short arguments without whitespace stay untouched.
///
/// This is the inverse of what `CommandLineToArgvW` (the CRT argv parser)
/// does, so `args.iter().map(quote_arg).join(" ")` round-trips a parsed
/// command line back into an equivalent raw command line.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.chars().any(|c| c.is_whitespace() || c == '"') {
        return a.to_string();
    }
    let mut out = String::with_capacity(a.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Double the backslashes that precede the quote, then escape
                // the quote itself.
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote must be doubled as well.
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

/// Canonicalize a path and strip the `\\?\` verbatim prefix that Rust adds on
/// Windows. Verbatim paths are technically valid for most Win32 file APIs but
/// break cmd.exe, some programs that inspect their own path, and look ugly in
/// logs / the GUI / exported TOML. `\\?\UNC\server\share` maps back to
/// `\\server\share`.
pub fn canonicalize_plain(p: &str) -> String {
    match std::fs::canonicalize(p) {
        Ok(path) => strip_verbatim(&path.to_string_lossy()),
        Err(_) => p.to_string(),
    }
}

/// Strip a verbatim prefix from an already-rendered path string.
fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    s.to_string()
}

/// Validate a service name chosen by the user. The service name becomes a
/// registry key under `HKLM\...\Services`, so the classic forbidden
/// characters apply; we additionally reject whitespace and control
/// characters so the ImagePath argument parsing stays unambiguous.
pub fn validate_service_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("服务名不能为空。".into());
    }
    if name != name.trim() {
        return Err("服务名前后不能包含空白字符。".into());
    }
    const BAD: &str = "\\/:*?\"<>|";
    for c in name.chars() {
        if BAD.contains(c) {
            return Err(format!(
                "服务名不能包含 \\ / : * ? \" < > | 等字符 (非法字符: {c})。"
            ));
        }
        if c.is_whitespace() {
            return Err("服务名不能包含空白字符 (空格 / 制表符等)。".into());
        }
        if (c as u32) < 0x20 {
            return Err("服务名不能包含控制字符。".into());
        }
    }
    if name.len() > 256 {
        return Err("服务名过长 (最多 256 字节)。".into());
    }
    Ok(())
}

/// Read a NUL-terminated UTF-16 string from a raw PWSTR pointer.
/// Used to recover the service name from the ServiceMain argv (SCM passes
/// the service name as argv[0]).
pub fn pwstr_to_string(p: *mut u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(p, len);
        String::from_utf16_lossy(slice)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_arg_leaves_plain_tokens_untouched() {
        assert_eq!(quote_arg("--message"), "--message");
        assert_eq!(quote_arg("hello"), "hello");
        assert_eq!(quote_arg("a=b;c"), "a=b;c");
    }

    #[test]
    fn quote_arg_wraps_whitespace() {
        assert_eq!(quote_arg("hello world"), "\"hello world\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(" path\\to dir "), "\" path\\to dir \"");
    }

    #[test]
    fn quote_arg_escapes_quotes_and_backslashes() {
        // The canonical CommandLineToArgvW round-trip examples.
        assert_eq!(quote_arg(r#"he said \"hi\""#), r#""he said \\\"hi\\\"""#);
        // Trailing backslash without whitespace/quote stays untouched.
        assert_eq!(quote_arg(r"dir\"), r"dir\");
        assert_eq!(quote_arg(r"a\\b"), r"a\\b");
        // ...but a trailing backslash inside a quoted arg is doubled.
        assert_eq!(quote_arg(r"trail \"), r#""trail \\""#);
        assert_eq!(quote_arg("say \"ok\" now"), r#""say \"ok\" now""#);
    }

    #[test]
    fn quote_path_always_wraps_unquoted() {
        assert_eq!(quote_path("C:\\a b\\x.exe"), "\"C:\\a b\\x.exe\"");
        assert_eq!(quote_path("\"already\""), "\"already\"");
    }

    #[test]
    fn strip_verbatim_prefixes() {
        // exercised through the public fallback path (canonicalize fails on
        // most hosts for these fake paths, so canonicalize_plain returns the
        // input unchanged); test strip_verbatim directly.
        assert_eq!(super::strip_verbatim(r"\\?\C:\app\x.exe"), r"C:\app\x.exe");
        assert_eq!(
            super::strip_verbatim(r"\\?\UNC\srv\share\a.txt"),
            r"\\srv\share\a.txt"
        );
        assert_eq!(
            super::strip_verbatim(r"C:\plain\path.exe"),
            r"C:\plain\path.exe"
        );
    }

    #[test]
    fn canonicalize_plain_falls_back_on_error() {
        assert_eq!(
            canonicalize_plain(r"Z:\definitely\not\here\x.exe"),
            r"Z:\definitely\not\here\x.exe"
        );
    }

    #[test]
    fn service_name_validation() {
        assert!(validate_service_name("my-app").is_ok());
        assert!(validate_service_name("My.Service_1").is_ok());
        assert!(validate_service_name("").is_err());
        assert!(validate_service_name(" spaced name").is_err());
        assert!(validate_service_name("with space").is_err());
        assert!(validate_service_name("with\ttab").is_err());
        assert!(validate_service_name("quo\"te").is_err());
        assert!(validate_service_name("sl/ash").is_err());
        assert!(validate_service_name("back\\slash").is_err());
        assert!(validate_service_name("star*").is_err());
    }

    #[test]
    fn pwstr_to_string_roundtrip() {
        let s = "rssvc svc name";
        let mut wide = to_wide(s);
        let p = wide.as_mut_ptr();
        assert_eq!(pwstr_to_string(p), s);
        assert_eq!(pwstr_to_string(std::ptr::null_mut()), "");
    }
}
