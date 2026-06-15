//! Linux confinement via Bubblewrap.

use super::SandboxPolicy;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::{Duration, Instant};

/// The Bubblewrap binary, resolved from `PATH`.
pub const BWRAP_PROGRAM: &str = "bwrap";

/// Treat bwrap as unusable if the probe has not exited within this window.
const BWRAP_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const BWRAP_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Build the Bubblewrap arguments that confine writes to `policy`'s
/// roots: mount the whole filesystem read-only, re-bind each writable
/// root read-write, expose `/dev` and `/proc`, and close the network
/// unless the policy opens it. Denied read subpaths are masked —
/// `/dev/null` over a file, an empty tmpfs over a directory or over a
/// not-yet-created path inside a writable root where a secret could
/// appear later — and allow exceptions are re-bound on top. The confined
/// process runs in fresh user and process namespaces (`--unshare-user`,
/// `--unshare-pid`) so it cannot see or signal host processes, and it
/// dies with sofos
/// (`--die-with-parent`). The caller appends `-- <shell> -c <command>`.
pub fn bwrap_arguments(policy: &SandboxPolicy) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("--die-with-parent"),
        OsString::from("--unshare-user"),
        OsString::from("--unshare-pid"),
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

/// Probe whether Bubblewrap can actually create the user namespace it
/// relies on. `bwrap` may be installed yet unusable — user namespaces
/// are restricted on some hardened kernels, inside containers, and on
/// WSL1 — in which case it exits with a namespace error. Running a
/// throwaway `bwrap … /bin/true` tells the caller whether confinement
/// will work, so it can fall back to the permission prompt instead of
/// failing every command with an unclear bwrap error. A short timeout
/// guards against a bwrap that hangs.
pub fn bwrap_can_unshare_user() -> bool {
    use std::process::{Command, Stdio};

    let mut child = match Command::new(BWRAP_PROGRAM)
        .args([
            "--unshare-user",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "/bin/true",
        ])
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
        let _ = bwrap_can_unshare_user();
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
}
