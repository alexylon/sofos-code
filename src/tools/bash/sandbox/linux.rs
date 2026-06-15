//! Linux confinement via Bubblewrap.

use super::SandboxPolicy;
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
/// unless the policy opens it. `--die-with-parent` ties the confined
/// process to sofos so it cannot outlive an abrupt parent exit. The
/// caller appends `-- <shell> -c <command>`.
pub fn bwrap_arguments(policy: &SandboxPolicy) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        OsString::from("--die-with-parent"),
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
}
