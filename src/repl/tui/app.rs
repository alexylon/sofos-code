//! UI-side state for the inline-viewport TUI: input textarea, queue, busy
//! flag, picker overlay, status snapshot, and exit summary.
//!
//! We deliberately do **not** maintain our own scrollback buffer here — the
//! inline viewport only owns a small region at the bottom of the terminal,
//! and all captured stdout/stderr is pushed above it via
//! `Terminal::insert_before`, so the terminal emulator's native scrollback
//! holds the log (and provides the scrollbar, copy-paste, and wheel scroll).

use std::collections::VecDeque;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use tui_textarea::{Input, Key, TextArea, WrapMode};

use crate::clipboard::PastedImage;
use crate::session::SessionMetadata;
use crate::tools::utils::ConfirmationType;

use super::event::{ExitSummary, Job, StatusSnapshot};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Entire UI state for one REPL session.
pub struct App {
    /// Multi-line input widget.
    pub textarea: TextArea<'static>,
    /// True while the worker is running a job.
    pub busy: bool,
    /// Short label shown next to the spinner ("processing", "thinking", ...).
    pub busy_label: String,
    /// Jobs queued while the worker was busy. Drained FIFO once it becomes idle.
    pub queue: VecDeque<Job>,
    /// Spinner frame cursor, advanced on tick.
    pub spinner_tick: usize,
    /// Start time of the current busy period (for the elapsed counter).
    pub busy_since: Option<Instant>,
    /// If Some, render the resume picker overlay.
    pub picker: Option<Picker>,
    /// If Some, render a confirmation modal blocking the worker thread
    /// until the user answers. Used by destructive tool prompts like
    /// `delete_file`.
    pub confirmation: Option<ConfirmationPrompt>,
    /// Model name displayed in the header area when no status snapshot
    /// has arrived yet.
    pub model_label: String,
    /// Set when the user has requested shutdown — main loop exits next turn.
    pub should_quit: bool,
    /// Summary captured from the worker right before it exits. Consumed by
    /// the main thread after teardown so it can print to the real tty.
    pub exit_summary: Option<ExitSummary>,
    /// Latest status snapshot (model, mode, reasoning, tokens) pushed from
    /// the worker; rendered on the status line under the input box.
    pub status: Option<StatusSnapshot>,
    /// Images pasted from the system clipboard via Ctrl+V. The textarea
    /// shows a circled-number marker (`①②③…`) for each entry; on submit
    /// we strip the markers, look up the corresponding image, and attach
    /// them to `Job::Message`.
    pub pasted_images: Vec<PastedImage>,
    /// Ring of previously submitted (plain-text) inputs used for
    /// `Alt+Up` / `Alt+Down` history navigation. Capped at
    /// [`INPUT_HISTORY_CAP`]; the oldest entries are dropped once full.
    pub input_history: VecDeque<String>,
    /// Current position in the history ring while navigating.
    /// `None` means the textarea holds the user's live draft (not a
    /// historical entry).
    pub history_cursor: Option<usize>,
}

pub const INPUT_HISTORY_CAP: usize = 100;

pub struct Picker {
    pub sessions: Vec<SessionMetadata>,
    pub cursor: usize,
}

/// In-flight confirmation dialog driven by a synchronous tool (`delete_file`,
/// `delete_directory`, permission grants). The worker thread is blocked on
/// the receiving end of `responder`; when the user picks a choice (or
/// cancels back to `default_index`) we send the selected index, the worker
/// unblocks, and we clear this field.
pub struct ConfirmationPrompt {
    pub prompt: String,
    pub choices: Vec<String>,
    /// Currently highlighted choice — advanced by Up/Down before Enter.
    pub cursor: usize,
    /// Safe fallback used when the user presses Esc / Ctrl+C.
    pub default_index: usize,
    pub kind: ConfirmationType,
    pub responder: std::sync::mpsc::Sender<usize>,
}

impl App {
    pub fn new(model_label: String) -> Self {
        let mut textarea = TextArea::default();
        style_textarea(&mut textarea);
        Self {
            textarea,
            busy: false,
            busy_label: String::new(),
            queue: VecDeque::new(),
            spinner_tick: 0,
            busy_since: None,
            picker: None,
            model_label,
            should_quit: false,
            exit_summary: None,
            status: None,
            confirmation: None,
            pasted_images: Vec::new(),
            input_history: VecDeque::new(),
            history_cursor: None,
        }
    }

    /// Whether the UI should render as safe mode — reads the live mode
    /// from the latest status snapshot so `/s` and `/n` take effect
    /// immediately without a stale per-session flag.
    pub fn is_safe_mode(&self) -> bool {
        matches!(
            self.status.as_ref().map(|s| s.mode),
            Some(super::event::Mode::Safe)
        )
    }

    /// Push a successfully-submitted line into the input-history ring.
    /// No-op on empty strings and on consecutive duplicates (so hammering
    /// Enter on the same message doesn't pollute history).
    pub fn remember_submitted(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.input_history.back().map(String::as_str) == Some(text) {
            self.history_cursor = None;
            return;
        }
        self.input_history.push_back(text.to_string());
        while self.input_history.len() > INPUT_HISTORY_CAP {
            self.input_history.pop_front();
        }
        self.history_cursor = None;
    }

    /// Move one step backward (older) through input history and load the
    /// resulting entry into the textarea. No-op when the history is
    /// empty. When currently showing the live draft, snapshot... no — we
    /// deliberately don't snapshot the live draft; going forward past the
    /// newest entry restores an empty textarea, matching reedline's
    /// default behaviour.
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => self.input_history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_cursor = Some(next);
        self.load_history_entry(next);
    }

    /// Move one step forward (newer) through input history. When stepping
    /// past the newest entry, clears the textarea back to the live-draft
    /// state (`history_cursor = None`).
    pub fn history_next(&mut self) {
        let Some(cursor) = self.history_cursor else {
            return;
        };
        if cursor + 1 >= self.input_history.len() {
            self.history_cursor = None;
            self.clear_input();
            return;
        }
        let next = cursor + 1;
        self.history_cursor = Some(next);
        self.load_history_entry(next);
    }

    fn load_history_entry(&mut self, index: usize) {
        let Some(entry) = self.input_history.get(index).cloned() else {
            return;
        };
        self.clear_input();
        self.textarea.insert_str(entry);
    }

    pub fn start_busy(&mut self, label: impl Into<String>) {
        self.busy = true;
        self.busy_label = label.into();
        self.busy_since = Some(Instant::now());
        self.spinner_tick = 0;
    }

    pub fn finish_busy(&mut self) {
        self.busy = false;
        self.busy_label.clear();
        self.busy_since = None;
    }

    pub fn spinner_frame(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_tick % SPINNER_FRAMES.len()]
    }

    pub fn advance_spinner(&mut self) {
        self.spinner_tick = (self.spinner_tick + 1) % SPINNER_FRAMES.len();
    }

    /// Current text in the input (may be empty or multi-line).
    pub fn input_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Clear the input widget after submitting.
    pub fn clear_input(&mut self) {
        self.textarea = TextArea::default();
        style_textarea(&mut self.textarea);
    }

    /// Route a key event into the textarea (idle) or picker (overlay).
    pub fn handle_textarea_input(&mut self, key: KeyEvent) {
        let input = key_to_input(key);
        // Ignore Enter here — the caller handles submission.
        if matches!(input.key, Key::Enter) && !input.shift && !input.alt {
            return;
        }
        self.textarea.input(input);
    }
}

/// Convert our crossterm 0.29 KeyEvent into the `tui_textarea::Input` shape.
/// We can't use the crate's `From` impl because tui-textarea 0.7 links against
/// an older crossterm bundled through ratatui, so the types are nominally
/// distinct at the type-system level.
fn key_to_input(key: KeyEvent) -> Input {
    let tkey = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Esc => Key::Esc,
        KeyCode::F(n) => Key::F(n),
        _ => Key::Null,
    };
    Input {
        key: tkey,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

fn style_textarea(textarea: &mut TextArea<'_>) {
    textarea.set_cursor_line_style(Style::default());
    textarea.set_placeholder_text("Ask anything… (Enter to send, Shift+Enter for newline)");
    textarea.set_placeholder_style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    );
    // Soft-wrap long lines at word boundaries (falling back to grapheme
    // wrap for words longer than the viewport) instead of horizontally
    // scrolling — the input box grows vertically up to
    // `MAX_INPUT_CONTENT_ROWS` and then scrolls internally.
    textarea.set_wrap_mode(WrapMode::WordOrGlyph);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new("test-model".into())
    }

    #[test]
    fn busy_lifecycle() {
        let mut a = app();
        assert!(!a.busy);
        a.start_busy("processing");
        assert!(a.busy);
        assert_eq!(a.busy_label, "processing");
        assert!(a.busy_since.is_some());
        a.finish_busy();
        assert!(!a.busy);
        assert!(a.busy_label.is_empty());
        assert!(a.busy_since.is_none());
    }

    #[test]
    fn spinner_advances_cyclically() {
        let mut a = app();
        let start = a.spinner_tick;
        for _ in 0..SPINNER_FRAMES.len() {
            a.advance_spinner();
        }
        assert_eq!(a.spinner_tick, start);
    }

    #[test]
    fn clear_input_empties_textarea() {
        let mut a = app();
        a.textarea.insert_str("hello\nworld");
        assert!(!a.input_text().is_empty());
        a.clear_input();
        assert_eq!(a.input_text(), "");
    }

    #[test]
    fn textarea_has_soft_wrap_enabled() {
        let a = app();
        assert_eq!(a.textarea.wrap_mode(), WrapMode::WordOrGlyph);
    }

    #[test]
    fn wrap_mode_persists_after_clear_input() {
        let mut a = app();
        a.textarea.insert_str("one two three");
        a.clear_input();
        assert_eq!(a.textarea.wrap_mode(), WrapMode::WordOrGlyph);
    }

    #[test]
    fn wrap_mode_persists_after_history_load() {
        let mut a = app();
        a.remember_submitted("hi");
        a.history_prev();
        assert_eq!(a.textarea.wrap_mode(), WrapMode::WordOrGlyph);
    }

    #[test]
    fn shift_enter_inserts_newline_through_textarea_handler() {
        let mut a = app();
        a.textarea.insert_str("hello");
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        a.handle_textarea_input(shift_enter);
        assert_eq!(a.textarea.lines(), &["hello", ""]);
    }

    #[test]
    fn is_safe_mode_reads_from_status_snapshot() {
        use crate::repl::tui::event::{Mode, StatusSnapshot};
        let mut a = app();
        assert!(!a.is_safe_mode(), "defaults to non-safe when status unset");
        a.status = Some(StatusSnapshot {
            model: "m".into(),
            mode: Mode::Safe,
            reasoning: String::new(),
            input_tokens: 0,
            output_tokens: 0,
        });
        assert!(a.is_safe_mode());
        a.status.as_mut().unwrap().mode = Mode::Normal;
        assert!(!a.is_safe_mode());
    }

    #[test]
    fn history_push_and_navigate() {
        let mut a = app();
        a.remember_submitted("one");
        a.remember_submitted("two");
        a.remember_submitted("three");
        assert_eq!(a.history_cursor, None);

        // Alt+Up sequence: pulls newest first.
        a.history_prev();
        assert_eq!(a.input_text(), "three");
        a.history_prev();
        assert_eq!(a.input_text(), "two");
        a.history_prev();
        assert_eq!(a.input_text(), "one");
        // Bottom — stays.
        a.history_prev();
        assert_eq!(a.input_text(), "one");

        // Alt+Down sequence: walks forward.
        a.history_next();
        assert_eq!(a.input_text(), "two");
        a.history_next();
        assert_eq!(a.input_text(), "three");
        // Past newest → back to live draft (empty).
        a.history_next();
        assert_eq!(a.input_text(), "");
        assert_eq!(a.history_cursor, None);
    }

    #[test]
    fn history_deduplicates_consecutive_duplicates() {
        let mut a = app();
        a.remember_submitted("foo");
        a.remember_submitted("foo");
        a.remember_submitted("foo");
        assert_eq!(a.input_history.len(), 1);
    }

    #[test]
    fn history_ignores_empty_strings() {
        let mut a = app();
        a.remember_submitted("");
        assert!(a.input_history.is_empty());
    }

    #[test]
    fn history_caps_at_limit() {
        let mut a = app();
        for i in 0..(INPUT_HISTORY_CAP + 20) {
            a.remember_submitted(&format!("msg {i}"));
        }
        assert_eq!(a.input_history.len(), INPUT_HISTORY_CAP);
        // Oldest were dropped.
        assert_eq!(a.input_history.front().map(String::as_str), Some("msg 20"));
    }

    #[test]
    fn status_snapshot_roundtrip() {
        use crate::repl::tui::event::{Mode, StatusSnapshot};
        let mut a = app();
        assert!(a.status.is_none());
        a.status = Some(StatusSnapshot {
            model: "claude-opus-4-6".into(),
            mode: Mode::Safe,
            reasoning: "thinking: 10000 tok".into(),
            input_tokens: 123,
            output_tokens: 456,
        });
        let s = a.status.as_ref().unwrap();
        assert_eq!(s.mode.label(), "safe");
        assert_eq!(s.model, "claude-opus-4-6");
        assert_eq!(s.input_tokens, 123);
    }
}
