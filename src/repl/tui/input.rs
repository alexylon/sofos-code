use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::commands::{COMMAND_CATALOG, Command};
use crate::repl::SteerBuffer;
use crate::repl::tui::app::App;
use crate::repl::tui::event::{Job, UiEvent};
use crate::repl::tui::request_shutdown;
use tui_textarea::CursorMove;

/// Extract the three modifier flags the input handlers care about, in
/// `(shift, alt, ctrl)` order. SUPER/HYPER/META keys, which crossterm
/// also tracks, do not affect any of the key bindings below.
fn key_modifiers(key: &KeyEvent) -> (bool, bool, bool) {
    (
        key.modifiers.contains(KeyModifiers::SHIFT),
        key.modifiers.contains(KeyModifiers::ALT),
        key.modifiers.contains(KeyModifiers::CONTROL),
    )
}

pub(super) fn handle_idle_key(
    app: &mut App,
    key: KeyEvent,
    job_tx: &std_mpsc::Sender<Job>,
    interrupt: &Arc<AtomicBool>,
    steer_buffer: &SteerBuffer,
) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }

    let (shift, alt, ctrl) = key_modifiers(&key);
    let bare = !shift && !alt && !ctrl;

    // While the slash-command popup is open we intercept the navigation
    // keys (Up/Down/Tab/Enter/Esc) so they steer the popup instead of
    // editing the textarea. The trailing sync still runs afterwards so
    // the popup re-aligns with whatever the textarea now holds.
    let popup_consumed =
        app.slash_popup.is_visible() && handle_slash_popup_key(app, key, job_tx, steer_buffer);

    if !popup_consumed {
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if app.busy() {
                    // First Ctrl+C while busy: politely interrupt the
                    // running job. Second Ctrl+C while *still* busy means
                    // the worker isn't responding (panicked / deadlocked /
                    // wedged in a syscall) — escalate to a hard shutdown so
                    // the user always has an escape hatch. The worker
                    // resets the flag at the start of each new job so a
                    // fresh interrupt always starts from "polite".
                    if interrupt.load(Ordering::SeqCst) {
                        request_shutdown(app, job_tx);
                    } else {
                        interrupt.store(true, Ordering::SeqCst);
                    }
                } else {
                    request_shutdown(app, job_tx);
                }
            }
            KeyCode::Char('d') if ctrl && !app.busy() && app.textarea.is_empty() => {
                request_shutdown(app, job_tx);
            }
            KeyCode::Char('v') if ctrl => {
                handle_clipboard_paste(app);
            }
            // Ctrl+U deletes from the cursor to the start of the line, matching
            // readline / Claude Code. tui-textarea's default would undo the last
            // edit instead, which looks like a single-char backspace.
            KeyCode::Char('u') if ctrl => {
                app.textarea.delete_line_by_head();
            }
            // Ctrl+W deletes the previous word, matching bash/zsh/readline.
            KeyCode::Char('w') if ctrl => {
                app.textarea.delete_word();
            }
            // Ctrl+K deletes from the cursor to the end of the line, also
            // standard readline. Mirrors Ctrl+U on the trailing side.
            KeyCode::Char('k') if ctrl => {
                app.textarea.delete_line_by_end();
            }
            // Alt+Up / Alt+Down cycle previously-submitted messages
            // without shadowing the textarea's own Up/Down cursor keys.
            KeyCode::Up if alt && !ctrl => {
                app.history_prev();
            }
            KeyCode::Down if alt && !ctrl => {
                app.history_next();
            }
            KeyCode::Esc if app.busy() => {
                interrupt.store(true, Ordering::SeqCst);
            }
            // Plain Enter (no shift/alt/ctrl) submits. Any *modified* Enter
            // inserts a newline by falling through to the textarea handler.
            // We accept multiple modifier combinations because terminal
            // support for Shift+Enter varies wildly:
            //   - Apple Terminal.app and many defaults do NOT distinguish
            //     Shift+Enter from Enter — the shift modifier is dropped
            //     and the keypress arrives here as a bare `Enter`, which
            //     matches this arm and submits.
            //   - Alt+Enter and Ctrl+Enter are reliably distinguishable on
            //     essentially every terminal, so users on terminals
            //     without Shift+Enter support can use those as a fallback
            //     newline binding.
            //   - Shift+Enter works on terminals that implement the kitty
            //     keyboard protocol (Ghostty, kitty, Alacritty, WezTerm,
            //     iTerm with the flag turned on). `TerminalGuard` pushes
            //     the `DISAMBIGUATE_ESCAPE_CODES` flag so those terminals
            //     start delivering Shift+Enter with the SHIFT modifier set.
            KeyCode::Enter if bare => {
                submit_input(app, job_tx, steer_buffer);
            }
            // Plain Tab on a `/…` line opens the slash-command popup
            // (auto-completing immediately when only one match remains).
            // Outside that case it falls through to the textarea so the
            // key inserts a real tab.
            KeyCode::Tab if bare => {
                if !try_open_slash_popup(app) {
                    app.handle_textarea_input(key);
                }
            }
            _ => {
                app.handle_textarea_input(key);
            }
        }
    }
    app.sync_slash_popup();
}

/// React to a key event while the slash-command popup is open. Returns
/// `true` when the key was consumed by the popup and the caller should
/// stop dispatching it further.
fn handle_slash_popup_key(
    app: &mut App,
    key: KeyEvent,
    job_tx: &std_mpsc::Sender<Job>,
    steer_buffer: &SteerBuffer,
) -> bool {
    let (shift, alt, ctrl) = key_modifiers(&key);
    let bare = !shift && !alt && !ctrl;
    match key.code {
        KeyCode::Up if bare => {
            app.slash_popup.move_up();
            true
        }
        KeyCode::Down if bare => {
            app.slash_popup.move_down();
            true
        }
        KeyCode::Esc if !app.busy() => {
            // Interrupt takes priority while the worker is busy, so the
            // popup-aware Esc only fires when the user is idle.
            let snapshot = app.input_text();
            app.slash_popup.dismiss(&snapshot);
            true
        }
        // Ctrl+C while the popup is open dismisses the popup rather
        // than quitting the session — the user is mid-input and almost
        // certainly meant to bail out of the suggestion list, not to
        // exit. Outside the popup, Ctrl+C keeps its shutdown behaviour
        // through the outer handler.
        KeyCode::Char('c') if ctrl => {
            let snapshot = app.input_text();
            app.slash_popup.dismiss(&snapshot);
            true
        }
        KeyCode::Tab if bare => {
            apply_selected_command(app);
            true
        }
        KeyCode::Enter if bare => {
            // Enter inserts the highlighted command into the textarea
            // and submits it in one gesture.
            apply_selected_command(app);
            submit_input(app, job_tx, steer_buffer);
            true
        }
        _ => false,
    }
}

/// React to a Tab press while the slash-command popup is hidden.
/// Returns `true` when the keystroke has been consumed (so the caller
/// must not also insert a literal tab) and `false` when Tab should fall
/// through to the textarea — for example to indent a multi-line draft
/// or to type a tab into a non-command line.
fn try_open_slash_popup(app: &mut App) -> bool {
    let text = app.input_text();
    if !text.starts_with('/') || text.contains('\n') {
        return false;
    }
    // Pressing Tab is an explicit completion request, so a previous
    // Esc-dismissal should not keep the popup suppressed.
    app.slash_popup.hide();
    app.sync_slash_popup();
    // A single remaining match is completed inline so typing
    // `/clea<Tab>` lands on `/clear` ready for arguments.
    if app.slash_popup.is_visible() && app.slash_popup.matches().len() == 1 {
        apply_selected_command(app);
    }
    // Always consume Tab on a slash-command line — even when nothing
    // matches — so a stray Tab doesn't drop a literal `\t` into the
    // input the user is composing.
    true
}

/// Replace the textarea contents with the currently selected command
/// from the popup and park the cursor at the end of the inserted text.
/// `clear_input` already hides the popup, so this is no-op when nothing
/// is highlighted.
fn apply_selected_command(app: &mut App) {
    let Some(entry) = app.slash_popup.selected() else {
        return;
    };
    app.clear_input();
    app.textarea.insert_str(entry.name);
    app.textarea.move_cursor(CursorMove::End);
}

/// Handle `Ctrl+V`. Tries the clipboard for an image first; if one is
/// present, store it on `App` and insert a circled-number marker into the
/// textarea so `submit_input` can correlate markers to images. Otherwise
/// falls back to pasting text from the clipboard.
fn handle_clipboard_paste(app: &mut App) {
    if let Some(image) = crate::clipboard::get_clipboard_image() {
        let idx = app.pasted_images.len();
        match crate::clipboard::marker_for_index(idx) {
            Some(marker) => {
                app.pasted_images.push(image);
                app.textarea.insert_str(format!("{} ", marker));
            }
            None => {
                use colored::Colorize;
                println!(
                    "{} Too many images in one message (limit: {}). Send what you have and paste the rest separately.",
                    "✗".bright_red().bold(),
                    crate::clipboard::MAX_PASTED_IMAGES_PER_MESSAGE
                );
                println!();
            }
        }
        return;
    }
    // No image on the clipboard — try plain text so Ctrl+V still pastes
    // something useful. Terminals with bracketed paste deliver text via
    // `Event::Paste`, but users on terminals without that feature rely on
    // this path.
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        if let Ok(text) = clipboard.get_text() {
            if !text.is_empty() {
                app.textarea.insert_str(&text);
            }
        }
    }
}

pub(super) fn handle_picker_key(app: &mut App, key: KeyEvent, job_tx: &std_mpsc::Sender<Job>) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(picker) = app.picker.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if picker.cursor > 0 {
                picker.cursor -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if picker.cursor + 1 < picker.sessions.len() {
                picker.cursor += 1;
            }
        }
        KeyCode::Enter => {
            let id = picker.sessions[picker.cursor].id.clone();
            app.picker = None;
            let _ = job_tx.send(Job::ResumeSelected(Some(id)));
        }
        KeyCode::Esc => {
            app.picker = None;
            let _ = job_tx.send(Job::ResumeSelected(None));
        }
        KeyCode::Char('c') if ctrl => {
            app.picker = None;
            let _ = job_tx.send(Job::ResumeSelected(None));
        }
        _ => {}
    }
}

/// Key handler used while the `/effort` picker overlay is open.
/// Up/Down step through the supported levels; Enter sends the
/// highlighted level back to the worker; Esc / Ctrl+C cancel.
pub(super) fn handle_effort_picker_key(
    app: &mut App,
    key: KeyEvent,
    job_tx: &std_mpsc::Sender<Job>,
) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(picker) = app.effort_picker.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
        KeyCode::Down | KeyCode::Char('j') => picker.move_down(),
        KeyCode::Enter => {
            let effort = picker.selected().map(|e| e.effort);
            app.effort_picker = None;
            let _ = job_tx.send(Job::EffortSelected(effort));
        }
        KeyCode::Esc => {
            app.effort_picker = None;
            let _ = job_tx.send(Job::EffortSelected(None));
        }
        KeyCode::Char('c') if ctrl => {
            app.effort_picker = None;
            let _ = job_tx.send(Job::EffortSelected(None));
        }
        _ => {}
    }
}

/// Key handler used while the `/model` picker overlay is open.
/// Up/Down skip disabled (other-provider) rows; Enter sends the
/// highlighted model name back to the worker; Esc / Ctrl+C cancel.
pub(super) fn handle_model_picker_key(
    app: &mut App,
    key: KeyEvent,
    job_tx: &std_mpsc::Sender<Job>,
) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(picker) = app.model_picker.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => picker.move_up(),
        KeyCode::Down | KeyCode::Char('j') => picker.move_down(),
        KeyCode::Enter => {
            // `selected()` returns the highlighted entry; the cursor
            // can only land on an `is_available` row by construction,
            // so a bare unwrap-or-skip is enough.
            let name = picker.selected().filter(|e| e.is_available).map(|e| e.name);
            app.model_picker = None;
            let _ = job_tx.send(Job::ModelSelected(name));
        }
        KeyCode::Esc => {
            app.model_picker = None;
            let _ = job_tx.send(Job::ModelSelected(None));
        }
        KeyCode::Char('c') if ctrl => {
            app.model_picker = None;
            let _ = job_tx.send(Job::ModelSelected(None));
        }
        _ => {}
    }
}

fn submit_input(app: &mut App, job_tx: &std_mpsc::Sender<Job>, steer_buffer: &SteerBuffer) {
    let raw = app.input_text();
    // Strip the circled-number markers Ctrl+V inserted and recover the
    // image indices they referred to. `cleaned` is the plain text we'll
    // send to the AI; `indices` maps to slots in `app.pasted_images`.
    let (cleaned, indices) = crate::clipboard::strip_paste_markers(&raw);
    if cleaned.is_empty() && indices.is_empty() {
        return;
    }
    app.clear_input();

    // Pull the images by index, defensively clamping to the actual pool
    // size in case of a stray marker. Drain `app.pasted_images` so the
    // next message starts with a clean slate.
    let pool = std::mem::take(&mut app.pasted_images);
    let images: Vec<crate::clipboard::PastedImage> = indices
        .into_iter()
        .filter_map(|idx| pool.get(idx).cloned())
        .collect();

    // Remember the submitted text for Alt+Up / Alt+Down history navigation.
    app.remember_submitted(&cleaned);

    // Decide up-front whether this submission qualifies as a mid-turn
    // "steer" message. Steering only applies to plain-text messages sent
    // while a turn is already running: commands still need to go through
    // the job queue so they execute in their own context, and messages
    // carrying images need the full `Job::Message` path so the image
    // bytes reach the worker. Everything else is a candidate for the
    // steer channel, which the tool loop drains between iterations and
    // folds into the same user turn that carries tool results.
    // Match commands ignoring surrounding whitespace so "/exit", "/exit\n",
    // or "  /exit  " all dispatch as the same command. Without this, a
    // stray Shift+Enter or trailing space would turn a command into a
    // plain message.
    let command = if images.is_empty() {
        Command::from_str(cleaned.trim())
    } else {
        None
    };
    let is_command = command.is_some();
    let will_steer = app.busy() && !is_command && images.is_empty();

    // Echo the submitted line into the log so the user sees what they
    // sent, even while the worker is still processing or the message is
    // queued. Steered messages use a distinct glyph and a subtitle so
    // the user knows they've been accepted but won't land until the
    // next tool-call boundary.
    use colored::Colorize;
    let glyph = if will_steer {
        "↑"
    } else if app.is_readonly() {
        ":"
    } else {
        ">"
    };
    let glyph_styled = if will_steer {
        glyph.bright_magenta().bold()
    } else {
        glyph.bright_green().bold()
    };
    println!("{} {}", glyph_styled, cleaned);
    if will_steer {
        println!(
            "  {}",
            "queued for delivery before the next tool call".dimmed()
        );
    }
    if !images.is_empty() {
        println!(
            "{} {} image(s) from clipboard",
            "📋".bright_cyan(),
            images.len()
        );
    }
    println!();

    // Commands don't take images and don't need the pool. Reuse the
    // already-parsed `command` from the is_command branch above so the
    // trim rule only lives in one place.
    if let Some(cmd) = command {
        let job = Job::Command(cmd);
        if app.busy() {
            // Commands can't be injected mid-turn — they need to run
            // as their own job. Queue FIFO so they execute in the
            // order the user typed them once the current job ends.
            app.queue.push_back(job);
        } else {
            let _ = job_tx.send(job);
        }
        return;
    }

    // The submission starts with `/` but did not parse as a command —
    // a typo like `/resuem` or an unsupported variant like
    // `/effort turbo`. Surface a local error instead of forwarding it
    // to the model as a plain message, where the user pays tokens and
    // gets an irrelevant explanation back.
    let trimmed = cleaned.trim();
    if images.is_empty() && trimmed.starts_with('/') {
        let known = COMMAND_CATALOG
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
            .join(" ");
        println!("{} Unknown command `{}`.", "✗".bright_red().bold(), trimmed);
        println!("  Try: {}", known.dimmed());
        println!();
        return;
    }

    if will_steer {
        // Recover from a poisoned lock rather than silently dropping
        // the user's mid-turn message. `into_inner` returns the same
        // `Vec` the panicking thread was holding; we're still the
        // only writer on the UI side.
        steer_buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(cleaned);
        return;
    }

    let job = Job::Message {
        text: cleaned,
        images,
    };
    if app.busy() {
        app.queue.push_back(job);
    } else {
        let _ = job_tx.send(job);
    }
}

pub(super) fn spawn_input_reader(tx: UnboundedSender<UiEvent>) -> std::io::Result<()> {
    // Poll with a short timeout rather than blocking indefinitely in
    // `event::read()`. Both take crossterm's process-global
    // `INTERNAL_EVENT_READER` mutex; a blocking read holds the lock
    // forever, which deadlocks the main thread's `cursor::position()`
    // call (via `Terminal::draw → autoresize → compute_inline_size`)
    // on every resize and errors with "The cursor position could not
    // be read within a normal duration". Polling with a small timeout
    // keeps the lock available between iterations so the main thread
    // can acquire it to issue the DSR, then we proceed to `read()`
    // for whatever event made `poll` return true.
    const POLL_TIMEOUT: Duration = Duration::from_millis(50);
    thread::Builder::new()
        .name("sofos-input".into())
        .spawn(move || {
            loop {
                match crossterm::event::poll(POLL_TIMEOUT) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(_) => break,
                }
                let event = match crossterm::event::read() {
                    Ok(e) => e,
                    Err(_) => break,
                };
                // Paste is forwarded as an atomic unit; the event loop
                // decides whether to apply it based on the current
                // modal state.
                let ui_event = match event {
                    Event::Key(k) => UiEvent::Key(k),
                    Event::Resize(_, _) => UiEvent::Resize,
                    Event::Paste(s) => UiEvent::Paste(s),
                    _ => continue,
                };
                if tx.send(ui_event).is_err() {
                    break;
                }
            }
        })?;
    Ok(())
}
