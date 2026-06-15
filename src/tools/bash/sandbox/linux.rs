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
/// unless the policy opens it. The confined process runs in fresh user
/// and process namespaces (`--unshare-user`, `--unshare-pid`) so it
/// cannot see or signal host processes, and it dies with sofos
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
}
