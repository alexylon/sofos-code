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
pub mod event_loop;
pub mod inline_terminal;
pub mod inline_tui;
pub mod input;
pub mod keymap;
pub mod output;
pub mod scrollback;
pub mod sgr;
pub mod slash_popup;
pub mod ui;
pub mod worker;

use std::fs::OpenOptions;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::repl::{Repl, SteerBuffer};
use crate::ui::UI;

use app::App;
use event::{Job, UiEvent};
use event_loop::event_loop;
use input::spawn_input_reader;
use keymap::install_confirm_handler;
use output::OutputCapture;

/// Background tick cadence — frame rate vs. event-loop responsiveness
/// tradeoff. ~11 Hz is fast enough that streamed output looks fluid
/// without burning CPU on a quiet conversation.
pub(super) const TICK_INTERVAL: Duration = Duration::from_millis(90);
/// Maximum captured output lines coalesced into a single `insert_before`
/// call. A high value amortises terminal I/O when a tool streams a lot of
/// text; a finite cap keeps one burst from monopolising the event loop and
/// lets `Key` / interrupt events fire while a large log is being drained.
pub(super) const MAX_OUTPUT_BATCH: usize = 256;

/// Windows console code page identifier for UTF-8. The setting is
/// process-global, so byte-oriented writes that follow (including the
/// `CONOUT$` handle ratatui draws through) are interpreted as UTF-8.
#[cfg(windows)]
const CP_UTF8: u32 = 65001;

/// Snapshot of the Windows console's input and output code pages,
/// returned by [`switch_console_to_utf8`] so [`restore_console_code_pages`]
/// can put them back exactly as they were.
#[cfg(windows)]
#[derive(Copy, Clone)]
struct ConsoleCodePages {
    output: u32,
    input: u32,
}

/// Switch the Windows console's input and output code pages to UTF-8,
/// returning the previous values so the caller can restore them.
#[cfg(windows)]
fn switch_console_to_utf8() -> ConsoleCodePages {
    use windows_sys::Win32::System::Console::{
        GetConsoleCP, GetConsoleOutputCP, SetConsoleCP, SetConsoleOutputCP,
    };
    unsafe {
        let saved = ConsoleCodePages {
            output: GetConsoleOutputCP(),
            input: GetConsoleCP(),
        };
        SetConsoleOutputCP(CP_UTF8);
        SetConsoleCP(CP_UTF8);
        saved
    }
}

/// Restore the Windows console's input and output code pages to the
/// values captured by [`switch_console_to_utf8`].
#[cfg(windows)]
fn restore_console_code_pages(saved: ConsoleCodePages) {
    use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
    unsafe {
        SetConsoleOutputCP(saved.output);
        SetConsoleCP(saved.input);
    }
}

/// RAII guard that restores the terminal on drop no matter how we exit
/// (error, panic, early return).
///
/// In inline-viewport mode we deliberately do *not* enter the alternate
/// screen or enable mouse capture — that lets the terminal emulator keep
/// its native scrollback, scrollbar, mouse wheel, and copy-paste. We only
/// need raw mode for key-by-key input; everything else is the terminal's
/// job.
struct TerminalGuard {
    #[cfg(windows)]
    saved_code_pages: ConsoleCodePages,
    #[cfg(not(windows))]
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
        // Also push the kitty keyboard protocol's
        // `DISAMBIGUATE_ESCAPE_CODES` flag so terminals that implement it
        // (Ghostty, kitty, Alacritty, WezTerm, foot, recent iTerm with
        // the flag turned on in its profile) deliver Shift+Enter with
        // the SHIFT modifier set, rather than as a bare `Enter` — which
        // is what our newline binding needs to trigger. Terminals that
        // don't implement the protocol (Apple Terminal.app) silently
        // ignore the request, so the push is best-effort and harmless
        // elsewhere.
        //
        // We write through `stdout` rather than `/dev/tty` because
        // `OutputCapture` hasn't been installed yet at this point in
        // `tui::run`, so fd 1 is still the real tty.
        use std::io::Write;
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::EnableBracketedPaste,
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            ),
        );
        let _ = std::io::stdout().flush();

        // Switch the console to UTF-8 last so a failure in any earlier
        // setup step cannot leave the user's shell on a different code
        // page without a matching Drop to restore it. The default page
        // on `cmd.exe` is a legacy single-byte encoding that would
        // render our box-drawing glyphs as garbled multi-byte sequences.
        #[cfg(windows)]
        let saved_code_pages = switch_console_to_utf8();

        Ok(Self {
            #[cfg(windows)]
            saved_code_pages,
            #[cfg(not(windows))]
            _private: (),
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Tear down in reverse order of `install`: code page first, then
        // bracketed paste / keyboard flags, then raw mode. By the time
        // we run, `OutputCapture` has already been dropped (the teardown
        // order in `run` restores fds before this guard drops), so
        // writing to stdout reaches the real terminal again.
        #[cfg(windows)]
        restore_console_code_pages(self.saved_code_pages);

        use std::io::Write;
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags,
            crossterm::event::DisableBracketedPaste,
        );
        let _ = std::io::stdout().flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Run the sofos-code REPL with the TUI front end. Takes ownership of the
/// fully-initialized `Repl` and blocks until the user quits.
pub fn run(mut repl: Repl) -> Result<()> {
    // The backend writes to a real terminal handle so ratatui rendering
    // does not travel through the stdout pipe we are about to install
    // for output capture. `/dev/tty` is the canonical handle on Unix;
    // `CONOUT$` is the equivalent device name on Windows for the active
    // console's output buffer. Opening it directly gives us a write path
    // that survives the upcoming fd 1/2 redirection.
    #[cfg(unix)]
    let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
    #[cfg(windows)]
    let tty = OpenOptions::new().write(true).open("CONOUT$")?;
    let tty_for_backend = tty.try_clone()?;

    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiEvent>();
    let (job_tx, job_rx) = std_mpsc::channel::<Job>();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| crate::error::SofosError::Config(format!("runtime: {}", e)))?;

    // Construct the terminal BEFORE installing output capture. The
    // custom `Terminal` queries `cursor::position` once at construction
    // to anchor the initial viewport — if fd 1 were already redirected
    // into our pipe, that DSR would never reach the tty.
    let _terminal_guard = TerminalGuard::install()?;
    let backend = CrosstermBackend::new(tty_for_backend);
    let terminal = inline_terminal::Terminal::new(backend)?;
    drop(tty);
    // Wrap the raw `Terminal` in `InlineTui` (based on Codex's `Tui`
    // wrapper): every frame now runs inside a BSU/ESU bracket so the
    // emulator applies viewport-fit + history-flush + render atomically,
    // instead of painting them as three separate partial updates.
    let mut inline_tui = inline_tui::InlineTui::new(terminal);
    // The very first `inline_tui.draw` call (from `event_loop`) will
    // size the viewport to the bottom-pane's desired height via
    // `InlineTui::fit_viewport_height`, so we don't need an explicit
    // initial placement here — `Terminal::new` leaves viewport_area
    // anchored at `(0, cursor_pos.y, 0, 0)` and the first draw fills
    // in width/height.

    let capture = OutputCapture::install(ui_tx.clone())?;
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
    let steer_buffer: SteerBuffer = Arc::new(Mutex::new(Vec::new()));
    repl.install_steer_buffer(Arc::clone(&steer_buffer));

    let model_label = repl.model_label();
    // Grab the deferred startup text (logo + workspace / model / etc.)
    // before moving `repl` into the worker — we replay it through the
    // capture pipe below so it lands above the viewport.
    let startup_banner = repl.take_startup_banner();

    let worker_handle = worker::spawn(repl, job_rx, ui_tx.clone(), Arc::clone(&interrupt))?;
    spawn_input_reader(ui_tx.clone())?;

    let mut app = App::new(model_label.clone());
    // Everything we emit here rides the `OutputCapture` pipe (installed
    // above) and is handed to `scrollback::scroll_strings_above_viewport`
    // in the event loop — the same path every later tool/stdout line
    // takes. Printing the banner here, rather than before the TUI
    // started, is what guarantees it's visible on Ghostty / iTerm with
    // slow DSR, where our cursor-position fallback would otherwise
    // place the viewport on top of it.
    if !startup_banner.is_empty() {
        print!("{}", startup_banner);
    }
    UI::print_welcome();

    drop(ui_tx);

    let result = runtime.block_on(async {
        event_loop(
            &mut inline_tui,
            &mut app,
            ui_rx,
            job_tx.clone(),
            Arc::clone(&interrupt),
            Arc::clone(&steer_buffer),
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
        let summary_printed = UI::display_session_summary(
            &summary.model,
            summary.input_tokens,
            summary.output_tokens,
            summary.cache_read_tokens,
            summary.cache_creation_tokens,
            summary.peak_single_turn_input_tokens,
        );
        // The summary emits its own leading newline when it prints; if
        // it short-circuited, the cursor is still parked at the end of
        // the status row, so emit an escape-newline ourselves —
        // otherwise "Goodbye!" would land flush against "thinking: …
        // tok".
        if !summary_printed {
            println!();
        }
        UI::print_goodbye();
    }

    result
}

/// Ask the worker to shut down. The worker will reply with
/// `UiEvent::WorkerShutdown(summary)`, which drives the final redraw and
/// breaks the event loop from `event_loop`. Setting `should_quit` directly
/// here would race the summary event and lose it.
pub(super) fn request_shutdown(app: &mut App, job_tx: &std_mpsc::Sender<Job>) {
    if job_tx.send(Job::Shutdown).is_err() {
        // Worker already gone — nothing will reply, quit immediately.
        app.should_quit = true;
    }
}
