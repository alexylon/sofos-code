//! Rendering for the inline-viewport TUI: a rounded input box, a hint
//! line, and a live status line — nothing else. The rest of the terminal
//! above the viewport is the terminal emulator's own scrollback.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};

use super::app::{App, ConfirmationPrompt, Picker};
use super::event::Mode;
use crate::tools::utils::ConfirmationType;

const ACCENT: Color = Color::Rgb(0xFF, 0x99, 0x33);
const BORDER_IDLE: Color = Color::Rgb(120, 120, 120);
const TITLE_FG: Color = Color::Rgb(180, 180, 180);
const HINT_KEY: Color = Color::Rgb(120, 180, 120);
const QUEUE_FG: Color = Color::Rgb(0xCC, 0xCC, 0x66);
const MODEL_FG: Color = Color::Rgb(110, 110, 110);
const STATUS_KEY: Color = Color::Rgb(150, 150, 150);
const STATUS_VAL: Color = Color::Rgb(200, 200, 200);
const SAFE_MODE_FG: Color = Color::Rgb(0xFF, 0xA5, 0x00);
const PICKER_BORDER: Color = Color::Rgb(140, 140, 140);
const SEP: &str = "  ·  ";
/// Maximum number of *content* rows the input box is allowed to grow to
/// before the textarea starts scrolling internally.
pub const MAX_INPUT_CONTENT_ROWS: u16 = 6;
const INPUT_BORDER_ROWS: u16 = 2;
const HINT_ROW_HEIGHT: u16 = 1;
const STATUS_ROW_HEIGHT: u16 = 1;
/// Total rows reserved by the inline viewport: enough to fit the input box
/// at its maximum height plus the hint and status rows. Shared with
/// [`super::INLINE_VIEWPORT_HEIGHT`] so the viewport construction and the
/// layout calculation agree.
pub const INLINE_VIEWPORT_ROWS: u16 =
    MAX_INPUT_CONTENT_ROWS + INPUT_BORDER_ROWS + HINT_ROW_HEIGHT + STATUS_ROW_HEIGHT;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let input_height = input_box_height(app);

    // Anchor input / hint / status at the bottom of the viewport and let
    // the top row (`Min(0)`) absorb whatever height is left over. When the
    // textarea is one line tall the filler is several rows of empty cells;
    // as the user adds newlines with Shift+Enter the input grows upward and
    // the filler shrinks. Removing a line collapses the input back down.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(input_height),
            Constraint::Length(HINT_ROW_HEIGHT),
            Constraint::Length(STATUS_ROW_HEIGHT),
        ])
        .split(area);

    draw_input(frame, rows[1], app);
    draw_hint(frame, rows[2], app);
    draw_status(frame, rows[3], app);

    if let Some(picker) = &app.picker {
        draw_picker(frame, area, picker);
    }
    if let Some(confirmation) = &app.confirmation {
        draw_confirmation(frame, area, confirmation);
    }
}

/// Compute the input box's rendered height (content rows clamped into the
/// supported range, plus 2 for the rounded border).
fn input_box_height(app: &App) -> u16 {
    let content_lines = u16::try_from(app.textarea.lines().len()).unwrap_or(u16::MAX);
    content_lines.clamp(1, MAX_INPUT_CONTENT_ROWS) + INPUT_BORDER_ROWS
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let border_color = if app.busy {
        Color::DarkGray
    } else {
        BORDER_IDLE
    };

    let safe = app.is_safe_mode();
    let prompt_glyph = if safe { " λ: " } else { " > " };
    let title = Line::from(vec![Span::styled(
        prompt_glyph,
        Style::default()
            .fg(if safe { Color::Yellow } else { TITLE_FG })
            .add_modifier(Modifier::BOLD),
    )]);

    let mut textarea = app.textarea.clone();
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
    if app.picker.is_some() {
        spans.push(Span::styled(" ❯ ", Style::default().fg(HINT_KEY)));
        spans.push(Span::styled(
            "pick a session",
            Style::default().fg(Color::Gray),
        ));
        spans.push(Span::styled(SEP, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(
            "↑↓ ⏎  select  ·  esc cancel",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    if app.busy {
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
        spans.push(Span::styled(
            format!("{}… {}s", label, elapsed),
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
        spans.push(Span::styled("/exit ", Style::default().fg(HINT_KEY)));
        spans.push(Span::styled("quit", Style::default().fg(Color::DarkGray)));
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
    let (model, mode, reasoning, in_tok, out_tok) = match &app.status {
        Some(s) => (
            s.model.as_str(),
            s.mode,
            s.reasoning.as_str(),
            s.input_tokens,
            s.output_tokens,
        ),
        None => (app.model_label.as_str(), Mode::Normal, "", 0u32, 0u32),
    };

    let mode_style = match mode {
        Mode::Safe => Style::default()
            .fg(SAFE_MODE_FG)
            .add_modifier(Modifier::BOLD),
        Mode::Normal => Style::default().fg(STATUS_VAL),
    };
    let mut spans: Vec<Span> = vec![
        Span::styled(" model: ", Style::default().fg(STATUS_KEY)),
        Span::styled(
            model,
            Style::default().fg(STATUS_VAL).add_modifier(Modifier::BOLD),
        ),
        Span::styled(SEP, Style::default().fg(Color::DarkGray)),
        Span::styled("mode: ", Style::default().fg(STATUS_KEY)),
        Span::styled(mode.label(), mode_style),
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

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_confirmation(frame: &mut Frame, area: Rect, prompt: &ConfirmationPrompt) {
    // Auto-size: prompt row + blank separator + one row per choice +
    // blank separator + hint row + two border rows.
    //
    // Width is derived from the widest line the modal will draw so the
    // prompt and choice labels don't wrap inside the block (which would
    // silently clip them). We only fall back to a fractional width if
    // the terminal is too narrow for the content — in that case the
    // Paragraph wraps, but at least we've made the box as wide as
    // physically possible.
    const HINT_LINE: &str = "  ↑↓ navigate  ·  ⏎ select  ·  esc cancel";
    const MIN_WIDTH: u16 = 40;
    const HORIZONTAL_PADDING: u16 = 6; // 2 border + 4 inner padding
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
    let height: u16 = 2 + 1 + 1 + choices_rows + 1 + 1;
    let height = height.min(area.height);
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

    let mut lines: Vec<Line> = Vec::with_capacity(prompt.choices.len() + 4);
    lines.push(Line::from(Span::styled(
        format!("  {}", prompt.prompt),
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(Span::raw("")));
    for (i, choice) in prompt.choices.iter().enumerate() {
        let selected = i == prompt.cursor;
        let marker = if selected { "❯ " } else { "  " };
        let style = if selected {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(marker, style),
            Span::styled(choice.clone(), style),
        ]));
    }
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

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup);
}

fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker) {
    let width = area.width.saturating_mul(8) / 10;
    let height = area.height.saturating_mul(6) / 10;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = picker
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let selected = i == picker.cursor;
            let marker = if selected { "❯ " } else { "  " };
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

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(PICKER_BORDER))
        .title(Line::from(vec![Span::styled(
            " Resume session ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )]));

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}
