//! Output capping and signal-name lookup for the bash executor. Both
//! stdout and stderr are capped at [`MAX_BASH_OUTPUT_BYTES`] before
//! truncation — large per-stream output is rejected outright so the
//! tool result stays under the API's payload ceiling.

use std::time::Duration;

/// Hard per-stream byte cap on bash output. Past this the executor
/// returns a `ToolExecution` error rather than truncating, so the
/// model sees the failure clearly instead of a silently chopped
/// `stdout` that might happen to end mid-statement.
pub(super) const MAX_BASH_OUTPUT_BYTES: usize = 10 * 1024 * 1024;

/// Wall-clock ceiling on a single bash invocation. Past this the
/// supervisor kills the child process tree and surfaces a clear
/// "timeout" message instead of blocking the turn forever on a stuck
/// build, hung test runner, or accidental `tail -f`.
pub(super) const BASH_COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the supervisor polls for child exit, output overflow,
/// interrupt, and timeout. Short enough that the user does not feel
/// latency on ESC; long enough that the loop is not a busy wait.
pub(super) const SUPERVISOR_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Why the supervisor terminated a running bash command before its
/// natural exit. The executor maps each variant to a distinct error
/// message so the model can recognise the failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminationReason {
    Timeout,
    Interrupt,
    StdoutCapExceeded,
    StderrCapExceeded,
}

/// Convert a Unix signal number to its standard short name. Used by
/// the executor when a command was killed by a signal rather than
/// exiting normally — `signal: 9 (SIGKILL)` reads better than the
/// bare integer for both humans and the model.
#[cfg(unix)]
pub(super) fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "unknown",
    }
}
