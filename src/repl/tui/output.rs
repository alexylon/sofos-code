//! Stdout/stderr capture for the TUI.
//!
//! Redirects fds 1 and 2 into OS pipes so every `println!`, `eprintln!`
//! and direct-to-stdout colored output from the worker thread flows into
//! the terminal's scrollback via `Terminal::insert_before`.
//!
//! Earlier versions of this module had `pause` / `resume` hooks that
//! un-did the redirection for the duration of a render frame so
//! `crossterm::cursor::position` could reach the tty. That had a
//! nasty side-effect: during pause, `println!` from the streaming
//! worker thread wrote *directly* onto the screen at whatever column
//! the cursor happened to be parked at, racing the rendered viewport
//! and landing as scattered text / orange fragments of "Assistant:".
//! Current code never calls `cursor::position` during the draw loop
//! (only once at startup, before this capture is installed), so the
//! redirection stays active for the whole session.

use os_pipe::{PipeReader, PipeWriter};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::io::IntoRawFd;
#[cfg(windows)]
use std::os::windows::io::IntoRawHandle;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::sync::mpsc::UnboundedSender;

use crate::repl::tui::event::{OutputKind, UiEvent};

/// Standard-output and standard-error file descriptors. POSIX fixes these
/// at 1 and 2; the Microsoft CRT matches. `libc` exposes `STDOUT_FILENO` /
/// `STDERR_FILENO` on Unix but not on Windows, so we keep our own aliases
/// to stay cfg-free at the call sites.
const STDOUT_FD: libc::c_int = 1;
const STDERR_FD: libc::c_int = 2;

/// Env var that, when set to a writable file path, makes every byte
/// read from the stdout/stderr capture pipes also get appended to
/// that file in a hex-escaped format. Intended purely for debugging
/// pipe-side rendering bugs (scattered text, CSI fragments, etc.) —
/// the file is ground truth for "what the UI actually received" and
/// keeps that stream separate from the TUI itself.
const RAW_LOG_ENV_VAR: &str = "SOFOS_RAW_LOG";

/// Shared handle to an optional hex-log file. `spawn_reader` clones
/// this across stdout and stderr reader threads so both streams land
/// in the same log file, tagged with their kind, preserving arrival
/// order between them via the mutex.
type RawLogSink = Option<Arc<Mutex<std::fs::File>>>;

fn open_raw_log() -> RawLogSink {
    let path = std::env::var(RAW_LOG_ENV_VAR).ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    // Truncate on first open so one `sofos` session gives one clean
    // file; append mode would accumulate bytes from previous runs and
    // make it harder to tell what came from *this* reproduction.
    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(file) => {
            // Write a header line so the user can confirm the file
            // is actually being created by *this* `sofos` binary
            // (vs a cached install that ignores the env var).
            let mut file = file;
            let _ = writeln!(file, "[sofos raw pipe log — path={path}]");
            let _ = file.flush();
            Some(Arc::new(Mutex::new(file)))
        }
        Err(err) => {
            // Surface the failure via stderr — which, at this point
            // in startup, is still the real tty (OutputCapture is
            // mid-install), so the user sees it on their terminal
            // rather than it getting swallowed by the pipe we're
            // about to wire up.
            eprintln!(
                "SOFOS_RAW_LOG: failed to open {path:?} for writing: {err}. \
                 Raw-log capture disabled for this session."
            );
            None
        }
    }
}

/// Append one captured line's raw bytes to the hex log. Format is
/// `[<kind>] <hex-escaped payload>\n` where printable ASCII is passed
/// through verbatim and everything else (ESC, CSI params, control
/// bytes, UTF-8 continuation bytes) appears as `\xNN`. That keeps
/// the file readable in a plain editor while preserving byte
/// boundaries exactly.
fn write_raw_log(sink: &RawLogSink, kind: OutputKind, bytes: &[u8]) {
    let Some(handle) = sink else { return };
    let Ok(mut file) = handle.lock() else { return };
    let mut rendered = format!("[{:?}] ", kind);
    for &b in bytes {
        if (0x20..=0x7e).contains(&b) && b != b'\\' {
            rendered.push(b as char);
        } else {
            rendered.push_str(&format!("\\x{:02X}", b));
        }
    }
    rendered.push('\n');
    let _ = file.write_all(rendered.as_bytes());
    let _ = file.flush();
}

pub struct OutputCapture {
    saved_stdout: libc::c_int,
    saved_stderr: libc::c_int,
    pipe_stdout: libc::c_int,
    pipe_stderr: libc::c_int,
}

impl OutputCapture {
    /// Redirect fds 1 and 2 to pipes and spawn reader threads that forward
    /// complete lines to the UI event channel.
    pub fn install(tx: UnboundedSender<UiEvent>) -> std::io::Result<Self> {
        let saved_stdout = dup_fd(STDOUT_FD)?;
        let saved_stderr = match dup_fd(STDERR_FD) {
            Ok(fd) => fd,
            Err(e) => {
                unsafe { libc::close(saved_stdout) };
                return Err(e);
            }
        };

        let (stdout_reader, stdout_writer) = match os_pipe::pipe() {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                }
                return Err(e);
            }
        };
        let (stderr_reader, stderr_writer) = match os_pipe::pipe() {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                }
                return Err(e);
            }
        };

        let pipe_stdout = match redirect(stdout_writer, STDOUT_FD) {
            Ok(fd) => fd,
            Err(e) => {
                unsafe {
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                }
                return Err(e);
            }
        };
        let pipe_stderr = match redirect(stderr_writer, STDERR_FD) {
            Ok(fd) => fd,
            Err(e) => {
                unsafe {
                    libc::dup2(saved_stdout, STDOUT_FD);
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                    libc::close(pipe_stdout);
                }
                return Err(e);
            }
        };

        // From here, any later failure must run `Drop` to restore fds.
        let this = OutputCapture {
            saved_stdout,
            saved_stderr,
            pipe_stdout,
            pipe_stderr,
        };

        let raw_log = open_raw_log();
        spawn_reader(
            stdout_reader,
            OutputKind::Stdout,
            tx.clone(),
            raw_log.clone(),
        )?;
        spawn_reader(stderr_reader, OutputKind::Stderr, tx, raw_log)?;

        Ok(this)
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved_stdout, STDOUT_FD);
            libc::dup2(self.saved_stderr, STDERR_FD);
            libc::close(self.saved_stdout);
            libc::close(self.saved_stderr);
            libc::close(self.pipe_stdout);
            libc::close(self.pipe_stderr);
        }
    }
}

fn dup_fd(fd: libc::c_int) -> std::io::Result<libc::c_int> {
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(dup)
    }
}

/// Redirect `target_fd` (usually 1 or 2) to the given pipe writer and return
/// the raw fd of that writer. The returned fd stays alive (not closed) so
/// the caller can dup2 it back over `target_fd` to resume redirection after
/// a `pause`.
fn redirect(writer: PipeWriter, target_fd: libc::c_int) -> std::io::Result<libc::c_int> {
    let write_fd = pipe_writer_into_fd(writer)?;
    let rc = unsafe { libc::dup2(write_fd, target_fd) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(write_fd) };
        return Err(err);
    }
    Ok(write_fd)
}

/// Take ownership of a `PipeWriter` and return a C-style file descriptor
/// suitable for `dup2`. On Unix the pipe is already an fd; on Windows
/// `os_pipe` hands us a `HANDLE`, which we register with the MSVCRT via
/// `_open_osfhandle` so the CRT's fd table knows about it. The fd then
/// owns the underlying handle — closing the fd closes the handle.
#[cfg(unix)]
fn pipe_writer_into_fd(writer: PipeWriter) -> std::io::Result<libc::c_int> {
    Ok(writer.into_raw_fd())
}

#[cfg(windows)]
fn pipe_writer_into_fd(writer: PipeWriter) -> std::io::Result<libc::c_int> {
    let handle = writer.into_raw_handle();
    let fd =
        unsafe { libc::open_osfhandle(handle as libc::intptr_t, libc::O_BINARY | libc::O_WRONLY) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

fn spawn_reader(
    reader: PipeReader,
    kind: OutputKind,
    tx: UnboundedSender<UiEvent>,
    raw_log: RawLogSink,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name(format!("sofos-{:?}-reader", kind))
        .spawn(move || {
            let mut buf = BufReader::new(reader);
            let mut line = Vec::<u8>::new();
            // Track SGR state across lines. The pipe reader delivers one
            // captured line per event and the event loop only guarantees
            // SGR continuity *within* a batch. Without this, a writer
            // that leaves styles open at a line boundary (e.g. the
            // markdown highlighter emitting a bold paragraph, or a tool
            // streaming colored output) would lose its styling on every
            // line 2+. We prepend the accumulated prefix to each line
            // before sending so the batch-joiner sees a self-contained
            // styled line.
            let mut state = SgrState::default();
            loop {
                line.clear();
                match buf.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        // Log the *raw* bytes — including the `\n` /
                        // `\r` suffix — before any stripping. That
                        // way the log shows exactly what arrived in
                        // the pipe, not what we think we saw.
                        write_raw_log(&raw_log, kind, &line);
                        while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                            line.pop();
                        }
                        let mut text = String::from_utf8_lossy(&line).into_owned();
                        // Drop any trailing incomplete CSI sequence
                        // (`\x1b[` + params with no final byte before
                        // end-of-line). These arise when a writer
                        // emits a multi-byte ANSI sequence that gets
                        // split at `\n`, which `read_until` uses as
                        // its terminator. Without stripping,
                        // `ansi-to-tui` ignores the malformed prefix
                        // but the parameter tail on the next line —
                        // `110;192;197m` etc. — has no `\x1b[` to go
                        // with it and renders as literal text. We
                        // strip on this side because `state.apply`
                        // already breaks out of its loop on the same
                        // incomplete-sequence condition, so its
                        // resolved-state prefix is still accurate
                        // across the drop.
                        strip_trailing_incomplete_csi(&mut text);
                        let prefix = state.to_ansi_prefix();
                        state.apply(&text);
                        let payload = if prefix.is_empty() {
                            text
                        } else {
                            let mut out = String::with_capacity(prefix.len() + text.len());
                            out.push_str(&prefix);
                            out.push_str(&text);
                            out
                        };
                        if tx
                            .send(UiEvent::Output {
                                kind,
                                text: payload,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(())
}

/// Truncate `text` so it does not end in an unterminated CSI
/// sequence. A CSI here is `ESC [ P* I* F` per ECMA-48:
/// - `ESC [` introducer (2 bytes)
/// - any number of parameter bytes `0-9 ; :`
/// - any number of intermediate bytes `0x20..=0x2F`
/// - a final byte `0x40..=0x7E`
///
/// We walk forward from the *last* `ESC [` in the string. If the
/// sequence that starts there has no final byte before the end of
/// the input, every byte from that `ESC` onward is dropped. Any
/// earlier, properly terminated CSIs are preserved untouched so the
/// styling of the rest of the line still parses correctly.
fn strip_trailing_incomplete_csi(text: &mut String) {
    let bytes = text.as_bytes();
    // Find the last `ESC [` in the input. Short-circuit when there's
    // no `ESC` at all — the hot path for plain-text lines.
    let esc_positions: Vec<usize> = bytes
        .iter()
        .enumerate()
        .filter_map(|(idx, &b)| (b == 0x1b).then_some(idx))
        .collect();
    for &start in esc_positions.iter().rev() {
        if bytes.get(start + 1) != Some(&b'[') {
            continue;
        }
        // Walk parameter bytes, then intermediate bytes.
        let mut j = start + 2;
        while let Some(&b) = bytes.get(j) {
            if matches!(b, b'0'..=b'9' | b';' | b':' | b'?' | b'<' | b'=' | b'>') {
                j += 1;
            } else {
                break;
            }
        }
        while let Some(&b) = bytes.get(j) {
            if (0x20..=0x2f).contains(&b) {
                j += 1;
            } else {
                break;
            }
        }
        match bytes.get(j) {
            Some(&terminator) if (0x40..=0x7e).contains(&terminator) => {
                // Terminated — this CSI is fine, and any earlier CSI
                // must also be terminated (otherwise we'd be looking
                // at *this* one's un-terminated form instead). Done.
                return;
            }
            Some(_) => {
                // A non-terminator, non-param, non-intermediate byte
                // before end-of-string means the CSI is malformed but
                // structurally over — we've already walked past it.
                return;
            }
            None => {
                // Reached end of string while still inside the CSI —
                // this is the "trailing incomplete" case. Drop it.
                text.truncate(start);
                return;
            }
        }
    }
}

/// Semantic Select Graphic Rendition state tracker. Parses SGR sequences
/// (`\x1b[...m`) and maintains the *resolved* style — a fixed set of
/// boolean attributes plus a foreground and background color — rather
/// than accumulating raw parameter history. Non-SGR CSI sequences
/// (`\x1b[1;5H`, `\x1b[?25l`, …) are skipped cleanly.
///
/// This matters for multi-line output: a writer that emits `\x1b[1m`
/// before a paragraph and `\x1b[0m` after it leaves bold "open" at every
/// intermediate newline. The pipe reader splits on `\n` and `ansi-to-tui`
/// parses each captured line in isolation, so lines 2+ would lose their
/// style unless we replay the carried-over prefix on each.
///
/// Accumulating raw params (an earlier design) would lose correctness
/// when independent attributes piled up — e.g. `[1, 3]` (bold+italic)
/// followed by 30 more fg-color changes would bump `1` out of the front
/// on cap, silently dropping bold even though no writer ever turned it
/// off. The semantic model bounds state at a few dozen bytes no matter
/// how many SGR commands the writer issues.
#[derive(Default)]
struct SgrState {
    attrs: SgrAttrs,
    fg: SgrColor,
    bg: SgrColor,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct SgrAttrs {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    slow_blink: bool,
    rapid_blink: bool,
    reverse: bool,
    conceal: bool,
    strike: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum SgrColor {
    /// Default (whatever the terminal's inherited fg/bg is).
    #[default]
    Default,
    /// Legacy 4-bit colour specified by the SGR param number itself
    /// (30..=37, 90..=97 for fg; 40..=47, 100..=107 for bg).
    Basic(u16),
    /// 256-colour palette index (`38;5;N` / `48;5;N`).
    Indexed(u8),
    /// 24-bit truecolour (`38;2;r;g;b` / `48;2;r;g;b`).
    Rgb(u8, u8, u8),
}

impl SgrState {
    fn reset(&mut self) {
        self.attrs = SgrAttrs::default();
        self.fg = SgrColor::Default;
        self.bg = SgrColor::Default;
    }

    fn to_ansi_prefix(&self) -> String {
        let mut params: Vec<u16> = Vec::new();
        if self.attrs.bold {
            params.push(1);
        }
        if self.attrs.dim {
            params.push(2);
        }
        if self.attrs.italic {
            params.push(3);
        }
        if self.attrs.underline {
            params.push(4);
        }
        if self.attrs.slow_blink {
            params.push(5);
        }
        if self.attrs.rapid_blink {
            params.push(6);
        }
        if self.attrs.reverse {
            params.push(7);
        }
        if self.attrs.conceal {
            params.push(8);
        }
        if self.attrs.strike {
            params.push(9);
        }
        let mut out = String::new();
        if !params.is_empty() {
            out.push_str("\x1b[");
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    out.push(';');
                }
                out.push_str(&p.to_string());
            }
            out.push('m');
        }
        push_color_ansi(&mut out, self.fg, ColorSlot::Fg);
        push_color_ansi(&mut out, self.bg, ColorSlot::Bg);
        out
    }

    fn apply(&mut self, text: &str) {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != 0x1b || i + 1 >= bytes.len() || bytes[i + 1] != b'[' {
                i += 1;
                continue;
            }
            // Parse a CSI sequence per ECMA-48:
            //   CSI = ESC [ P* I* F
            //     P = private prefix byte (0x3C..=0x3F, `?<=>`) or
            //         param byte (digits + `;:`)
            //     I = intermediate byte (0x20..=0x2F)
            //     F = final byte (0x40..=0x7E)
            let mut j = i + 2;
            let mut private = false;
            if let Some(&b) = bytes.get(j) {
                if matches!(b, b'?' | b'<' | b'=' | b'>') {
                    private = true;
                    j += 1;
                }
            }
            let param_start = j;
            while let Some(&b) = bytes.get(j) {
                if matches!(b, b'0'..=b'9' | b';' | b':') {
                    j += 1;
                } else {
                    break;
                }
            }
            let param_end = j;
            while let Some(&b) = bytes.get(j) {
                if (0x20..=0x2f).contains(&b) {
                    j += 1;
                } else {
                    break;
                }
            }
            let Some(&terminator) = bytes.get(j) else {
                break;
            };
            let consumed_end = j + 1;
            if private || terminator != b'm' {
                // Non-SGR CSI (cursor, erase, private mode, …) — skip.
                i = consumed_end;
                continue;
            }
            let params_str = std::str::from_utf8(&bytes[param_start..param_end]).unwrap_or("");
            self.apply_sgr_params(params_str);
            i = consumed_end;
        }
    }

    fn apply_sgr_params(&mut self, params_str: &str) {
        if params_str.is_empty() {
            // `\x1b[m` is equivalent to `\x1b[0m`.
            self.reset();
            return;
        }
        let nums: Vec<u16> = params_str
            .split(';')
            .filter_map(|p| p.parse::<u16>().ok())
            .collect();
        let mut k = 0;
        while k < nums.len() {
            let n = nums[k];
            match n {
                0 => self.reset(),
                1 => self.attrs.bold = true,
                2 => self.attrs.dim = true,
                3 => self.attrs.italic = true,
                4 => self.attrs.underline = true,
                5 => self.attrs.slow_blink = true,
                6 => self.attrs.rapid_blink = true,
                7 => self.attrs.reverse = true,
                8 => self.attrs.conceal = true,
                9 => self.attrs.strike = true,
                22 => {
                    self.attrs.bold = false;
                    self.attrs.dim = false;
                }
                23 => self.attrs.italic = false,
                24 => self.attrs.underline = false,
                25 => {
                    self.attrs.slow_blink = false;
                    self.attrs.rapid_blink = false;
                }
                27 => self.attrs.reverse = false,
                28 => self.attrs.conceal = false,
                29 => self.attrs.strike = false,
                30..=37 | 90..=97 => self.fg = SgrColor::Basic(n),
                39 => self.fg = SgrColor::Default,
                40..=47 | 100..=107 => self.bg = SgrColor::Basic(n),
                49 => self.bg = SgrColor::Default,
                38 => {
                    if let Some(color) = parse_extended_color(&nums, &mut k) {
                        self.fg = color;
                        continue;
                    }
                }
                48 => {
                    if let Some(color) = parse_extended_color(&nums, &mut k) {
                        self.bg = color;
                        continue;
                    }
                }
                _ => {}
            }
            k += 1;
        }
    }
}

/// Parse a `38;5;N` / `38;2;R;G;B` subsequence (or the `48` bg variant)
/// starting at `nums[*cursor]` which points at the `38` / `48`. Advances
/// `*cursor` past the whole subsequence on success. Returns `None` (and
/// leaves `cursor` untouched) if the subsequence is malformed — the
/// caller will then fall through to the default `cursor += 1` and skip
/// only the `38` / `48` byte, which is the usual robustness behaviour.
fn parse_extended_color(nums: &[u16], cursor: &mut usize) -> Option<SgrColor> {
    let mode = *nums.get(*cursor + 1)?;
    match mode {
        5 => {
            let idx = *nums.get(*cursor + 2)?;
            let color = SgrColor::Indexed(u8::try_from(idx).ok()?);
            *cursor += 3;
            Some(color)
        }
        2 => {
            let r = u8::try_from(*nums.get(*cursor + 2)?).ok()?;
            let g = u8::try_from(*nums.get(*cursor + 3)?).ok()?;
            let b = u8::try_from(*nums.get(*cursor + 4)?).ok()?;
            *cursor += 5;
            Some(SgrColor::Rgb(r, g, b))
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum ColorSlot {
    Fg,
    Bg,
}

fn push_color_ansi(out: &mut String, color: SgrColor, slot: ColorSlot) {
    let prefix = match slot {
        ColorSlot::Fg => 38u16,
        ColorSlot::Bg => 48u16,
    };
    match color {
        SgrColor::Default => {}
        SgrColor::Basic(n) => {
            out.push_str("\x1b[");
            out.push_str(&n.to_string());
            out.push('m');
        }
        SgrColor::Indexed(idx) => {
            out.push_str("\x1b[");
            out.push_str(&prefix.to_string());
            out.push_str(";5;");
            out.push_str(&idx.to_string());
            out.push('m');
        }
        SgrColor::Rgb(r, g, b) => {
            out.push_str("\x1b[");
            out.push_str(&prefix.to_string());
            out.push_str(";2;");
            out.push_str(&r.to_string());
            out.push(';');
            out.push_str(&g.to_string());
            out.push(';');
            out.push_str(&b.to_string());
            out.push('m');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_emits_nothing() {
        let s = SgrState::default();
        assert_eq!(s.to_ansi_prefix(), "");
    }

    #[test]
    fn bold_is_tracked_and_reset_clears_it() {
        let mut s = SgrState::default();
        s.apply("\x1b[1mhello");
        assert!(s.attrs.bold);
        assert_eq!(s.to_ansi_prefix(), "\x1b[1m");
        s.apply(" world\x1b[0m");
        assert!(!s.attrs.bold);
        assert_eq!(s.to_ansi_prefix(), "");
    }

    #[test]
    fn combined_bold_italic_fg_round_trip() {
        let mut s = SgrState::default();
        s.apply("\x1b[1;3;31mfoo");
        assert!(s.attrs.bold);
        assert!(s.attrs.italic);
        assert_eq!(s.fg, SgrColor::Basic(31));
        // Attrs emit first, then fg, each as its own sequence.
        assert_eq!(s.to_ansi_prefix(), "\x1b[1;3m\x1b[31m");
    }

    #[test]
    fn reset_via_empty_m_clears_state() {
        let mut s = SgrState::default();
        s.apply("\x1b[1mfoo\x1b[mbar");
        assert_eq!(s.to_ansi_prefix(), "");
    }

    #[test]
    fn ignores_non_sgr_csi_sequences() {
        let mut s = SgrState::default();
        s.apply("\x1b[2J\x1b[1;5H\x1b[1mstill bold");
        assert!(s.attrs.bold);
        assert_eq!(s.to_ansi_prefix(), "\x1b[1m");
    }

    #[test]
    fn skips_dec_private_mode_sequences() {
        let mut s = SgrState::default();
        s.apply("\x1b[1m\x1b[?25lstill bold\x1b[?25h");
        assert!(s.attrs.bold);
        assert_eq!(s.to_ansi_prefix(), "\x1b[1m");
    }

    #[test]
    fn skips_cursor_movement_sequences() {
        let mut s = SgrState::default();
        s.apply("\x1b[31m\x1b[1;5Hred\x1b[0mreset");
        assert_eq!(s.fg, SgrColor::Default);
        assert!(!s.attrs.bold);
        assert_eq!(s.to_ansi_prefix(), "");
    }

    #[test]
    fn handles_256_color_fg() {
        let mut s = SgrState::default();
        s.apply("\x1b[38;5;202mtext");
        assert_eq!(s.fg, SgrColor::Indexed(202));
        assert_eq!(s.to_ansi_prefix(), "\x1b[38;5;202m");
    }

    #[test]
    fn handles_truecolor_fg() {
        let mut s = SgrState::default();
        s.apply("\x1b[38;2;255;128;0mhello");
        assert_eq!(s.fg, SgrColor::Rgb(255, 128, 0));
        assert_eq!(s.to_ansi_prefix(), "\x1b[38;2;255;128;0m");
    }

    #[test]
    fn handles_truecolor_bg() {
        let mut s = SgrState::default();
        s.apply("\x1b[48;2;10;20;30mhello");
        assert_eq!(s.bg, SgrColor::Rgb(10, 20, 30));
        assert_eq!(s.to_ansi_prefix(), "\x1b[48;2;10;20;30m");
    }

    #[test]
    fn independent_attrs_are_not_lost() {
        // Regression: the old accumulating tracker would drop `bold`
        // (param 1) when enough fg-color changes pushed it off the
        // front. The semantic tracker keeps each attribute independent.
        let mut s = SgrState::default();
        s.apply("\x1b[1m"); // bold on
        for code in 31..=37 {
            s.apply(&format!("\x1b[{}m", code));
        }
        for code in 90..=97 {
            s.apply(&format!("\x1b[{}m", code));
        }
        // Bold is still set; fg is the last applied (97).
        assert!(s.attrs.bold);
        assert_eq!(s.fg, SgrColor::Basic(97));
    }

    #[test]
    fn partial_off_leaves_other_attrs_alone() {
        let mut s = SgrState::default();
        s.apply("\x1b[1;3munderline"); // bold + italic
        s.apply("\x1b[23mno italic"); // italic off
        assert!(s.attrs.bold);
        assert!(!s.attrs.italic);
    }

    #[test]
    fn bold_dim_share_22_off() {
        // `22` disables *both* bold and dim together per ECMA-48.
        let mut s = SgrState::default();
        s.apply("\x1b[1;2mboth");
        s.apply("\x1b[22mneither");
        assert!(!s.attrs.bold);
        assert!(!s.attrs.dim);
    }

    #[test]
    fn default_fg_restores_via_39() {
        let mut s = SgrState::default();
        s.apply("\x1b[31mred");
        s.apply("\x1b[39mdefault");
        assert_eq!(s.fg, SgrColor::Default);
        assert_eq!(s.to_ansi_prefix(), "");
    }

    #[test]
    fn malformed_extended_color_is_ignored() {
        // `\x1b[38;5m` is missing the index — the tracker must not
        // panic and must leave fg unchanged.
        let mut s = SgrState::default();
        s.apply("\x1b[31m\x1b[38;5m");
        assert_eq!(s.fg, SgrColor::Basic(31));
    }

    #[test]
    fn strip_trailing_incomplete_csi_drops_unterminated_tail() {
        // Classic syntect failure mode: the pipe reader's
        // `read_until(b'\n')` caught the split in the middle of a
        // truecolor sequence. Without stripping, `ansi-to-tui` would
        // emit `110;192;197m…` as literal text on the next line.
        let mut text = "text\x1b[38;2;".to_string();
        strip_trailing_incomplete_csi(&mut text);
        assert_eq!(text, "text");
    }

    #[test]
    fn strip_trailing_incomplete_csi_preserves_terminated_tail() {
        // A fully-terminated CSI at the end must survive intact so
        // downstream parsers (or the emulator directly) can use it.
        let mut text = "text\x1b[31mred".to_string();
        let original = text.clone();
        strip_trailing_incomplete_csi(&mut text);
        assert_eq!(text, original);
    }

    #[test]
    fn strip_trailing_incomplete_csi_keeps_earlier_sequences() {
        // Only the *last* CSI is unterminated. Anything before it
        // stays — we don't want to throw away valid styling because
        // the writer happened to pipe-boundary after setting another
        // color.
        let mut text = "\x1b[31mred\x1b[38;2;".to_string();
        strip_trailing_incomplete_csi(&mut text);
        assert_eq!(text, "\x1b[31mred");
    }

    #[test]
    fn strip_trailing_incomplete_csi_is_noop_on_plain_text() {
        let mut text = "plain text with no escapes".to_string();
        strip_trailing_incomplete_csi(&mut text);
        assert_eq!(text, "plain text with no escapes");
    }

    #[test]
    fn strip_trailing_incomplete_csi_tolerates_bare_esc() {
        // `\x1b` alone (no `[`) isn't a CSI — leave the text alone.
        let mut text = "text\x1ball".to_string();
        let original = text.clone();
        strip_trailing_incomplete_csi(&mut text);
        assert_eq!(text, original);
    }
}
