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

use super::event::{
    EffortPickerEntry, ExitSummary, Job, ModePickerEntry, ModelPickerEntry, PermissionsPickerEntry,
    UiEvent,
};

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
        // Reaching `send_now` without an installed summary means the
        // worker is panicking on the way out — set `panicked: true`
        // so the UI can prefix the goodbye line instead of pretending
        // a fully zeroed normal exit.
        let summary = self.summary.take().unwrap_or(ExitSummary {
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            peak_single_turn_input_tokens: 0,
            panicked: true,
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
                // Send WorkerBusy before clearing the interrupt flag
                // so an early Ctrl+C is routed to the polite-interrupt
                // path rather than `request_shutdown`.
                let _ = ui_tx.send(UiEvent::WorkerBusy("processing".into()));
                interrupt.store(false, Ordering::SeqCst);
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
                let label = match cmd {
                    crate::commands::Command::Compact => "compacting",
                    _ => "command",
                };
                let _ = ui_tx.send(UiEvent::WorkerBusy(label.into()));
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
                // Slash commands can mutate the conversation (`/compact`
                // rewrites the history, `/clear` resets it, `/permissions`
                // toggles the mode preamble). Persist after the command runs
                // so a `/exit` or Ctrl+C before the next prompt doesn't lose
                // the change.
                if let Err(e) = repl.save_current_session() {
                    UI::print_warning(&format!("Failed to save session: {}", e));
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
            Job::ModelSelected(Some(name)) => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("switching model".into()));
                repl.handle_model_set(name);
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::ModelSelected(None) => {
                // User cancelled the model picker — same idle handling
                // as the resume picker cancel branch.
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::EffortSelected(Some(effort)) => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("switching effort".into()));
                repl.handle_effort_set(effort);
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::EffortSelected(None) => {
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::ModeSelected(Some(mode)) => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("switching mode".into()));
                repl.handle_mode_set(mode);
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::ModeSelected(None) => {
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::PermissionsSelected(Some(preset)) => {
                interrupt.store(false, Ordering::SeqCst);
                let _ = ui_tx.send(UiEvent::WorkerBusy("switching permissions".into()));
                repl.apply_permission_preset(preset);
                // Applying a preset appends a mode preamble to the
                // conversation, so persist it now as the typed path does.
                if let Err(e) = repl.save_current_session() {
                    UI::print_warning(&format!("Failed to save session: {}", e));
                }
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::Status(repl.status_snapshot()));
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
            Job::PermissionsSelected(None) => {
                flush_captured_streams();
                let _ = ui_tx.send(UiEvent::WorkerIdle);
            }
        }
    }

    let summary = repl.get_session_summary();
    if let Err(e) = repl.save_current_session() {
        UI::print_warning(&format!("Failed to save session: {}", e));
    }
    flush_captured_streams();
    shutdown.set_summary(summary);
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
        Command::ModelPicker => {
            let entries = build_model_picker_entries(repl);
            let _ = ui_tx.send(UiEvent::ShowModelPicker { entries });
            Ok(CommandResult::Continue)
        }
        Command::EffortPicker => {
            let entries = build_effort_picker_entries(repl);
            let _ = ui_tx.send(UiEvent::ShowEffortPicker { entries });
            Ok(CommandResult::Continue)
        }
        Command::PermissionsPicker => {
            let entries = build_permissions_picker_entries(repl);
            let _ = ui_tx.send(UiEvent::ShowPermissionsPicker { entries });
            Ok(CommandResult::Continue)
        }
        Command::ModePicker => {
            let entries = build_mode_picker_entries(repl);
            let _ = ui_tx.send(UiEvent::ShowModePicker { entries });
            Ok(CommandResult::Continue)
        }
        _ => cmd.execute(repl),
    }
}

fn build_effort_picker_entries(repl: &Repl) -> Vec<EffortPickerEntry> {
    use crate::api::model_info;
    let info = model_info::lookup(&repl.model_label());
    let current = repl.current_reasoning_effort();
    info.supported_efforts
        .iter()
        .map(|effort| EffortPickerEntry {
            effort: *effort,
            is_current: *effort == current,
        })
        .collect()
}

fn build_permissions_picker_entries(repl: &Repl) -> Vec<PermissionsPickerEntry> {
    use crate::config::{PERMISSION_PRESETS, PermissionPreset};
    let available = repl.sandbox_available();
    let current = PermissionPreset::current(repl.mode, repl.approval_policy);
    PERMISSION_PRESETS
        .into_iter()
        .map(|preset| PermissionsPickerEntry {
            preset,
            is_current: preset == current,
            is_available: preset.is_available(available),
        })
        .collect()
}

fn build_mode_picker_entries(repl: &Repl) -> Vec<ModePickerEntry> {
    use crate::api::{ReasoningMode, model_info};
    let current = repl.current_reasoning_mode();
    let pro_available = model_info::supports_pro_mode(&repl.model_label());
    [ReasoningMode::Standard, ReasoningMode::Pro]
        .into_iter()
        .map(|mode| ModePickerEntry {
            mode,
            is_current: mode == current,
            is_available: mode == ReasoningMode::Standard || pro_available,
        })
        .collect()
}

fn build_model_picker_entries(repl: &Repl) -> Vec<ModelPickerEntry> {
    use crate::api::model_info;
    let current_model = repl.model_label();
    // Other-provider rows are unreachable mid-session — the LlmClient
    // is built once at startup.
    let current_provider = model_info::provider_for(&current_model);
    model_info::SUPPORTED_MODELS
        .iter()
        .map(|choice| ModelPickerEntry {
            name: choice.name,
            description: choice.description,
            is_current: choice.name == current_model,
            is_available: choice.provider == current_provider,
        })
        .collect()
}
