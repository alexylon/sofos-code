//! Removing sensitive variables from the environment a child process
//! inherits. Shell commands and ripgrep run with the rest of the
//! environment intact — build tools and interpreters need it — but must
//! not be handed Sofos's own credentials or a way to inject code.

use std::process::Command;

/// Environment variables that carry Sofos's own API credentials. They are
/// read at start-up through the `env` declarations in `cli.rs`, so every
/// child process would otherwise inherit them and could print them into
/// its output or write them to disk. Keep this list in step with the
/// `#[arg(env = "...")]` keys in `cli.rs`; the `scrub_list_covers_*` test
/// fails if a credential argument is added there without one here.
const SECRET_ENV_KEYS: &[&str] = &["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "MORPH_API_KEY"];

/// Dynamic-loader variables that force a library into a process at start.
/// They are removed so a value inherited by Sofos cannot inject code into
/// the commands Sofos runs. The library *search-path* variables
/// (`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`) are deliberately left in place
/// because ordinary builds rely on them.
const LOADER_INJECTION_ENV_KEYS: &[&str] = &["LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES"];

/// Remove Sofos's API credentials and the dynamic-loader injection
/// variables from `cmd`'s environment before it is spawned. The child
/// still inherits every other variable, so commands that rely on the
/// environment keep working.
pub(crate) fn scrub_sensitive_env(cmd: &mut Command) {
    for key in SECRET_ENV_KEYS.iter().chain(LOADER_INJECTION_ENV_KEYS) {
        cmd.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the keys `scrub_sensitive_env` marked for removal.
    fn removed_keys(cmd: &Command) -> Vec<String> {
        cmd.get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn scrub_removes_secrets_and_loader_injection() {
        let mut cmd = Command::new("true");
        scrub_sensitive_env(&mut cmd);
        let removed = removed_keys(&cmd);

        for key in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "MORPH_API_KEY",
            "LD_PRELOAD",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
        ] {
            assert!(
                removed.contains(&key.to_string()),
                "{key} must be removed from a child's environment"
            );
        }

        // A library search path is left in place for ordinary builds.
        assert!(
            !removed.contains(&"LD_LIBRARY_PATH".to_string()),
            "LD_LIBRARY_PATH must not be stripped"
        );
    }

    /// Every credential command-line argument in `cli.rs` must be covered
    /// by [`SECRET_ENV_KEYS`], so a new API key cannot be added there and
    /// silently leak into child processes.
    #[test]
    fn scrub_list_covers_every_credential_argument() {
        use clap::CommandFactory;

        for arg in crate::cli::Cli::command().get_arguments() {
            let Some(env) = arg.get_env() else { continue };
            let env = env.to_string_lossy();
            if env.ends_with("_API_KEY") || env.ends_with("_TOKEN") {
                assert!(
                    SECRET_ENV_KEYS.contains(&env.as_ref()),
                    "{env} is a credential argument in cli.rs but is not scrubbed from child processes"
                );
            }
        }
    }
}
