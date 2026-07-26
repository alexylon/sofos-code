use crate::ui::UI;
use colored::Colorize;

/// True for models hosted by OpenAI. The two providers disagree on
/// whether cached tokens are counted inside the input total, so the
/// displayed "input tokens" figure needs to know which one produced
/// the counters. Reads the same per-model record as the rest of the
/// application, so a new OpenAI model only has to be added to
/// `SUPPORTED_MODELS` for the display to pick it up.
fn is_openai_model(model: &str) -> bool {
    crate::api::model_info::provider_for(model) == crate::api::model_info::Provider::OpenAI
}

impl UI {
    /// Print the post-turn usage summary. Returns `true` when something
    /// was printed, `false` when the early-return path skipped it — the
    /// TUI teardown uses that return to decide whether to emit its own
    /// escape-newline before [`Self::print_goodbye`] so "Goodbye!"
    /// never collides with the status row.
    pub fn display_session_summary(
        model: &str,
        total_input_tokens: u32,
        total_output_tokens: u32,
        total_cache_read_tokens: u32,
        total_cache_creation_tokens: u32,
        estimated_cost: f64,
    ) -> bool {
        // A fully-cached session has `total_input_tokens == 0` and
        // `total_output_tokens == 0` because the new-input field
        // doesn't include cache reads. Without the cache-read clause
        // a session that only re-hit cache would print no summary at
        // all, which looks like a bug to users running short
        // exploratory prompts.
        if total_input_tokens == 0 && total_output_tokens == 0 && total_cache_read_tokens == 0 {
            return false;
        }

        println!();
        println!("{}", "─".repeat(50).bright_cyan());
        println!("{}", "Session Summary".bright_cyan().bold());
        println!("{}", "─".repeat(50).bright_cyan());

        let total_input_seen =
            Self::total_input_seen_by_model(model, total_input_tokens, total_cache_read_tokens)
                + total_cache_creation_tokens;
        let cache_hit_pct = if total_input_seen > 0 {
            (total_cache_read_tokens as f64 / total_input_seen as f64) * 100.0
        } else {
            0.0
        };

        println!(
            "{:<20} {}",
            "Input tokens:".bright_white(),
            Self::format_number(total_input_seen).bright_green()
        );
        if total_cache_read_tokens > 0 || total_cache_creation_tokens > 0 {
            println!(
                "{:<20} {} {}",
                "  cache read:".bright_white(),
                Self::format_number(total_cache_read_tokens).bright_green(),
                format!("({:.0}% hit)", cache_hit_pct).dimmed()
            );
            if total_cache_creation_tokens > 0 {
                println!(
                    "{:<20} {}",
                    "  cache write:".bright_white(),
                    Self::format_number(total_cache_creation_tokens).bright_green()
                );
            }
        }
        println!(
            "{:<20} {}",
            "Output tokens:".bright_white(),
            Self::format_number(total_output_tokens).bright_green()
        );
        println!(
            "{:<20} {}",
            "Total tokens:".bright_white(),
            Self::format_number(total_input_seen + total_output_tokens).bright_green()
        );
        println!();
        println!(
            "{:<20} {}",
            "Estimated cost:".bright_white().bold(),
            format!("${:.4}", estimated_cost).bright_yellow().bold()
        );

        println!("{}", "─".repeat(50).bright_cyan());
        println!();
        true
    }

    /// Returns the count of input tokens the model actually saw (cached
    /// plus uncached, excluding cache-creation writes which are billed
    /// separately). Hides the per-provider semantic difference of
    /// `total_input_tokens` (OpenAI already includes cached, Anthropic
    /// excludes them).
    fn total_input_seen_by_model(
        model: &str,
        total_input_tokens: u32,
        cache_read_tokens: u32,
    ) -> u32 {
        if is_openai_model(model) {
            total_input_tokens
        } else {
            total_input_tokens + cache_read_tokens
        }
    }

    /// Render the elapsed turn time as a short human-readable string for
    /// the "your turn" prompt-ready signal at the end of a completed
    /// agent loop. Unit picks adapt to magnitude so quick turns stay
    /// concise and long agent runs stay legible.
    pub fn format_turn_finished(elapsed: std::time::Duration) -> String {
        let total_secs = elapsed.as_secs();
        if total_secs < 1 {
            "Finished in <1s".to_string()
        } else if total_secs < 60 {
            format!("Finished in {}s", total_secs)
        } else if total_secs < 3600 {
            format!("Finished in {}m {}s", total_secs / 60, total_secs % 60)
        } else {
            format!(
                "Finished in {}h {}m",
                total_secs / 3600,
                (total_secs % 3600) / 60
            )
        }
    }

    fn format_number(n: u32) -> String {
        let s = n.to_string();
        let mut result = String::new();
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                result.push(',');
            }
            result.push(c);
        }
        result.chars().rev().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_finished_format_picks_unit_by_magnitude() {
        use std::time::Duration;
        assert_eq!(
            UI::format_turn_finished(Duration::from_millis(400)),
            "Finished in <1s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(7)),
            "Finished in 7s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(94)),
            "Finished in 1m 34s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(60)),
            "Finished in 1m 0s"
        );
        assert_eq!(
            UI::format_turn_finished(Duration::from_secs(3725)),
            "Finished in 1h 2m"
        );
    }
}
