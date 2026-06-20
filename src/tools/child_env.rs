//! Removing sensitive variables from the environment a child process
//! inherits. Shell commands and ripgrep run with the rest of the
//! environment intact — build tools and interpreters need it — but must
//! not be handed Sofos's own credentials or a way to inject code.

use std::ffi::OsString;
use std::process::Command;

/// Environment variables that carry Sofos's own API credentials. They are
/// read at start-up through the `env` declarations in `cli.rs`, so every
/// child process would otherwise inherit them and could print them into
/// its output or write them to disk. Keep this list in step with the
/// `#[arg(env = "...")]` keys in `cli.rs`; the `scrub_list_covers_*` test
/// fails if a credential argument is added there without one here.
const SECRET_ENV_KEYS: &[&str] = &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "MORPH_API_KEY"];

/// Loader variables ordinary builds rely on, kept in the child even though
/// the rest of the `LD_*` / `DYLD_*` family is stripped: the library search
/// paths and the linker's run-path.
const LOADER_KEEP_ENV_KEYS: &[&str] = &["LD_LIBRARY_PATH", "LD_RUN_PATH", "DYLD_LIBRARY_PATH"];

/// True for a dynamic-loader variable that can force or redirect what a child
/// loads (`LD_PRELOAD`, `DYLD_FRAMEWORK_PATH`, `DYLD_FORCE_FLAT_NAMESPACE`, …).
/// Matching the whole `LD_*` / `DYLD_*` family by prefix (minus the build
/// paths above) also catches loader knobs a future OS release adds, and
/// mirrors how `command_parse` already treats an inline `DYLD_*=` assignment.
fn is_loader_injection_var(key: &str) -> bool {
    (key.starts_with("LD_") || key.starts_with("DYLD_")) && !LOADER_KEEP_ENV_KEYS.contains(&key)
}

/// Remove Sofos's API credentials and the dynamic-loader injection
/// variables from `cmd`'s environment before it is spawned. The child
/// still inherits every other variable, so commands that rely on the
/// environment keep working.
pub(crate) fn scrub_sensitive_env(cmd: &mut Command) {
    scrub_env_from(cmd, std::env::vars_os());
}

/// Body of [`scrub_sensitive_env`] with the inherited environment passed in,
/// so the prefix sweep can be tested without mutating the process.
fn scrub_env_from(cmd: &mut Command, inherited: impl Iterator<Item = (OsString, OsString)>) {
    for key in SECRET_ENV_KEYS {
        cmd.env_remove(key);
    }
    // The loader family is matched by prefix, so the keys can only be named
    // by scanning what the child would actually inherit.
    for (key, _) in inherited {
        if is_loader_injection_var(&key.to_string_lossy()) {
            cmd.env_remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the keys `scrub_env_from` marked for removal.
    fn removed_keys(cmd: &Command) -> Vec<String> {
        cmd.get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect()
    }

    fn fake_env(keys: &[&str]) -> Vec<(OsString, OsString)> {
        keys.iter()
            .map(|k| (OsString::from(k), OsString::from("x")))
            .collect()
    }

    #[test]
    fn loader_predicate_strips_the_family_but_keeps_build_paths() {
        for key in [
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_DEBUG",
            "DYLD_INSERT_LIBRARIES",
            "DYLD_FRAMEWORK_PATH",
            "DYLD_FALLBACK_FRAMEWORK_PATH",
            "DYLD_FORCE_FLAT_NAMESPACE",
            "DYLD_VERSIONED_LIBRARY_PATH",
        ] {
            assert!(is_loader_injection_var(key), "{key} should be stripped");
        }
        for key in [
            "LD_LIBRARY_PATH",
            "LD_RUN_PATH",
            "DYLD_LIBRARY_PATH",
            "PATH",
            "LDFLAGS",
            "HOME",
        ] {
            assert!(!is_loader_injection_var(key), "{key} should be kept");
        }
    }

    #[test]
    fn scrub_removes_secrets_and_the_loader_family() {
        let inherited = fake_env(&[
            "DYLD_INSERT_LIBRARIES",
            "DYLD_FRAMEWORK_PATH",
            "DYLD_FALLBACK_FRAMEWORK_PATH",
            "DYLD_FORCE_FLAT_NAMESPACE",
            "DYLD_LIBRARY_PATH",
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_DEBUG",
            "LD_LIBRARY_PATH",
            "LD_RUN_PATH",
            "PATH",
            "HOME",
        ]);

        let mut cmd = Command::new("true");
        scrub_env_from(&mut cmd, inherited.into_iter());
        let removed = removed_keys(&cmd);

        // Credentials are always removed, even when absent from the scan.
        for key in SECRET_ENV_KEYS {
            assert!(removed.contains(&key.to_string()), "{key} must be removed");
        }
        // The loader-injection family is removed across both prefixes.
        for key in [
            "DYLD_INSERT_LIBRARIES",
            "DYLD_FRAMEWORK_PATH",
            "DYLD_FALLBACK_FRAMEWORK_PATH",
            "DYLD_FORCE_FLAT_NAMESPACE",
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_DEBUG",
        ] {
            assert!(removed.contains(&key.to_string()), "{key} must be removed");
        }
        // Build search/link paths and unrelated vars stay.
        for key in [
            "DYLD_LIBRARY_PATH",
            "LD_LIBRARY_PATH",
            "LD_RUN_PATH",
            "PATH",
            "HOME",
        ] {
            assert!(!removed.contains(&key.to_string()), "{key} must be kept");
        }
    }

    /// Every credential command-line argument in `cli.rs` must be covered
    /// by [`SECRET_ENV_KEYS`], so a new key/secret/token/password cannot be
    /// added there and silently leak into child processes. The markers are
    /// matched as substrings so a future `AZURE_OPENAI_KEY`, `*_SECRET`, or
    /// `*_PASSWORD` argument is caught, not only `*_API_KEY` / `*_TOKEN`.
    #[test]
    fn scrub_list_covers_every_credential_argument() {
        use clap::CommandFactory;

        const CREDENTIAL_MARKERS: [&str; 6] =
            ["KEY", "SECRET", "TOKEN", "PASSWORD", "PASSWD", "CREDENTIAL"];

        for arg in crate::cli::Cli::command().get_arguments() {
            let Some(env) = arg.get_env() else { continue };
            let env = env.to_string_lossy();
            let upper = env.to_uppercase();
            if CREDENTIAL_MARKERS
                .iter()
                .any(|marker| upper.contains(marker))
            {
                assert!(
                    SECRET_ENV_KEYS.contains(&env.as_ref()),
                    "{env} looks like a credential argument in cli.rs but is not scrubbed from child processes"
                );
            }
        }
    }
}
