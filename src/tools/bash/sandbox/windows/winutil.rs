//! String conversion and command-line quoting helpers for the Windows
//! sandbox spawn path.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_ALLOCATE_BUFFER;
use windows_sys::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_FROM_SYSTEM;
use windows_sys::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_IGNORE_INSERTS;
use windows_sys::Win32::System::Diagnostics::Debug::FormatMessageW;

/// Encode an `OsStr` as a null-terminated UTF-16 buffer that the Win32
/// `*W` APIs expect.
pub fn to_wide<S: AsRef<OsStr>>(s: S) -> Vec<u16> {
    let mut v: Vec<u16> = s.as_ref().encode_wide().collect();
    v.push(0);
    v
}

/// Quote one Windows command-line argument so that spaces, quotes, and
/// backslashes round-trip through the spawn API, following the rules
/// `CommandLineToArgvW` and the C runtime use. Matches what
/// `std::process::Command` does on Windows.
pub fn quote_windows_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '"'));
    if !needs_quotes {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
            }
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                if backslashes > 0 {
                    quoted.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                }
                quoted.push(ch);
            }
        }
    }
    if backslashes > 0 {
        quoted.push_str(&"\\".repeat(backslashes * 2));
    }
    quoted.push('"');
    quoted
}

/// Build a command-line string from an argument vector, quoting each
/// entry independently.
pub fn argv_to_command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote_windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Human-readable description for a Win32 error code.
pub fn format_last_error(err: i32) -> String {
    unsafe {
        let mut buf_ptr: *mut u16 = std::ptr::null_mut();
        let flags = FORMAT_MESSAGE_ALLOCATE_BUFFER
            | FORMAT_MESSAGE_FROM_SYSTEM
            | FORMAT_MESSAGE_IGNORE_INSERTS;
        let len = FormatMessageW(
            flags,
            std::ptr::null(),
            err as u32,
            0,
            (&mut buf_ptr as *mut *mut u16) as *mut u16,
            0,
            std::ptr::null_mut(),
        );
        if len == 0 || buf_ptr.is_null() {
            return format!("Win32 error {err}");
        }
        let slice = std::slice::from_raw_parts(buf_ptr, len as usize);
        let mut s = String::from_utf16_lossy(slice);
        s = s.trim().to_string();
        LocalFree(buf_ptr as HLOCAL);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::argv_to_command_line;

    #[test]
    fn argv_to_command_line_quotes_each_argument_independently() {
        let argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoProfile -EncodedCommand abc=="
                .to_string(),
        ];

        assert_eq!(
            argv_to_command_line(&argv),
            "cmd.exe /c \"\\\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\\\" -NoProfile -EncodedCommand abc==\""
        );
    }
}
