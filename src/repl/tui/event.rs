//! Shared event types for the TUI event loop and worker thread.

use crate::clipboard::PastedImage;
use crate::commands::Command;
use crate::session::SessionMetadata;
use crate::tools::utils::ConfirmationType;

/// Summary values captured from the `Repl` right before the worker exits,
/// so the main thread can print them on the restored (post-alt-screen) tty.
#[derive(Debug, Clone)]
pub struct ExitSummary {
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Tool access mode shown in the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Safe,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Safe => "safe",
        }
    }
}

/// Human-readable snapshot of the `Repl`'s live state, pushed to the UI so
/// the status line can reflect it without sharing the `Repl` across threads.
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub model: String,
    pub mode: Mode,
    /// Human label for the reasoning config (e.g. "thinking: 10k tok",
    /// "thinking: off", "effort: high"). Empty string hides the field.
    pub reasoning: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Which standard stream a captured line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Stdout,
    Stderr,
}

/// A job submitted from the UI to the worker thread that owns the `Repl`.
#[derive(Debug)]
pub enum Job {
    Message {
        text: String,
        images: Vec<PastedImage>,
    },
    Command(Command),
    /// User confirmed a choice inside the resume picker.
    ResumeSelected(Option<String>),
    /// Graceful shutdown — save session, print summary, exit worker loop.
    Shutdown,
}

/// Event pushed to the UI thread. Sources: output readers, keyboard reader,
/// worker thread, periodic tick.
#[derive(Debug)]
#[allow(dead_code)]
pub enum UiEvent {
    /// A complete line captured from stdout/stderr.
    Output { kind: OutputKind, text: String },
    /// Key pressed on the tty.
    Key(crossterm::event::KeyEvent),
    /// Bracketed paste delivered by the terminal emulator. Forwarded as a
    /// single atomic unit instead of being re-synthesized into per-char
    /// `Key` events — otherwise a paste like `"yes"` would trigger the
    /// confirmation modal's `y` shortcut and auto-answer before the user
    /// could review.
    Paste(String),
    /// Terminal resized.
    Resize,
    /// Periodic tick for the spinner animation.
    Tick,
    /// Worker started processing a job.
    WorkerBusy(String),
    /// Worker finished the current job (regardless of success).
    WorkerIdle,
    /// Worker wants the UI to show the session picker.
    ShowResumePicker(Vec<SessionMetadata>),
    /// Worker pushes a fresh status snapshot (model / mode / reasoning).
    Status(StatusSnapshot),
    /// A tool call needs user confirmation. The UI renders a modal list of
    /// `choices`; the user picks one (or cancels back to `default_index`)
    /// and the 0-based selected index flows back through `responder`. The
    /// worker thread is blocked on `responder`'s receiver until then.
    ConfirmRequest {
        prompt: String,
        choices: Vec<String>,
        default_index: usize,
        kind: ConfirmationType,
        responder: std::sync::mpsc::Sender<usize>,
    },
    /// Worker has exited the main loop. Carries the final summary so the UI
    /// can print it after leaving the alternate screen.
    WorkerShutdown(ExitSummary),
}
