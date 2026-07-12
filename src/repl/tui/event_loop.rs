use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc as std_mpsc;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Interval, interval};

use crate::error::Result;
use crate::repl::SteerBuffer;
use crate::repl::tui::app::{
    self, App, EffortPicker, ModePicker, ModelPicker, PermissionsPicker, Picker,
};
use crate::repl::tui::event::{Job, UiEvent};
use crate::repl::tui::input::{
    handle_effort_picker_key, handle_idle_key, handle_mode_picker_key, handle_model_picker_key,
    handle_permissions_picker_key, handle_picker_key,
};
use crate::repl::tui::keymap::handle_confirmation_key;
use crate::repl::tui::{MAX_OUTPUT_BATCH, TICK_INTERVAL, inline_tui, ui};

pub(super) async fn event_loop(
    tui: &mut inline_tui::InlineTui,
    app: &mut App,
    mut ui_rx: UnboundedReceiver<UiEvent>,
    job_tx: std_mpsc::Sender<Job>,
    interrupt: Arc<AtomicBool>,
    steer_buffer: SteerBuffer,
) -> Result<()> {
    let mut tick: Interval = interval(TICK_INTERVAL);
    // Track the last size we've rendered at so we can detect resizes
    // before the next draw so `scrollback` can operate on the
    // current dimensions. Start at a sentinel so the very first draw
    // always updates it.
    let mut last_size: (u16, u16) = (0, 0);
    render_frame(tui, app, &mut last_size)?;

    loop {
        let event = tokio::select! {
            ev = ui_rx.recv() => ev,
            _ = tick.tick() => Some(UiEvent::Tick),
        };

        let Some(first) = event else {
            break;
        };

        // Inner loop lets the `Output` arm re-dispatch a non-Output event
        // it pulled out of the queue during its batch drain, without
        // returning to the outer `select!` (which would add another round
        // of draw+quit-check).
        let mut current = first;
        loop {
            match current {
                UiEvent::Tick => {
                    // Skip the spinner animation while a confirmation
                    // modal is open — the worker is blocked waiting for
                    // the user, not processing, so a spinning indicator
                    // would be misleading.
                    if app.busy() && app.confirmation.is_none() {
                        app.advance_spinner();
                    }
                    break;
                }
                UiEvent::Output { kind: _, text } => {
                    // If the terminal has been resized since the last
                    // draw, flush first so the next history insert runs
                    // against the current screen dimensions — otherwise
                    // `scroll_strings_above_viewport` computes its
                    // DECSTBM regions against the stale viewport rect.
                    if crossterm::terminal::size()? != last_size {
                        render_frame(tui, app, &mut last_size)?;
                    }
                    // Batch-drain consecutive `Output` events so a tool
                    // that streams thousands of lines resolves in a
                    // single `queue_history_lines` call instead of one
                    // per line. Non-Output events interrupt the drain
                    // so keypresses (especially ESC / Ctrl+C) aren't
                    // stuck behind an output backlog.
                    let mut batch: Vec<String> = Vec::with_capacity(32);
                    batch.push(text);
                    let mut forwarded: Option<UiEvent> = None;
                    while batch.len() < MAX_OUTPUT_BATCH {
                        match ui_rx.try_recv() {
                            Ok(UiEvent::Output { text, .. }) => batch.push(text),
                            Ok(other) => {
                                forwarded = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    // Queue the batch — it'll be flushed above the
                    // viewport inside the next `InlineTui::draw`'s
                    // synchronized-update bracket.
                    tui.queue_history_lines(batch);
                    if let Some(next) = forwarded {
                        current = next;
                        continue;
                    }
                    break;
                }
                UiEvent::Key(key) => {
                    if app.confirmation.is_some() {
                        handle_confirmation_key(app, key);
                    } else if app.picker.is_some() {
                        handle_picker_key(app, key, &job_tx);
                    } else if app.model_picker.is_some() {
                        handle_model_picker_key(app, key, &job_tx);
                    } else if app.effort_picker.is_some() {
                        handle_effort_picker_key(app, key, &job_tx);
                    } else if app.permissions_picker.is_some() {
                        handle_permissions_picker_key(app, key, &job_tx);
                    } else if app.mode_picker.is_some() {
                        handle_mode_picker_key(app, key, &job_tx);
                    } else {
                        handle_idle_key(app, key, &job_tx, &interrupt, &steer_buffer);
                    }
                    break;
                }
                UiEvent::Paste(text) => {
                    // Drop pastes while a modal is open — otherwise
                    // pasting e.g. "yes" would hit the confirmation
                    // modal's letter shortcut and auto-answer. When
                    // idle, `insert_str` handles multi-line text
                    // natively so embedded `\n`s become real newlines
                    // in the textarea rather than Enter-submits.
                    if app.confirmation.is_none()
                        && app.picker.is_none()
                        && app.model_picker.is_none()
                        && app.effort_picker.is_none()
                        && app.permissions_picker.is_none()
                        && app.mode_picker.is_none()
                    {
                        app.textarea.insert_str(text);
                        app.sync_slash_popup();
                    }
                    break;
                }
                UiEvent::Resize => {
                    // Handled at draw time by `render_frame`; the
                    // variant is kept as a wake-up signal so a resize
                    // is reflected immediately instead of waiting for
                    // the next tick.
                    break;
                }
                UiEvent::WorkerBusy(label) => {
                    app.start_busy(label);
                    break;
                }
                UiEvent::WorkerIdle => {
                    app.finish_busy();
                    // Don't drain the queue while a picker modal is open —
                    // the user hasn't committed to a choice yet and a queued
                    // message would race with the selection.
                    if app.picker.is_none()
                        && app.model_picker.is_none()
                        && app.effort_picker.is_none()
                        && app.permissions_picker.is_none()
                        && app.mode_picker.is_none()
                    {
                        // Steer messages the tool loop didn't consume —
                        // e.g. the turn ended without ever hitting a
                        // tool-call boundary, or the user submitted after
                        // the last drain — are flushed here as a new
                        // `Job::Message` so they still reach the model.
                        // Recover from lock poisoning via `into_inner`
                        // so a panic elsewhere doesn't eat the user's
                        // pending mid-turn messages.
                        let residual: Vec<String> = std::mem::take(
                            &mut *steer_buffer
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        );
                        if !residual.is_empty() {
                            let _ = job_tx.send(Job::Message {
                                text: residual.join("\n\n"),
                                images: Vec::new(),
                            });
                        } else if let Some(next) = app.queue.pop_front() {
                            let _ = job_tx.send(next);
                        }
                    }
                    break;
                }
                UiEvent::Status(snapshot) => {
                    app.status = Some(snapshot);
                    break;
                }
                UiEvent::ShowResumePicker(sessions) => {
                    app.picker = Some(Picker {
                        sessions,
                        cursor: 0,
                    });
                    break;
                }
                UiEvent::ShowModelPicker { entries } => {
                    app.model_picker = Some(ModelPicker::new(entries));
                    break;
                }
                UiEvent::ShowEffortPicker { entries } => {
                    app.effort_picker = Some(EffortPicker::new(entries));
                    break;
                }
                UiEvent::ShowPermissionsPicker { entries } => {
                    app.permissions_picker = Some(PermissionsPicker::new(entries));
                    break;
                }
                UiEvent::ShowModePicker { entries } => {
                    app.mode_picker = Some(ModePicker::new(entries));
                    break;
                }
                UiEvent::ConfirmRequest {
                    prompt,
                    choices,
                    default_index,
                    kind,
                    responder,
                } => {
                    // Permission prompts list "Yes" as the first choice and
                    // we want that highlighted on open so a bare Enter
                    // approves. The Esc/Ctrl+C fallback still resolves to
                    // `default_index` ("No"), so cancelling stays safe.
                    let initial_cursor =
                        if matches!(kind, crate::tools::utils::ConfirmationType::Permission) {
                            0
                        } else {
                            default_index.min(choices.len().saturating_sub(1))
                        };
                    app.confirmation = Some(app::ConfirmationPrompt {
                        prompt,
                        cursor: initial_cursor,
                        default_index,
                        choices,
                        kind,
                        responder,
                    });
                    break;
                }
                UiEvent::WorkerShutdown(summary) => {
                    app.exit_summary = Some(summary);
                    app.should_quit = true;
                    // Unblock any worker parked on a confirmation modal
                    // — without this, `thread.join()` would deadlock
                    // because the responder lives inside `app`.
                    if let Some(prompt) = app.confirmation.take() {
                        let _ = prompt.responder.send(prompt.default_index);
                    }
                    // Drain any pending output events before tearing down —
                    // the stderr/stdout reader threads are a different mpsc
                    // sender than the worker, so a pre-shutdown
                    // `print_warning` can still be in flight and arrive
                    // moments after `WorkerShutdown`. Without this drain
                    // those lines would never reach the log.
                    let mut pending_batch: Vec<String> = Vec::new();
                    while let Ok(pending) = ui_rx.try_recv() {
                        if let UiEvent::Output { text, .. } = pending {
                            pending_batch.push(text);
                        }
                    }
                    if !pending_batch.is_empty() {
                        tui.queue_history_lines(pending_batch);
                    }
                    break;
                }
            }
        }

        render_frame(tui, app, &mut last_size)?;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Drive one `InlineTui::draw` cycle (BSU → refit viewport → flush
/// pending history → render → ESU → flush) with `OutputCapture`
/// paused for its duration.
///
/// We pause capture while drawing because some of the bytes we emit
/// (DECSTBM, reverse-index, BSU/ESU) must reach the real tty; if they
/// got caught in the fd-1 pipe they'd arrive later, out of order, as
/// visible garbage on the next screen redraw.
///
/// Historical footnote: we used to wrap this call in
/// `capture.pause()` / `capture.resume()` so DSR bytes emitted by
/// `cursor::position()` could reach the tty instead of the capture
/// pipe. That had a nasty side-effect — during pause, fd 1/2 point
/// back at the real tty, so any `println!` from the worker thread
/// that landed mid-draw wrote *directly* onto the screen at whatever
/// column the cursor was parked at, bypassing `scrollback` and
/// the diff engine. That's where the orange-`A`-before-"Hello!" and
/// the scattered code-block output were coming from: streaming text
/// racing with our render. Current code never calls
/// `cursor::position()` during the draw loop (only once at startup,
/// before `OutputCapture::install`), so the pause is pure harm — we
/// keep the pipe active the whole time.
fn render_frame(
    tui: &mut inline_tui::InlineTui,
    app: &mut App,
    last_size: &mut (u16, u16),
) -> Result<()> {
    let current_size = crossterm::terminal::size()?;
    let desired_height = ui::desired_viewport_height(app, current_size.0);
    tui.draw(desired_height, |f| ui::draw(f, app))?;
    *last_size = current_size;
    Ok(())
}
