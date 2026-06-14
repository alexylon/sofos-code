//! Linux confinement via Bubblewrap.

use super::SandboxPolicy;
use std::ffi::OsString;

/// The Bubblewrap binary, resolved from `PATH`.
pub const BWRAP_PROGRAM: &str = "bwrap";

/// Build the Bubblewrap arguments that confine writes to `policy`'s
/// roots: mount the whole filesystem read-only, re-bind each writable
/// root read-write, expose `/dev` and `/proc`, and close the network
/// unless the policy opens it. The caller appends `-- <shell> -c
/// <command>`.
pub fn bwrap_arguments(policy: &SandboxPolicy) -> Vec<OsString> {
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
}
