//! Shared event types for the TUI event loop and worker thread.

use crate::api::ReasoningEffort;
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
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    /// Largest single-turn input observed; used to detect tiered
    /// pricing cliffs (e.g. premium-tier models at 272K) so the
    /// displayed session cost reflects the rate the provider actually
    /// billed.
    pub peak_single_turn_input_tokens: u32,
    /// True when the worker exits because it panicked rather than via
    /// the normal shutdown path. Lets the UI prefix the goodbye line
    /// with a "Session ended unexpectedly" notice instead of pretending
    /// the run finished cleanly with zeroed totals.
    pub panicked: bool,
}

/// Human-readable snapshot of the `Repl`'s live state, pushed to the UI so
/// the status line can reflect it without sharing the `Repl` across threads.
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub model: String,
    pub mode: crate::config::SandboxMode,
    /// Human label for the reasoning config (e.g. "thinking: 10k tok",
    /// "thinking: off", "effort: high"). Empty string hides the field.
    pub reasoning: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Cumulative cache-read tokens for the session — exposed in the
    /// status line so users can see their prompt-cache hit rate
    /// without quitting to view the session summary.
    pub cache_read_tokens: u32,
    /// Cumulative cache-creation tokens billed at the premium rate.
    pub cache_creation_tokens: u32,
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
    /// User confirmed a choice inside the `/model` picker; `None` on
    /// cancel. Carries the canonical `&'static str` slug from
    /// [`crate::api::model_info::SUPPORTED_MODELS`] — model names are
    /// fixed at compile time so the channel doesn't need to own a
    /// freshly allocated `String` per pick.
    ModelSelected(Option<&'static str>),
    /// User confirmed a level inside the `/effort` picker; `None` on cancel.
    EffortSelected(Option<ReasoningEffort>),
    /// Graceful shutdown — save session, print summary, exit worker loop.
    Shutdown,
}

/// One row in the inline `/model` picker. Borrows the name and
/// description straight out of [`crate::api::model_info::Model`] so
/// opening the picker doesn't allocate a string per row — the
/// records live in `SUPPORTED_MODELS` for the lifetime of the
/// process.
#[derive(Debug, Clone, Copy)]
pub struct ModelPickerEntry {
    /// Model id sent on the wire and back through `Job::ModelSelected`.
    pub name: &'static str,
    /// Short blurb rendered next to the name.
    pub description: &'static str,
    /// True when the row is the model currently in use — drawn with a
    /// "(current)" tag so the user can see what they are about to
    /// replace.
    pub is_current: bool,
    /// False when the row is on the other provider — the running
    /// session can't switch there without a relaunch. Disabled rows
    /// stay visible; the cursor skips over them.
    pub is_available: bool,
}

/// One row in the inline `/effort` picker. The worker only puts
/// model-supported levels in the list, so every row is selectable.
#[derive(Debug, Clone, Copy)]
pub struct EffortPickerEntry {
    pub effort: ReasoningEffort,
    pub is_current: bool,
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
    /// Worker wants the UI to show the model picker.
    ShowModelPicker { entries: Vec<ModelPickerEntry> },
    /// Worker wants the UI to show the reasoning-effort picker.
    ShowEffortPicker { entries: Vec<EffortPickerEntry> },
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
