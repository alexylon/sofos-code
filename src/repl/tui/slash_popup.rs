//! Inline pop-up that appears beneath the input box when the user starts a
//! line with `/`. Lists the slash commands that match the current typed
//! prefix, lets the user pick one with the arrow keys and confirm it with
//! Enter, and stays in sync with the textarea on every keystroke.
//!
//! The popup is purely UI state — it never executes a command; that work
//! still happens through [`crate::commands::Command::execute`] once the
//! selected name has been written into the textarea and submitted.

use crate::commands::{COMMAND_CATALOG, CommandEntry};

/// Maximum number of rows shown at once. Larger lists scroll under the
/// cursor.
pub const MAX_VISIBLE_ROWS: usize = 6;

/// Selection-aware state for the slash-command popup.
#[derive(Debug, Default)]
pub struct SlashPopup {
    /// True while the popup should be rendered.
    visible: bool,
    /// Entries that currently match the user-typed filter. Re-derived on
    /// every text change.
    matches: Vec<CommandEntry>,
    /// Index of the highlighted entry inside `matches`.
    cursor: usize,
    /// Index of the first visible row when scrolling.
    scroll_top: usize,
    /// Input contents the user explicitly dismissed (with Escape). While
    /// the input still matches this string the popup stays hidden even
    /// if the catalog has matches; any change to the input clears the
    /// marker and re-enables the popup.
    dismissed_for: Option<String>,
}

/// Return `input` itself when it looks like a single-line slash command
/// the popup should help with. Multi-line input means the user is
/// composing a message rather than a command, and Tab needs to keep its
/// "insert real tab" meaning instead of replacing the textarea.
fn slash_filter(input: &str) -> Option<&str> {
    if input.contains('\n') {
        return None;
    }
    if input.starts_with('/') {
        Some(input)
    } else {
        None
    }
}

impl SlashPopup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the popup is currently displayed.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Entries that match the current filter, in catalog (display) order.
    pub fn matches(&self) -> &[CommandEntry] {
        &self.matches
    }

    /// Currently highlighted match, if any.
    pub fn selected(&self) -> Option<CommandEntry> {
        self.matches.get(self.cursor).copied()
    }

    /// Index of the currently highlighted match.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Row index of the first visible entry — used by the renderer when
    /// the match list is longer than [`MAX_VISIBLE_ROWS`].
    pub fn scroll_top(&self) -> usize {
        self.scroll_top
    }

    /// Force the popup hidden without remembering the dismissal — used
    /// from programmatic paths such as submitting or clearing input.
    pub fn hide(&mut self) {
        self.reset_visible_state();
        self.dismissed_for = None;
    }

    /// Dismiss the popup as if the user pressed Escape. The popup stays
    /// hidden until [`SlashPopup::sync`] sees a different input string.
    pub fn dismiss(&mut self, input: &str) {
        self.reset_visible_state();
        self.dismissed_for = Some(input.to_string());
    }

    fn reset_visible_state(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.cursor = 0;
        self.scroll_top = 0;
    }

    /// Reconcile the popup state with the current textarea contents.
    /// Show the popup whenever the first line still looks like a partially
    /// typed slash command; hide it otherwise.
    pub fn sync(&mut self, input: &str) {
        // Honour a prior `dismiss` while the user is editing within the
        // same slash-command "family" — either the current input still
        // begins with the dismissed text (kept typing) or the dismissed
        // text begins with the current input (backspaced). Switching
        // to an unrelated command (e.g. dismiss `/clear`, then type
        // `/list`) breaks both prefix tests and re-opens the popup.
        //
        // An empty input fully resets the dismissal: every dismissed
        // string trivially starts with "", which would otherwise keep
        // the popup suppressed even after the textarea has been
        // emptied and a fresh `/` is typed.
        if !input.is_empty() {
            if let Some(prev) = self.dismissed_for.as_deref() {
                if prev == input || input.starts_with(prev) || prev.starts_with(input) {
                    return;
                }
            }
        }
        self.dismissed_for = None;

        let Some(prefix) = slash_filter(input) else {
            self.hide();
            return;
        };
        let matches = filter_commands(prefix);
        if matches.is_empty() {
            self.reset_visible_state();
            return;
        }
        self.matches = matches;
        self.visible = true;
        if self.cursor >= self.matches.len() {
            self.cursor = 0;
        }
        self.ensure_visible();
    }

    /// Move the cursor up; wraps to the bottom past row 0.
    pub fn move_up(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.cursor = if self.cursor == 0 {
            self.matches.len() - 1
        } else {
            self.cursor - 1
        };
        self.ensure_visible();
    }

    /// Move the cursor down; wraps to the top past the last row.
    pub fn move_down(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.cursor = if self.cursor + 1 >= self.matches.len() {
            0
        } else {
            self.cursor + 1
        };
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let visible = MAX_VISIBLE_ROWS.min(self.matches.len());
        if visible == 0 {
            self.scroll_top = 0;
            return;
        }
        if self.cursor < self.scroll_top {
            self.scroll_top = self.cursor;
        } else if self.cursor >= self.scroll_top + visible {
            self.scroll_top = self.cursor + 1 - visible;
        }
    }
}

/// Pick out catalog entries whose name starts with `prefix`. The match is
/// case-insensitive so users can type `/Cl` and still see `/clear`.
fn filter_commands(prefix: &str) -> Vec<CommandEntry> {
    let lower = prefix.to_lowercase();
    COMMAND_CATALOG
        .iter()
        .copied()
        .filter(|entry| entry.name.to_lowercase().starts_with(&lower))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_input_is_empty_or_plain_text() {
        let mut popup = SlashPopup::new();
        popup.sync("");
        assert!(!popup.is_visible());
        popup.sync("hello world");
        assert!(!popup.is_visible());
    }

    #[test]
    fn bare_slash_shows_full_catalog() {
        let mut popup = SlashPopup::new();
        popup.sync("/");
        assert!(popup.is_visible());
        assert_eq!(popup.matches().len(), COMMAND_CATALOG.len());
    }

    #[test]
    fn prefix_filters_to_matching_commands() {
        let mut popup = SlashPopup::new();
        popup.sync("/cl");
        assert!(popup.is_visible());
        assert!(popup.matches().iter().any(|e| e.name == "/clear"));
        assert!(popup.matches().iter().all(|e| e.name.starts_with("/cl")));
    }

    #[test]
    fn slash_with_trailing_space_keeps_popup_open() {
        // `/e ` should keep the popup open and narrow to `/effort`
        // — multi-word commands no longer exist in the catalog
        // (the picker opens for `/effort`).
        let mut popup = SlashPopup::new();
        popup.sync("/e");
        assert!(popup.is_visible());
        let names: Vec<&str> = popup.matches().iter().map(|e| e.name).collect();
        assert!(names.contains(&"/effort"));
    }

    #[test]
    fn hides_when_no_match() {
        let mut popup = SlashPopup::new();
        popup.sync("/zzzz");
        assert!(!popup.is_visible());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let mut popup = SlashPopup::new();
        popup.sync("/CL");
        assert!(popup.matches().iter().any(|e| e.name == "/clear"));
    }

    #[test]
    fn move_down_wraps_around() {
        let mut popup = SlashPopup::new();
        popup.sync("/");
        let total = popup.matches().len();
        for _ in 0..total {
            popup.move_down();
        }
        // After `total` steps we should be back at the top.
        assert_eq!(popup.cursor(), 0);
    }

    #[test]
    fn move_up_from_top_wraps_to_bottom() {
        let mut popup = SlashPopup::new();
        popup.sync("/");
        popup.move_up();
        assert_eq!(popup.cursor(), popup.matches().len() - 1);
    }

    #[test]
    fn scroll_top_follows_cursor_past_visible_window() {
        let mut popup = SlashPopup::new();
        popup.sync("/");
        // Walk down past the visible window.
        for _ in 0..MAX_VISIBLE_ROWS {
            popup.move_down();
        }
        assert!(popup.scroll_top() > 0);
        assert!(popup.cursor() >= popup.scroll_top());
        assert!(popup.cursor() < popup.scroll_top() + MAX_VISIBLE_ROWS);
    }

    #[test]
    fn sync_clamps_cursor_when_filter_shrinks_list() {
        let mut popup = SlashPopup::new();
        popup.sync("/");
        // Park the cursor near the bottom of the full list.
        for _ in 0..(COMMAND_CATALOG.len() - 1) {
            popup.move_down();
        }
        assert_eq!(popup.cursor(), COMMAND_CATALOG.len() - 1);
        // Filter down so only one entry remains — the cursor must not
        // point past the end of the shorter list.
        popup.sync("/exit");
        assert!(popup.is_visible());
        assert!(popup.cursor() < popup.matches().len());
    }

    #[test]
    fn second_line_is_ignored_when_first_line_is_not_a_command() {
        let mut popup = SlashPopup::new();
        popup.sync("hello\n/clear");
        assert!(!popup.is_visible());
    }

    #[test]
    fn newline_anywhere_suppresses_the_popup() {
        // Multi-line input is a message draft; Tab and Enter must not
        // wipe it to insert a slash command.
        let mut popup = SlashPopup::new();
        popup.sync("/clear\nsome more text");
        assert!(!popup.is_visible());
    }

    #[test]
    fn slash_with_no_catalog_match_leaves_popup_invisible() {
        // A slash command that nobody recognises must not leave a stale
        // visible state behind; the renderer relies on `is_visible`
        // being false in this case.
        let mut popup = SlashPopup::new();
        popup.sync("/zz-unknown");
        assert!(!popup.is_visible());
        assert!(popup.matches().is_empty());
    }

    #[test]
    fn dismiss_keeps_popup_hidden_until_input_changes() {
        let mut popup = SlashPopup::new();
        popup.sync("/c");
        assert!(popup.is_visible());

        popup.dismiss("/c");
        // Re-syncing with the same input should leave the popup hidden.
        popup.sync("/c");
        assert!(!popup.is_visible());

        // Typing more inside the same prefix family keeps the dismissal
        // — the user is still editing the same slash-command attempt,
        // and a small typo fix shouldn't reopen the suggestion list.
        popup.sync("/cl");
        assert!(!popup.is_visible());

        // Backspacing into a shared prefix likewise keeps the dismissal.
        popup.sync("/");
        assert!(!popup.is_visible());

        // Switching to a clearly different slash-command attempt
        // re-opens the popup: neither input is a prefix of the
        // dismissed text (`/r` is a real prefix in the catalog —
        // `/resume`).
        popup.sync("/r");
        assert!(popup.is_visible());
    }
}
