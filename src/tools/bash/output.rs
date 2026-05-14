//! Output capping and signal-name lookup for the bash executor. Both
//! stdout and stderr are capped at [`MAX_BASH_OUTPUT_BYTES`] before
//! truncation — large per-stream output is rejected outright so the
//! tool result stays under the API's payload ceiling.

/// Hard per-stream byte cap on bash output. Past this the executor
/// returns a `ToolExecution` error rather than truncating, so the
/// model sees the failure clearly instead of a silently chopped
/// `stdout` that might happen to end mid-statement.
pub(super) const MAX_BASH_OUTPUT_BYTES: usize = 10 * 1024 * 1024;

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
