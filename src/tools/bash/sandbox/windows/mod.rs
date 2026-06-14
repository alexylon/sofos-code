//! Windows workspace sandbox.
//!
//! Windows ships no single-binary equivalent of `sandbox-exec` or
//! `bwrap`, so the confinement is assembled from three pieces:
//!
//! 1. A per-workspace security identifier persisted at
//!    `<workspace>/.sofos/cap_sid`.
//! 2. An inheritable allow-write rule for that identifier on every
//!    writable root (workspace + temporary directories).
//! 3. A restricted access token built from the current process token,
//!    naming that identifier in its restricting set, used to spawn the
//!    shell.
//!
//! Writes inside the writable roots succeed because the kernel
//! intersects "the user can write here" with "the restricting
//! identifier is allowed here". Writes elsewhere are refused because
//! no rule names the identifier. Reads stay open and the existing
//! permission gate keeps reading-from-external-paths in check.
//!
//! Network confinement is not provided on Windows: closing the network
//! the way codex does needs administrator privileges and applies to
//! the whole user account.

mod acl;
mod cap;
mod proc_thread_attr;
mod process;
mod token;
mod winutil;

use super::SandboxPolicy;
use crate::tools::bash::output::{
    BASH_COMMAND_TIMEOUT, BASH_READ_CHUNK_BYTES, MAX_BASH_OUTPUT_BYTES, SUPERVISOR_POLL_INTERVAL,
    TerminationReason,
};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};

/// Grace period the supervisor waits for a child to actually exit after
/// `TerminateProcess` is called, before reading the exit code.
const TERMINATE_WAIT_MS: u32 = 2_000;

/// Output of a supervised confined run, matching the shape the Unix
/// supervisor in `executor.rs` produces.
pub(in crate::tools::bash) struct SupervisedOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub terminated_for: Option<TerminationReason>,
}

/// Whether the Windows sandbox can run on this machine. The probe
/// opens the current process token, which is a prerequisite for any
/// real spawn. Kept available for future re-enabling once the shell
/// story is solved; the parent `sandbox` module currently reports
/// `is_available` as false on Windows so this is not on the hot path.
#[allow(dead_code)]
pub fn is_available() -> bool {
    unsafe {
        match token::get_current_token_for_restriction() {
            Ok(h) => {
                CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// Spawn `<shell> -c <command>` under the workspace sandbox and
/// supervise it to completion: per-stream byte caps, wall-clock
/// timeout, and the shared interrupt flag, matching the Unix path in
/// `executor::spawn_supervised`.
pub(in crate::tools::bash) fn run_confined(
    shell: &OsStr,
    command: &str,
    workspace: &Path,
    extra_path_dir: Option<&Path>,
    policy: &SandboxPolicy,
    interrupt_flag: &AtomicBool,
) -> io::Result<SupervisedOutput> {
    let cap_sid_string = cap::workspace_cap_sid(workspace)?;
    let cap_sid = token::LocalSid::from_string(&cap_sid_string)?;
    let cap_ptrs: [*mut std::ffi::c_void; 1] = [cap_sid.as_ptr()];

    for root in &policy.writable_roots {
        if root.is_dir() {
            unsafe {
                acl::ensure_allow_write_aces(root, &cap_ptrs)?;
            }
        }
    }

    let restricted_token = unsafe {
        let base = token::get_current_token_for_restriction()?;
        let result = token::create_workspace_write_token_with_caps_from(base, &cap_ptrs);
        CloseHandle(base);
        result?
    };
    let _token_guard = TokenHandle(restricted_token);

    let env_map = build_child_env(extra_path_dir);
    let argv = vec![
        shell.to_string_lossy().into_owned(),
        "-c".to_string(),
        command.to_string(),
    ];

    let (stdin_read, stdin_write) = create_pipe(true, false)?;
    let (stdout_read, stdout_write) = create_pipe(false, true).inspect_err(|_| unsafe {
        CloseHandle(stdin_read);
        CloseHandle(stdin_write);
    })?;
    let (stderr_read, stderr_write) = create_pipe(false, true).inspect_err(|_| unsafe {
        CloseHandle(stdin_read);
        CloseHandle(stdin_write);
        CloseHandle(stdout_read);
        CloseHandle(stdout_write);
    })?;

    let pi = unsafe {
        process::create_process_as_user(
            restricted_token,
            &argv,
            workspace,
            &env_map,
            (stdin_read, stdout_write, stderr_write),
        )
    };
    // Drop the parent's references to the child-side handles so the
    // child sees EOF on stdin and the readers see EOF on stdout/stderr
    // once the child exits.
    unsafe {
        CloseHandle(stdin_read);
        CloseHandle(stdin_write);
        CloseHandle(stdout_write);
        CloseHandle(stderr_write);
    }
    let pi = match pi {
        Ok(pi) => pi,
        Err(err) => {
            unsafe {
                CloseHandle(stdout_read);
                CloseHandle(stderr_read);
            }
            return Err(err);
        }
    };
    let _process_guard = ProcessHandles {
        process: pi.hProcess,
        thread: pi.hThread,
    };

    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stdout_overflow = Arc::new(AtomicBool::new(false));
    let stderr_overflow = Arc::new(AtomicBool::new(false));

    let stdout_handle = spawn_pipe_reader(
        stdout_read,
        Arc::clone(&stdout_buf),
        Arc::clone(&stdout_overflow),
    );
    let stderr_handle = spawn_pipe_reader(
        stderr_read,
        Arc::clone(&stderr_buf),
        Arc::clone(&stderr_overflow),
    );

    let start = Instant::now();
    let mut termination: Option<TerminationReason> = None;
    loop {
        let wait = unsafe { WaitForSingleObject(pi.hProcess, 0) };
        if wait == WAIT_OBJECT_0 {
            break;
        }
        // WAIT_FAILED on a freshly-spawned process handle is unusual;
        // fall into the kill-and-reap path so the child is not orphaned.
        if wait != WAIT_TIMEOUT {
            termination = Some(TerminationReason::Timeout);
            break;
        }

        if interrupt_flag.load(Ordering::SeqCst) {
            termination = Some(TerminationReason::Interrupt);
            break;
        }
        if start.elapsed() > BASH_COMMAND_TIMEOUT {
            termination = Some(TerminationReason::Timeout);
            break;
        }
        if stdout_overflow.load(Ordering::SeqCst) {
            termination = Some(TerminationReason::StdoutCapExceeded);
            break;
        }
        if stderr_overflow.load(Ordering::SeqCst) {
            termination = Some(TerminationReason::StderrCapExceeded);
            break;
        }

        thread::sleep(SUPERVISOR_POLL_INTERVAL);
    }

    if termination.is_some() {
        // `TerminateProcess` is asynchronous, so wait for the actual
        // exit before reading the exit code.
        unsafe {
            TerminateProcess(pi.hProcess, 1);
            WaitForSingleObject(pi.hProcess, TERMINATE_WAIT_MS);
        }
    }

    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    if termination.is_none() {
        if stdout_overflow.load(Ordering::SeqCst) {
            termination = Some(TerminationReason::StdoutCapExceeded);
        } else if stderr_overflow.load(Ordering::SeqCst) {
            termination = Some(TerminationReason::StderrCapExceeded);
        }
    }

    let mut exit_code: u32 = 0;
    unsafe {
        GetExitCodeProcess(pi.hProcess, &mut exit_code);
    }

    let stdout = drain_into_vec(stdout_buf);
    let stderr = drain_into_vec(stderr_buf);

    Ok(SupervisedOutput {
        stdout,
        stderr,
        exit_code: Some(exit_code as i32),
        terminated_for: termination,
    })
}

/// Inherit the parent environment with `extra_path_dir` prepended to
/// `PATH` and `CYGWIN=nontsec` / `MSYS=nontsec` appended so the Git for
/// Windows runtime relaxes the security checks that misbehave under a
/// `WRITE_RESTRICTED` token.
fn build_child_env(extra_path_dir: Option<&Path>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    if let Some(extra) = extra_path_dir {
        let original = env.remove("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = vec![extra.to_path_buf()];
        dirs.extend(std::env::split_paths(&original));
        if let Ok(joined) = std::env::join_paths(dirs) {
            env.insert("PATH".to_string(), joined.to_string_lossy().into_owned());
        } else {
            env.insert("PATH".to_string(), original);
        }
    }
    let cygwin = env.remove("CYGWIN").unwrap_or_default();
    env.insert("CYGWIN".to_string(), merge_token(&cygwin, "nontsec"));
    let msys = env.remove("MSYS").unwrap_or_default();
    env.insert("MSYS".to_string(), merge_token(&msys, "nontsec"));
    env
}

fn merge_token(existing: &str, token: &str) -> String {
    if existing.split_whitespace().any(|t| t == token) {
        existing.to_string()
    } else if existing.is_empty() {
        token.to_string()
    } else {
        format!("{existing} {token}")
    }
}

fn create_pipe(inherit_read: bool, inherit_write: bool) -> io::Result<(HANDLE, HANDLE)> {
    let mut read: HANDLE = ptr::null_mut();
    let mut write: HANDLE = ptr::null_mut();
    let ok = unsafe { CreatePipe(&mut read, &mut write, ptr::null_mut(), 0) };
    if ok == 0 {
        return Err(io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    unsafe {
        if inherit_read {
            SetHandleInformation(read, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        }
        if inherit_write {
            SetHandleInformation(write, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        }
    }
    Ok((read, write))
}

fn spawn_pipe_reader(
    handle: HANDLE,
    buf: Arc<Mutex<Vec<u8>>>,
    overflow: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    // `*mut c_void` is not Send; an integer is. Cast on both sides.
    let raw: usize = handle as usize;
    thread::spawn(move || {
        let handle = raw as HANDLE;
        let mut chunk = [0u8; BASH_READ_CHUNK_BYTES];
        loop {
            let mut read: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    handle,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    &mut read,
                    ptr::null_mut(),
                )
            };
            if ok == 0 || read == 0 {
                break;
            }
            if overflow.load(Ordering::Relaxed) {
                continue;
            }
            let Ok(mut stored) = buf.lock() else {
                break;
            };
            let remaining = MAX_BASH_OUTPUT_BYTES.saturating_sub(stored.len());
            if remaining == 0 {
                overflow.store(true, Ordering::SeqCst);
                continue;
            }
            let take = (read as usize).min(remaining);
            stored.extend_from_slice(&chunk[..take]);
            if take < read as usize {
                overflow.store(true, Ordering::SeqCst);
            }
        }
        unsafe {
            CloseHandle(handle);
        }
    })
}

fn drain_into_vec(buf: Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    Arc::try_unwrap(buf)
        .map(|inner| inner.into_inner().unwrap_or_default())
        .unwrap_or_else(|shared| shared.lock().map(|guard| guard.clone()).unwrap_or_default())
}

struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct ProcessHandles {
    process: HANDLE,
    thread: HANDLE,
}

impl Drop for ProcessHandles {
    fn drop(&mut self) {
        unsafe {
            if !self.thread.is_null() {
                CloseHandle(self.thread);
            }
            if !self.process.is_null() {
                CloseHandle(self.process);
            }
        }
    }
}
