//! Operating-system confinement for shell commands.
//!
//! In the sandboxed mode the permission gate stops refusing unknown or
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
/// reads unrestricted (not gated by this policy).
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
    /// command may not read; a deny with a glob falls back to its longest
    /// glob-free leading path so it is widened rather than dropped (see
    /// [`resolve_read_subpath`]). An allow is kept only when it carves an
    /// exception out of a denied subtree — an exact path strictly inside a
    /// deny — so a broad or glob allow cannot re-open a denied path, which
    /// matches the per-argument check where an exact allow outranks a glob
    /// deny but a broad allow does not.
    pub fn with_read_rules(mut self, workspace: &Path, deny: &[String], allow: &[String]) -> Self {
        let deny_subpaths: Vec<PathBuf> = deny
            .iter()
            .filter_map(|p| resolve_read_subpath(p, workspace, ReadRuleSide::Deny))
            // Drop a deny that covers the workspace itself or an ancestor of
            // it: the confined command runs inside the workspace and must be
            // able to read it, so such a deny would break the command rather
            // than hide a secret. It only arises when a pattern's glob-free
            // prefix climbs to the workspace root; the per-argument read
            // check still applies to that pattern.
            .filter(|subpath| !workspace.starts_with(subpath))
            .collect();
        self.read_allow_subpaths = allow
            .iter()
            .filter_map(|p| resolve_read_subpath(p, workspace, ReadRuleSide::Allow))
            .filter(|a| {
                !deny_subpaths.contains(a) && deny_subpaths.iter().any(|d| a.starts_with(d))
            })
            .collect();
        self.read_deny_subpaths = deny_subpaths;
        self
    }
}

/// Characters a `Read(...)` pattern can use as glob metacharacters; a path
/// component holding any of them is not a literal path segment.
const GLOB_METACHARACTERS: &[char] = &['*', '?', '[', ']', '{', '}'];

/// Which side of the workspace read rules a pattern came from, which
/// decides how a glob in it is handled. A deny is widened so the kernel
/// never reads less than the rule names: a trailing `/**` is dropped (a
/// subpath rule already covers the whole tree) and any remaining glob
/// falls back to the pattern's longest glob-free leading path. An allow
/// must stay an exact path, so a glob allow is dropped and left to the
/// per-argument read check; this keeps a broad allow from re-opening a
/// denied path.
#[derive(Clone, Copy)]
enum ReadRuleSide {
    Deny,
    Allow,
}

/// Resolve a `Read(...)` pattern to one absolute subpath for the kernel
/// rules, or `None` when it cannot be expressed as one (an empty pattern,
/// or a glob allow). `side` decides how a glob is handled — see
/// [`ReadRuleSide`]. `.` and `..` are folded and the existing prefix is
/// canonicalized — resolving symlinks like macOS `/tmp` -> `/private/tmp`
/// — so the rule matches the path the kernel enforces on even when the
/// target does not exist yet.
fn resolve_read_subpath(pattern: &str, workspace: &Path, side: ReadRuleSide) -> Option<PathBuf> {
    let trimmed = match side {
        ReadRuleSide::Deny => pattern.strip_suffix("/**").unwrap_or(pattern),
        ReadRuleSide::Allow => pattern,
    }
    .trim();
    if trimmed.is_empty() {
        return None;
    }
    if matches!(side, ReadRuleSide::Allow) && trimmed.contains(GLOB_METACHARACTERS) {
        return None;
    }
    let expanded = if trimmed == "~" {
        PathBuf::from(std::env::var_os("HOME")?)
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else if Path::new(trimmed).is_absolute() {
        PathBuf::from(trimmed)
    } else {
        workspace.join(trimmed.strip_prefix("./").unwrap_or(trimmed))
    };
    let normalized = crate::tools::utils::lexically_normalize(&expanded);
    let resolved = match side {
        ReadRuleSide::Deny => longest_glob_free_prefix(&normalized),
        ReadRuleSide::Allow => normalized,
    };
    Some(canonicalize_existing_prefix(&resolved))
}

/// The longest leading path made only of components without a glob
/// character: `/a/b*/c` yields `/a`, and a path with no glob is returned
/// whole. Used to widen a glob deny to a real path the kernel can hide
/// instead of dropping it. The result stays absolute when `path` is,
/// because the root component carries no glob.
fn longest_glob_free_prefix(path: &Path) -> PathBuf {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        // Only a named path segment can carry a glob; stop at the first one
        // that does. Root and drive-prefix components are always kept, so a
        // Windows verbatim prefix (`\\?\…`) is not mistaken for a glob.
        if let std::path::Component::Normal(part) = component {
            if part.to_string_lossy().contains(GLOB_METACHARACTERS) {
                break;
            }
        }
        prefix.push(component);
    }
    prefix
}

/// Canonicalize `path`, resolving symlinks even when the leaf does not
/// exist yet: canonicalize the longest existing ancestor and re-append the
/// missing trailing components. Plain `canonicalize` fails outright on a
/// missing target, which would leave a symlinked prefix (such as macOS
/// `/tmp` -> `/private/tmp`, or a workspace-relative path whose symlink
/// points elsewhere) unresolved.
pub(super) fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
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

/// Install the network seccomp program on the current thread, or do
/// nothing when there is none (an architecture the filter does not
/// target). The caller runs this in the child between `fork` and `exec`
/// so the confined command inherits the filter; the executor, the
/// availability probe, and the end-to-end test all install through here,
/// so one path is exercised. Must not allocate after `fork`.
#[cfg(target_os = "linux")]
pub fn apply_network_seccomp(program: Option<&seccompiler::BpfProgram>) -> std::io::Result<()> {
    match program {
        Some(program) => seccompiler::apply_filter(program)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::Other)),
        None => Ok(()),
    }
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

    /// A deny with a glob in the middle is widened to its longest glob-free
    /// leading path instead of being dropped, so the kernel still hides the
    /// directory the secret lives under.
    #[test]
    fn read_deny_with_inner_glob_falls_back_to_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let policy = SandboxPolicy::for_workspace(&workspace).with_read_rules(
            &workspace,
            &["./config/*/secret.env".to_string()],
            &[],
        );
        assert_eq!(policy.read_deny_subpaths, vec![workspace.join("config")]);
    }

    /// A deny whose glob-free prefix is the workspace itself is dropped
    /// rather than blocking every read in the project the confined command
    /// runs in.
    #[test]
    fn read_deny_covering_the_workspace_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let policy = SandboxPolicy::for_workspace(&workspace).with_read_rules(
            &workspace,
            &["*/secret".to_string()],
            &[],
        );
        assert!(policy.read_deny_subpaths.is_empty());
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
