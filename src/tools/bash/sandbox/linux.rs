//! Linux confinement via Bubblewrap.

use super::SandboxPolicy;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The Bubblewrap binary name searched for on `PATH`.
const BWRAP_PROGRAM: &str = "bwrap";

/// Resolve `bwrap` to a trusted absolute path, cached for the process
/// lifetime. The result is the first `PATH` entry that is an absolute
/// directory holding a regular, executable `bwrap`, canonicalised so a
/// symlink resolves to the real binary. Relative `PATH` entries (a bare
/// `.` or `bin`) are skipped, because they resolve against the current
/// directory — the workspace — where a planted `bwrap` could stand in
/// for the sandbox wrapper. Resolving once and spawning by this absolute
/// path keeps the binary the probe checks identical to the one the
/// confined command runs. `None` when no such binary is found, which
/// makes the caller fall back to the permission prompt.
pub fn resolved_bwrap() -> Option<&'static Path> {
    static RESOLVED: OnceLock<Option<PathBuf>> = OnceLock::new();
    RESOLVED.get_or_init(find_trusted_bwrap).as_deref()
}

fn find_trusted_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    first_trusted_program(std::env::split_paths(&path), BWRAP_PROGRAM)
}

/// First absolute directory in `dirs` that holds a regular, executable
/// `program`, returned as its canonical absolute path. Relative
/// directories are skipped so a binary reached through the current
/// directory cannot be chosen.
fn first_trusted_program(dirs: impl Iterator<Item = PathBuf>, program: &str) -> Option<PathBuf> {
    dirs.filter(|dir| dir.is_absolute())
        .find_map(|dir| trusted_executable(&dir.join(program)))
}

/// Canonicalise `candidate` and return it when it is a regular file with
/// an execute bit set; `None` otherwise.
fn trusted_executable(candidate: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let resolved = std::fs::canonicalize(candidate).ok()?;
    let metadata = std::fs::metadata(&resolved).ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(resolved)
}

/// Treat bwrap as unusable if the probe has not exited within this window.
const BWRAP_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const BWRAP_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Namespaces every confined command is isolated into, regardless of
/// policy. The network namespace is added separately because the policy
/// can leave it open. The startup probe runs `bwrap` with these same
/// flags, so an older `bwrap` that rejects one is detected before a
/// command depends on it.
const BWRAP_UNSHARE_FLAGS: &[&str] = &[
    "--unshare-user",
    "--unshare-pid",
    "--unshare-ipc",
    "--unshare-uts",
    "--unshare-cgroup-try",
];

/// Build the Bubblewrap arguments that confine writes to `policy`'s
/// roots: mount the whole filesystem read-only, re-bind each writable
/// root read-write, expose `/dev` and `/proc`, and close the network
/// unless the policy opens it. Denied read subpaths are masked —
/// `/dev/null` over a file, an empty tmpfs over a directory or over a
/// not-yet-created path inside a writable root where a secret could
/// appear later — and allow exceptions are re-bound on top. The confined
/// process runs in fresh user, process, IPC, UTS, and — where the kernel
/// supports it — cgroup namespaces, so it cannot see or signal host
/// processes or reach host inter-process channels, and it dies with the
/// parent process (`--die-with-parent`). The caller appends
/// `-- <shell> -c <command>`.
pub fn bwrap_arguments(policy: &SandboxPolicy) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![OsString::from("--die-with-parent")];
    args.extend(BWRAP_UNSHARE_FLAGS.iter().map(|flag| OsString::from(*flag)));
    args.extend(
        ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
            .iter()
            .map(|arg| OsString::from(*arg)),
    );

    for root in &policy.writable_roots {
        if root.is_dir() {
            args.push(OsString::from("--bind"));
            args.push(root.clone().into_os_string());
            args.push(root.clone().into_os_string());
        }
    }

    // Keep project metadata read-only inside the writable workspace so a
    // confined command can read it but not rewrite it. An existing path is
    // re-bound read-only. One that does not exist yet is masked with an
    // empty read-only tmpfs, so the command cannot create persistent
    // content there (for example a `.sofos` config that would relax the
    // next command's gate). bwrap applies mounts in order, so both override
    // the writable bind. The tmpfs leaves an empty mount point on the
    // workspace; the executor removes it after the run.
    for path in &policy.write_protect_subpaths {
        if path.exists() {
            args.push(OsString::from("--ro-bind"));
            args.push(path.clone().into_os_string());
            args.push(path.clone().into_os_string());
        } else {
            args.push(OsString::from("--tmpfs"));
            args.push(path.clone().into_os_string());
            args.push(OsString::from("--remount-ro"));
            args.push(path.clone().into_os_string());
        }
    }

    // Read confinement: hide each denied subpath, then re-expose the
    // allow exceptions on top so a specific allow overrides a broader
    // deny. A file is masked with `/dev/null`; a directory — or a path
    // not created yet but inside a writable root, where a secret could
    // appear later — is masked with an empty tmpfs. A missing path
    // outside the writable roots is left alone: a tmpfs there would have
    // to create its mount point on the read-only base, which bwrap cannot.
    for path in &policy.read_deny_subpaths {
        if path.is_file() {
            args.push(OsString::from("--ro-bind"));
            args.push(OsString::from("/dev/null"));
            args.push(path.clone().into_os_string());
        } else if path.is_dir()
            || policy
                .writable_roots
                .iter()
                .any(|root| path.starts_with(root))
        {
            args.push(OsString::from("--tmpfs"));
            args.push(path.clone().into_os_string());
        }
    }
    for path in &policy.read_allow_subpaths {
        if path.exists() {
            args.push(OsString::from("--ro-bind"));
            args.push(path.clone().into_os_string());
            args.push(path.clone().into_os_string());
        }
    }

    if !policy.allow_network {
        args.push(OsString::from("--unshare-net"));
    }

    args
}

/// Probe whether Bubblewrap can actually create the namespaces the
/// confined command relies on. `bwrap` may be installed yet unusable —
/// user namespaces are restricted on some hardened kernels, inside
/// containers, and on WSL1 — in which case it exits with a namespace
/// error. An older `bwrap` may also not recognise every `--unshare-*`
/// flag used below. Running a throwaway `bwrap … /bin/true` with the
/// same unshare flags tells the caller whether confinement will work, so
/// it can fall back to the permission prompt instead of failing every
/// command with an unclear bwrap error. A short timeout guards against a
/// bwrap that hangs.
pub fn bwrap_can_unshare_namespaces() -> bool {
    use std::process::{Command, Stdio};

    let Some(program) = resolved_bwrap() else {
        return false;
    };
    let mut child = match Command::new(program)
        .args(BWRAP_UNSHARE_FLAGS)
        .args(["--unshare-net", "--ro-bind", "/", "/", "/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let deadline = Instant::now() + BWRAP_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(BWRAP_PROBE_POLL_INTERVAL);
            }
            Err(_) => return false,
        }
    }
}

/// Build a seccomp program that finishes closing the network for a
/// confined command. `--unshare-net` isolates IP networking, but it does
/// not stop a `connect()` to a filesystem unix socket such as
/// `/var/run/docker.sock`, a root-equivalent endpoint. This denies
/// `connect`, blocks creating network sockets (every family except
/// `AF_UNIX` and `AF_NETLINK`, which bubblewrap's loopback setup and
/// local name resolution need and which cannot reach the network), and
/// blocks the `io_uring` setup calls that could otherwise reach the
/// network without the filtered syscalls. The caller installs the
/// program in the child before `exec`, so bubblewrap and everything it
/// spawns inherit it. Returns `None` on an architecture the filter does
/// not target.
pub fn network_seccomp_program() -> Option<BpfProgram> {
    let arch = if cfg!(target_arch = "x86_64") {
        TargetArch::x86_64
    } else if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        return None;
    };

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for nr in [
        libc::SYS_connect,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
    ] {
        // An empty rule list matches the syscall unconditionally.
        rules.insert(nr, Vec::new());
    }
    // Deny network-capable socket domains, but keep AF_UNIX (local IPC)
    // and AF_NETLINK (kernel routing info that bubblewrap's loopback
    // setup and local name resolution need; it cannot reach the network).
    // The rule matches — and denies — only a domain that is neither.
    let blocked_socket = SeccompRule::new(vec![
        SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            libc::AF_UNIX as u64,
        )
        .ok()?,
        SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            libc::AF_NETLINK as u64,
        )
        .ok()?,
    ])
    .ok()?;
    rules.insert(libc::SYS_socket, vec![blocked_socket]);

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .ok()?;
    let program: BpfProgram = filter.try_into().ok()?;
    Some(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bwrap_arguments_bind_roots_and_close_network() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let closed = SandboxPolicy {
            writable_roots: vec![root.clone()],
            allow_network: false,
            read_deny_subpaths: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
        };
        let args: Vec<String> = bwrap_arguments(&closed)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|a| a == "--die-with-parent"),
            "confined process tied to the parent's lifetime"
        );
        assert!(
            args.iter().any(|a| a == "--unshare-user"),
            "fresh user namespace"
        );
        assert!(
            args.iter().any(|a| a == "--unshare-pid"),
            "fresh process namespace"
        );
        assert!(
            args.iter().any(|a| a == "--unshare-ipc"),
            "fresh IPC namespace"
        );
        assert!(
            args.iter().any(|a| a == "--unshare-uts"),
            "fresh UTS namespace"
        );
        assert!(
            args.iter().any(|a| a == "--unshare-cgroup-try"),
            "fresh cgroup namespace where the kernel supports it"
        );
        assert!(
            args.iter().any(|a| a == "--ro-bind"),
            "root mounted read-only"
        );
        assert!(args.iter().any(|a| a == "--bind"), "writable root re-bound");
        assert!(args.iter().any(|a| a == "--unshare-net"), "network closed");

        let open = SandboxPolicy {
            writable_roots: vec![root],
            allow_network: true,
            read_deny_subpaths: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
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

    /// Environment-dependent smoke test: with no usable bwrap the probe
    /// returns false, with a working one it returns true. Either way it
    /// must run to completion without panicking or hanging.
    #[test]
    fn bwrap_probe_yields_a_bool() {
        let _ = bwrap_can_unshare_namespaces();
    }

    fn set_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Only a regular file with an execute bit counts as the sandbox
    /// wrapper; a plain file or a directory is rejected.
    #[test]
    fn trusted_executable_accepts_only_executable_files() {
        let dir = tempfile::tempdir().unwrap();

        let exe = dir.path().join("bwrap");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        set_mode(&exe, 0o755);
        assert_eq!(
            trusted_executable(&exe),
            Some(std::fs::canonicalize(&exe).unwrap()),
            "a regular executable file is accepted"
        );

        let plain = dir.path().join("not-exec");
        std::fs::write(&plain, "x").unwrap();
        set_mode(&plain, 0o644);
        assert_eq!(
            trusted_executable(&plain),
            None,
            "a non-executable file is rejected"
        );

        assert_eq!(
            trusted_executable(dir.path()),
            None,
            "a directory is rejected"
        );
    }

    /// The search skips a relative directory and a directory whose
    /// `bwrap` is not executable, and returns the first absolute
    /// directory holding an executable one.
    #[test]
    fn first_trusted_program_skips_relative_and_non_executable_dirs() {
        let empty = tempfile::tempdir().unwrap();

        let nonexec = tempfile::tempdir().unwrap();
        let nonexec_bin = nonexec.path().join("bwrap");
        std::fs::write(&nonexec_bin, "x").unwrap();
        set_mode(&nonexec_bin, 0o644);

        let good = tempfile::tempdir().unwrap();
        let good_bin = good.path().join("bwrap");
        std::fs::write(&good_bin, "#!/bin/sh\n").unwrap();
        set_mode(&good_bin, 0o755);

        let dirs = vec![
            std::path::PathBuf::from("relative/bin"),
            empty.path().to_path_buf(),
            nonexec.path().to_path_buf(),
            good.path().to_path_buf(),
        ];
        assert_eq!(
            first_trusted_program(dirs.into_iter(), "bwrap"),
            Some(std::fs::canonicalize(&good_bin).unwrap()),
            "skips the relative, empty, and non-executable entries and picks the executable"
        );
    }

    /// The network filter fires correctly: a child that installs it is
    /// refused an `AF_INET` socket while `AF_UNIX` and `AF_NETLINK` — the
    /// families bubblewrap's loopback setup and name resolution need —
    /// still work. Forking keeps the filter off the test harness's own
    /// threads.
    #[test]
    fn network_seccomp_denies_inet_keeps_unix_and_netlink() {
        let program = network_seccomp_program().expect("filter builds on this architecture");
        let status = unsafe {
            match libc::fork() {
                0 => {
                    let applied = seccompiler::apply_filter(&program).is_ok();
                    let inet = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
                    let unix = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
                    let netlink =
                        libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE);
                    let ok = applied && inet < 0 && unix >= 0 && netlink >= 0;
                    libc::_exit(if ok { 0 } else { 1 });
                }
                child if child > 0 => {
                    let mut status = 0;
                    libc::waitpid(child, &mut status, 0);
                    status
                }
                _ => panic!("fork failed"),
            }
        };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "a confined process must keep AF_UNIX and AF_NETLINK but lose AF_INET"
        );
    }

    /// A denied read directory is masked with a tmpfs, a denied file with
    /// `/dev/null`, and an allow exception is re-bound on top.
    #[test]
    fn bwrap_masks_denied_read_subpaths() {
        let dir = tempfile::tempdir().unwrap();
        let secret_dir = dir.path().join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        let secret_file = dir.path().join("token");
        std::fs::write(&secret_file, "x").unwrap();
        let allowed = dir.path().join("secret-ok");
        std::fs::write(&allowed, "y").unwrap();

        let policy = SandboxPolicy {
            writable_roots: vec![dir.path().to_path_buf()],
            allow_network: false,
            read_deny_subpaths: vec![secret_dir.clone(), secret_file.clone()],
            read_allow_subpaths: vec![allowed.clone()],
            write_protect_subpaths: Vec::new(),
        };
        let args: Vec<String> = bwrap_arguments(&policy)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let secret_dir = secret_dir.to_string_lossy().into_owned();
        let secret_file = secret_file.to_string_lossy().into_owned();
        let allowed = allowed.to_string_lossy().into_owned();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == secret_dir),
            "denied directory masked with tmpfs"
        );
        assert!(
            args.windows(3)
                .any(|w| w[0] == "--ro-bind" && w[1] == "/dev/null" && w[2] == secret_file),
            "denied file masked with /dev/null"
        );
        assert!(
            args.windows(3)
                .any(|w| w[0] == "--ro-bind" && w[1] == allowed && w[2] == allowed),
            "allow exception re-bound"
        );
    }

    /// A denied path that does not exist yet is still masked with a tmpfs
    /// when it lies inside a writable root, so a secret created there
    /// during the command cannot be read. One outside every writable root
    /// is left alone, since a tmpfs cannot be mounted over the read-only
    /// base.
    #[test]
    fn bwrap_masks_missing_denied_subpath_only_inside_writable_roots() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("not-created-yet");
        let outside = std::path::PathBuf::from("/var/empty/sofos-not-created-yet");
        let policy = SandboxPolicy {
            writable_roots: vec![dir.path().to_path_buf()],
            allow_network: false,
            read_deny_subpaths: vec![inside.clone(), outside.clone()],
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
        };
        let args: Vec<String> = bwrap_arguments(&policy)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let inside = inside.to_string_lossy().into_owned();
        let outside = outside.to_string_lossy().into_owned();
        assert!(
            args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == inside),
            "a missing deny target inside a writable root is masked"
        );
        assert!(
            !args.contains(&outside),
            "a missing deny target outside the writable roots is not masked"
        );
    }

    /// End-to-end proof on a live bubblewrap that read confinement masks a
    /// denied directory with a tmpfs while a specific allow exception
    /// below it stays readable — the case the argument-only tests cannot
    /// prove. Skipped where bubblewrap cannot create user namespaces
    /// (hardened kernels, unprivileged containers, WSL1).
    #[test]
    fn bwrap_keeps_allow_exception_readable_below_a_masked_directory() {
        if !bwrap_can_unshare_namespaces() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let secret_dir = workspace.join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        let hidden = secret_dir.join("key.txt");
        std::fs::write(&hidden, "top secret").unwrap();
        let allowed = secret_dir.join("ok.txt");
        std::fs::write(&allowed, "fine to read").unwrap();

        let policy = SandboxPolicy {
            writable_roots: vec![workspace.clone()],
            allow_network: false,
            read_deny_subpaths: vec![secret_dir.clone()],
            read_allow_subpaths: vec![allowed.clone()],
            write_protect_subpaths: Vec::new(),
        };

        let read = |path: &std::path::Path| {
            let command = format!("cat {}", path.display());
            let (program, args) = super::super::confined_invocation(
                std::ffi::OsStr::new("/bin/sh"),
                &command,
                &policy,
            )
            .unwrap();
            std::process::Command::new(program)
                .args(args)
                .output()
                .expect("spawn bwrap")
        };

        let denied = read(&hidden);
        assert!(
            !denied.status.success(),
            "a file under a tmpfs-masked deny directory must be unreadable"
        );

        let exception = read(&allowed);
        assert!(
            exception.status.success(),
            "the allow exception below the masked directory must stay readable; stderr: {}",
            String::from_utf8_lossy(&exception.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&exception.stdout), "fine to read");
    }

    /// An existing metadata directory is re-bound read-only after the
    /// writable workspace bind; a missing one is masked with a read-only
    /// tmpfs so it cannot be created with persistent content.
    #[test]
    fn bwrap_write_protects_existing_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir(&git).unwrap();
        let sofos = dir.path().join(".sofos");

        let policy = SandboxPolicy::for_workspace(dir.path());
        let args: Vec<String> = bwrap_arguments(&policy)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let git = git.to_string_lossy().into_owned();
        let robind = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == git && w[2] == git)
            .expect("existing .git re-bound read-only");
        let bind = args.iter().position(|a| a == "--bind").unwrap();
        assert!(
            robind > bind,
            "metadata ro-bind must follow the writable bind"
        );

        let sofos = sofos.to_string_lossy().into_owned();
        assert!(
            args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == sofos),
            "a missing metadata dir is masked with a tmpfs"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--remount-ro" && w[1] == sofos),
            "the tmpfs mask is remounted read-only"
        );
    }

    /// On a live bubblewrap, a confined command writes inside the
    /// workspace but cannot write into `.git`, which stays readable.
    /// Skipped where bubblewrap cannot create user namespaces.
    #[test]
    fn bwrap_write_protects_metadata_end_to_end() {
        if !bwrap_can_unshare_namespaces() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(dir.path()).unwrap();
        let git = workspace.join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(git.join("config"), "[core]\n").unwrap();

        let policy = SandboxPolicy::for_workspace(&workspace);
        let run = |command: String| {
            let (program, args) = super::super::confined_invocation(
                std::ffi::OsStr::new("/bin/sh"),
                &command,
                &policy,
            )
            .unwrap();
            std::process::Command::new(program)
                .args(args)
                .output()
                .expect("spawn bwrap")
        };

        let _ = run(format!(
            "echo ok > {}",
            workspace.join("file.txt").display()
        ));
        assert!(
            workspace.join("file.txt").is_file(),
            "workspace write blocked"
        );

        let _ = run(format!("echo hacked >> {}", git.join("config").display()));
        assert_eq!(
            std::fs::read_to_string(git.join("config")).unwrap(),
            "[core]\n",
            ".git must stay read-only"
        );

        let read = run(format!("cat {}", git.join("config").display()));
        assert!(read.status.success(), ".git must stay readable");
    }
}
