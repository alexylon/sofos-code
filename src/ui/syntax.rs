use colored::Colorize;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

#[allow(dead_code)]
pub struct SyntaxHighlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    #[allow(dead_code)]
    pub fn highlight_text(&self, text: &str) -> String {
        let mut result = String::new();
        let mut in_code_block = false;
        let mut code_block = String::new();
        let mut language = String::new();

        for line in text.lines() {
            if line.starts_with("```") {
                if in_code_block {
                    result.push_str(&self.highlight_code(&code_block, &language));
                    result.push('\n');
                    code_block.clear();
                    language.clear();
                    in_code_block = false;
                } else {
                    language = line.trim_start_matches('`').trim().to_string();
                    in_code_block = true;
                }
            } else if in_code_block {
                code_block.push_str(line);
                code_block.push('\n');
            } else {
                result.push_str(line);
                result.push('\n');
            }
        }

        if in_code_block {
            result.push_str(&self.highlight_code(&code_block, &language));
        }

        result.trim_end().to_string()
    }

    fn highlight_code(&self, code: &str, language: &str) -> String {
        let syntax = if language.is_empty() {
            self.syntax_set.find_syntax_plain_text()
        } else {
            self.syntax_set
                .find_syntax_by_token(language)
                .or_else(|| self.syntax_set.find_syntax_by_extension(language))
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
        };

        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut highlighter = HighlightLines::new(syntax, theme);

        let mut result = String::new();
        result.push_str(&format!("{}\n", "┌─────".dimmed()));

        for line in code.lines() {
            let ranges: Vec<(Style, &str)> = highlighter
                .highlight_line(line, &self.syntax_set)
                .unwrap_or_default();
            let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
            result.push_str(&format!("{}  {}\n", "│".dimmed(), escaped));
        }

        result.push_str(&format!("{}", "└─────".dimmed()));
        result
    }
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlighter_creation() {
        let highlighter = SyntaxHighlighter::new();
        assert!(!highlighter.syntax_set.syntaxes().is_empty());
    }

    #[test]
    fn test_plain_text() {
        let highlighter = SyntaxHighlighter::new();
        let text = "Hello, world!";
        let result = highlighter.highlight_text(text);
        assert!(result.contains("Hello, world!"));
    }

    #[test]
    fn test_code_block_detection() {
        let highlighter = SyntaxHighlighter::new();
        let text = "Here is some code:\n```rust\nfn main() {}\n```";
        let result = highlighter.highlight_text(text);
        assert!(result.contains("Here is some code:"));
        assert!(result.contains("main"));
    }
}
