use std::sync::mpsc as std_mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::repl::tui::app::App;
use crate::repl::tui::event::UiEvent;

/// Install a process-wide confirmation handler that turns synchronous
/// `confirm_multi_choice` calls from the worker thread into
/// `UiEvent::ConfirmRequest` messages on the TUI channel. The closure
/// blocks on a std mpsc receiver so the worker stays in-flight until the
/// UI answers.
pub(super) fn install_confirm_handler(ui_tx: UnboundedSender<UiEvent>) {
    let handler = Box::new(
        move |prompt: &str,
              choices: &[String],
              default_index: usize,
              kind: crate::tools::utils::ConfirmationType| {
            let (reply_tx, reply_rx) = std_mpsc::channel::<usize>();
            let event = UiEvent::ConfirmRequest {
                prompt: prompt.to_string(),
                choices: choices.to_vec(),
                default_index,
                kind,
                responder: reply_tx,
            };
            if ui_tx.send(event).is_err() {
                // UI gone — fall back to the default (safe) choice.
                return default_index;
            }
            reply_rx.recv().unwrap_or(default_index)
        },
    );
    crate::tools::utils::set_confirm_handler(handler);
}

/// Route a key into the confirmation modal.
///
/// - `Up`/`k` / `Down`/`j` — move the selection
/// - `Enter` — confirm the highlighted choice
/// - Digit `1..=9` — jump-select by position (1-based)
/// - Plain letter — jump-select the first choice whose label starts with
///   that letter (case-insensitive); repeated presses cycle between
///   choices that share the same leading letter
/// - `Esc` / `Ctrl+C` — cancel back to the safe `default_index`
///
/// Sending the answer unblocks the worker thread waiting on the mpsc
/// receiver.
pub(super) fn handle_confirmation_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(confirmation) = app.confirmation.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Up => {
            if confirmation.cursor > 0 {
                confirmation.cursor -= 1;
            }
        }
        KeyCode::Down => {
            if confirmation.cursor + 1 < confirmation.choices.len() {
                confirmation.cursor += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(c) = app.confirmation.take() {
                let _ = c.responder.send(c.cursor);
            }
        }
        KeyCode::Esc => {
            if let Some(c) = app.confirmation.take() {
                let _ = c.responder.send(c.default_index);
            }
        }
        KeyCode::Char('c') if ctrl => {
            if let Some(c) = app.confirmation.take() {
                let _ = c.responder.send(c.default_index);
            }
        }
        KeyCode::Char(ch) if !ctrl => {
            // Priority order: digit shortcuts → letter shortcuts → vim
            // navigation. Letter shortcuts beat `j`/`k` so a choice
            // label like "Kill it" / "Just retry" works as expected
            // instead of being swallowed by the vim binding.
            if let Some(idx) = digit_shortcut(ch, confirmation.choices.len()) {
                if let Some(c) = app.confirmation.take() {
                    let _ = c.responder.send(idx);
                }
                return;
            }
            if let Some(idx) = letter_shortcut(ch, &confirmation.choices, confirmation.cursor) {
                confirmation.cursor = idx;
                return;
            }
            match ch {
                'k' if confirmation.cursor > 0 => {
                    confirmation.cursor -= 1;
                }
                'j' if confirmation.cursor + 1 < confirmation.choices.len() => {
                    confirmation.cursor += 1;
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// `'1'..'9'` → 0..8, otherwise `None`. Capped at `choices_len - 1` so
/// pressing `5` with only three choices does nothing.
fn digit_shortcut(ch: char, choices_len: usize) -> Option<usize> {
    let n = ch.to_digit(10)?;
    if n == 0 {
        return None;
    }
    let idx = (n as usize).checked_sub(1)?;
    if idx < choices_len { Some(idx) } else { None }
}

/// Find the next choice whose first letter matches `ch`
/// (case-insensitive), starting the search *after* `cursor` so repeated
/// presses of the same letter cycle through matching choices.
fn letter_shortcut(ch: char, choices: &[String], cursor: usize) -> Option<usize> {
    let needle = ch.to_ascii_lowercase();
    if !needle.is_ascii_alphabetic() {
        return None;
    }
    let n = choices.len();
    (1..=n).find_map(|offset| {
        let idx = (cursor + offset) % n;
        let first = choices[idx].chars().next()?.to_ascii_lowercase();
        (first == needle).then_some(idx)
    })
}
