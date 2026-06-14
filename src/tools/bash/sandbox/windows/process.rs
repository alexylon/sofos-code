//! Process spawn for the Windows workspace sandbox.
//!
//! Derived from the codex Windows sandbox `process` module, narrowed
//! to a single shape: spawn `<shell> -c <command>` under the supplied
//! restricted token with the parent's stdio pipes wired in. The
//! desktop-isolation and pseudo-console paths from codex are not
//! ported; sofos does not need an interactive TTY here.

use super::proc_thread_attr::ProcThreadAttributeList;
use super::winutil::argv_to_command_line;
use super::winutil::format_last_error;
use super::winutil::to_wide;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
use windows_sys::Win32::Foundation::SetHandleInformation;
use windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;
use windows_sys::Win32::System::Threading::CreateProcessAsUserW;
use windows_sys::Win32::System::Threading::EXTENDED_STARTUPINFO_PRESENT;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::STARTF_USESTDHANDLES;
use windows_sys::Win32::System::Threading::STARTUPINFOEXW;

/// Pack the environment map into the null-separated, double-null-
/// terminated UTF-16 block the spawn API expects.
pub fn make_env_block(env: &HashMap<String, String>) -> Vec<u16> {
    let mut items: Vec<(String, String)> =
        env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    items.sort_by(|a, b| {
        a.0.to_uppercase()
            .cmp(&b.0.to_uppercase())
            .then(a.0.cmp(&b.0))
    });
    let mut w: Vec<u16> = Vec::new();
    for (k, v) in items {
        let mut s = to_wide(format!("{k}={v}"));
        s.pop();
        w.extend_from_slice(&s);
        w.push(0);
    }
    w.push(0);
    w
}

/// Spawn `argv[0] argv[1..]` under `h_token`, wiring stdio to the
/// supplied pipe handles. The caller owns the returned process and
/// thread handles.
///
/// # Safety
/// `h_token` must be a valid primary token, the stdio handles must be
/// inheritable, and `argv` / `cwd` / `env_map` must remain valid for
/// the duration of the call.
pub unsafe fn create_process_as_user(
    h_token: HANDLE,
    argv: &[String],
    cwd: &Path,
    env_map: &HashMap<String, String>,
    stdio: (HANDLE, HANDLE, HANDLE),
) -> io::Result<PROCESS_INFORMATION> {
    let cmdline_str = argv_to_command_line(argv);
    let mut cmdline: Vec<u16> = to_wide(&cmdline_str);
    let env_block = make_env_block(env_map);
    let cwd_wide = to_wide(cwd);

    let (stdin_h, stdout_h, stderr_h) = stdio;

    let mut si: STARTUPINFOEXW = std::mem::zeroed();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    // Pinning the desktop avoids `STATUS_DLL_INIT_FAILED` in some
    // restricted-token edge cases observed by codex.
    si.StartupInfo.lpDesktop = std::ptr::null_mut();
    si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = stdin_h;
    si.StartupInfo.hStdOutput = stdout_h;
    si.StartupInfo.hStdError = stderr_h;

    let mut inherited_handles = vec![stdin_h, stdout_h];
    if !inherited_handles.contains(&stderr_h) {
        inherited_handles.push(stderr_h);
    }
    for &handle in &inherited_handles {
        if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
            return Err(io::Error::other(format!(
                "SetHandleInformation failed for stdio handle: {}",
                GetLastError()
            )));
        }
    }
    let mut attrs = ProcThreadAttributeList::new(1)?;
    attrs.set_handle_list(inherited_handles)?;
    si.lpAttributeList = attrs.as_mut_ptr();

    let creation_flags = CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let ok = CreateProcessAsUserW(
        h_token,
        std::ptr::null(),
        cmdline.as_mut_ptr(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        1,
        creation_flags,
        env_block.as_ptr() as *mut c_void,
        cwd_wide.as_ptr(),
        &si.StartupInfo,
        &mut pi,
    );
    if ok == 0 {
        let err = GetLastError() as i32;
        return Err(io::Error::other(format!(
            "CreateProcessAsUserW failed: {} ({}) | cwd={} | cmd={}",
            err,
            format_last_error(err),
            cwd.display(),
            cmdline_str
        )));
    }
    Ok(pi)
}
