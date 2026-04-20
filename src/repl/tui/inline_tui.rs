//! Sofos inline-viewport frame driver. Owns a
//! [`inline_terminal::Terminal`] plus a queue of history lines, and
//! exposes a single [`InlineTui::draw`] entry point that walks the
//! "synchronized-update → resize replay → viewport fit → history
//! flush → render" sequence atomically.
//!
//! Resize policy: sofos keeps a bounded `history_log` of every line
//! ever queued for scrollback. When the terminal resizes, we wipe
//! the visible screen and replay the log from the top. The
//! alternative (querying the cursor via DSR on every resize and
//! offsetting the viewport by the cursor-y delta) is unreliable on
//! emulators where CPR is slow or where content reflow races the
//! query — we saw ghost viewports and overdrawn banners on Ghostty
//! and iTerm2. Replay is deterministic at the cost of bounded extra
//! work on a drag, which we consider worth it for visual
//! correctness.
//!
//! Frame lifecycle (`InlineTui::draw`):
//!
//! 1. Detect "screen size changed since last frame" (ioctl, not DSR).
//! 2. `BeginSynchronizedUpdate` (DCS 2026) — emulator buffers the rest.
//! 3. On resize: `clear_visible_screen` + reset viewport to screen
//!    origin + replay `history_log` into `pending_history_lines`.
//! 4. [`InlineTui::fit_viewport_height`] — set the viewport to the
//!    bottom-pane's desired height, DECSTBM-scrolling content above
//!    it up if the bottom would overflow.
//! 5. Flush `pending_history_lines` above the viewport via
//!    [`scrollback::scroll_strings_above_viewport`].
//! 6. Run the caller's render closure against the [`Frame`].
//! 7. `EndSynchronizedUpdate` + flush, always — a mid-frame error
//!    otherwise leaves the emulator stuck buffering.
//!
//! The overall frame shape (synchronized-update bracket around a
//! viewport-fit + history-flush + render triple) is patterned on the
//! OpenAI Codex CLI's `Tui::draw`
//! (<https://github.com/openai/codex/blob/main/codex-rs/tui/src/tui.rs>,
//! Apache-2.0); the sofos implementation drops the Zellij fallback,
//! alt-screen support, and job-control plumbing.

use std::fs::File;
use std::io;
use std::io::Write;

use crossterm::queue;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use super::inline_terminal;
use super::scrollback;

/// The concrete backend the TUI is built on: a [`CrosstermBackend`]
/// wrapping a duplicated `/dev/tty` file handle (so the app's fd 1/2
/// capture pipe never eats TUI escape sequences).
pub type TuiBackend = CrosstermBackend<File>;

/// Owns the low-level [`Terminal`](inline_terminal::Terminal) plus a
/// queue of history lines waiting to be inserted above the inline
/// viewport on the next [`draw`](Self::draw).
pub struct InlineTui {
    terminal: inline_terminal::Terminal<TuiBackend>,
    /// Lines the next [`draw`](Self::draw) should flush above the
    /// viewport. Drained on every frame that finds them non-empty.
    pending_history_lines: Vec<String>,
    /// Every line ever queued for scrollback, capped at
    /// [`HISTORY_LOG_CAPACITY`]. On resize we emit the whole log so a
    /// screen-wipe-and-repaint can reconstruct the banner + startup
    /// info + tool output from scratch. Needed because sofos operates
    /// on the terminal's native scrollback, which emulators can (and
    /// do) reflow in user-visible ways during drag-resize.
    history_log: Vec<String>,
    /// Set by [`compose_frame`](Self::compose_frame) when a resize is
    /// detected; drives the flush step to emit `history_log` in full
    /// rather than just `pending_history_lines`. Replaces an earlier
    /// pattern of `pending = history_log.clone()`, which paid an O(n)
    /// copy of up to [`HISTORY_LOG_CAPACITY`] owned strings on every
    /// resize tick — and drag-resize fires the event per-tick.
    replay_full_history: bool,
}

/// Upper bound on the retained history. Past that we drop from the
/// front. A long REPL session can accumulate thousands of tool-output
/// lines; we cap at a size that keeps resize-replay bounded (a few
/// hundred KB) while still covering any practical number of banner +
/// reasonably-sized tool calls.
const HISTORY_LOG_CAPACITY: usize = 10_000;

impl InlineTui {
    pub fn new(terminal: inline_terminal::Terminal<TuiBackend>) -> Self {
        Self {
            terminal,
            pending_history_lines: Vec::new(),
            history_log: Vec::new(),
            replay_full_history: false,
        }
    }

    /// Append history lines to be flushed above the viewport on the
    /// next [`draw`](Self::draw), *and* retain them in `history_log`
    /// for resize-replay.
    pub fn queue_history_lines(&mut self, lines: Vec<String>) {
        self.pending_history_lines.extend(lines.iter().cloned());
        self.history_log.extend(lines);
        // Evict the oldest lines in one drain call rather than
        // `remove(0)`-per-push: `drain(..n)` is a single shift, so a
        // batch push only pays the shift cost once regardless of how
        // many entries overflowed. Front-of-`Vec` drops are fine
        // because the oldest lines are long gone from the visible
        // screen — they live in the emulator's native scrollback,
        // which our resize replay can't reach anyway.
        if self.history_log.len() > HISTORY_LOG_CAPACITY {
            let excess = self.history_log.len() - HISTORY_LOG_CAPACITY;
            self.history_log.drain(..excess);
        }
    }

    /// One atomic frame. `desired_height` is the bottom-pane's desired
    /// viewport rows (hint + input + status). `render_callback` paints
    /// into the inline viewport's `Frame`.
    pub fn draw<F>(&mut self, desired_height: u16, render_callback: F) -> io::Result<()>
    where
        F: FnOnce(&mut inline_terminal::Frame),
    {
        // Detect "screen size changed since last frame" before opening
        // the sync bracket. Sofos deliberately avoids the CPR-based
        // offset heuristic Codex uses — it's unreliable on Ghostty
        // (slow DSR) and leaves ghost viewports when content reflow
        // outpaces the cursor query. Our resize path instead nukes
        // the visible screen and replays `history_log`, which is
        // deterministic and removes the whole class of "emulator and
        // app disagree on viewport row" bugs.
        let screen_size_changed = {
            let live = self.terminal.backend().size()?;
            live != self.terminal.last_known_screen_size
        };

        queue!(self.terminal.backend_mut(), BeginSynchronizedUpdate)?;

        let frame_result = self.compose_frame(screen_size_changed, desired_height, render_callback);

        // Close the BSU bracket and flush. Run these regardless of
        // whether the frame body succeeded — otherwise a mid-frame
        // error leaves the emulator stuck buffering.
        queue!(self.terminal.backend_mut(), EndSynchronizedUpdate)?;
        Write::flush(self.terminal.backend_mut())?;

        frame_result
    }

    /// The inside of [`InlineTui::draw`]'s synchronized-update bracket,
    /// pulled into its own function so the BSU/ESU pair can be emitted
    /// by a single caller no matter how the body exits (`?` early
    /// return included).
    fn compose_frame<F>(
        &mut self,
        screen_size_changed: bool,
        desired_height: u16,
        render_callback: F,
    ) -> io::Result<()>
    where
        F: FnOnce(&mut inline_terminal::Frame),
    {
        if screen_size_changed {
            // Nuke-and-replay path. ED2 (`\e[2J`) wipes every visible
            // row — including any residue the emulator painted when it
            // reflowed our viewport during the drag. We then reset the
            // viewport to the screen origin (fresh slate) and set
            // `replay_full_history` so the flush step below repaints
            // banner + welcome + tool output from `history_log` in
            // place. `invalidate_viewport` marks the diff engine's
            // back buffer stale so the bottom-pane paints in full
            // rather than only the cells that changed.
            //
            // Any lines queued this frame via `queue_history_lines`
            // were already extended into `history_log`, so dropping
            // `pending_history_lines` on the floor here is correct —
            // the replay re-emits them via the log.
            self.terminal.clear_visible_screen()?;
            let live = self.terminal.backend().size()?;
            self.terminal
                .set_viewport_area(Rect::new(0, 0, live.width, 0));
            self.terminal.invalidate_viewport();
            self.pending_history_lines.clear();
            self.replay_full_history = true;
        }

        // Re-size the inline viewport to the bottom-pane's desired
        // height before emitting history, so Phase 2 of
        // `scroll_strings_above_viewport` runs against the final
        // viewport rect.
        Self::fit_viewport_height(&mut self.terminal, desired_height)?;

        // Flush any queued history lines above the viewport. On a
        // resize frame we borrow `history_log` directly for the full
        // replay (no clone — drag-resize can fire per-tick, and the
        // retained log is up to `HISTORY_LOG_CAPACITY` owned strings).
        // On a steady-state frame we drain the incrementally-queued
        // `pending_history_lines`. No `clear()` / `invalidate_viewport`
        // between flush and render — the next step's cell emits will
        // repaint the bottom pane in place.
        if self.replay_full_history {
            scrollback::scroll_strings_above_viewport(&mut self.terminal, &self.history_log)?;
            // `pending_history_lines` is always a tail-subset of
            // `history_log` (every push via `queue_history_lines`
            // extends both, and capacity eviction only removes entries
            // that were already drained from pending on an earlier
            // flush). Drop pending here so that if it got re-populated
            // via `queue_history_lines` *between* a failed replay and
            // this successful one, the subsequent steady-state frame
            // doesn't emit those same lines a second time.
            self.pending_history_lines.clear();
            self.replay_full_history = false;
        } else if !self.pending_history_lines.is_empty() {
            let batch = std::mem::take(&mut self.pending_history_lines);
            scrollback::scroll_strings_above_viewport(&mut self.terminal, &batch)?;
        }

        // Run the render closure — `try_draw` flushes the backend for
        // us once the cursor-position rule is resolved.
        self.terminal.draw(render_callback)
    }

    /// Re-size the inline viewport to `desired_height`. If the new
    /// bottom would overflow the screen, scroll the content above the
    /// viewport up via DECSTBM by exactly enough rows to make it fit —
    /// that push-content-up step is what makes a growing viewport work
    /// without trampling existing scrollback.
    fn fit_viewport_height(
        terminal: &mut inline_terminal::Terminal<TuiBackend>,
        desired_height: u16,
    ) -> io::Result<()> {
        let screen = terminal.size()?;
        let mut area = terminal.viewport_area;
        area.height = desired_height.min(screen.height);
        area.width = screen.width;
        if area.bottom() > screen.height {
            let scroll_by = area.bottom() - screen.height;
            if area.top() > 0 {
                terminal
                    .backend_mut()
                    .scroll_region_up(0..area.top(), scroll_by)?;
            }
            area.y = screen.height - area.height;
        }
        if area != terminal.viewport_area {
            terminal.clear()?;
            terminal.set_viewport_area(area);
        }
        Ok(())
    }
}
