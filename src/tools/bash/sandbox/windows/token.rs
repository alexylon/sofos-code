//! Restricted access token construction for the workspace sandbox.
//!
//! Derived from the codex Windows sandbox `token` module, narrowed to
//! the single path sofos uses: take the current process token, return
//! a restricted copy whose restricting set names the workspace
//! identifier. The kernel then allows a write only when both the user
//! token and the restricting set are permitted on the target.

use super::winutil::to_wide;
use std::ffi::c_void;
use std::io;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LUID;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::AdjustTokenPrivileges;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GRANT_ACCESS;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::CopySid;
use windows_sys::Win32::Security::CreateRestrictedToken;
use windows_sys::Win32::Security::CreateWellKnownSid;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::LookupPrivilegeValueW;
use windows_sys::Win32::Security::SID_AND_ATTRIBUTES;
use windows_sys::Win32::Security::SetTokenInformation;
use windows_sys::Win32::Security::TOKEN_ADJUST_DEFAULT;
use windows_sys::Win32::Security::TOKEN_ADJUST_PRIVILEGES;
use windows_sys::Win32::Security::TOKEN_ADJUST_SESSIONID;
use windows_sys::Win32::Security::TOKEN_ASSIGN_PRIMARY;
use windows_sys::Win32::Security::TOKEN_DUPLICATE;
use windows_sys::Win32::Security::TOKEN_PRIVILEGES;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Security::TokenDefaultDacl;
use windows_sys::Win32::Security::TokenGroups;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

const DISABLE_MAX_PRIVILEGE: u32 = 0x01;
const LUA_TOKEN: u32 = 0x04;
const WRITE_RESTRICTED: u32 = 0x08;
const GENERIC_ALL: u32 = 0x1000_0000;
const WIN_WORLD_SID: i32 = 1;
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
const SE_PRIVILEGE_ENABLED: u32 = 0x0000_0002;

// Parameter names match the Win32 SDK so the FFI declarations stay
// readable next to the API documentation.
#[allow(non_snake_case)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSidToSidW(StringSid: *const u16, Sid: *mut *mut c_void) -> i32;
    fn OpenProcessToken(ProcessHandle: HANDLE, DesiredAccess: u32, TokenHandle: *mut HANDLE)
    -> i32;
}

/// Owns a security identifier allocated by `ConvertStringSidToSidW`
/// and releases it with `LocalFree` when dropped.
pub struct LocalSid {
    psid: *mut c_void,
}

impl LocalSid {
    pub fn from_string(sid: &str) -> io::Result<Self> {
        let wide = to_wide(sid);
        let mut psid: *mut c_void = std::ptr::null_mut();
        let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut psid) };
        if ok == 0 {
            return Err(io::Error::other(format!("invalid SID string: {sid}")));
        }
        Ok(Self { psid })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.psid
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.psid.is_null() {
            unsafe {
                LocalFree(self.psid as HLOCAL);
            }
        }
    }
}

/// Open the current process token with the access mask required to
/// derive a restricted version.
///
/// # Safety
/// Caller must close the returned handle with [`CloseHandle`].
pub unsafe fn get_current_token_for_restriction() -> io::Result<HANDLE> {
    let desired = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut h: HANDLE = std::ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut h) };
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    Ok(h)
}

/// Build the "World" identifier (`S-1-1-0`) into an owned byte buffer.
/// The kernel needs a real identifier structure in the restricting
/// set; this is the simplest one to hand it.
unsafe fn world_sid() -> io::Result<Vec<u8>> {
    let mut size: u32 = 0;
    CreateWellKnownSid(
        WIN_WORLD_SID,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut size,
    );
    let mut buf: Vec<u8> = vec![0u8; size as usize];
    let ok = CreateWellKnownSid(
        WIN_WORLD_SID,
        std::ptr::null_mut(),
        buf.as_mut_ptr() as *mut c_void,
        &mut size,
    );
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(GetLastError() as i32));
    }
    Ok(buf)
}

/// Scan the token's groups for the logon-session identifier and return
/// a copy. Including it in the restricting set lets the child still
/// allocate kernel objects bound to the session.
unsafe fn get_logon_sid_bytes(h_token: HANDLE) -> io::Result<Vec<u8>> {
    let mut needed: u32 = 0;
    GetTokenInformation(h_token, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
    if needed == 0 {
        return Err(io::Error::other("TokenGroups size query returned 0"));
    }
    let mut buf: Vec<u8> = vec![0u8; needed as usize];
    let ok = GetTokenInformation(
        h_token,
        TokenGroups,
        buf.as_mut_ptr() as *mut c_void,
        needed,
        &mut needed,
    );
    if ok == 0 || (needed as usize) < std::mem::size_of::<u32>() {
        return Err(io::Error::other(format!(
            "GetTokenInformation(TokenGroups) failed: {}",
            GetLastError()
        )));
    }
    let group_count = std::ptr::read_unaligned(buf.as_ptr() as *const u32) as usize;
    let after_count = buf.as_ptr().add(std::mem::size_of::<u32>()) as usize;
    let align = std::mem::align_of::<SID_AND_ATTRIBUTES>();
    let aligned = (after_count + (align - 1)) & !(align - 1);
    let groups_ptr = aligned as *const SID_AND_ATTRIBUTES;
    for i in 0..group_count {
        let entry: SID_AND_ATTRIBUTES = std::ptr::read_unaligned(groups_ptr.add(i));
        if (entry.Attributes & SE_GROUP_LOGON_ID) == SE_GROUP_LOGON_ID {
            let sid = entry.Sid;
            let sid_len = GetLengthSid(sid);
            if sid_len == 0 {
                return Err(io::Error::other("GetLengthSid for logon SID returned 0"));
            }
            let mut out = vec![0u8; sid_len as usize];
            if CopySid(sid_len, out.as_mut_ptr() as *mut c_void, sid) == 0 {
                return Err(io::Error::from_raw_os_error(GetLastError() as i32));
            }
            return Ok(out);
        }
    }
    Err(io::Error::other("Logon SID not present on token"))
}

/// Replace the token's default permission list so the sandboxed
/// process can create pipes and other kernel objects without hitting
/// `ACCESS_DENIED` when the shell builds pipelines.
unsafe fn set_default_dacl(h_token: HANDLE, sids: &[*mut c_void]) -> io::Result<()> {
    if sids.is_empty() {
        return Ok(());
    }
    let entries: Vec<EXPLICIT_ACCESS_W> = sids
        .iter()
        .map(|sid| EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 0,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: *sid as *mut u16,
            },
        })
        .collect();
    let mut p_new_dacl: *mut windows_sys::Win32::Security::ACL = std::ptr::null_mut();
    let res = SetEntriesInAclW(
        entries.len() as u32,
        entries.as_ptr(),
        std::ptr::null_mut(),
        &mut p_new_dacl,
    );
    if res != ERROR_SUCCESS {
        return Err(io::Error::other(format!("SetEntriesInAclW failed: {res}")));
    }
    #[repr(C)]
    struct TokenDefaultDaclInfo {
        default_dacl: *mut windows_sys::Win32::Security::ACL,
    }
    let mut info = TokenDefaultDaclInfo {
        default_dacl: p_new_dacl,
    };
    let ok = SetTokenInformation(
        h_token,
        TokenDefaultDacl,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<TokenDefaultDaclInfo>() as u32,
    );
    if ok == 0 {
        let err = GetLastError();
        if !p_new_dacl.is_null() {
            LocalFree(p_new_dacl as HLOCAL);
        }
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    if !p_new_dacl.is_null() {
        LocalFree(p_new_dacl as HLOCAL);
    }
    Ok(())
}

/// Restricted children need `SeChangeNotifyPrivilege` enabled,
/// otherwise common file-system operations refuse them with
/// `ACCESS_DENIED`.
unsafe fn enable_single_privilege(h_token: HANDLE, name: &str) -> io::Result<()> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    let ok = LookupPrivilegeValueW(std::ptr::null(), to_wide(name).as_ptr(), &mut luid);
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(GetLastError() as i32));
    }
    let mut tp: TOKEN_PRIVILEGES = std::mem::zeroed();
    tp.PrivilegeCount = 1;
    tp.Privileges[0].Luid = luid;
    tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
    let ok2 = AdjustTokenPrivileges(
        h_token,
        0,
        &tp,
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if ok2 == 0 {
        return Err(io::Error::from_raw_os_error(GetLastError() as i32));
    }
    Ok(())
}

/// Build a restricted token whose restricting set contains the
/// supplied workspace identifiers plus the logon-session and World
/// identifiers.
///
/// # Safety
/// `base_token` must be a valid primary token. The caller must close
/// the returned handle with [`CloseHandle`].
pub unsafe fn create_workspace_write_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
) -> io::Result<HANDLE> {
    if psid_capabilities.is_empty() {
        return Err(io::Error::other("no workspace identifiers provided"));
    }
    let mut logon_sid_bytes = get_logon_sid_bytes(base_token)?;
    let psid_logon = logon_sid_bytes.as_mut_ptr() as *mut c_void;
    let mut everyone = world_sid()?;
    let psid_everyone = everyone.as_mut_ptr() as *mut c_void;

    let mut entries: Vec<SID_AND_ATTRIBUTES> =
        vec![std::mem::zeroed(); psid_capabilities.len() + 2];
    for (i, psid) in psid_capabilities.iter().enumerate() {
        entries[i].Sid = *psid;
        entries[i].Attributes = 0;
    }
    let logon_idx = psid_capabilities.len();
    entries[logon_idx].Sid = psid_logon;
    entries[logon_idx].Attributes = 0;
    entries[logon_idx + 1].Sid = psid_everyone;
    entries[logon_idx + 1].Attributes = 0;

    let mut new_token: HANDLE = std::ptr::null_mut();
    let flags = DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED;
    let ok = CreateRestrictedToken(
        base_token,
        flags,
        0,
        std::ptr::null(),
        0,
        std::ptr::null(),
        entries.len() as u32,
        entries.as_mut_ptr(),
        &mut new_token,
    );
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(GetLastError() as i32));
    }

    let mut dacl_sids: Vec<*mut c_void> = Vec::with_capacity(psid_capabilities.len() + 2);
    dacl_sids.push(psid_logon);
    dacl_sids.push(psid_everyone);
    dacl_sids.extend_from_slice(psid_capabilities);
    if let Err(e) = set_default_dacl(new_token, &dacl_sids) {
        CloseHandle(new_token);
        return Err(e);
    }

    if let Err(e) = enable_single_privilege(new_token, "SeChangeNotifyPrivilege") {
        CloseHandle(new_token);
        return Err(e);
    }
    Ok(new_token)
}
