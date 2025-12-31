use crate::ui::{set_normal_mode_cursor_style, set_safe_mode_cursor_style};
use colored::Colorize;
use reedline::{Prompt, PromptEditMode, PromptHistorySearch};

#[derive(Clone)]
pub struct ReplPrompt {
    safe_mode: bool,
}

impl ReplPrompt {
    pub fn new(safe_mode: bool) -> Self {
        Self { safe_mode }
    }

    pub fn set_safe_mode(&mut self, safe_mode: bool) {
        if safe_mode {
            set_safe_mode_cursor_style().ok();
        } else {
            set_normal_mode_cursor_style().ok();
        }

        self.safe_mode = safe_mode;
    }
}

impl Prompt for ReplPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        let symbol = if self.safe_mode { "λ:" } else { "λ>" };
        std::borrow::Cow::Owned(symbol.bright_green().bold().to_string() + " ")
    }

    fn render_prompt_right(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("… ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed("")
    }
}
