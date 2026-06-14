//! Operating-system confinement for shell commands.
//!
//! In workspace mode the permission gate stops refusing unknown or
//! structurally unusual commands and instead runs them confined: writes
//! are limited to the workspace and the temporary directories, and the
//! network is closed. The model can use the shell freely while the
//! kernel keeps side effects inside the project.
//!
//! Each platform wraps the shell with its native tool:
//! - macOS: the Seatbelt profile compiler, `sandbox-exec`.
//! - Linux: Bubblewrap, `bwrap`.
//!
//! On platforms without a supported wrapper (currently Windows),
//! [`confined_invocation`] returns `None` and the caller keeps the
//! permission checks as the only boundary.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Locations a confined command may write to, plus whether it may reach
/// the network. Reads are left open; only writes and the network are
/// narrowed, which is what the permission gate protects against.
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
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        if !tmpdir.is_empty() {
            candidates.push(PathBuf::from(tmpdir));
        }
    }
    candidates.push(PathBuf::from("/tmp"));

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

/// Platforms without a wrapper never report a sandbox as available.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    let mut args = bwrap_arguments(policy);
    args.push(OsString::from("--"));
    args.push(shell.to_os_string());
    args.push(OsString::from("-c"));
    args.push(OsString::from(command));
    Some((OsString::from(linux::BWRAP_PROGRAM), args))
}

/// Platforms without a wrapper (Windows): no confinement is applied, so
/// the caller keeps the permission checks as the boundary. A native
/// backend (a restricted token confined to the workspace) is the planned
/// replacement.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn confined_invocation(
    _shell: &OsStr,
    _command: &str,
    _policy: &SandboxPolicy,
) -> Option<(OsString, Vec<OsString>)> {
    None
}

/// Build the Bubblewrap arguments that confine writes to `policy`'s
/// roots: mount the whole filesystem read-only, re-bind each writable
/// root read-write, expose `/dev` and `/proc`, and close the network
/// unless the policy opens it. The caller appends `-- <shell> -c
/// <command>`. Defined for every platform so the argument construction
/// can be unit-tested; only the Linux backend invokes it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bwrap_arguments(policy: &SandboxPolicy) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("--ro-bind"),
        OsString::from("/"),
        OsString::from("/"),
        OsString::from("--dev"),
        OsString::from("/dev"),
        OsString::from("--proc"),
        OsString::from("/proc"),
    ];

    for root in &policy.writable_roots {
        if root.is_dir() {
            args.push(OsString::from("--bind"));
            args.push(root.clone().into_os_string());
            args.push(root.clone().into_os_string());
        }
    }

    if !policy.allow_network {
        args.push(OsString::from("--unshare-net"));
    }

    args
}

#[cfg(target_os = "macos")]
mod macos {
    use super::SandboxPolicy;

    /// The Seatbelt profile compiler shipped with macOS.
    pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

    /// Build a Seatbelt profile that allows everything by default, then
    /// closes the two things the workspace boundary cares about: writes
    /// outside the writable roots, and the network. Reads and process
    /// execution stay open so ordinary tools keep working.
    pub fn seatbelt_profile(policy: &SandboxPolicy) -> String {
        let mut profile = String::from("(version 1)\n(allow default)\n");

        if !policy.allow_network {
            profile.push_str("(deny network*)\n");
        }

        profile.push_str("(deny file-write*)\n");
        profile.push_str("(allow file-write*\n");
        for root in &policy.writable_roots {
            profile.push_str("  (subpath ");
            profile.push_str(&quote(&root.to_string_lossy()));
            profile.push_str(")\n");
        }
        // Character devices a normal shell pipeline needs to write to.
        for device in ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"] {
            profile.push_str("  (literal ");
            profile.push_str(&quote(device));
            profile.push_str(")\n");
        }
        profile.push_str(")\n");
        profile
    }

    /// Render a path as a Seatbelt double-quoted string literal,
    /// escaping the backslash and double-quote characters that would
    /// otherwise break the literal.
    fn quote(value: &str) -> String {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    /// The Bubblewrap binary, resolved from `PATH`.
    pub const BWRAP_PROGRAM: &str = "bwrap";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_policy_includes_workspace_and_closes_network() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        assert!(!policy.allow_network);
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            policy.writable_roots.contains(&canonical)
                || policy.writable_roots.contains(&dir.path().to_path_buf())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_denies_writes_and_network_then_reopens_roots() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        let profile = macos::seatbelt_profile(&policy);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow file-write*"));
    }

    /// End-to-end proof that the macOS sandbox actually confines writes:
    /// a write inside the single writable root succeeds, and a write to a
    /// sibling directory outside it is blocked by the kernel. The policy
    /// deliberately omits the temporary directories so the sibling temp
    /// directory is a clean "outside" target.
    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_blocks_writes_outside_the_writable_root() {
        use std::ffi::OsStr;
        use std::process::Command;

        let workspace_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(workspace_dir.path()).unwrap();
        let outside = std::fs::canonicalize(outside_dir.path()).unwrap();
        let policy = SandboxPolicy {
            writable_roots: vec![workspace.clone()],
            allow_network: false,
        };

        let run = |target: &Path| {
            let command = format!("echo confined > {}", target.display());
            let (program, args) =
                confined_invocation(OsStr::new("/bin/sh"), &command, &policy).unwrap();
            Command::new(program)
                .args(args)
                .output()
                .expect("spawn sandbox-exec");
        };

        let inside_file = workspace.join("inside.txt");
        run(&inside_file);
        assert!(
            inside_file.is_file(),
            "a write inside the writable root should succeed"
        );

        let outside_file = outside.join("outside.txt");
        run(&outside_file);
        assert!(
            !outside_file.exists(),
            "a write outside the writable root must be blocked by the sandbox"
        );
    }

    #[test]
    fn bwrap_arguments_bind_roots_and_close_network() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let closed = SandboxPolicy {
            writable_roots: vec![root.clone()],
            allow_network: false,
        };
        let args: Vec<String> = bwrap_arguments(&closed)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.iter().any(|a| a == "--ro-bind"), "root mounted read-only");
        assert!(args.iter().any(|a| a == "--bind"), "writable root re-bound");
        assert!(args.iter().any(|a| a == "--unshare-net"), "network closed");

        let open = SandboxPolicy {
            writable_roots: vec![root],
            allow_network: true,
        };
        let open_args: Vec<String> = bwrap_arguments(&open)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !open_args.iter().any(|a| a == "--unshare-net"),
            "network stays open when the policy allows it"
        );
    }
}
