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
        .chain(&policy.write_protect_subpaths)
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
    // reaching another terminal. The pattern is end-anchored so it matches
    // exactly one device path and nothing that merely starts with it.
    profile.push_str(
        "  (require-all (regex #\"^/dev/ttys[0-9]+$\") (extension \"com.apple.sandbox.pty\"))\n",
    );
    profile.push_str(")\n");

    // Keep project metadata read-only. Placed after the writable-root
    // allow so Seatbelt's last-matching rule denies writes here, while
    // reads and the rest of the workspace are untouched.
    for path in &policy.write_protect_subpaths {
        profile.push_str("(deny file-write* (subpath ");
        profile.push_str(&quote(&path.to_string_lossy()));
        profile.push_str("))\n");
    }

    // Read confinement: deny the configured subpaths, then re-open the
    // explicit exceptions. The allow rules come last so a specific allow
    // overrides a broader deny.
    for path in &policy.read_deny_subpaths {
        push_file_read_rule(&mut profile, "deny", path);
    }
    // Glob read-denies that could not be a subpath mask (a system tree
    // outside the workspace and home) become precise regex denies. A glob
    // we cannot translate safely refuses confinement rather than emitting a
    // rule that lets some reads through.
    for glob in &policy.read_deny_globs {
        let regex = glob_to_seatbelt_regex(glob)?;
        profile.push_str("(deny file-read* (regex #\"");
        profile.push_str(&regex);
        profile.push_str("\"))\n");
    }
    for path in &policy.read_allow_subpaths {
        push_file_read_rule(&mut profile, "allow", path);
    }

    Some(profile)
}

/// Convert an absolute glob into a Seatbelt `file-read*` regex body, or
/// `None` when it uses a construct we will not translate precisely. `*`
/// matches within a path segment, `**` crosses separators, and `?` matches
/// one non-separator character; every other character is matched literally,
/// with regex metacharacters escaped. Character classes (`[`, `{`), a double
/// quote (which would close the regex literal), and control characters are
/// refused so the caller falls back to refusing confinement rather than
/// emitting a rule that lets some reads through. The result is anchored so
/// it matches a whole path.
fn glob_to_seatbelt_regex(glob: &str) -> Option<String> {
    if glob.contains(|c: char| c.is_control() || matches!(c, '"' | '[' | ']' | '{' | '}')) {
        return None;
    }
    let mut regex = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    regex.push_str(".*");
                    // `**` spans whole components, including zero, so absorb
                    // the trailing separator — otherwise `/a/**/b` would
                    // require a middle directory and miss `/a/b`.
                    if chars.peek() == Some(&'/') {
                        chars.next();
                    }
                } else {
                    regex.push_str("[^/]*");
                }
            }
            '?' => regex.push_str("[^/]"),
            '.' | '\\' | '+' | '(' | ')' | '^' | '$' | '|' => {
                regex.push('\\');
                regex.push(c);
            }
            other => regex.push(other),
        }
    }
    regex.push('$');
    Some(regex)
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
            read_deny_globs: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
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
            read_deny_globs: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
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
            read_deny_globs: Vec::new(),
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
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
        assert!(
            profile.contains("#\"^/dev/ttys[0-9]+$\""),
            "the pseudo-terminal slave pattern must be end-anchored"
        );
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
            read_deny_globs: Vec::new(),
            read_allow_subpaths: vec![public.clone()],
            write_protect_subpaths: Vec::new(),
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

    #[test]
    fn glob_to_seatbelt_regex_translates_globs_and_refuses_unsafe() {
        assert_eq!(
            glob_to_seatbelt_regex("/etc/ssl/private/*.key").unwrap(),
            r"^/etc/ssl/private/[^/]*\.key$"
        );
        // `**` absorbs its trailing separator so it matches zero directories
        // too: `/a/**/b` must still match `/a/b`.
        assert_eq!(glob_to_seatbelt_regex("/a/**/b").unwrap(), r"^/a/.*b$");
        assert_eq!(
            glob_to_seatbelt_regex("/etc/**/passwd").unwrap(),
            r"^/etc/.*passwd$"
        );
        assert_eq!(glob_to_seatbelt_regex("/a/?.c").unwrap(), r"^/a/[^/]\.c$");
        // Constructs we will not translate precisely refuse confinement.
        assert!(glob_to_seatbelt_regex("/a/[ab].key").is_none());
        assert!(glob_to_seatbelt_regex("/a/{x,y}").is_none());
        assert!(glob_to_seatbelt_regex("/a/\"q").is_none());
        assert!(glob_to_seatbelt_regex("/a/\nb").is_none());
    }

    #[test]
    fn seatbelt_profile_emits_regex_deny_for_glob_and_refuses_unconvertible() {
        let dir = tempfile::tempdir().unwrap();
        let mut policy = SandboxPolicy::for_workspace(dir.path());
        policy.read_deny_globs = vec!["/etc/ssl/private/*.key".to_string()];
        let profile = seatbelt_profile(&policy).expect("a convertible glob builds a profile");
        assert!(profile.contains("(deny file-read* (regex"));
        assert!(profile.contains(r"^/etc/ssl/private/[^/]*\.key$"));

        policy.read_deny_globs = vec!["/etc/[ab].key".to_string()];
        assert!(
            seatbelt_profile(&policy).is_none(),
            "an unconvertible glob refuses confinement"
        );
    }

    /// End-to-end S6d proof: a glob read-deny onto a tree outside the
    /// workspace and home is enforced by a seatbelt regex, so a confined
    /// command cannot read a matching file even by reaching it indirectly
    /// through a variable. A sibling that does not match stays readable.
    #[test]
    fn seatbelt_blocks_reads_matching_a_glob_deny_including_indirect() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(workspace_dir.path()).unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let keys = std::fs::canonicalize(outside_dir.path())
            .unwrap()
            .join("keys");
        std::fs::create_dir(&keys).unwrap();
        let secret = keys.join("id.key");
        std::fs::write(&secret, "top secret").unwrap();
        let public = keys.join("note.txt");
        std::fs::write(&public, "fine to read").unwrap();

        let policy = SandboxPolicy {
            writable_roots: vec![workspace],
            allow_network: false,
            read_deny_subpaths: Vec::new(),
            read_deny_globs: vec![format!("{}/*.key", keys.display())],
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
        };

        let run = |command: String| {
            let (program, args) =
                super::super::confined_invocation(OsStr::new("/bin/sh"), &command, &policy)
                    .unwrap();
            Command::new(program)
                .args(args)
                .output()
                .expect("spawn sandbox-exec")
        };

        assert!(
            !run(format!("cat {}", secret.display())).status.success(),
            "a direct read of a glob-denied file must be blocked"
        );
        // The kernel enforces it regardless of how the path reaches open(),
        // so an indirect read through a variable is blocked too.
        assert!(
            !run(format!("f={}; cat \"$f\"", secret.display()))
                .status
                .success(),
            "an indirect read of a glob-denied file must be blocked"
        );
        let allowed = run(format!("cat {}", public.display()));
        assert!(
            allowed.status.success(),
            "a non-matching sibling must stay readable; stderr: {}",
            String::from_utf8_lossy(&allowed.stderr)
        );
    }

    /// A `**` glob deny matches zero directories too, so the file directly
    /// under the prefix is blocked, not only nested ones (regression: `**`
    /// used to require a middle directory).
    #[test]
    fn seatbelt_double_star_glob_blocks_the_zero_directory_case() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(workspace_dir.path()).unwrap();
        let base_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(base_dir.path()).unwrap();
        let direct = base.join("secret");
        std::fs::write(&direct, "top secret").unwrap();
        let nested_dir = base.join("sub");
        std::fs::create_dir(&nested_dir).unwrap();
        let nested = nested_dir.join("secret");
        std::fs::write(&nested, "top secret").unwrap();

        let policy = SandboxPolicy {
            writable_roots: vec![workspace],
            allow_network: false,
            read_deny_subpaths: Vec::new(),
            read_deny_globs: vec![format!("{}/**/secret", base.display())],
            read_allow_subpaths: Vec::new(),
            write_protect_subpaths: Vec::new(),
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
        assert!(
            !read(&direct).status.success(),
            "the zero-directory match must be blocked"
        );
        assert!(
            !read(&nested).status.success(),
            "a nested match must be blocked"
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

    /// End-to-end proof that the macOS sandbox denies the network: a
    /// confined command cannot open a TCP connection that the same
    /// command makes successfully when unconfined. The target is a local
    /// listener so the unconfined control does not depend on outside
    /// network, and `(deny network*)` covers loopback too.
    #[test]
    fn seatbelt_blocks_outbound_network() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Keep accepting so the unconfined control connection completes;
        // the listener lives in this thread until the test process exits.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });

        let command = format!("/usr/bin/nc -z -w 1 127.0.0.1 {port}");

        let unconfined = Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .expect("spawn nc");
        assert!(
            unconfined.status.success(),
            "the unconfined control connection must succeed; stderr: {}",
            String::from_utf8_lossy(&unconfined.stderr)
        );

        let dir = tempfile::tempdir().unwrap();
        let run_confined = |allow_network: bool| {
            let policy = SandboxPolicy {
                allow_network,
                ..SandboxPolicy::for_workspace(dir.path())
            };
            let (program, args) =
                super::super::confined_invocation(OsStr::new("/bin/sh"), &command, &policy)
                    .unwrap();
            Command::new(program)
                .args(args)
                .output()
                .expect("spawn sandbox-exec")
        };

        // Positive control: with the network allowed the same confined
        // command launches and connects, so the denial below can only be
        // the network rule rather than the sandbox failing to start.
        let allowed = run_confined(true);
        assert!(
            allowed.status.success(),
            "a confined command must connect when the policy allows the network; stderr: {}",
            String::from_utf8_lossy(&allowed.stderr)
        );
        assert!(
            !run_confined(false).status.success(),
            "a confined command must not be able to open an outbound socket"
        );
    }

    /// The metadata write-deny lands after the writable-root allow, so
    /// the last-matching rule keeps `.git` read-only.
    #[test]
    fn seatbelt_profile_write_protects_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let policy = SandboxPolicy::for_workspace(dir.path());
        let profile = seatbelt_profile(&policy).expect("clean policy paths build a profile");
        let git = dir.path().join(".git");
        let deny = format!("(deny file-write* (subpath \"{}\"))", git.to_string_lossy());
        assert!(profile.contains(&deny), "missing metadata write-deny");
        assert!(
            profile.find(&deny).unwrap() > profile.find("(allow file-write*").unwrap(),
            "metadata deny must follow the writable-root allow"
        );
    }

    /// A confined command writes inside the workspace but cannot touch
    /// `.git` or `.sofos`, and `.git` stays readable.
    #[test]
    fn seatbelt_write_protects_metadata_end_to_end() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(workspace_dir.path()).unwrap();
        let git = workspace.join(".git");
        std::fs::create_dir(&git).unwrap();
        std::fs::write(git.join("config"), "[core]\n").unwrap();
        let sofos = workspace.join(".sofos");
        std::fs::create_dir(&sofos).unwrap();

        let policy = SandboxPolicy::for_workspace(&workspace);
        let run = |command: String| {
            let (program, args) =
                super::super::confined_invocation(OsStr::new("/bin/sh"), &command, &policy)
                    .unwrap();
            Command::new(program)
                .args(args)
                .output()
                .expect("spawn sandbox-exec")
        };

        run(format!(
            "echo ok > {}",
            workspace.join("file.txt").display()
        ));
        assert!(
            workspace.join("file.txt").is_file(),
            "workspace write blocked"
        );

        run(format!("echo hacked >> {}", git.join("config").display()));
        assert_eq!(
            std::fs::read_to_string(git.join("config")).unwrap(),
            "[core]\n",
            ".git must stay read-only"
        );

        run(format!(
            "printf x > {}",
            sofos.join("config.local.toml").display()
        ));
        assert!(
            !sofos.join("config.local.toml").exists(),
            ".sofos must stay read-only"
        );

        let read = run(format!("cat {}", git.join("config").display()));
        assert!(
            read.status.success() && String::from_utf8_lossy(&read.stdout).contains("[core]"),
            ".git must stay readable"
        );
    }
}
