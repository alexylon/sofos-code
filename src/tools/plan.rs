use crate::error::{Result, SofosError};
use crate::ui::ACCENT_RGB;
use colored::Colorize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

impl PlanStepStatus {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            other => Err(SofosError::ToolExecution(format!(
                "Invalid plan status '{}'. Expected one of: pending, in_progress, completed.",
                other
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub text: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanUpdate {
    pub explanation: Option<String>,
    pub steps: Vec<PlanStep>,
}

impl PlanUpdate {
    fn counts(&self) -> PlanCounts {
        let mut counts = PlanCounts::default();
        for step in &self.steps {
            match step.status {
                PlanStepStatus::Pending => counts.pending += 1,
                PlanStepStatus::InProgress => counts.in_progress += 1,
                PlanStepStatus::Completed => counts.completed += 1,
            }
        }
        counts
    }

    fn current_step(&self) -> Option<&str> {
        self.steps
            .iter()
            .find(|step| step.status == PlanStepStatus::InProgress)
            .map(|step| step.text.as_str())
    }
}

#[derive(Debug, Default)]
struct PlanCounts {
    pending: usize,
    in_progress: usize,
    completed: usize,
}

impl PlanCounts {
    fn total(&self) -> usize {
        self.pending + self.in_progress + self.completed
    }
}

fn normalise_plan_text(text: &str) -> Option<String> {
    let mut normalised = String::new();
    for part in text.split_whitespace() {
        if !normalised.is_empty() {
            normalised.push(' ');
        }
        normalised.push_str(part);
    }

    if normalised.is_empty() {
        None
    } else {
        Some(normalised)
    }
}

pub fn parse_plan_update(input: &Value) -> Result<PlanUpdate> {
    let object = input.as_object().ok_or_else(|| {
        SofosError::ToolExecution("update_plan input must be a JSON object".to_string())
    })?;

    let explanation = match object.get("explanation") {
        Some(Value::Null) | None => None,
        Some(Value::String(text)) => normalise_plan_text(text),
        Some(_) => {
            return Err(SofosError::ToolExecution(
                "'explanation' must be a string when provided".to_string(),
            ));
        }
    };

    let plan = object
        .get("plan")
        .ok_or_else(|| SofosError::ToolExecution("Missing 'plan' parameter".to_string()))?
        .as_array()
        .ok_or_else(|| SofosError::ToolExecution("'plan' must be an array".to_string()))?;

    let mut steps = Vec::with_capacity(plan.len());
    let mut in_progress_count = 0usize;

    for (index, item) in plan.iter().enumerate() {
        let item_object = item.as_object().ok_or_else(|| {
            SofosError::ToolExecution(format!("Plan item {} must be an object", index + 1))
        })?;

        let step_text = item_object
            .get("step")
            .and_then(|value| value.as_str())
            .and_then(normalise_plan_text)
            .ok_or_else(|| {
                SofosError::ToolExecution(format!(
                    "Plan item {} must include a non-empty 'step' string",
                    index + 1
                ))
            })?;

        let status_text = item_object
            .get("status")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                SofosError::ToolExecution(format!(
                    "Plan item {} must include a 'status' string",
                    index + 1
                ))
            })?;
        let status = PlanStepStatus::parse(status_text)?;

        if status == PlanStepStatus::InProgress {
            in_progress_count += 1;
        }

        steps.push(PlanStep {
            text: step_text,
            status,
        });
    }

    if in_progress_count > 1 {
        return Err(SofosError::ToolExecution(
            "At most one plan item can be in_progress at a time".to_string(),
        ));
    }

    Ok(PlanUpdate { explanation, steps })
}

pub fn model_summary(update: &PlanUpdate) -> String {
    let counts = update.counts();
    let mut summary = format!(
        "Plan updated: {} step{}, {} completed, {} in progress, {} pending.",
        counts.total(),
        if counts.total() == 1 { "" } else { "s" },
        counts.completed,
        counts.in_progress,
        counts.pending,
    );

    if let Some(current) = update.current_step() {
        summary.push_str(&format!(" Current step: {}.", current));
    }

    summary
}

pub fn render_plan(update: &PlanUpdate) -> String {
    let (accent_r, accent_g, accent_b) = ACCENT_RGB;
    let counts = update.counts();
    let mut lines = Vec::new();

    lines.push(format!(
        "╭─ {}",
        "Plan updated"
            .truecolor(accent_r, accent_g, accent_b)
            .bold()
    ));

    if let Some(explanation) = &update.explanation {
        lines.push(format!("│ {}", explanation.dimmed()));
        lines.push("├─ Steps".to_string());
    }

    if update.steps.is_empty() {
        lines.push(format!("│ {}", "No active plan items".dimmed()));
    } else {
        for step in &update.steps {
            lines.push(format_plan_step(step));
        }
    }

    lines.push(format!(
        "╰─ {} completed · {} in progress · {} pending",
        counts.completed.to_string().bright_green(),
        counts
            .in_progress
            .to_string()
            .truecolor(accent_r, accent_g, accent_b),
        counts.pending.to_string().dimmed(),
    ));

    lines.join("\n")
}

fn format_plan_step(step: &PlanStep) -> String {
    let (accent_r, accent_g, accent_b) = ACCENT_RGB;
    match step.status {
        PlanStepStatus::Completed => format!(
            "│ {} {}",
            "✓".bright_green().bold(),
            step.text.bright_green()
        ),
        PlanStepStatus::InProgress => format!(
            "│ {} {}",
            "●".truecolor(accent_r, accent_g, accent_b).bold(),
            step.text.bold()
        ),
        PlanStepStatus::Pending => format!("│ {} {}", "○".dimmed(), step.text.dimmed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strip_ansi(input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut chars = input.chars();
        while let Some(char) = chars.next() {
            if char == '\x1b' {
                for inner in chars.by_ref() {
                    if inner.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                output.push(char);
            }
        }
        output
    }

    #[test]
    fn parses_valid_plan_update() {
        let update = parse_plan_update(&json!({
            "explanation": "Making progress",
            "plan": [
                {"step": "Inspect files", "status": "completed"},
                {"step": "Implement tool", "status": "in_progress"},
                {"step": "Run tests", "status": "pending"}
            ]
        }))
        .unwrap();

        assert_eq!(update.explanation.as_deref(), Some("Making progress"));
        assert_eq!(update.steps.len(), 3);
        assert_eq!(update.current_step(), Some("Implement tool"));
    }

    #[test]
    fn normalises_multiline_text_fields() {
        let update = parse_plan_update(&json!({
            "explanation": "  first line\n  second line  ",
            "plan": [
                {"step": "  Inspect\nfiles  ", "status": "in_progress"}
            ]
        }))
        .unwrap();

        assert_eq!(
            update.explanation.as_deref(),
            Some("first line second line")
        );
        assert_eq!(update.steps[0].text, "Inspect files");
    }

    #[test]
    fn rejects_multiple_in_progress_steps() {
        let result = parse_plan_update(&json!({
            "plan": [
                {"step": "One", "status": "in_progress"},
                {"step": "Two", "status": "in_progress"}
            ]
        }));

        assert!(matches!(result, Err(SofosError::ToolExecution(_))));
    }

    #[test]
    fn renders_clean_plan_text_after_removing_ansi() {
        let update = parse_plan_update(&json!({
            "plan": [
                {"step": "Done", "status": "completed"},
                {"step": "Now", "status": "in_progress"},
                {"step": "Later", "status": "pending"}
            ]
        }))
        .unwrap();

        let rendered = strip_ansi(&render_plan(&update));

        assert!(rendered.contains("╭─ Plan updated"));
        assert!(rendered.contains("✓ Done"));
        assert!(rendered.contains("● Now"));
        assert!(rendered.contains("○ Later"));
        assert!(rendered.contains("1 completed · 1 in progress · 1 pending"));
    }

    #[test]
    fn model_summary_mentions_current_step() {
        let update = parse_plan_update(&json!({
            "plan": [
                {"step": "Build", "status": "in_progress"}
            ]
        }))
        .unwrap();

        assert_eq!(
            model_summary(&update),
            "Plan updated: 1 step, 0 completed, 1 in progress, 0 pending. Current step: Build."
        );
    }
}
