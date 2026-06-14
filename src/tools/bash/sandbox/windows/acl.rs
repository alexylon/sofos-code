//! Workspace permission-list manipulation for the Windows sandbox.
//!
//! Derived from the codex Windows sandbox `acl` module, narrowed to
//! the single operation sofos needs: idempotently add an allow-write
//! rule for each workspace identifier on a path. The matching rule on
//! the directory is what lets writes through the restricted token
//! land inside the workspace.
//!
//! No revoke is exposed. The rule is meant to persist across restarts,
//! and the identifier it names is also persisted under
//! `.sofos/cap_sid`, so the rule always points at a live token.

use super::winutil::to_wide;
use std::ffi::c_void;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACCESS_ALLOWED_ACE;
use windows_sys::Win32::Security::ACE_HEADER;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::ACL_SIZE_INFORMATION;
use windows_sys::Win32::Security::AclSizeInformation;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::Security::GENERIC_MAPPING;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::GetAclInformation;
use windows_sys::Win32::Security::MapGenericMask;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::Storage::FileSystem::FILE_DELETE_CHILD;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_EXECUTE;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;

const SE_FILE_OBJECT: i32 = 1;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const INHERIT_ONLY_ACE: u8 = 0x08;
const CONTAINER_INHERIT_ACE: u32 = 0x2;
const OBJECT_INHERIT_ACE: u32 = 0x1;
const SET_ACCESS: i32 = 2;
const WRITE_ALLOW_MASK: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE | FILE_DELETE_CHILD;

/// Fetch the permission list for `path`. Caller must free the
/// returned security descriptor with [`LocalFree`].
unsafe fn fetch_dacl_handle(path: &Path) -> io::Result<(*mut ACL, *mut c_void)> {
    let wpath = to_wide(path);
    let h = CreateFileW(
        wpath.as_ptr(),
        READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        std::ptr::null_mut(),
    );
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::other(format!(
            "CreateFileW failed for {}",
            path.display()
        )));
    }
    let mut p_sd: *mut c_void = std::ptr::null_mut();
    let mut p_dacl: *mut ACL = std::ptr::null_mut();
    let code = GetSecurityInfo(
        h,
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut p_dacl,
        std::ptr::null_mut(),
        &mut p_sd,
    );
    CloseHandle(h);
    if code != ERROR_SUCCESS {
        return Err(io::Error::other(format!(
            "GetSecurityInfo failed for {}: {}",
            path.display(),
            code
        )));
    }
    Ok((p_dacl, p_sd))
}

/// True when at least one applies-here allow rule already grants every
/// bit of `desired_mask` to any of the supplied identifiers.
unsafe fn dacl_mask_allows(p_dacl: *mut ACL, psids: &[*mut c_void], desired_mask: u32) -> bool {
    if p_dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    let ok = GetAclInformation(
        p_dacl as *const ACL,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    );
    if ok == 0 {
        return false;
    }
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    for i in 0..(info.AceCount as usize) {
        let mut p_ace: *mut c_void = std::ptr::null_mut();
        if GetAce(p_dacl as *const ACL, i as u32, &mut p_ace) == 0 {
            continue;
        }
        let hdr = &*(p_ace as *const ACE_HEADER);
        if hdr.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }
        if (hdr.AceFlags & INHERIT_ONLY_ACE) != 0 {
            continue;
        }
        let base = p_ace as usize;
        let sid_ptr =
            (base + std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>()) as *mut c_void;
        let mut matched = false;
        for sid in psids {
            if EqualSid(sid_ptr, *sid) != 0 {
                matched = true;
                break;
            }
        }
        if !matched {
            continue;
        }
        let ace = &*(p_ace as *const ACCESS_ALLOWED_ACE);
        let mut mask = ace.Mask;
        MapGenericMask(&mut mask, &mapping);
        if (mask & desired_mask) == desired_mask {
            return true;
        }
    }
    false
}

/// Add an inheritable allow-write rule on `path` for each identifier
/// in `sids` that does not already have one. Returns true when at
/// least one rule was added.
///
/// # Safety
/// Each identifier pointer must remain valid for the duration of the
/// call and the path must refer to an existing file or directory.
pub unsafe fn ensure_allow_write_aces(path: &Path, sids: &[*mut c_void]) -> io::Result<bool> {
    let (p_dacl, p_sd) = fetch_dacl_handle(path)?;
    let mut entries: Vec<EXPLICIT_ACCESS_W> = Vec::new();
    for sid in sids {
        if dacl_mask_allows(p_dacl, &[*sid], WRITE_ALLOW_MASK) {
            continue;
        }
        entries.push(EXPLICIT_ACCESS_W {
            grfAccessPermissions: WRITE_ALLOW_MASK,
            grfAccessMode: SET_ACCESS,
            grfInheritance: CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: *sid as *mut u16,
            },
        });
    }
    if entries.is_empty() {
        if !p_sd.is_null() {
            LocalFree(p_sd as HLOCAL);
        }
        return Ok(false);
    }
    let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
    let code2 = SetEntriesInAclW(
        entries.len() as u32,
        entries.as_ptr(),
        p_dacl,
        &mut p_new_dacl,
    );
    if code2 != ERROR_SUCCESS {
        if !p_sd.is_null() {
            LocalFree(p_sd as HLOCAL);
        }
        return Err(io::Error::other(format!(
            "SetEntriesInAclW failed: {code2}"
        )));
    }
    let code3 = SetNamedSecurityInfoW(
        to_wide(path).as_ptr() as *mut u16,
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        p_new_dacl,
        std::ptr::null_mut(),
    );
    if !p_new_dacl.is_null() {
        LocalFree(p_new_dacl as HLOCAL);
    }
    if !p_sd.is_null() {
        LocalFree(p_sd as HLOCAL);
    }
    if code3 != ERROR_SUCCESS {
        return Err(io::Error::other(format!(
            "SetNamedSecurityInfoW failed: {code3}"
        )));
    }
    Ok(true)
}
