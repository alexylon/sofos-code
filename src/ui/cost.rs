use crate::ui::UI;
use colored::Colorize;

/// Fraction of the base input price charged for tokens served from
/// the provider prompt cache. Anthropic and OpenAI both publish this
/// at 10% for the current supported families, so the rate lives here
/// as a single constant instead of being repeated on every model
/// record.
const CACHE_READ_RATE: f64 = 0.10;
/// Multiplier applied to the base input price for tokens written to a
/// 5-minute Anthropic cache breakpoint. OpenAI has no separate
/// creation charge (the wire format never reports cache-creation
/// tokens for OpenAI requests), so the multiplier only fires on
/// Anthropic responses.
const CACHE_CREATION_RATE: f64 = 1.25;

/// True for models hosted by OpenAI. Used by the cost and
/// token-display paths to route into the OpenAI pricing /
/// uncached-tokens branches. The decision flows from the same
/// per-model record as the rest of the application, so a new
/// OpenAI model only has to be added to `SUPPORTED_MODELS` for
/// costing to pick it up.
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
        peak_single_turn_input_tokens: u32,
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

        let estimated_cost = Self::calculate_cost(
            model,
            total_input_tokens,
            total_output_tokens,
            total_cache_read_tokens,
            total_cache_creation_tokens,
            peak_single_turn_input_tokens,
        );

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

        // Surface the per-prompt cliff when premium pricing kicked in
        // — users otherwise have no way to tell that crossing the
        // GPT-5.5 / GPT-5.4 input-token threshold doubled the rate
        // for every later turn in this session.
        let info = crate::api::model_info::lookup(model);
        if let Some(tier) = info.premium_tier {
            if peak_single_turn_input_tokens > tier.input_threshold {
                println!(
                    "{:<20} {}",
                    "".bright_white(),
                    format!(
                        "(premium tier: peak input {} exceeded {} threshold)",
                        Self::format_number(peak_single_turn_input_tokens),
                        Self::format_number(tier.input_threshold)
                    )
                    .dimmed()
                );
            }
        }

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

    fn calculate_cost(
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
        peak_single_turn_input_tokens: u32,
    ) -> f64 {
        let info = crate::api::model_info::lookup(model);
        // Tiered pricing: gpt-5.4/5.5 flip the entire session to a
        // premium rate once any single prompt's input crosses the
        // documented threshold. Compare the per-call high-water mark
        // (not the cumulative session total) against the threshold,
        // because the cliff is per-prompt, not per-session-cumulative.
        let (input_price, output_price) = match info.premium_tier {
            Some(tier) if peak_single_turn_input_tokens > tier.input_threshold => {
                (tier.price_input_per_m, tier.price_output_per_m)
            }
            _ => (info.price_input_per_m, info.price_output_per_m),
        };

        // OpenAI's `input_tokens` is the total (cached + uncached);
        // Anthropic's is uncached new tokens only. Normalize to "tokens
        // billed at the full input rate" before pricing.
        let uncached = if is_openai_model(model) {
            input_tokens.saturating_sub(cache_read_tokens)
        } else {
            input_tokens
        };

        let uncached_cost = (uncached as f64 / 1_000_000.0) * input_price;
        let cached_cost = (cache_read_tokens as f64 / 1_000_000.0) * input_price * CACHE_READ_RATE;
        let creation_cost =
            (cache_creation_tokens as f64 / 1_000_000.0) * input_price * CACHE_CREATION_RATE;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * output_price;

        uncached_cost + cached_cost + creation_cost + output_cost
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

    fn approx(a: f64, b: f64) {
        assert!(
            (a - b).abs() < 1e-9,
            "expected ≈{}, got {} (delta {})",
            b,
            a,
            (a - b).abs()
        );
    }

    #[test]
    fn openai_cost_uses_full_rate_when_no_cache() {
        // 100k input @ $5/M, 5k output @ $30/M, no cache. Peak below
        // the 272K cliff so standard pricing applies.
        let cost = UI::calculate_cost("gpt-5.5", 100_000, 5_000, 0, 0, 100_000);
        approx(cost, 100_000.0 / 1e6 * 5.0 + 5_000.0 / 1e6 * 30.0);
    }

    #[test]
    fn openai_cost_discounts_cache_reads_at_10pct() {
        let cost = UI::calculate_cost("gpt-5.5", 100_000, 5_000, 75_000, 0, 100_000);
        approx(cost, 0.1625 + 0.15);
    }

    #[test]
    fn openai_cost_3x_lower_than_pre_fix_at_75pct_hit_input_only() {
        let pre_fix_input = 100_000.0 / 1e6 * 5.0;
        let post_fix_input = UI::calculate_cost("gpt-5.5", 100_000, 0, 75_000, 0, 100_000);
        let ratio = pre_fix_input / post_fix_input;
        assert!(
            (2.9..=3.2).contains(&ratio),
            "expected pre/post ratio ≈3x at 75% hit, got {:.2}x",
            ratio
        );
    }

    #[test]
    fn anthropic_cost_input_tokens_already_excludes_cache() {
        let cost = UI::calculate_cost("claude-opus-4-7", 25_000, 5_000, 75_000, 0, 100_000);
        approx(cost, 0.1625 + 0.125);
    }

    #[test]
    fn anthropic_cost_charges_creation_at_125pct() {
        let cost = UI::calculate_cost("claude-opus-4-7", 0, 0, 0, 50_000, 0);
        approx(cost, 50_000.0 / 1e6 * 5.0 * 1.25);
    }

    #[test]
    fn cache_hit_does_not_underflow_when_read_exceeds_input() {
        let cost = UI::calculate_cost("gpt-5.5", 50_000, 0, 100_000, 0, 100_000);
        approx(cost, 100_000.0 / 1e6 * 5.0 * 0.10);
    }

    #[test]
    fn cliff_crossing_doubles_input_rate_for_gpt_5_5() {
        // Below cliff: standard rate ($5/M input). 100K input × $5/M = $0.50.
        let standard = UI::calculate_cost("gpt-5.5", 100_000, 0, 0, 0, 200_000);
        approx(standard, 100_000.0 / 1e6 * 5.0);

        // Above cliff (peak observed > 272K): premium rate ($10/M input
        // for gpt-5.5). 100K × $10/M = $1.00. Same input/cache numbers,
        // double the bill — that's the user-visible effect of the cliff.
        let premium = UI::calculate_cost("gpt-5.5", 100_000, 0, 0, 0, 300_000);
        approx(premium, 100_000.0 / 1e6 * 10.0);
        assert!((premium / standard - 2.0).abs() < 0.01);
    }

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

    #[test]
    fn unknown_model_falls_back_without_panic() {
        // Default fallback uses the application-default model
        // (`claude-sonnet-4-6`) pricing ($3 / $15) and the Anthropic
        // semantics branch (input_tokens is uncached).
        let cost = UI::calculate_cost("some-future-model", 1_000, 1_000, 0, 0, 1_000);
        approx(cost, 1_000.0 / 1e6 * 3.0 + 1_000.0 / 1e6 * 15.0);
    }
}
