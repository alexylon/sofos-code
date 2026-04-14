//! Stdout/stderr capture for the TUI.
//!
//! Redirects fds 1 and 2 into OS pipes so every `println!`, `eprintln!`
//! and direct-to-stdout colored output from the worker thread flows into
//! the terminal's scrollback via `Terminal::insert_before`.
//!
//! `pause` / `resume` temporarily swap fd 1 / fd 2 back to the real
//! terminal so calls that bypass the ratatui backend and write directly to
//! `io::stdout()` — notably `crossterm::cursor::position()` — can reach
//! the tty and read its DSR response. Without this, any resize-driven
//! viewport recomputation would time out against a closed pipe.

use os_pipe::{PipeReader, PipeWriter};
use std::io::{BufRead, BufReader};
use std::os::unix::io::IntoRawFd;
use std::thread;
use tokio::sync::mpsc::UnboundedSender;

use crate::repl::tui::event::{OutputKind, UiEvent};

pub struct OutputCapture {
    saved_stdout: libc::c_int,
    saved_stderr: libc::c_int,
    pipe_stdout: libc::c_int,
    pipe_stderr: libc::c_int,
    paused: bool,
}

impl OutputCapture {
    /// Redirect fds 1 and 2 to pipes and spawn reader threads that forward
    /// complete lines to the UI event channel.
    pub fn install(tx: UnboundedSender<UiEvent>) -> std::io::Result<Self> {
        let saved_stdout = dup_fd(libc::STDOUT_FILENO)?;
        let saved_stderr = match dup_fd(libc::STDERR_FILENO) {
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

        let pipe_stdout = match redirect(stdout_writer, libc::STDOUT_FILENO) {
            Ok(fd) => fd,
            Err(e) => {
                unsafe {
                    libc::close(saved_stdout);
                    libc::close(saved_stderr);
                }
                return Err(e);
            }
        };
        let pipe_stderr = match redirect(stderr_writer, libc::STDERR_FILENO) {
            Ok(fd) => fd,
            Err(e) => {
                unsafe {
                    libc::dup2(saved_stdout, libc::STDOUT_FILENO);
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
            paused: false,
        };

        spawn_reader(stdout_reader, OutputKind::Stdout, tx.clone())?;
        spawn_reader(stderr_reader, OutputKind::Stderr, tx)?;

        Ok(this)
    }

    /// Temporarily route fd 1 / fd 2 back to the real terminal. Calls that
    /// go directly to `io::stdout()` (e.g. `crossterm::cursor::position`)
    /// will reach the tty and read its DSR response. Idempotent.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        unsafe {
            libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
            libc::dup2(self.saved_stderr, libc::STDERR_FILENO);
        }
        self.paused = true;
    }

    /// Re-apply the pipe redirection after a `pause`. Idempotent.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        unsafe {
            libc::dup2(self.pipe_stdout, libc::STDOUT_FILENO);
            libc::dup2(self.pipe_stderr, libc::STDERR_FILENO);
        }
        self.paused = false;
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved_stdout, libc::STDOUT_FILENO);
            libc::dup2(self.saved_stderr, libc::STDERR_FILENO);
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
    let write_fd = writer.into_raw_fd();
    let rc = unsafe { libc::dup2(write_fd, target_fd) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(write_fd) };
        return Err(err);
    }
    Ok(write_fd)
}

fn spawn_reader(
    reader: PipeReader,
    kind: OutputKind,
    tx: UnboundedSender<UiEvent>,
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
                        while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                            line.pop();
                        }
                        let text = String::from_utf8_lossy(&line).into_owned();
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
}
