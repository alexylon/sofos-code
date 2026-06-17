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

/// Locations a confined command may write to, whether it may reach the
/// network, and which paths it may not read. `allow_network`, the read
/// lists, and `write_protect_subpaths` are consulted by the macOS and
/// Linux backends only; the Windows backend leaves the network open and
/// reads unrestricted.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub struct SandboxPolicy {
    pub writable_roots: Vec<PathBuf>,
    pub allow_network: bool,
    /// Directory subtrees and files the confined command may not read,
    /// enforced by the kernel so a denied path cannot be reached even
    /// when its name never appears as a command argument.
    pub read_deny_subpaths: Vec<PathBuf>,
    /// Subpaths that stay readable even when a broader entry denies them,
    /// so a specific allow can carve an exception out of a denied tree.
    pub read_allow_subpaths: Vec<PathBuf>,
    /// Subpaths that stay readable but never writable, even inside a
    /// writable root: project metadata a confined command must not
    /// rewrite, since doing so could plant a hook or relax the next
    /// command's gate.
    pub write_protect_subpaths: Vec<PathBuf>,
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
            read_deny_subpaths: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: metadata_protect_subpaths(workspace),
        }
    }

    /// Add the kernel-enforced read rules from the workspace's `Read(...)`
    /// deny and allow patterns. A deny pattern becomes a subpath the
    /// command may not read. An allow is kept only when it carves an
    /// exception out of a denied subtree — an exact path strictly inside a
    /// deny — so a broad or glob allow cannot re-open a denied path, which
    /// matches the per-argument check where an exact allow outranks a glob
    /// deny but a broad allow does not. Patterns that are not a plain
    /// directory subtree or file are skipped and stay covered by the
    /// per-argument read check alone.
    pub fn with_read_rules(mut self, workspace: &Path, deny: &[String], allow: &[String]) -> Self {
        let deny_subpaths: Vec<PathBuf> = deny
            .iter()
            .filter_map(|p| resolve_read_subpath(p, workspace, true))
            .collect();
        self.read_allow_subpaths = allow
            .iter()
            .filter_map(|p| resolve_read_subpath(p, workspace, false))
            .filter(|a| {
                !deny_subpaths.contains(a) && deny_subpaths.iter().any(|d| a.starts_with(d))
            })
            .collect();
        self.read_deny_subpaths = deny_subpaths;
        self
    }
}

/// Resolve a `Read(...)` pattern to one absolute subpath, or `None` when
/// it is not a plain path. With `strip_subtree`, a trailing `/**` is
/// dropped because a subpath rule already covers the whole tree;
/// otherwise (and for any other glob metacharacter) the pattern is left
/// out so the per-argument read check stays its only boundary. `.` and
/// `..` are folded and the existing prefix is canonicalized — resolving
/// symlinks like macOS `/tmp` -> `/private/tmp` — so the rule matches the
/// path the kernel enforces on even when the target does not exist yet.
fn resolve_read_subpath(pattern: &str, workspace: &Path, strip_subtree: bool) -> Option<PathBuf> {
    let core = if strip_subtree {
        pattern.strip_suffix("/**").unwrap_or(pattern)
    } else {
        pattern
    }
    .trim();
    if core.is_empty() || core.contains(['*', '?', '[', ']', '{', '}']) {
        return None;
    }
    let expanded = if core == "~" {
        PathBuf::from(std::env::var_os("HOME")?)
    } else if let Some(rest) = core.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else if Path::new(core).is_absolute() {
        PathBuf::from(core)
    } else {
        workspace.join(core.strip_prefix("./").unwrap_or(core))
    };
    let normalized = crate::tools::utils::lexically_normalize(&expanded);
    Some(canonicalize_existing_prefix(&normalized))
}

/// Canonicalize `path`, resolving symlinks even when the leaf does not
/// exist yet: canonicalize the longest existing ancestor and re-append
/// the missing trailing components. Plain `canonicalize` fails outright
/// on a missing target, which would leave a symlinked prefix (macOS
/// `/tmp` -> `/private/tmp`) unresolved and the rule unmatched by the
/// kernel.
fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut trailing: Vec<&OsStr> = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            trailing.push(name);
        }
        if let Ok(mut canonical) = std::fs::canonicalize(parent) {
            canonical.extend(trailing.iter().rev());
            return canonical;
        }
        current = parent;
    }
    path.to_path_buf()
}

/// Project metadata kept read-only inside the sandbox even though the
/// workspace is writable. Each can run code or relax the command gate on
/// a later turn — Git hooks/config, the `.sofos` permission rules
/// (re-read every command), and agent settings — so a confined command
/// must not rewrite them. Joined onto the workspace as-is so the path
/// matches the writable-root bind.
/// The repository's Git directory. Write-protected like the other
/// metadata directories, but lifted for commands that run only git, which
/// need to write it for `checkout`, `config`, and similar.
pub(super) const GIT_METADATA_DIR: &str = ".git";

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
const METADATA_PROTECT_DIRS: &[&str] =
    &[GIT_METADATA_DIR, ".sofos", ".agents", ".claude", ".codex"];

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn metadata_protect_subpaths(workspace: &Path) -> Vec<PathBuf> {
    METADATA_PROTECT_DIRS
        .iter()
        .map(|name| workspace.join(name))
        .collect()
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

/// Linux: Bubblewrap is an optional package, so confirm it is installed
/// and can actually create user namespaces. On hardened kernels, inside
/// containers, and on WSL1, `bwrap` is present but cannot unshare the
/// user namespace; reporting it unavailable there makes the caller fall
/// back to the permission prompt instead of failing every command with a
/// bwrap error. The network seccomp filter must also build, since it
/// closes the unix-socket hole `--unshare-net` leaves open; without it
/// the prompt is the safer boundary. The probe is cached for the process
/// lifetime.
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    use std::sync::OnceLock;
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        linux::resolved_bwrap().is_some()
            && linux::bwrap_can_unshare_namespaces()
            && linux::network_seccomp_program().is_some()
    })
}

/// Build the network seccomp program for a confined Linux command, or
/// `None` if it cannot be built on this architecture.
#[cfg(target_os = "linux")]
pub fn network_seccomp_program() -> Option<seccompiler::BpfProgram> {
    linux::network_seccomp_program()
}

/// Install a network seccomp program on the current thread. The caller
/// runs this in the child between `fork` and `exec` so the confined
/// command inherits the filter.
#[cfg(target_os = "linux")]
pub fn apply_network_seccomp(program: &seccompiler::BpfProgram) -> std::io::Result<()> {
    seccompiler::apply_filter(program).map_err(|_| std::io::Error::from(std::io::ErrorKind::Other))
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
    let profile = macos::seatbelt_profile(policy)?;
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
    let program = linux::resolved_bwrap()?;
    let mut args = linux::bwrap_arguments(policy);
    args.push(OsString::from("--"));
    args.push(shell.to_os_string());
    args.push(OsString::from("-c"));
    args.push(OsString::from(command));
    Some((program.as_os_str().to_os_string(), args))
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

    /// The workspace policy marks the project's metadata directories as
    /// write-protected so a confined command cannot plant a Git hook,
    /// rewrite Git or `.sofos` config, or edit agent settings even though
    /// the workspace around them is writable.
    #[test]
    fn workspace_policy_write_protects_metadata_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        for name in [".git", ".sofos", ".agents", ".claude", ".codex"] {
            assert!(
                policy
                    .write_protect_subpaths
                    .contains(&dir.path().join(name)),
                "{name} must be write-protected"
            );
        }
    }

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

    /// An allow is kept only as an exact exception strictly inside a
    /// denied subtree; a broad or glob allow, or an allow outside the
    /// deny, is dropped so it cannot re-open a denied path.
    #[test]
    fn read_rules_keep_only_exceptions_inside_a_deny() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let policy = SandboxPolicy::for_workspace(&workspace).with_read_rules(
            &workspace,
            &["./secret/**".to_string()],
            &[
                "./**".to_string(),
                "./other".to_string(),
                "./secret/ok.txt".to_string(),
            ],
        );
        let secret = workspace.join("secret");
        assert_eq!(policy.read_deny_subpaths, vec![secret.clone()]);
        assert_eq!(policy.read_allow_subpaths, vec![secret.join("ok.txt")]);
    }

    /// A `..` segment is collapsed so the rule matches the path the kernel
    /// sees rather than a literal `..` path that would match nothing.
    #[test]
    fn read_rule_normalizes_parent_segments() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let policy = SandboxPolicy::for_workspace(&workspace).with_read_rules(
            &workspace,
            &["./secret/../private".to_string()],
            &[],
        );
        assert_eq!(policy.read_deny_subpaths, vec![workspace.join("private")]);
    }

    /// A deny on a path that does not exist yet still resolves through a
    /// symlinked prefix to the real directory, so the kernel — which sees
    /// the resolved inode — matches the rule. Without this, a create-then
    /// -read inside the command could reach the secret.
    #[cfg(unix)]
    #[test]
    fn read_rule_resolves_symlinked_prefix_for_missing_target() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let real = workspace.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = workspace.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let policy = SandboxPolicy::for_workspace(&workspace).with_read_rules(
            &workspace,
            &[format!("{}/missing/**", link.display())],
            &[],
        );
        assert_eq!(policy.read_deny_subpaths, vec![real.join("missing")]);
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
