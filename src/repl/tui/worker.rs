//! Worker thread that owns the `Repl` and processes jobs from the TUI.
//!
//! The worker runs on a dedicated `std::thread` with its own tokio runtime so
//! the synchronous `Repl` methods (which internally call `block_on`) keep
//! working unchanged. Communication is via an `mpsc` channel for jobs and a
//! tokio `UnboundedSender` for UI events.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc::UnboundedSender;

use crate::commands::{Command, CommandResult};
use crate::repl::Repl;
use crate::ui::UI;

use super::event::{ExitSummary, Job, UiEvent};

/// Flush both stdout and stderr before signalling a turn is over.
/// Stdout is fully buffered when fd 1 is redirected to a pipe (our
/// `OutputCapture` setup) — writes from `println!` and the markdown
/// renderer sit in libstd's 8 KB buffer until it fills or we flush
/// explicitly. Without this, the last chunk of a response (especially
/// a final code block from the markdown highlighter) can stay buffered
/// past `UiEvent::WorkerIdle`, and the user sees the turn end with the
/// final lines missing from history.
fn flush_captured_streams() {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

pub struct WorkerHandle {
    pub thread: JoinHandle<()>,
}

pub fn spawn(
    mut repl: Repl,
    job_rx: Receiver<Job>,
    ui_tx: UnboundedSender<UiEvent>,
    interrupt: Arc<AtomicBool>,
) -> std::io::Result<WorkerHandle> {
    let thread = thread::Builder::new()
        .name("sofos-worker".into())
        .spawn(move || run(&mut repl, job_rx, ui_tx, interrupt))?;
    Ok(WorkerHandle { thread })
}

/// RAII guard that guarantees the UI receives exactly one
/// `UiEvent::WorkerShutdown` — even if the worker thread panics mid-job.
/// Without it, a panic unwind would skip the post-loop send, leaving the
/// event loop stuck with `app.busy = true` and Ctrl+C routed to the
/// interrupt-flag path instead of the shutdown path.
struct ShutdownSender<'a> {
    ui_tx: &'a UnboundedSender<UiEvent>,
    summary: Option<ExitSummary>,
    sent: bool,
}

impl<'a> ShutdownSender<'a> {
    fn new(ui_tx: &'a UnboundedSender<UiEvent>) -> Self {
        Self {
            ui_tx,
            summary: None,
            sent: false,
        }
    }

    fn set_summary(&mut self, summary: ExitSummary) {
        self.summary = Some(summary);
    }

    fn send_now(&mut self) {
        if self.sent {
            return;
        }
        let summary = self.summary.take().unwrap_or(ExitSummary {
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
        });
        let _ = self.ui_tx.send(UiEvent::WorkerShutdown(summary));
        self.sent = true;
    }
}

impl Drop for ShutdownSender<'_> {
    fn drop(&mut self) {
        self.send_now();
    }
}

fn run(
    repl: &mut Repl,
    job_rx: Receiver<Job>,
    ui_tx: UnboundedSender<UiEvent>,
    interrupt: Arc<AtomicBool>,
) {
    let mut shutdown = ShutdownSender::new(&ui_tx);
    let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));

    while let Ok(job) = job_rx.recv() {
        match job {
            Job::Shutdown => break,
            Job::Message { text, images } => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("processing".into()));
                if let Err(e) = repl.process_message(&text, images) {
                    if !matches!(e, crate::error::SofosError::Interrupted) {
                        if e.is_blocked() {
                            UI::print_blocked_with_hint(&e);
                        } else {
                            UI::print_error_with_hint(&e);
                        }
                    }
                }
                if let Err(e) = repl.save_current_session() {
                    UI::print_warning(&format!("Failed to save session: {}", e));
                }
                println!();
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::Command(cmd) => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("command".into()));
                match run_command(repl, cmd, &ui_tx) {
                    Ok(CommandResult::Exit) => {
                        // Don't send `WorkerIdle` here — the UI would
                        // drain the queue and dispatch the next job
                        // into a channel we're about to stop reading,
                        // dropping the queued message on the floor.
                        // Proceed straight to the post-loop shutdown.
                        break;
                    }
                    Ok(CommandResult::Continue) => {}
                    Err(e) => {
                        if e.is_blocked() {
                            UI::print_blocked_with_hint(&e);
                        } else {
                            UI::print_error_with_hint(&e);
                        }
                    }
                }
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::ResumeSelected(Some(session_id)) => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("loading".into()));
                if let Err(e) = repl.load_session_by_id(&session_id) {
                    UI::print_error_with_hint(&e);
                }
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::ResumeSelected(None) => {
                // User cancelled the picker — nothing to do besides
                // signalling idle so the queue can resume draining.
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
        }
    }

    let (model, input_tokens, output_tokens) = repl.get_session_summary();
    if let Err(e) = repl.save_current_session() {
        UI::print_warning(&format!("Failed to save session: {}", e));
    }
    flush_captured_streams();
    shutdown.set_summary(ExitSummary {
        model,
        input_tokens,
        output_tokens,
    });
    shutdown.send_now();
}

fn run_command(
    repl: &mut Repl,
    cmd: Command,
    ui_tx: &UnboundedSender<UiEvent>,
) -> crate::error::Result<CommandResult> {
    match cmd {
        Command::Resume => {
            // Collect sessions and hand control to the UI picker. The picker
            // calls back via `Job::ResumeSelected`.
            let sessions = repl.list_saved_sessions()?;
            if sessions.is_empty() {
                println!("No saved sessions found.");
                return Ok(CommandResult::Continue);
            }
            let _ = ui_tx.send(UiEvent::ShowResumePicker(sessions));
            Ok(CommandResult::Continue)
        }
        _ => cmd.execute(repl),
    }
}
