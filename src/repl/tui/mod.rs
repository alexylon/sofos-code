//! Ratatui-based TUI for sofos-code.
//!
//! Architecture (module layout):
//!
//! - [`event`] — `Job`, `UiEvent`, channel payloads
//! - [`output`] — redirects fd 1/2 to pipes and streams lines back
//! - [`worker`] — dedicated thread that owns the `Repl`
//! - [`app`] — UI-side mutable state (log, input, queue, picker)
//! - [`ui`] — pure rendering functions
//! - this module — wires the pieces together and runs the event loop

pub mod app;
pub mod event;
pub mod output;
pub mod ui;
pub mod worker;

use std::fs::OpenOptions;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ansi_to_tui::IntoText;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{TerminalOptions, Viewport};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Interval, interval};

use crate::commands::{COMMANDS, Command};
use crate::error::Result;
use crate::repl::{Repl, SteerQueue};
use crate::ui::UI;

use app::{App, Picker};
use event::{Job, UiEvent};
use output::OutputCapture;

const TICK_INTERVAL: Duration = Duration::from_millis(90);
/// Maximum captured output lines coalesced into a single `insert_before`
/// call. A high value amortises terminal I/O when a tool streams a lot of
/// text; a finite cap keeps one burst from monopolising the event loop and
/// lets `Key` / interrupt events fire while a large log is being drained.
const MAX_OUTPUT_BATCH: usize = 256;
/// Height of the inline viewport — reserved at construction time and
/// fixed for the life of the session. Sized to fit the input box at its
/// maximum height plus the hint and status rows; see
/// [`ui::INLINE_VIEWPORT_ROWS`] for the breakdown. The input itself grows
/// and shrinks inside this region; unused rows render as blank cells
/// (indistinguishable from normal terminal rows).
const INLINE_VIEWPORT_HEIGHT: u16 = ui::INLINE_VIEWPORT_ROWS;

/// RAII guard that restores the terminal on drop no matter how we exit
/// (error, panic, early return).
///
/// In inline-viewport mode we deliberately do *not* enter the alternate
/// screen or enable mouse capture — that lets the terminal emulator keep
/// its native scrollback, scrollbar, mouse wheel, and copy-paste. We only
/// need raw mode for key-by-key input; everything else is the terminal's
/// job.
struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    fn install() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        // Enable bracketed paste so the terminal wraps pasted text in
        // `ESC [ 200 ~ ... ESC [ 201 ~` markers; crossterm then surfaces
        // it as a single `Event::Paste(String)` instead of a flood of
        // `Key` events. Without this, pasting "yes" while a confirmation
        // modal is open would auto-answer.
        //
        // We write through `stdout` rather than `/dev/tty` because
        // `OutputCapture` hasn't been installed yet at this point in
        // `tui::run`, so fd 1 is still the real tty.
        use std::io::Write;
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste);
        let _ = std::io::stdout().flush();
        Ok(Self { _private: () })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // By the time we run, `OutputCapture` has already been dropped
        // (the teardown order in `run` restores fds before this guard
        // drops), so writing to stdout reaches the real terminal again.
        use std::io::Write;
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
        let _ = std::io::stdout().flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Wraps `CrosstermBackend` so that cursor-position queries stop
/// hitting `crossterm::cursor::position()` once the input reader is
/// running. That call writes a DSR to the terminal and waits up to 2 s
/// for the response via crossterm's global `INTERNAL_EVENT_READER`
/// mutex — the same mutex `event::read()` holds while blocked. If
/// ratatui's `autoresize` triggers a cursor query while the input
/// reader is parked in `read()`, the 2 s timeout fires and the app
/// exits with "cursor position could not be read within a normal
/// duration".
///
/// We let the real DSR run exactly once — during `Terminal::with_options`,
/// before the input reader is spawned — so the viewport is placed
/// correctly relative to the shell's scrollback on startup. From then
/// on `skip_cursor_query` is flipped and we synthesize a cursor at
/// `height - viewport_height` (the top row of the inline viewport).
/// That position makes `compute_inline_size` emit just enough newlines
/// to move the cursor down to the terminal's bottom without scrolling
/// any rows into scrollback, and place the new viewport back at the
/// bottom. Synthesizing `(0, bottom)` instead would make it append a
/// full viewport-height of newlines, scrolling the previous viewport
/// contents off-screen on every resize.
struct SafeBackend<W: std::io::Write> {
    inner: CrosstermBackend<W>,
    skip_cursor_query: Arc<AtomicBool>,
    viewport_height: u16,
}

impl<W: std::io::Write> SafeBackend<W> {
    fn new(writer: W, skip_cursor_query: Arc<AtomicBool>, viewport_height: u16) -> Self {
        Self {
            inner: CrosstermBackend::new(writer),
            skip_cursor_query,
            viewport_height,
        }
    }
}

impl<W: std::io::Write> Backend for SafeBackend<W> {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        if self.skip_cursor_query.load(Ordering::SeqCst) {
            // Report the top row of the inline viewport as the
            // cursor. `compute_inline_size` will then emit exactly
            // `viewport_height - 1` newlines to reach the terminal's
            // bottom row without scrolling any visible rows into
            // scrollback, and place the new viewport back at the
            // bottom. Reporting the true bottom row would instead
            // scroll a full viewport-height of content off-screen on
            // every resize.
            let size = self.inner.size()?;
            let y = size.height.saturating_sub(self.viewport_height);
            return Ok(Position { x: 0, y });
        }
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> std::io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Run the sofos-code REPL with the TUI front end. Takes ownership of the
/// fully-initialized `Repl` and blocks until the user quits.
pub fn run(mut repl: Repl) -> Result<()> {
    // The backend writes to /dev/tty so ratatui rendering doesn't travel
    // through the stdout pipe we're about to install for output capture.
    let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    let tty_for_backend = tty.try_clone()?;

    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiEvent>();
    let (job_tx, job_rx) = std_mpsc::channel::<Job>();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| crate::error::SofosError::Config(format!("runtime: {}", e)))?;

    // Construct the terminal BEFORE installing output capture. Ratatui's
    // inline viewport queries the cursor position during construction via
    // `crossterm::cursor::position`, which writes its DSR to
    // `io::stdout()` — if fd 1 were already redirected into our pipe that
    // query would never reach the tty and the construction would hang.
    let _terminal_guard = TerminalGuard::install()?;
    let skip_cursor_query = Arc::new(AtomicBool::new(false));
    let backend = SafeBackend::new(
        tty_for_backend,
        Arc::clone(&skip_cursor_query),
        INLINE_VIEWPORT_HEIGHT,
    );
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(INLINE_VIEWPORT_HEIGHT),
        },
    )?;
    drop(tty);
    // From this point on, any cursor::position DSR would race with the
    // input reader's blocking `event::read()` for crossterm's global
    // event-reader mutex. Flipping the flag makes the backend
    // synthesize cursor positions instead — safe because the only
    // caller is ratatui's `compute_inline_size` during autoresize, and
    // our inline viewport is always anchored at the terminal's bottom
    // anyway. The construction call above has already placed the
    // viewport using the real cursor position.
    skip_cursor_query.store(true, Ordering::SeqCst);

    let mut capture = OutputCapture::install(ui_tx.clone())?;
    // colored detects its output is a pipe after redirection and disables
    // styling — force it back on so ANSI reaches the log.
    colored::control::set_override(true);

    // Register a confirmation handler so destructive tools like
    // `delete_file` can prompt the user through the TUI modal instead of
    // trying to read from a raw-mode stdin that the user can't reach. The
    // closure is stored in a process-wide `OnceLock` so it only installs
    // once per process.
    install_confirm_handler(ui_tx.clone());

    let interrupt = Arc::new(AtomicBool::new(false));
    repl.install_interrupt_flag(Arc::clone(&interrupt));

    // Shared steering buffer: the TUI pushes text onto this vec when the
    // user types while a turn is already running, and the worker's tool
    // loop drains it between iterations so the model can see the new
    // message before its next API call.
    let steer_queue: SteerQueue = Arc::new(Mutex::new(Vec::new()));
    repl.install_steer_queue(Arc::clone(&steer_queue));

    let model_label = repl.model_label();

    let worker_handle = worker::spawn(repl, job_rx, ui_tx.clone(), Arc::clone(&interrupt))?;
    spawn_input_reader(ui_tx.clone())?;

    let mut app = App::new(model_label);
    UI::print_welcome();

    drop(ui_tx);

    let result = runtime.block_on(async {
        event_loop(
            &mut terminal,
            &mut app,
            &mut capture,
            ui_rx,
            job_tx.clone(),
            Arc::clone(&interrupt),
            Arc::clone(&steer_queue),
        )
        .await
    });

    let _ = job_tx.send(Job::Shutdown);
    let _ = worker_handle.thread.join();

    // Drop capture FIRST so fd 1 / fd 2 point at the real terminal
    // again — then `TerminalGuard::drop` can write the
    // disable-bracketed-paste sequence through stdout and actually
    // reach the tty. With the reverse order the sequence would land in
    // the (already-dead) pipe and the user's shell would be left with
    // bracketed paste enabled.
    drop(capture);
    drop(_terminal_guard);
    colored::control::unset_override();

    if let Some(summary) = app.exit_summary.take() {
        UI::display_session_summary(&summary.model, summary.input_tokens, summary.output_tokens);
        UI::print_goodbye();
    }

    result
}

async fn event_loop(
    terminal: &mut Terminal<SafeBackend<std::fs::File>>,
    app: &mut App,
    capture: &mut OutputCapture,
    mut ui_rx: UnboundedReceiver<UiEvent>,
    job_tx: std_mpsc::Sender<Job>,
    interrupt: Arc<AtomicBool>,
    steer_queue: SteerQueue,
) -> Result<()> {
    let mut tick: Interval = interval(TICK_INTERVAL);
    // Track the last size we've rendered at so we can detect resizes *before*
    // `Terminal::draw` runs `autoresize` and trips the captured-stdout path.
    // Start at a sentinel so the very first draw always goes through the
    // `pause → draw → resume` path: there's a window between
    // `Terminal::with_options` and this point during which the user could
    // resize, and ratatui's internal `last_known_area` would be stale —
    // `autoresize` on that first draw would then route through
    // `cursor::position` against the captured stdout and time out.
    let mut last_size: (u16, u16) = (0, 0);
    draw_with_capture_support(terminal, app, capture, &mut last_size)?;

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
                    if app.busy && app.confirmation.is_none() {
                        app.advance_spinner();
                    }
                    break;
                }
                UiEvent::Output { kind: _, text } => {
                    // If the terminal has been resized since the last
                    // draw, ratatui's `last_known_area` is stale —
                    // `insert_before` would compute scroll/viewport
                    // positions against the old dimensions. Sync first
                    // via a normal draw, which also picks up the size
                    // change safely via `pause`/`resume`.
                    if crossterm::terminal::size()? != last_size {
                        draw_with_capture_support(terminal, app, capture, &mut last_size)?;
                    }
                    // Batch-drain consecutive `Output` events so a tool
                    // that streams thousands of lines resolves in a
                    // single `insert_before` call instead of one per
                    // line. Non-Output events interrupt the drain so
                    // keypresses (especially ESC / Ctrl+C) aren't stuck
                    // behind an output backlog.
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
                    push_output_above_viewport(terminal, &batch)?;
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
                    } else {
                        handle_idle_key(app, key, &job_tx, &interrupt, &steer_queue);
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
                    if app.confirmation.is_none() && app.picker.is_none() {
                        app.textarea.insert_str(text);
                    }
                    break;
                }
                UiEvent::Resize => {
                    // Handled at draw time by `draw_with_capture_support`; the
                    // variant is kept as a wake-up signal so a resize is
                    // reflected immediately instead of waiting for the next
                    // tick.
                    break;
                }
                UiEvent::WorkerBusy(label) => {
                    app.start_busy(label);
                    break;
                }
                UiEvent::WorkerIdle => {
                    app.finish_busy();
                    // Don't drain the queue while a modal (resume picker) is
                    // open — the user hasn't committed to a choice yet and a
                    // queued message would race with the selection.
                    if app.picker.is_none() {
                        // Steer messages the tool loop didn't consume —
                        // e.g. the turn ended without ever hitting a
                        // tool-call boundary, or the user submitted after
                        // the last drain — are flushed here as a new
                        // `Job::Message` so they still reach the model.
                        // Recover from lock poisoning via `into_inner`
                        // so a panic elsewhere doesn't eat the user's
                        // pending mid-turn messages.
                        let residual: Vec<String> = std::mem::take(
                            &mut *steer_queue
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
                        push_output_above_viewport(terminal, &pending_batch)?;
                    }
                    break;
                }
            }
        }

        draw_with_capture_support(terminal, app, capture, &mut last_size)?;

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Run `terminal.draw(...)` with output capture temporarily paused whenever
/// the terminal's size has changed since the last render. On a resize,
/// `Terminal::draw` triggers `autoresize` → `compute_inline_size` →
/// `crossterm::cursor::position`, which writes its DSR to `io::stdout()`
/// (not to the backend writer). Without pausing, the DSR goes into our pipe
/// and the 2-second poll times out.
fn draw_with_capture_support(
    terminal: &mut Terminal<SafeBackend<std::fs::File>>,
    app: &mut App,
    capture: &mut OutputCapture,
    last_size: &mut (u16, u16),
) -> Result<()> {
    // `crossterm::terminal::size` uses ioctl, not DSR, so it's safe to call
    // while stdout is captured.
    let current_size = crossterm::terminal::size()?;
    if current_size != *last_size {
        capture.pause();
        let result = terminal.draw(|f| ui::draw(f, app));
        capture.resume();
        *last_size = current_size;
        result?;
    } else {
        terminal.draw(|f| ui::draw(f, app))?;
    }
    Ok(())
}

fn handle_idle_key(
    app: &mut App,
    key: KeyEvent,
    job_tx: &std_mpsc::Sender<Job>,
    interrupt: &Arc<AtomicBool>,
    steer_queue: &SteerQueue,
) {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char('c') if ctrl => {
            if app.busy {
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
        KeyCode::Char('d') if ctrl && !app.busy && app.textarea.is_empty() => {
            request_shutdown(app, job_tx);
        }
        KeyCode::Char('v') if ctrl => {
            handle_clipboard_paste(app);
        }
        // Alt+Up / Alt+Down cycle previously-submitted messages
        // without shadowing the textarea's own Up/Down cursor keys.
        KeyCode::Up if alt && !ctrl => {
            app.history_prev();
        }
        KeyCode::Down if alt && !ctrl => {
            app.history_next();
        }
        KeyCode::Esc if app.busy => {
            interrupt.store(true, Ordering::SeqCst);
        }
        // Plain Enter (no shift/alt/ctrl) submits. Shift+Enter inserts a
        // newline and falls through to the textarea.
        KeyCode::Enter if !shift && !alt && !ctrl => {
            submit_input(app, job_tx, steer_queue);
        }
        // Plain Tab on a `/…` line tries to complete the slash command;
        // otherwise it falls through to the textarea (indent).
        KeyCode::Tab if !shift && !alt && !ctrl => {
            if !try_complete_command(app) {
                app.handle_textarea_input(key);
            }
        }
        _ => app.handle_textarea_input(key),
    }
}

/// Autocomplete a `/command` prefix in the textarea against the static
/// `COMMANDS` list. Returns `true` when the key was consumed (the input
/// started with `/`) and `false` when the caller should fall through to
/// the textarea's default Tab behaviour.
///
/// - Zero matches: consume the key and do nothing (prevents a `\t` from
///   sneaking into an otherwise-command-looking line).
/// - One match: complete fully.
/// - Multiple matches: complete to their longest common prefix, leaving
///   the cursor where the next character must be typed.
fn try_complete_command(app: &mut App) -> bool {
    let text = app.input_text();
    // Commands are single-token; if the user has multi-line input the
    // textarea cursor is past a newline and inserting the completion
    // delta would land on the wrong line. Bail and let Tab fall
    // through to the textarea's normal indent behaviour.
    if text.contains('\n') {
        return false;
    }
    let trimmed = text.trim_end();
    if !trimmed.starts_with('/') {
        return false;
    }

    let matches: Vec<&'static str> = COMMANDS
        .iter()
        .copied()
        .filter(|cmd| cmd.starts_with(trimmed))
        .collect();
    match matches.as_slice() {
        [] => {}
        [single] => {
            let delta = &single[trimmed.len()..];
            if !delta.is_empty() {
                // Move to end-of-line so the inserted delta always
                // lands at the tail, even if the user's cursor was
                // mid-edit when they pressed Tab.
                app.textarea.move_cursor(tui_textarea::CursorMove::End);
                app.textarea.insert_str(delta);
            }
        }
        many => {
            let lcp = longest_common_prefix(many);
            if lcp.len() > trimmed.len() {
                let delta = &lcp[trimmed.len()..];
                app.textarea.move_cursor(tui_textarea::CursorMove::End);
                app.textarea.insert_str(delta);
            }
        }
    }
    true
}

fn longest_common_prefix(items: &[&'static str]) -> &'static str {
    let Some(&first) = items.first() else {
        return "";
    };
    let mut end = first.len();
    for item in &items[1..] {
        let mut i = 0;
        let a = first.as_bytes();
        let b = item.as_bytes();
        while i < end && i < b.len() && a[i] == b[i] {
            i += 1;
        }
        end = i;
        if end == 0 {
            break;
        }
    }
    // Safe: we only shrink `end` to positions we verified matched the
    // first string's bytes, so the slice is valid UTF-8 (it's a prefix of
    // an `&'static str`).
    &first[..end]
}

/// Handle `Ctrl+V`. Tries the clipboard for an image first; if one is
/// present, store it on `App` and insert a circled-number marker into the
/// textarea so `submit_input` can correlate markers to images. Otherwise
/// falls back to pasting text from the clipboard.
fn handle_clipboard_paste(app: &mut App) {
    if let Some(image) = crate::clipboard::get_clipboard_image() {
        let idx = app.pasted_images.len();
        app.pasted_images.push(image);
        let marker = crate::clipboard::marker_for_index(idx);
        app.textarea.insert_str(format!("{} ", marker));
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

/// Push one or more captured stdout/stderr lines above the inline viewport
/// in a single `insert_before` call. Combining a burst of captured lines
/// into one call is critical for responsiveness: a tool streaming thousands
/// of lines would otherwise issue thousands of individual `insert_before`
/// calls, blocking the event loop long enough that keypresses and ESC
/// appear unresponsive.
///
/// The input slice preserves original line boundaries — each element is one
/// captured line with its trailing newline already stripped.
fn push_output_above_viewport(
    terminal: &mut Terminal<SafeBackend<std::fs::File>>,
    texts: &[String],
) -> Result<()> {
    if texts.is_empty() {
        return Ok(());
    }

    // Re-join with `\n` so `ansi-to-tui` parses all captured lines in a
    // single pass and produces one `Text` with one `Line` per original
    // line. That keeps SGR state across line boundaries correct *within*
    // the batch — lines sent in the same batch inherit each other's
    // style. The trailing `\n` ensures a batch ending with an empty
    // line (e.g. a blank `println!()`) still yields its own row in the
    // parsed text; without it, `join` would swallow the empty tail.
    let mut joined = texts.join("\n");
    joined.push('\n');
    let parsed: Text<'static> = joined.as_bytes().into_text().unwrap_or_else(|_| {
        // Fallback when ANSI parsing fails: build one `Line` per input
        // string so `Span::raw` never contains a `\n` (Spans are
        // single-line by construction).
        Text::from(
            texts
                .iter()
                .map(|t| Line::from(Span::raw(t.clone())))
                .collect::<Vec<_>>(),
        )
    });

    let width = terminal.size()?.width;
    let height = {
        let probe = Paragraph::new(parsed.clone()).wrap(Wrap { trim: false });
        u16::try_from(probe.line_count(width).max(1)).unwrap_or(u16::MAX)
    };
    terminal.insert_before(height, |buf| {
        Paragraph::new(parsed)
            .wrap(Wrap { trim: false })
            .render(buf.area, buf);
    })?;
    Ok(())
}

/// Ask the worker to shut down. The worker will reply with
/// `UiEvent::WorkerShutdown(summary)`, which drives the final redraw and
/// breaks the event loop from `event_loop`. Setting `should_quit` directly
/// here would race the summary event and lose it.
fn request_shutdown(app: &mut App, job_tx: &std_mpsc::Sender<Job>) {
    if job_tx.send(Job::Shutdown).is_err() {
        // Worker already gone — nothing will reply, quit immediately.
        app.should_quit = true;
    }
}

/// Install a process-wide confirmation handler that turns synchronous
/// `confirm_multi_choice` calls from the worker thread into
/// `UiEvent::ConfirmRequest` messages on the TUI channel. The closure
/// blocks on a std mpsc receiver so the worker stays in-flight until the
/// UI answers.
fn install_confirm_handler(ui_tx: UnboundedSender<UiEvent>) {
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
fn handle_confirmation_key(app: &mut App, key: KeyEvent) {
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

fn handle_picker_key(app: &mut App, key: KeyEvent, job_tx: &std_mpsc::Sender<Job>) {
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

fn submit_input(app: &mut App, job_tx: &std_mpsc::Sender<Job>, steer_queue: &SteerQueue) {
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
    let will_steer = app.busy && !is_command && images.is_empty();

    // Echo the submitted line into the log so the user sees what they
    // sent, even while the worker is still processing or the message is
    // queued. Steered messages use a distinct glyph and a subtitle so
    // the user knows they've been accepted but won't land until the
    // next tool-call boundary.
    use colored::Colorize;
    let glyph = if will_steer {
        "↑"
    } else if app.is_safe_mode() {
        "λ:"
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
        if app.busy {
            // Commands can't be injected mid-turn — they need to run
            // as their own job. Queue FIFO so they execute in the
            // order the user typed them once the current job ends.
            app.queue.push_back(job);
        } else {
            let _ = job_tx.send(job);
        }
        return;
    }

    if will_steer {
        // Recover from a poisoned lock rather than silently dropping
        // the user's mid-turn message. `into_inner` returns the same
        // `Vec` the panicking thread was holding; we're still the
        // only writer on the UI side.
        steer_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(cleaned);
        return;
    }

    let job = Job::Message {
        text: cleaned,
        images,
    };
    if app.busy {
        app.queue.push_back(job);
    } else {
        let _ = job_tx.send(job);
    }
}

fn spawn_input_reader(tx: UnboundedSender<UiEvent>) -> std::io::Result<()> {
    thread::Builder::new()
        .name("sofos-input".into())
        .spawn(move || {
            while let Ok(event) = crossterm::event::read() {
                // Paste is forwarded as an atomic unit; the event loop
                // decides whether to apply it based on the current modal
                // state.
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
