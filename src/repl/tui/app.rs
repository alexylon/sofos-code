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

use super::event::{EffortPickerEntry, ExitSummary, Job, ModelPickerEntry, StatusSnapshot};
use super::slash_popup::SlashPopup;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Entire UI state for one REPL session.
pub struct App {
    /// Multi-line input widget.
    pub textarea: TextArea<'static>,
    /// True while the worker is running a job. Mutated only via
    /// `start_busy` / `finish_busy`.
    busy: bool,
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
    /// If Some, render the `/model` picker overlay.
    pub model_picker: Option<ModelPicker>,
    /// If Some, render the `/effort` picker overlay.
    pub effort_picker: Option<EffortPicker>,
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
    /// Live draft saved when the user first presses Alt+Up to enter
    /// history. Restored when `history_next` walks past the newest
    /// entry, so a typed-but-unsent draft isn't silently erased when
    /// the user wanted to scroll through prior prompts.
    pub history_draft: Option<String>,
    /// Current position in the history ring while navigating.
    /// `None` means the textarea holds the user's live draft (not a
    /// historical entry).
    pub history_cursor: Option<usize>,
    /// Inline overlay shown under the input box while the user is typing
    /// a `/…` command. Stays in sync with the textarea on every keystroke
    /// via [`App::sync_slash_popup`].
    pub slash_popup: SlashPopup,
}

pub const INPUT_HISTORY_CAP: usize = 100;

pub struct Picker {
    pub sessions: Vec<SessionMetadata>,
    pub cursor: usize,
}

/// Inline overlay shown by `/model`. Holds the rows, the cursor,
/// and a navigation helper that skips disabled rows (the
/// other-provider models a running session cannot reach).
pub struct ModelPicker {
    pub entries: Vec<ModelPickerEntry>,
    pub cursor: usize,
}

impl ModelPicker {
    pub fn new(entries: Vec<ModelPickerEntry>) -> Self {
        // Park the cursor on the current model so the user sees what
        // they're about to replace; if the current model is somehow
        // disabled (shouldn't happen because the active session
        // already uses it) fall back to the first available row.
        let cursor = entries
            .iter()
            .position(|e| e.is_current && e.is_available)
            .or_else(|| entries.iter().position(|e| e.is_available))
            .unwrap_or(0);
        Self { entries, cursor }
    }

    pub fn move_up(&mut self) {
        self.cursor = step_to_available(self.cursor, &self.entries, -1);
    }

    pub fn move_down(&mut self) {
        self.cursor = step_to_available(self.cursor, &self.entries, 1);
    }

    /// Currently highlighted entry, if any.
    pub fn selected(&self) -> Option<&ModelPickerEntry> {
        self.entries.get(self.cursor)
    }
}

/// Advance the cursor in `direction` (+1 or -1), skipping disabled
/// rows and stopping at the list edges. Stays put if no enabled row
/// can be reached.
fn step_to_available(cursor: usize, entries: &[ModelPickerEntry], direction: i32) -> usize {
    if entries.is_empty() {
        return 0;
    }
    let mut idx = cursor as i32 + direction;
    while idx >= 0 && (idx as usize) < entries.len() {
        let candidate = idx as usize;
        if entries[candidate].is_available {
            return candidate;
        }
        idx += direction;
    }
    cursor
}

/// Inline overlay shown by `/effort`. Holds the supported levels
/// plus the cursor; every row is selectable.
pub struct EffortPicker {
    pub entries: Vec<EffortPickerEntry>,
    pub cursor: usize,
}

impl EffortPicker {
    pub fn new(entries: Vec<EffortPickerEntry>) -> Self {
        let cursor = entries.iter().position(|e| e.is_current).unwrap_or(0);
        Self { entries, cursor }
    }

    pub fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
    }

    pub fn selected(&self) -> Option<&EffortPickerEntry> {
        self.entries.get(self.cursor)
    }
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
            model_picker: None,
            effort_picker: None,
            model_label,
            should_quit: false,
            exit_summary: None,
            status: None,
            confirmation: None,
            pasted_images: Vec::new(),
            input_history: VecDeque::new(),
            history_draft: None,
            history_cursor: None,
            slash_popup: SlashPopup::new(),
        }
    }

    /// Recompute the slash-command overlay so it tracks the textarea.
    /// Callers should invoke this after every keystroke that can change
    /// the input contents.
    pub fn sync_slash_popup(&mut self) {
        let text = self.input_text();
        self.slash_popup.sync(&text);
    }

    /// Whether the UI should render as safe mode — reads the live mode
    /// from the latest status snapshot so `/safe` and `/workspace` take effect
    /// immediately without a stale per-session flag.
    pub fn is_safe_mode(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.mode.is_read_only())
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
    /// empty. Snapshots the in-progress draft on the first press so a
    /// later `history_next` past the newest entry can put it back.
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        // First step into history captures the live draft so we can
        // restore it on the way out. Subsequent steps don't overwrite
        // it — moving between two history entries doesn't change the
        // original draft.
        if self.history_cursor.is_none() {
            self.history_draft = Some(self.input_text());
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
    /// past the newest entry, restores the live draft captured by
    /// `history_prev`, falling back to an empty textarea when no draft
    /// was saved.
    pub fn history_next(&mut self) {
        let Some(cursor) = self.history_cursor else {
            return;
        };
        if cursor + 1 >= self.input_history.len() {
            self.history_cursor = None;
            let draft = self.history_draft.take().unwrap_or_default();
            self.clear_input();
            if !draft.is_empty() {
                self.textarea.insert_str(draft);
            }
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

    pub fn busy(&self) -> bool {
        self.busy
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
        self.slash_popup.hide();
    }

    /// Route a key event into the textarea (idle) or picker (overlay).
    pub fn handle_textarea_input(&mut self, key: KeyEvent) {
        let input = key_to_input(key);
        // Ignore plain Enter here — the caller handles submission.
        // Modified Enter (Shift/Alt/Ctrl) falls through so the
        // textarea inserts a newline, matching the fallback bindings
        // documented at the input dispatch site.
        if matches!(input.key, Key::Enter) && !input.shift && !input.alt && !input.ctrl {
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
    fn ctrl_enter_inserts_newline_through_textarea_handler() {
        // Ctrl+Enter is documented at the input dispatch site as a
        // fallback newline binding for terminals that don't deliver
        // Shift+Enter distinctly. The early-return used to drop the
        // event because only Shift and Alt were checked.
        let mut a = app();
        a.textarea.insert_str("hello");
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        a.handle_textarea_input(ctrl_enter);
        assert_eq!(a.textarea.lines(), &["hello", ""]);
    }

    #[test]
    fn alt_enter_inserts_newline_through_textarea_handler() {
        // Mirror of the Shift+Enter test; pinned so a future tweak
        // to the early-return guard does not silently regress the
        // documented Alt+Enter fallback.
        let mut a = app();
        a.textarea.insert_str("hello");
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        a.handle_textarea_input(alt_enter);
        assert_eq!(a.textarea.lines(), &["hello", ""]);
    }

    #[test]
    fn is_safe_mode_reads_from_status_snapshot() {
        use crate::config::SandboxMode;
        use crate::repl::tui::event::StatusSnapshot;
        let mut a = app();
        assert!(!a.is_safe_mode(), "defaults to non-safe when status unset");
        a.status = Some(StatusSnapshot {
            model: "m".into(),
            mode: SandboxMode::ReadOnly,
            reasoning: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        });
        assert!(a.is_safe_mode());
        a.status.as_mut().unwrap().mode = SandboxMode::Workspace;
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
    fn model_picker_lands_cursor_on_current_model() {
        let entries = vec![
            ModelPickerEntry {
                name: crate::api::model_info::CLAUDE_OPUS,
                description: "flagship",
                is_current: false,
                is_available: true,
            },
            ModelPickerEntry {
                name: crate::api::model_info::CLAUDE_SONNET,
                description: "default",
                is_current: true,
                is_available: true,
            },
            ModelPickerEntry {
                name: crate::api::model_info::GPT_FLAGSHIP,
                description: "openai",
                is_current: false,
                is_available: true,
            },
        ];
        let p = ModelPicker::new(entries);
        assert_eq!(p.cursor, 1);
        assert_eq!(
            p.selected().unwrap().name,
            crate::api::model_info::CLAUDE_SONNET
        );
    }

    #[test]
    fn model_picker_move_down_skips_disabled_rows() {
        let entries = vec![
            ModelPickerEntry {
                name: "a",
                description: "",
                is_current: true,
                is_available: true,
            },
            ModelPickerEntry {
                name: "b",
                description: "",
                is_current: false,
                is_available: false,
            },
            ModelPickerEntry {
                name: "c",
                description: "",
                is_current: false,
                is_available: true,
            },
        ];
        let mut p = ModelPicker::new(entries);
        assert_eq!(p.cursor, 0);
        p.move_down();
        assert_eq!(p.cursor, 2, "down should skip the disabled row at 1");
    }

    #[test]
    fn model_picker_move_up_skips_disabled_rows() {
        let entries = vec![
            ModelPickerEntry {
                name: "a",
                description: "",
                is_current: false,
                is_available: true,
            },
            ModelPickerEntry {
                name: "b",
                description: "",
                is_current: false,
                is_available: false,
            },
            ModelPickerEntry {
                name: "c",
                description: "",
                is_current: true,
                is_available: true,
            },
        ];
        let mut p = ModelPicker::new(entries);
        assert_eq!(p.cursor, 2);
        p.move_up();
        assert_eq!(p.cursor, 0, "up should skip the disabled row at 1");
    }

    #[test]
    fn model_picker_navigation_stops_at_list_edges() {
        let entries = vec![ModelPickerEntry {
            name: "only",
            description: "",
            is_current: true,
            is_available: true,
        }];
        let mut p = ModelPicker::new(entries);
        p.move_down();
        assert_eq!(p.cursor, 0);
        p.move_up();
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn effort_picker_lands_cursor_on_current_level() {
        use crate::api::ReasoningEffort::{High, Low, Medium, Off};
        let entries = vec![
            EffortPickerEntry {
                effort: Off,
                is_current: false,
            },
            EffortPickerEntry {
                effort: Low,
                is_current: false,
            },
            EffortPickerEntry {
                effort: Medium,
                is_current: true,
            },
            EffortPickerEntry {
                effort: High,
                is_current: false,
            },
        ];
        let p = EffortPicker::new(entries);
        assert_eq!(p.cursor, 2);
        assert_eq!(p.selected().unwrap().effort, Medium);
    }

    #[test]
    fn effort_picker_navigation_clamps_at_edges() {
        use crate::api::ReasoningEffort::{Low, Off};
        let entries = vec![
            EffortPickerEntry {
                effort: Off,
                is_current: true,
            },
            EffortPickerEntry {
                effort: Low,
                is_current: false,
            },
        ];
        let mut p = EffortPicker::new(entries);
        assert_eq!(p.cursor, 0);
        p.move_up();
        assert_eq!(p.cursor, 0);
        p.move_down();
        assert_eq!(p.cursor, 1);
        p.move_down();
        assert_eq!(p.cursor, 1);
    }

    #[test]
    fn model_picker_with_all_rows_disabled_keeps_cursor_at_zero() {
        let entries = vec![
            ModelPickerEntry {
                name: "x",
                description: "",
                is_current: false,
                is_available: false,
            },
            ModelPickerEntry {
                name: "y",
                description: "",
                is_current: false,
                is_available: false,
            },
        ];
        let mut p = ModelPicker::new(entries);
        assert_eq!(p.cursor, 0);
        p.move_down();
        assert_eq!(p.cursor, 0, "no enabled row to step to");
    }

    #[test]
    fn status_snapshot_roundtrip() {
        use crate::config::SandboxMode;
        use crate::repl::tui::event::StatusSnapshot;
        let mut a = app();
        assert!(a.status.is_none());
        a.status = Some(StatusSnapshot {
            model: crate::api::model_info::CLAUDE_OPUS.into(),
            mode: SandboxMode::ReadOnly,
            reasoning: "thinking: 10000 tok".into(),
            input_tokens: 123,
            output_tokens: 456,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        });
        let s = a.status.as_ref().unwrap();
        assert_eq!(s.mode.label(), "safe");
        assert_eq!(s.model, crate::api::model_info::CLAUDE_OPUS);
        assert_eq!(s.input_tokens, 123);
    }
}
