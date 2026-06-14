//! Operating-system confinement for shell commands.
//!
//! In workspace mode the permission gate stops refusing unknown or
//! structurally unusual commands and instead runs them confined: writes
//! are limited to the workspace and the temporary directories, and the
//! network is closed where the operating system allows it. The model
//! can use the shell freely while the kernel keeps side effects inside
//! the project.
//!
//! Each platform wraps the shell with its native primitive:
//! - macOS: Seatbelt, via `sandbox-exec`.
//! - Linux: Bubblewrap, via `bwrap`.
//! - Windows: a restricted access token combined with an allow-write
//!   rule on the workspace, used to spawn the shell. The network is
//!   left open because closing it on Windows needs administrator
//!   privileges and applies to the whole user account.
//!
//! On platforms without a supported wrapper, [`confined_invocation`]
//! returns `None` and the caller keeps the permission checks as the
//! only boundary.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

/// Locations a confined command may write to, plus whether it may
/// reach the network. Reads stay open; only writes and the network are
/// narrowed. `allow_network` is consulted by the macOS and Linux
/// backends only; the Windows backend leaves the network open.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub struct SandboxPolicy {
    pub writable_roots: Vec<PathBuf>,
    pub allow_network: bool,
}

impl SandboxPolicy {
    /// Confine writes to the workspace and the system temporary
    /// directories, with the network closed. Temporary directories are
    /// included because common build and test tools write there.
    pub fn for_workspace(workspace: &Path) -> Self {
        let mut writable_roots = vec![workspace.to_path_buf()];
        for dir in temporary_directories() {
            if !writable_roots.contains(&dir) {
                writable_roots.push(dir);
            }
        }
        Self {
            writable_roots,
            allow_network: false,
        }
    }
}

/// System temporary directories that stay writable inside the sandbox.
/// Paths are canonicalised so the rules match the real location the
/// kernel sees (on macOS `/tmp` resolves to `/private/tmp`).
fn temporary_directories() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(unix)]
    {
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            if !tmpdir.is_empty() {
                candidates.push(PathBuf::from(tmpdir));
            }
        }
        candidates.push(PathBuf::from("/tmp"));
    }
    #[cfg(windows)]
    {
        for key in ["TEMP", "TMP", "LOCALAPPDATA"] {
            if let Some(value) = std::env::var_os(key) {
                if !value.is_empty() {
                    let mut path = PathBuf::from(value);
                    if key == "LOCALAPPDATA" {
                        path.push("Temp");
                    }
                    candidates.push(path);
                }
            }
        }
    }

    let mut roots = Vec::new();
    for candidate in candidates {
        let resolved = std::fs::canonicalize(&candidate).unwrap_or(candidate);
        if resolved.is_dir() && !roots.contains(&resolved) {
            roots.push(resolved);
        }
    }
    roots
}

/// Whether a usable sandbox wrapper is present on this machine. When it
/// is false the caller keeps the permission checks as the boundary
/// instead of running a command unconfined.
#[cfg(target_os = "macos")]
pub fn is_available() -> bool {
    Path::new(macos::SANDBOX_EXEC_PATH).is_file()
}

/// Linux: Bubblewrap is an optional package, so confirm it is installed.
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    program_on_path(linux::BWRAP_PROGRAM)
}

/// Windows: the restricted-token backend is present but not yet engaged.
/// The default Windows shell (Git for Windows `sh.exe`, a Cygwin
/// binary) cannot start under the restricted access token because its
/// session-shared-memory attach is refused by the kernel. Until the
/// shell story is solved, `is_available` reports false so workspace
/// mode falls back to the permission gate on Windows. The backend
/// modules stay in the tree as the foundation for future re-enabling.
#[cfg(target_os = "windows")]
pub fn is_available() -> bool {
    false
}

/// Platforms without a wrapper never report a sandbox as available.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn is_available() -> bool {
    false
}

/// Whether `program` resolves to an executable file on the `PATH`.
#[cfg(target_os = "linux")]
fn program_on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Build the program and arguments that run `<shell> -c <command>`
/// confined by `policy`. Returns `None` when this platform has no
/// supported sandbox, signalling the caller to run the shell directly
/// and rely on the permission checks instead.
#[cfg(target_os = "macos")]
pub fn confined_invocation(
    shell: &OsStr,
    command: &str,
    policy: &SandboxPolicy,
) -> Option<(OsString, Vec<OsString>)> {
    let profile = macos::seatbelt_profile(policy);
    let mut args: Vec<OsString> = vec![OsString::from("-p"), OsString::from(profile)];
    args.push(shell.to_os_string());
    args.push(OsString::from("-c"));
    args.push(OsString::from(command));
    Some((OsString::from(macos::SANDBOX_EXEC_PATH), args))
}

/// Linux confinement via Bubblewrap. See the module documentation.
#[cfg(target_os = "linux")]
pub fn confined_invocation(
    shell: &OsStr,
    command: &str,
    policy: &SandboxPolicy,
) -> Option<(OsString, Vec<OsString>)> {
    let mut args = linux::bwrap_arguments(policy);
    args.push(OsString::from("--"));
    args.push(shell.to_os_string());
    args.push(OsString::from("-c"));
    args.push(OsString::from(command));
    Some((OsString::from(linux::BWRAP_PROGRAM), args))
}

/// Windows does not fit the `(program, args)` shape because the spawn
/// itself goes through `CreateProcessAsUserW` rather than a wrapper
/// command; the executor calls [`windows::run_confined`] directly.
#[cfg(target_os = "windows")]
pub fn confined_invocation(
    _shell: &OsStr,
    _command: &str,
    _policy: &SandboxPolicy,
) -> Option<(OsString, Vec<OsString>)> {
    None
}

/// Platforms without a wrapper: no confinement is applied, so the caller
/// keeps the permission checks as the boundary.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn confined_invocation(
    _shell: &OsStr,
    _command: &str,
    _policy: &SandboxPolicy,
) -> Option<(OsString, Vec<OsString>)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn workspace_policy_includes_workspace_and_closes_network_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        assert!(!policy.allow_network);
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            policy.writable_roots.contains(&canonical)
                || policy.writable_roots.contains(&dir.path().to_path_buf())
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn workspace_policy_includes_workspace_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        assert!(!policy.allow_network);
        let canonical =
            std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        assert!(
            policy.writable_roots.contains(&canonical)
                || policy.writable_roots.contains(&dir.path().to_path_buf())
        );
    }
}
