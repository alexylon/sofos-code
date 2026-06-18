//! Rendering for the inline-viewport TUI: a hint line, a rounded input
//! box, and a live status line — nothing else. The rest of the terminal
//! above the viewport is the terminal emulator's own scrollback.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};

use super::app::{App, ConfirmationPrompt, EffortPicker, ModelPicker, PermissionsPicker, Picker};
use super::inline_terminal::Frame;
use super::slash_popup::{MAX_VISIBLE_ROWS as SLASH_POPUP_MAX_ROWS, SlashPopup};
use crate::config::{PermissionPreset, SandboxMode};
use crate::tools::utils::ConfirmationType;

const ACCENT: Color = Color::Rgb(0xFF, 0x99, 0x33);
const BORDER_IDLE: Color = Color::Rgb(120, 120, 120);
const TITLE_FG: Color = Color::Rgb(180, 180, 180);
const HINT_KEY: Color = Color::Rgb(120, 180, 120);
const QUEUE_FG: Color = Color::Rgb(0xCC, 0xCC, 0x66);
const MODEL_FG: Color = Color::Rgb(110, 110, 110);
const STATUS_KEY: Color = Color::Rgb(150, 150, 150);
const STATUS_VAL: Color = Color::Rgb(200, 200, 200);
const READONLY_MODE_FG: Color = Color::Rgb(0xFF, 0xA5, 0x00);
const SANDBOX_OFF_FG: Color = Color::Rgb(0xFF, 0x55, 0x55);
const PICKER_BORDER: Color = Color::Rgb(140, 140, 140);
const SLASH_POPUP_BORDER: Color = Color::Rgb(120, 120, 120);
const SLASH_POPUP_NAME_FG: Color = Color::Rgb(200, 200, 200);
const SLASH_POPUP_DESC_FG: Color = Color::Rgb(130, 130, 130);
const SEP: &str = "  ·  ";
/// Rows reserved by [`draw_slash_popup`] beyond the visible row count
/// (top and bottom borders of the surrounding block).
const SLASH_POPUP_CHROME_ROWS: u16 = 2;
/// Minimum number of spaces between the command name column and the
/// description column inside the slash popup.
const SLASH_POPUP_COLUMN_GAP: usize = 2;
/// Glyph prefix for the highlighted row of any selection list.
const ROW_MARKER_SELECTED: &str = "❯ ";
/// Two-space prefix used on rows that are neither highlighted nor
/// carrying a scroll cue.
const ROW_MARKER_PLAIN: &str = "  ";
/// Maximum number of *content* rows the input box is allowed to grow to
/// before the textarea starts scrolling internally.
pub const MAX_INPUT_CONTENT_ROWS: u16 = 6;
const INPUT_BORDER_ROWS: u16 = 2;
const HINT_ROW_HEIGHT: u16 = 1;
const STATUS_ROW_HEIGHT: u16 = 1;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let input_height = input_box_height(app, area.width);
    let popup_height = slash_popup_height(&app.slash_popup);

    // Layout (top to bottom): hint · optional slash popup · input · status.
    // Every row is painted; an earlier revision reserved a blank spacer
    // row at the top, but that row stayed invisible to the diff engine
    // (default vs default = no-op) and could end up holding ghost
    // residue from a previous viewport position.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(HINT_ROW_HEIGHT),
            Constraint::Length(popup_height),
            Constraint::Length(input_height),
            Constraint::Length(STATUS_ROW_HEIGHT),
        ])
        .split(area);

    draw_hint(frame, rows[0], app);
    if popup_height > 0 {
        draw_slash_popup(frame, rows[1], &app.slash_popup);
    }
    draw_input(frame, rows[2], app);
    draw_status(frame, rows[3], app);

    if let Some(picker) = &app.picker {
        draw_picker(frame, area, picker);
    }
    if let Some(picker) = &app.model_picker {
        draw_model_picker(frame, area, picker);
    }
    if let Some(picker) = &app.effort_picker {
        draw_effort_picker(frame, area, picker);
    }
    if let Some(picker) = &app.permissions_picker {
        draw_permissions_picker(frame, area, picker);
    }
    if let Some(confirmation) = &app.confirmation {
        draw_confirmation(frame, area, confirmation);
    }
}

/// Non-choice rows reserved by `draw_confirmation`: 2 (borders) +
/// 1 (prompt) + 1 (separator) + 1 (separator) + 1 (hint). The per-choice
/// rows are added on top.
const CONFIRMATION_CHROME_ROWS: u16 = 6;
/// Non-entry rows reserved by `draw_picker` (top and bottom borders).
const PICKER_CHROME_ROWS: u16 = 2;
/// Ceiling on the picker's inline height so a long session list doesn't
/// swallow the screen; the picker scrolls internally past this.
const PICKER_MAX_VISIBLE_ENTRIES: u16 = 12;
/// Percentage of the available width occupied by the resume- and
/// model-picker overlays. Height tracks the available viewport
/// directly so every entry that fits is shown — the cursor never
/// drops below the visible window.
const PICKER_POPUP_WIDTH_PCT: u16 = 80;

/// Total viewport height: `HINT_ROW_HEIGHT` + `input_box_height` +
/// `STATUS_ROW_HEIGHT`. Feeds `InlineTui::draw`'s `desired_height`
/// each frame.
///
/// When a modal (confirmation prompt or resume picker) is active the
/// modal's height is used as a floor — otherwise the inline viewport
/// would clip multi-choice permission prompts to just the first row or
/// two, hiding options like "Yes and remember" / "No" / "No and
/// remember".
pub fn desired_viewport_height(app: &mut App, area_width: u16) -> u16 {
    let popup_rows = slash_popup_height(&app.slash_popup);
    let base = HINT_ROW_HEIGHT + popup_rows + input_box_height(app, area_width) + STATUS_ROW_HEIGHT;

    let modal_height = if let Some(confirmation) = &app.confirmation {
        let rows = u16::try_from(confirmation.choices.len()).unwrap_or(u16::MAX);
        CONFIRMATION_CHROME_ROWS.saturating_add(rows)
    } else if let Some(picker) = &app.picker {
        let rows = u16::try_from(picker.sessions.len())
            .unwrap_or(u16::MAX)
            .min(PICKER_MAX_VISIBLE_ENTRIES);
        PICKER_CHROME_ROWS.saturating_add(rows)
    } else if let Some(picker) = &app.model_picker {
        let rows = u16::try_from(picker.entries.len())
            .unwrap_or(u16::MAX)
            .min(PICKER_MAX_VISIBLE_ENTRIES);
        PICKER_CHROME_ROWS.saturating_add(rows)
    } else if let Some(picker) = &app.effort_picker {
        let rows = u16::try_from(picker.entries.len())
            .unwrap_or(u16::MAX)
            .min(PICKER_MAX_VISIBLE_ENTRIES);
        PICKER_CHROME_ROWS.saturating_add(rows)
    } else if let Some(picker) = &app.permissions_picker {
        let rows = u16::try_from(picker.entries.len())
            .unwrap_or(u16::MAX)
            .min(PICKER_MAX_VISIBLE_ENTRIES);
        PICKER_CHROME_ROWS.saturating_add(rows)
    } else {
        0
    };

    base.max(modal_height)
}

/// Rows the slash-command popup wants in the viewport. Zero when hidden.
fn slash_popup_height(popup: &SlashPopup) -> u16 {
    if !popup.is_visible() {
        return 0;
    }
    let visible = popup.matches().len().clamp(1, SLASH_POPUP_MAX_ROWS);
    let visible = u16::try_from(visible).unwrap_or(u16::MAX);
    visible.saturating_add(SLASH_POPUP_CHROME_ROWS)
}

/// Compute the input box's rendered height (content rows clamped into the
/// supported range, plus 2 for the rounded border). Uses the textarea's
/// own measurement so soft-wrapped logical lines occupy multiple visual
/// rows and the box grows to fit them.
fn input_box_height(app: &mut App, area_width: u16) -> u16 {
    // Borders::ALL reserves 1 column on each side; wrapping inside the
    // textarea happens at the inner width.
    let content_width = area_width.saturating_sub(2);
    let measure = app.textarea.measure(content_width);
    let content_rows = measure.content_rows.clamp(1, MAX_INPUT_CONTENT_ROWS);
    content_rows + INPUT_BORDER_ROWS
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let border_color = if app.busy() {
        Color::DarkGray
    } else {
        BORDER_IDLE
    };

    let mode = app.mode();
    let (prompt_glyph, prompt_color) = match mode {
        SandboxMode::ReadOnly => (" : ", Color::Yellow),
        SandboxMode::Sandboxed => (" > ", TITLE_FG),
        SandboxMode::Unsandboxed => (" # ", SANDBOX_OFF_FG),
    };
    let content_width = area.width.saturating_sub(2);

    let mut textarea = app.textarea.clone();
    let measure = textarea.measure(content_width);
    let overflow_rows = measure.content_rows.saturating_sub(MAX_INPUT_CONTENT_ROWS);

    let mut title_spans = vec![Span::styled(
        prompt_glyph,
        Style::default()
            .fg(prompt_color)
            .add_modifier(Modifier::BOLD),
    )];
    if overflow_rows > 0 {
        // Long paste / long draft: the textarea is showing only the
        // first `MAX_INPUT_CONTENT_ROWS` rows. Surface the cap in the
        // title line so the user knows their input continues below the
        // visible window.
        title_spans.push(Span::styled(
            format!(" … +{} more ", overflow_rows),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let title = Line::from(title_spans);

    // Cursor styling tracks the access mode: an orange underline in
    // read-only mode, a plain reversed block in the sandboxed mode, and a
    // red reversed block in the unsandboxed mode.
    let cursor_style = match mode {
        SandboxMode::ReadOnly => Style::default()
            .fg(READONLY_MODE_FG)
            .add_modifier(Modifier::UNDERLINED),
        SandboxMode::Sandboxed => Style::default().add_modifier(Modifier::REVERSED),
        SandboxMode::Unsandboxed => Style::default()
            .fg(SANDBOX_OFF_FG)
            .add_modifier(Modifier::REVERSED),
    };
    textarea.set_cursor_style(cursor_style);
    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .title(title),
    );
    textarea.set_style(Style::default().fg(Color::White));
    frame.render_widget(&textarea, area);
}

fn draw_hint(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    // While a modal confirmation is open the worker is blocked waiting
    // for the user's answer, not running — suppress the spinner and show
    // a clear "waiting on you" hint instead.
    if app.confirmation.is_some() {
        spans.push(Span::styled(
            " ⏸ ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "awaiting confirmation",
            Style::default().fg(ACCENT),
        ));
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "↑↓ ⏎  answer  ·  esc cancel",
            Style::default().fg(Color::DarkGray),
        ));
        if !app.queue.is_empty() {
            spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                format!("queued: {}", app.queue.len()),
                Style::default().fg(QUEUE_FG).add_modifier(Modifier::BOLD),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    // Resume picker takes over input — hint bar should reflect that
    // instead of advertising the normal send / newline keys.
    if app.picker.is_some()
        || app.model_picker.is_some()
        || app.effort_picker.is_some()
        || app.permissions_picker.is_some()
    {
        let label = if app.model_picker.is_some() {
            "pick a model"
        } else if app.effort_picker.is_some() {
            "pick an effort"
        } else if app.permissions_picker.is_some() {
            "pick what the assistant may do"
        } else {
            "pick a session"
        };
        spans.push(Span::styled(" ❯ ", Style::default().fg(HINT_KEY)));
        spans.push(Span::styled(label, Style::default().fg(Color::Gray)));
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "↑↓ ⏎  select  ·  esc cancel",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    if app.busy() {
        let frame_ch = app.spinner_frame();
        spans.push(Span::styled(
            format!(" {} ", frame_ch),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        let label = if app.busy_label.is_empty() {
            "working"
        } else {
            app.busy_label.as_str()
        };
        let elapsed = app.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let elapsed_str = if elapsed >= 60 {
            format!("{}m {}s", elapsed / 60, elapsed % 60)
        } else {
            format!("{}s", elapsed)
        };
        spans.push(Span::styled(
            format!("{}… {}", label, elapsed_str),
            Style::default().fg(ACCENT),
        ));
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "esc to interrupt",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        spans.push(Span::styled(" ⏎ ", Style::default().fg(HINT_KEY)));
        spans.push(Span::styled(
            "send  ·  ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled("shift+⏎ ", Style::default().fg(HINT_KEY)));
        spans.push(Span::styled(
            "newline  ·  ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled("/ ", Style::default().fg(HINT_KEY)));
        spans.push(Span::styled(
            "commands",
            Style::default().fg(Color::DarkGray),
        ));
    }

    if !app.queue.is_empty() {
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("queued: {}", app.queue.len()),
            Style::default().fg(QUEUE_FG).add_modifier(Modifier::BOLD),
        ));
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    // Fall back to the model name we already know if the worker hasn't
    // pushed a full snapshot yet (e.g. very first frame).
    // `app.mode()` is honest before the first snapshot too (it falls back to
    // the host default), so the status line and the prompt glyph agree.
    let mode = app.mode();
    let (model, approval, reasoning, in_tok, out_tok, cache_read, cache_create) = match &app.status
    {
        Some(s) => (
            s.model.as_str(),
            s.approval,
            s.reasoning.as_str(),
            s.input_tokens,
            s.output_tokens,
            s.cache_read_tokens,
            s.cache_creation_tokens,
        ),
        None => (
            app.model_label.as_str(),
            crate::config::ApprovalPolicy::OnRequest,
            "",
            0u32,
            0u32,
            0u32,
            0u32,
        ),
    };

    // The escalation policy is folded into the preset label, so the status
    // line shows one preset and no separate approval field. The mode is kept
    // honest at the source (a Sandboxed mode always has a usable sandbox), so
    // `current` needs no availability fix-up here.
    let preset = PermissionPreset::current(mode, approval);
    let mode_style = match preset.mode() {
        SandboxMode::ReadOnly => Style::default()
            .fg(READONLY_MODE_FG)
            .add_modifier(Modifier::BOLD),
        SandboxMode::Sandboxed => Style::default().fg(STATUS_VAL),
        SandboxMode::Unsandboxed => Style::default()
            .fg(SANDBOX_OFF_FG)
            .add_modifier(Modifier::BOLD),
    };
    let mut spans: Vec<Span> = vec![
        Span::styled(" model: ", Style::default().fg(STATUS_KEY)),
        Span::styled(
            model,
            Style::default().fg(STATUS_VAL).add_modifier(Modifier::BOLD),
        ),
        Span::styled(SEP, Style::default().fg(Color::DarkGray)),
        Span::styled("mode: ", Style::default().fg(STATUS_KEY)),
        Span::styled(preset.label(), mode_style),
    ];

    if !reasoning.is_empty() {
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(reasoning, Style::default().fg(MODEL_FG)));
    }

    if in_tok > 0 || out_tok > 0 {
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("tokens: {}↑ {}↓", in_tok, out_tok),
            Style::default().fg(MODEL_FG),
        ));
    }

    // Surface the cache numbers when either side has a non-zero
    // total. The cache-read percentage is the cheapest cost lever
    // the user has and they otherwise can't see it without
    // dropping out of the session.
    if cache_read > 0 || cache_create > 0 {
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            format!("cache: {}r {}w", cache_read, cache_create),
            Style::default().fg(MODEL_FG),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Per-frame layout decisions for the confirmation modal, derived once
/// from the available interior height so the draw code and the tests can
/// share the exact same arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfirmationLayout {
    /// When true, the modal includes both separators and the hint row.
    /// When false, only the prompt and the choice window fit.
    include_full_chrome: bool,
    /// First choice index (inclusive) visible in the scroll window.
    window_start: u16,
    /// One past the last choice index (exclusive) visible in the scroll
    /// window.
    window_end: u16,
}

/// Decide how to fit a confirmation modal into the space we were given.
///
/// * `interior` — interior height (already excluding the two border rows).
/// * `choices` — total number of choices the prompt carries.
/// * `cursor` — index of the currently-highlighted choice.
///
/// When everything fits we show the full chrome (prompt + separator +
/// choices + separator + hint). As interior shrinks we drop both
/// separators and the hint line; then we start scrolling the choice list
/// around the cursor so the selected option is always on screen.
fn confirmation_layout(interior: u16, choices: u16, cursor: u16) -> ConfirmationLayout {
    if choices == 0 {
        return ConfirmationLayout {
            include_full_chrome: false,
            window_start: 0,
            window_end: 0,
        };
    }

    // Non-choice rows the full chrome renders: prompt (always) +
    // leading separator + trailing separator + hint = 4. Choice rows
    // stack inside that budget, so the full-chrome threshold is
    // `4 + choices`.
    const PROMPT_ROWS: u16 = 1;
    const LEADING_SEP_ROWS: u16 = 1;
    const TRAILING_CHROME_ROWS: u16 = 2; // trailing separator + hint
    const FULL_NON_CHOICE_ROWS: u16 = PROMPT_ROWS + LEADING_SEP_ROWS + TRAILING_CHROME_ROWS;
    let include_full_chrome = interior >= FULL_NON_CHOICE_ROWS.saturating_add(choices);

    let leading_sep: u16 = if include_full_chrome {
        LEADING_SEP_ROWS
    } else {
        0
    };
    let trailing_chrome: u16 = if include_full_chrome {
        TRAILING_CHROME_ROWS
    } else {
        0
    };
    let choice_budget = interior
        .saturating_sub(PROMPT_ROWS)
        .saturating_sub(leading_sep)
        .saturating_sub(trailing_chrome);
    let visible = choice_budget.min(choices).max(1);

    let start = if visible >= choices {
        0
    } else {
        let half = visible / 2;
        let clamped_cursor = cursor.min(choices.saturating_sub(1));
        clamped_cursor
            .saturating_sub(half)
            .min(choices.saturating_sub(visible))
    };
    let end = start.saturating_add(visible).min(choices);

    ConfirmationLayout {
        include_full_chrome,
        window_start: start,
        window_end: end,
    }
}

fn draw_confirmation(frame: &mut Frame, area: Rect, prompt: &ConfirmationPrompt) {
    // Width is derived from the widest line the modal will draw so the
    // prompt and choice labels don't wrap inside the block (which would
    // silently clip them). We only fall back to a fractional width if
    // the terminal is too narrow for the content — in that case the
    // Paragraph wraps, but at least we've made the box as wide as
    // physically possible.
    const HINT_LINE: &str = "  ↑↓ navigate  ·  ⏎ select  ·  esc cancel";
    const MIN_WIDTH: u16 = 40;
    const HORIZONTAL_PADDING: u16 = 6; // 2 border + 4 inner padding
    /// Preferred minimum interior — enough for the prompt row and one
    /// choice row. Added to the two border rows to form a 4-row floor
    /// for the popup. On terminals shorter than 4 rows the popup
    /// clamps further down to `area.height` via the `min_height.min`
    /// on the next line, so this is a ceiling on the *minimum*, not a
    /// hard guarantee.
    const MIN_INTERIOR: u16 = 2;
    let longest_choice = prompt
        .choices
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(0);
    let prompt_chars = prompt.prompt.chars().count();
    let content_cols = prompt_chars
        .max(longest_choice + 4) // "  ❯ " prefix
        .max(HINT_LINE.chars().count());
    let ideal = u16::try_from(content_cols)
        .unwrap_or(u16::MAX)
        .saturating_add(HORIZONTAL_PADDING);
    let width = ideal.max(MIN_WIDTH).min(area.width);

    let choices_rows = u16::try_from(prompt.choices.len()).unwrap_or(u16::MAX);
    let full_height: u16 = 2 + 1 + 1 + choices_rows + 1 + 1;
    let min_height: u16 = 2 + MIN_INTERIOR;
    let height = full_height
        .min(area.height)
        .max(min_height.min(area.height));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let (title, accent) = match prompt.kind {
        ConfirmationType::Destructive => (" Confirm destructive action ", ACCENT),
        ConfirmationType::Permission => (" Permission required ", Color::Yellow),
        ConfirmationType::Info => (" Confirm ", HINT_KEY),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Line::from(vec![Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )]));

    let interior = popup.height.saturating_sub(2);
    let layout = confirmation_layout(
        interior,
        choices_rows,
        u16::try_from(prompt.cursor).unwrap_or(u16::MAX),
    );
    let include_full_chrome = layout.include_full_chrome;
    let start = layout.window_start;
    let end = layout.window_end;

    let mut lines: Vec<Line> = Vec::with_capacity(usize::from(interior));
    lines.push(Line::from(Span::styled(
        format!("  {}", prompt.prompt),
        Style::default().fg(Color::White),
    )));
    if include_full_chrome {
        lines.push(Line::from(Span::raw("")));
    }
    // If the cursor is parked on a scroll boundary, show the
    // direction cue on the nearest non-cursor row instead — that way
    // the cursor glyph and the "more content is hidden this way"
    // glyph don't have to fight for the same cell.
    let cursor_u16 = u16::try_from(prompt.cursor).unwrap_or(u16::MAX);
    let needs_top_cue = start > 0;
    let needs_bottom_cue = end < choices_rows;
    let cursor_at_top = cursor_u16 == start;
    let cursor_at_bottom = cursor_u16 + 1 == end;
    // `start + 1` is the row immediately below the top-edge row, so
    // the cue "slides down" one position when the cursor occupies
    // the top. Only set if we have at least two visible rows — on a
    // single-row window there's no second row to borrow.
    let top_cue_row: Option<u16> = if !needs_top_cue {
        None
    } else if cursor_at_top && end.saturating_sub(start) >= 2 {
        Some(start + 1)
    } else {
        Some(start)
    };
    let bottom_cue_row: Option<u16> = if !needs_bottom_cue {
        None
    } else if cursor_at_bottom && end.saturating_sub(start) >= 2 {
        Some(end - 2)
    } else {
        Some(end - 1)
    };

    for i in start..end {
        let idx = usize::from(i);
        let choice = &prompt.choices[idx];
        let selected = idx == prompt.cursor;
        let at_top_cue = Some(i) == top_cue_row && !selected;
        let at_bottom_cue = Some(i) == bottom_cue_row && !selected;
        let marker = if selected {
            ROW_MARKER_SELECTED
        } else if at_top_cue && at_bottom_cue {
            "⇅ "
        } else if at_top_cue {
            "▴ "
        } else if at_bottom_cue {
            "▾ "
        } else {
            ROW_MARKER_PLAIN
        };
        let style = if selected {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else if at_top_cue || at_bottom_cue {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(marker, style),
            Span::styled(choice.clone(), style),
        ]));
    }
    if include_full_chrome {
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("↑↓", Style::default().fg(HINT_KEY)),
            Span::styled(" navigate  ·  ", Style::default().fg(Color::DarkGray)),
            Span::styled("⏎", Style::default().fg(HINT_KEY)),
            Span::styled(" select  ·  ", Style::default().fg(Color::DarkGray)),
            Span::styled("esc", Style::default().fg(HINT_KEY)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup);
}

fn draw_slash_popup(frame: &mut Frame, area: Rect, popup: &SlashPopup) {
    if area.height <= SLASH_POPUP_CHROME_ROWS || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SLASH_POPUP_BORDER))
        .title(Line::from(Span::styled(
            " commands ",
            Style::default().fg(TITLE_FG).add_modifier(Modifier::BOLD),
        )));

    let interior_rows = area.height.saturating_sub(SLASH_POPUP_CHROME_ROWS);
    let matches = popup.matches();
    let visible = (interior_rows as usize).min(matches.len());
    let start = popup
        .scroll_top()
        .min(matches.len().saturating_sub(visible.max(1)));
    let end = (start + visible).min(matches.len());

    let name_col_width = matches
        .iter()
        .skip(start)
        .take(visible)
        .map(|entry| entry.name.chars().count())
        .max()
        .unwrap_or(0);

    let accent_bold = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let unselected_name = Style::default().fg(SLASH_POPUP_NAME_FG);
    let unselected_desc = Style::default().fg(SLASH_POPUP_DESC_FG);
    let unselected_marker = Style::default().fg(Color::DarkGray);
    let selected_desc = Style::default().fg(ACCENT);

    let mut lines: Vec<Line> = Vec::with_capacity(visible);
    for (offset, entry) in matches[start..end].iter().enumerate() {
        let idx = start + offset;
        let selected = idx == popup.cursor();
        let (marker, marker_style, name_style, desc_style) = if selected {
            (ROW_MARKER_SELECTED, accent_bold, accent_bold, selected_desc)
        } else {
            (
                ROW_MARKER_PLAIN,
                unselected_marker,
                unselected_name,
                unselected_desc,
            )
        };
        let name_padding = name_col_width.saturating_sub(entry.name.chars().count());
        lines.push(Line::from(vec![
            Span::styled(marker, marker_style),
            Span::styled(entry.name, name_style),
            Span::raw(" ".repeat(name_padding + SLASH_POPUP_COLUMN_GAP)),
            Span::styled(entry.description, desc_style),
        ]));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Centred popup rectangle. Width is a fraction of the area; height
/// is the full viewport so every entry fits.
fn picker_popup_rect(area: Rect) -> Rect {
    let width = area.width.saturating_mul(PICKER_POPUP_WIDTH_PCT) / 100;
    let x = area.x + area.width.saturating_sub(width) / 2;
    Rect {
        x,
        y: area.y,
        width,
        height: area.height,
    }
}

/// Block used by every picker overlay. `title` should already
/// include its leading and trailing spaces.
fn picker_block(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(PICKER_BORDER))
        .title(Line::from(vec![Span::styled(
            title,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )]))
}

/// Returns `(scroll, visible)` so a picker can render its entries
/// with `.skip(scroll).take(visible)` while keeping the cursor inside
/// the popup's interior.
fn picker_visible_window(popup_height: u16, cursor: usize, total: usize) -> (usize, usize) {
    let visible_slots = popup_height.saturating_sub(PICKER_CHROME_ROWS) as usize;
    let scroll = scroll_offset(cursor, total, visible_slots);
    let visible = visible_slots.min(total.saturating_sub(scroll));
    (scroll, visible)
}

/// Scroll offset that keeps `cursor` inside a `visible`-row window
/// over `total` items, with the cursor pinned near the middle.
fn scroll_offset(cursor: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || visible >= total || cursor < visible / 2 {
        0
    } else if cursor + visible / 2 >= total {
        total.saturating_sub(visible)
    } else {
        cursor.saturating_sub(visible / 2)
    }
}

/// Inline overlay for `/model`. Disabled rows render dim; the
/// highlight scrolls into view when the list exceeds the window.
fn draw_model_picker(frame: &mut Frame, area: Rect, picker: &ModelPicker) {
    let popup = picker_popup_rect(area);
    frame.render_widget(Clear, popup);

    let (scroll, visible) =
        picker_visible_window(popup.height, picker.cursor, picker.entries.len());

    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, entry)| {
            let selected = i == picker.cursor;
            let marker = if selected {
                ROW_MARKER_SELECTED
            } else {
                ROW_MARKER_PLAIN
            };
            // Three style tiers: selected/available, plain/available,
            // and disabled (always rendered dim regardless of cursor —
            // the cursor cannot land on a disabled row anyway).
            let (name_style, desc_style) = if !entry.is_available {
                (
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::DarkGray),
                )
            } else if selected {
                (
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(ACCENT),
                )
            } else {
                (
                    Style::default().fg(Color::Gray),
                    Style::default().fg(SLASH_POPUP_DESC_FG),
                )
            };
            let mut spans: Vec<Span> =
                vec![Span::raw(marker), Span::styled(entry.name, name_style)];
            if entry.is_current {
                spans.push(Span::styled(
                    "  (current)",
                    Style::default().fg(HINT_KEY).add_modifier(Modifier::ITALIC),
                ));
            }
            if !entry.is_available {
                spans.push(Span::styled(
                    "  (re-launch session to activate)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            spans.push(Span::raw("  "));
            spans.push(Span::styled(entry.description, desc_style));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(picker_block(" Select model "));
    frame.render_widget(list, popup);
}

/// Inline overlay for `/effort`. Every row is selectable — only
/// supported levels reach the entry list.
fn draw_effort_picker(frame: &mut Frame, area: Rect, picker: &EffortPicker) {
    let popup = picker_popup_rect(area);
    frame.render_widget(Clear, popup);

    let (scroll, visible) =
        picker_visible_window(popup.height, picker.cursor, picker.entries.len());

    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, entry)| {
            let selected = i == picker.cursor;
            let marker = if selected {
                ROW_MARKER_SELECTED
            } else {
                ROW_MARKER_PLAIN
            };
            let name_style = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let mut spans: Vec<Span> = vec![
                Span::raw(marker),
                Span::styled(entry.effort.as_label(), name_style),
            ];
            if entry.is_current {
                spans.push(Span::styled(
                    "  (current)",
                    Style::default().fg(HINT_KEY).add_modifier(Modifier::ITALIC),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(picker_block(" Select effort "));
    frame.render_widget(list, popup);
}

/// Inline overlay for `/permissions`. The `sandboxed-*` rows are dimmed and
/// skipped where the OS sandbox is unavailable (Windows, or a Linux box
/// without bubblewrap), mirroring the model picker's disabled rows.
fn draw_permissions_picker(frame: &mut Frame, area: Rect, picker: &PermissionsPicker) {
    let popup = picker_popup_rect(area);
    frame.render_widget(Clear, popup);

    let (scroll, visible) =
        picker_visible_window(popup.height, picker.cursor, picker.entries.len());

    let items: Vec<ListItem> = picker
        .entries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, entry)| {
            let selected = i == picker.cursor;
            let marker = if selected {
                ROW_MARKER_SELECTED
            } else {
                ROW_MARKER_PLAIN
            };
            let (name_style, desc_style) = if !entry.is_available {
                (
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::DarkGray),
                )
            } else if selected {
                (
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(ACCENT),
                )
            } else {
                (
                    Style::default().fg(Color::Gray),
                    Style::default().fg(SLASH_POPUP_DESC_FG),
                )
            };
            let mut spans: Vec<Span> = vec![
                Span::raw(marker),
                Span::styled(entry.preset.label(), name_style),
                Span::styled(format!("  {}", entry.preset.description()), desc_style),
            ];
            if entry.is_current {
                spans.push(Span::styled(
                    "  (current)",
                    Style::default().fg(HINT_KEY).add_modifier(Modifier::ITALIC),
                ));
            }
            if !entry.is_available {
                spans.push(Span::styled(
                    "  (unavailable on this platform)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(picker_block(" Permissions "));
    frame.render_widget(list, popup);
}

fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker) {
    let popup = picker_popup_rect(area);
    frame.render_widget(Clear, popup);

    let (scroll, visible) =
        picker_visible_window(popup.height, picker.cursor, picker.sessions.len());

    let items: Vec<ListItem> = picker
        .sessions
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, s)| {
            let selected = i == picker.cursor;
            let marker = if selected {
                ROW_MARKER_SELECTED
            } else {
                ROW_MARKER_PLAIN
            };
            let style = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let meta = format!(" ({} msgs)", s.message_count).dim();
            ListItem::new(Line::from(vec![
                Span::raw(marker),
                Span::styled(s.preview.clone(), style),
                meta,
            ]))
        })
        .collect();

    let list = List::new(items).block(picker_block(" Resume session "));
    frame.render_widget(list, popup);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new("test-model".into())
    }

    #[test]
    fn empty_input_height_is_one_content_row_plus_border() {
        let mut a = app();
        assert_eq!(input_box_height(&mut a, 80), 1 + INPUT_BORDER_ROWS);
    }

    #[test]
    fn long_line_grows_height_via_soft_wrap() {
        let mut a = app();
        // A 60-char line in a 20-col box wraps across multiple visual rows.
        a.textarea.insert_str("a".repeat(60));
        let h = input_box_height(&mut a, 20);
        assert!(
            h > 1 + INPUT_BORDER_ROWS,
            "expected wrapped height > {}, got {}",
            1 + INPUT_BORDER_ROWS,
            h
        );
        assert!(
            h <= MAX_INPUT_CONTENT_ROWS + INPUT_BORDER_ROWS,
            "expected height clamped to {}, got {}",
            MAX_INPUT_CONTENT_ROWS + INPUT_BORDER_ROWS,
            h
        );
    }

    #[test]
    fn very_long_content_is_clamped_to_max() {
        let mut a = app();
        a.textarea.insert_str("x".repeat(10_000));
        assert_eq!(
            input_box_height(&mut a, 40),
            MAX_INPUT_CONTENT_ROWS + INPUT_BORDER_ROWS
        );
    }

    #[test]
    fn multi_logical_lines_count_each_row() {
        let mut a = app();
        a.textarea.insert_str("a\nb\nc");
        // Three short logical lines in a wide box render as 3 rows.
        assert_eq!(input_box_height(&mut a, 80), 3 + INPUT_BORDER_ROWS);
    }

    #[test]
    fn tiny_width_does_not_panic() {
        let mut a = app();
        a.textarea.insert_str("hello world");
        for w in [0u16, 1, 2, 3] {
            let h = input_box_height(&mut a, w);
            assert!(h > INPUT_BORDER_ROWS);
            assert!(h <= MAX_INPUT_CONTENT_ROWS + INPUT_BORDER_ROWS);
        }
    }

    /// Simulated horizontal resize: a single long line that wraps in a
    /// narrow terminal should re-flow to fewer rows (and potentially one
    /// row) when the terminal grows wider, without stale cached values.
    #[test]
    fn height_reflows_when_width_changes() {
        let mut a = app();
        a.textarea.insert_str("a".repeat(50));
        let narrow = input_box_height(&mut a, 20);
        let wide = input_box_height(&mut a, 80);
        assert!(
            narrow > wide,
            "expected narrower width to wrap to more rows: narrow={narrow}, wide={wide}"
        );
        // Growing past the content width collapses to a single visual row.
        assert_eq!(input_box_height(&mut a, 200), 1 + INPUT_BORDER_ROWS);
    }

    /// Plenty of room → full chrome, all choices visible starting at 0.
    #[test]
    fn confirmation_layout_shows_all_choices_when_interior_is_generous() {
        let layout = confirmation_layout(
            /* interior */ 12, /* choices */ 4, /* cursor */ 2,
        );
        assert!(layout.include_full_chrome);
        assert_eq!(layout.window_start, 0);
        assert_eq!(layout.window_end, 4);
    }

    /// 4 choices on a 6-row terminal (interior=4): exactly the scenario
    /// the short-terminal fix targets. The hint and separators drop,
    /// and the window scrolls around the cursor. The cursor MUST stay
    /// within the visible window.
    #[test]
    fn confirmation_layout_scrolls_on_short_terminal() {
        for cursor in 0..4 {
            let layout = confirmation_layout(4, 4, cursor);
            assert!(!layout.include_full_chrome, "cursor={cursor}");
            let window_size = layout.window_end - layout.window_start;
            assert!(window_size >= 1, "cursor={cursor}: empty window {layout:?}");
            assert!(
                cursor >= layout.window_start && cursor < layout.window_end,
                "cursor={cursor} fell outside window {layout:?}",
            );
        }
    }

    /// Even in the tightest interior the layout reports a single-row
    /// window centred on the cursor. The *rendered* modal may still
    /// clip (the prompt row alone can eat the entire interior on a
    /// 3-row area) — that's the callsite's problem, not the layout's.
    /// This test asserts the layout-level invariant: the cursor index
    /// is always inside `[window_start, window_end)`.
    #[test]
    fn confirmation_layout_keeps_cursor_inside_window() {
        for cursor in [0u16, 1, 2, 5] {
            let layout = confirmation_layout(1, 6, cursor);
            assert_eq!(layout.window_end - layout.window_start, 1);
            let clamped = cursor.min(5);
            assert!(clamped >= layout.window_start && clamped < layout.window_end);
        }
    }

    /// Zero choices is a nonsense state, but the layout shouldn't panic
    /// or produce an `end < start` window if it ever arises.
    #[test]
    fn confirmation_layout_handles_zero_choices() {
        let layout = confirmation_layout(10, 0, 0);
        assert_eq!(layout.window_start, 0);
        assert_eq!(layout.window_end, 0);
    }

    /// Full-chrome threshold is exactly `prompt(1) + leading sep(1) +
    /// choices + trailing sep(1) + hint(1) = 4 + choices`. Catches the
    /// off-by-one that briefly required `5 + choices`.
    #[test]
    fn confirmation_layout_full_chrome_boundary_is_four_plus_choices() {
        // 4 choices need interior >= 8 for full chrome.
        assert!(confirmation_layout(8, 4, 0).include_full_chrome);
        assert!(!confirmation_layout(7, 4, 0).include_full_chrome);
        // 2 choices need interior >= 6 for full chrome.
        assert!(confirmation_layout(6, 2, 0).include_full_chrome);
        assert!(!confirmation_layout(5, 2, 0).include_full_chrome);
    }

    #[test]
    fn scroll_offset_keeps_cursor_inside_window() {
        // Fits in window — no scroll.
        assert_eq!(scroll_offset(0, 5, 10), 0);
        assert_eq!(scroll_offset(4, 5, 10), 0);

        // Cursor near top half — pinned to top.
        assert_eq!(scroll_offset(0, 20, 6), 0);
        assert_eq!(scroll_offset(2, 20, 6), 0);

        // Cursor in the middle — slides so cursor sits roughly centred.
        assert_eq!(scroll_offset(10, 20, 6), 10 - 3);
        assert_eq!(scroll_offset(8, 20, 6), 8 - 3);

        // Cursor near bottom — pinned so the last page stays flush.
        assert_eq!(scroll_offset(19, 20, 6), 20 - 6);
        assert_eq!(scroll_offset(18, 20, 6), 20 - 6);

        // Edge cases — empty window or total never panics.
        assert_eq!(scroll_offset(0, 0, 6), 0);
        assert_eq!(scroll_offset(5, 10, 0), 0);
    }

    #[test]
    fn scroll_offset_keeps_cursor_visible_for_every_position() {
        // For any (cursor, total, visible) the resulting window
        // [scroll, scroll + visible) must contain the cursor.
        for total in [1usize, 5, 7, 12, 30] {
            for visible in [1usize, 2, 3, 6, total] {
                for cursor in 0..total {
                    let s = scroll_offset(cursor, total, visible);
                    let window_end = s + visible.min(total.saturating_sub(s));
                    assert!(
                        cursor >= s && cursor < window_end,
                        "cursor={cursor} not in [{s}, {window_end}) (total={total}, visible={visible})"
                    );
                }
            }
        }
    }

    /// When chrome is on, every choice must be visible — we only drop
    /// into the scroll window path after dropping the chrome.
    #[test]
    fn confirmation_layout_full_chrome_shows_every_choice() {
        let layout = confirmation_layout(8, 4, 3);
        assert!(layout.include_full_chrome);
        assert_eq!(layout.window_start, 0);
        assert_eq!(layout.window_end, 4);
    }
}
