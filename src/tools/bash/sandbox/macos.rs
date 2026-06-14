//! macOS confinement via the Seatbelt profile compiler.

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

#[cfg(test)]
mod tests {
    use super::super::SandboxPolicy;
    use super::*;
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn seatbelt_profile_denies_writes_and_network_then_reopens_roots() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        let profile = seatbelt_profile(&policy);
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow file-write*"));
    }

    /// End-to-end proof that the macOS sandbox actually confines writes:
    /// a write inside the single writable root succeeds, and a write to a
    /// sibling directory outside it is blocked by the kernel. The policy
    /// deliberately omits the temporary directories so the sibling temp
    /// directory is a clean "outside" target.
    #[test]
    fn seatbelt_blocks_writes_outside_the_writable_root() {
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
                super::super::confined_invocation(OsStr::new("/bin/sh"), &command, &policy)
                    .unwrap();
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
}
