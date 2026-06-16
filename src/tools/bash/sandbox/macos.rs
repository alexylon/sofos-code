//! macOS confinement via the Seatbelt profile compiler.

use super::SandboxPolicy;

/// The Seatbelt profile compiler shipped with macOS.
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Build a Seatbelt profile that allows everything by default, then
/// closes the two things the workspace boundary cares about: writes
/// outside the writable roots, and the network. Reads and process
/// execution stay open so ordinary tools keep working.
///
/// Returns `None` when any path interpolated into the profile contains a
/// control character. `quote` escapes the backslash and double-quote
/// that would break a string literal, but a newline or other control
/// character could still break the profile's line structure and weaken a
/// rule, so the caller is made to fall back to asking instead of
/// confining with a malformed profile.
pub fn seatbelt_profile(policy: &SandboxPolicy) -> Option<String> {
    if policy
        .writable_roots
        .iter()
        .chain(&policy.read_deny_subpaths)
        .chain(&policy.read_allow_subpaths)
        .any(|path| path.to_string_lossy().contains(|c: char| c.is_control()))
    {
        return None;
    }

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
    // Character devices a normal shell pipeline needs to write to. The
    // pseudo-terminal master is included so a tool can allocate a PTY.
    for device in [
        "/dev/null",
        "/dev/stdout",
        "/dev/stderr",
        "/dev/tty",
        "/dev/ptmx",
    ] {
        profile.push_str("  (literal ");
        profile.push_str(&quote(device));
        profile.push_str(")\n");
    }
    // Pseudo-terminal slaves (`/dev/ttysNNN`), restricted to those the
    // command allocates inside the sandbox: the kernel tags those with
    // this extension, so a confined command can drive its own PTY without
    // reaching another terminal.
    profile.push_str(
        "  (require-all (regex #\"^/dev/ttys[0-9]+\") (extension \"com.apple.sandbox.pty\"))\n",
    );
    profile.push_str(")\n");

    // Read confinement: deny the configured subpaths, then re-open the
    // explicit exceptions. The allow rules come last so a specific allow
    // overrides a broader deny.
    for path in &policy.read_deny_subpaths {
        push_file_read_rule(&mut profile, "deny", path);
    }
    for path in &policy.read_allow_subpaths {
        push_file_read_rule(&mut profile, "allow", path);
    }

    Some(profile)
}

/// Append a `file-read*` rule (`deny` or `allow`) for `path`, scoped to
/// the whole subtree under it.
fn push_file_read_rule(profile: &mut String, verb: &str, path: &std::path::Path) {
    profile.push('(');
    profile.push_str(verb);
    profile.push_str(" file-read* (subpath ");
    profile.push_str(&quote(&path.to_string_lossy()));
    profile.push_str("))\n");
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
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    #[test]
    fn seatbelt_profile_denies_writes_and_network_then_reopens_roots() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        let profile = seatbelt_profile(&policy).expect("clean policy paths build a profile");
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(allow file-write*"));
    }

    /// A path with a control character cannot be expressed safely in the
    /// profile, so building one is refused and the caller falls back to
    /// asking instead of confining with a malformed profile.
    #[test]
    fn seatbelt_profile_refuses_paths_with_control_characters() {
        let mut policy = SandboxPolicy {
            writable_roots: vec![PathBuf::from("/workspace")],
            allow_network: false,
            read_deny_subpaths: Vec::new(),
            read_allow_subpaths: Vec::new(),
        };
        assert!(
            seatbelt_profile(&policy).is_some(),
            "an ordinary path builds a profile"
        );

        policy
            .writable_roots
            .push(PathBuf::from("/workspace/with\nnewline"));
        assert!(
            seatbelt_profile(&policy).is_none(),
            "a writable root containing a newline refuses confinement"
        );

        let denied = SandboxPolicy {
            writable_roots: vec![PathBuf::from("/workspace")],
            allow_network: false,
            read_deny_subpaths: vec![PathBuf::from("/workspace/secret\u{1}name")],
            read_allow_subpaths: Vec::new(),
        };
        assert!(
            seatbelt_profile(&denied).is_none(),
            "a denied read path containing a control character refuses confinement"
        );
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
            read_deny_subpaths: Vec::new(),
            read_allow_subpaths: Vec::new(),
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

    /// The profile reopens the pseudo-terminal master and the in-sandbox
    /// slave devices after the blanket file-write deny, so allocating a
    /// PTY is not blocked.
    #[test]
    fn seatbelt_profile_reopens_pty_devices() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        let profile = seatbelt_profile(&policy).expect("clean policy paths build a profile");
        assert!(profile.contains("(literal \"/dev/ptmx\")"));
        assert!(profile.contains("com.apple.sandbox.pty"));
    }

    /// End-to-end proof that a confined command can allocate a
    /// pseudo-terminal. macOS PTY setup writes to `/dev/ptmx` and a
    /// `/dev/ttys*` slave, both reopened after the blanket file-write
    /// deny. `script` allocates a PTY to run its command, so the run
    /// fails if that write is blocked.
    #[test]
    fn seatbelt_allows_pty_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        let command = "/usr/bin/script -q /dev/null /usr/bin/true";
        let (program, args) =
            super::super::confined_invocation(OsStr::new("/bin/sh"), command, &policy).unwrap();
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("spawn sandbox-exec");
        assert!(
            output.status.success(),
            "a confined command must be able to allocate a PTY; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A read-deny pattern becomes a `file-read*` deny rule, and a read
    /// allow becomes a following allow rule that overrides it.
    #[test]
    fn seatbelt_profile_denies_configured_read_subpaths() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path()).with_read_rules(
            dir.path(),
            &["./secret/**".to_string()],
            &["./secret/ok.txt".to_string()],
        );
        let profile = seatbelt_profile(&policy).expect("clean policy paths build a profile");
        assert!(profile.contains("(deny file-read* (subpath"));
        assert!(profile.contains("(allow file-read* (subpath"));
    }

    /// End-to-end proof that the macOS sandbox confines reads: a file in a
    /// denied subpath cannot be read by a confined command, while a
    /// specific allow exception under the same subpath stays readable.
    #[test]
    fn seatbelt_blocks_reads_of_denied_subpaths() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(workspace_dir.path()).unwrap();
        let secret_dir = workspace.join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        let secret = secret_dir.join("key.txt");
        std::fs::write(&secret, "top secret").unwrap();
        let public = secret_dir.join("public.txt");
        std::fs::write(&public, "fine to read").unwrap();

        let policy = SandboxPolicy {
            writable_roots: vec![workspace.clone()],
            allow_network: false,
            read_deny_subpaths: vec![secret_dir.clone()],
            read_allow_subpaths: vec![public.clone()],
        };

        let read = |target: &Path| {
            let command = format!("cat {}", target.display());
            let (program, args) =
                super::super::confined_invocation(OsStr::new("/bin/sh"), &command, &policy)
                    .unwrap();
            Command::new(program)
                .args(args)
                .output()
                .expect("spawn sandbox-exec")
        };

        let denied = read(&secret);
        assert!(
            !denied.status.success(),
            "a read of a denied subpath must be blocked"
        );

        let allowed = read(&public);
        assert!(
            allowed.status.success(),
            "a read of an allow exception must succeed; stderr: {}",
            String::from_utf8_lossy(&allowed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&allowed.stdout).trim(),
            "fine to read"
        );
    }

    /// A broad read-allow over the whole workspace must not re-open a
    /// denied secret. The allow is dropped because it is not a specific
    /// exception strictly inside the denied subtree.
    #[test]
    fn seatbelt_broad_read_allow_does_not_reopen_denied_subpath() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(workspace_dir.path()).unwrap();
        let secret_dir = workspace.join("secret");
        std::fs::create_dir(&secret_dir).unwrap();
        let secret = secret_dir.join("key.txt");
        std::fs::write(&secret, "top secret").unwrap();

        let policy = SandboxPolicy::for_workspace(&workspace).with_read_rules(
            &workspace,
            &[format!("{}/**", secret_dir.display())],
            &[format!("{}/**", workspace.display())],
        );
        let command = format!("cat {}", secret.display());
        let (program, args) =
            super::super::confined_invocation(OsStr::new("/bin/sh"), &command, &policy).unwrap();
        let output = Command::new(program)
            .args(args)
            .output()
            .expect("spawn sandbox-exec");
        assert!(
            !output.status.success(),
            "a broad allow must not re-open the denied secret"
        );
    }
}
