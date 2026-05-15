use colored::Colorize;
use similar::{ChangeTag, TextDiff};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, Style, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Shared syntax definitions. Each `SyntaxSet::load_defaults_newlines`
/// call deserialises several megabytes of bundled syntax data. Loading
/// once at first use keeps the per-diff cost down to a lookup instead
/// of a fresh decode every time `generate_contextual_diff` runs.
fn shared_syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Shared dark theme used for diff highlighting. Same rationale as
/// [`shared_syntax_set`] — `ThemeSet::load_defaults` is several
/// megabytes of theme data that doesn't change between calls. Falls
/// back to any other bundled theme, then to a default-constructed one,
/// if the named theme is ever removed upstream — keeps the diff
/// renderer panic-free rather than dying mid-edit on a future syntect
/// reorganisation.
fn shared_diff_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let mut theme_set = ThemeSet::load_defaults();
        theme_set
            .themes
            .remove("base16-ocean.dark")
            .or_else(|| theme_set.themes.into_values().next())
            .unwrap_or_default()
    })
}

const DELETE_BG: Color = Color {
    r: 0x5e,
    g: 0x00,
    b: 0x00,
    a: 0xFF,
};
const INSERT_BG: Color = Color {
    r: 0x00,
    g: 0x00,
    b: 0x5f,
    a: 0xFF,
};

fn highlight_line_with_bg(
    line: &str,
    bg: Color,
    highlighter: &mut HighlightLines,
    syntax_set: &SyntaxSet,
) -> String {
    let ranges: Vec<(Style, &str)> = highlighter
        .highlight_line(line, syntax_set)
        .unwrap_or_default();

    let mut out = String::new();
    for (style, text) in &ranges {
        let fg = style.foreground;
        out.push_str(&format!(
            "\x1b[38;2;{};{};{};48;2;{};{};{}m{}\x1b[0m",
            fg.r, fg.g, fg.b, bg.r, bg.g, bg.b, text
        ));
    }
    out
}

pub fn generate_contextual_diff(
    original: &str,
    modified: &str,
    context_lines: usize,
    file_path: &str,
) -> String {
    let diff = TextDiff::from_lines(original, modified);
    let mut output = Vec::new();

    let syntax_set = shared_syntax_set();
    let theme = shared_diff_theme();

    let ext = file_path.rsplit('.').next().unwrap_or("");
    let syntax = syntax_set
        .find_syntax_by_extension(ext)
        .or_else(|| syntax_set.find_syntax_by_token(ext))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());

    let mut hl_delete = HighlightLines::new(syntax, theme);
    let mut hl_insert = HighlightLines::new(syntax, theme);
    let mut hl_equal = HighlightLines::new(syntax, theme);

    for (idx, group) in diff.grouped_ops(context_lines).iter().enumerate() {
        if idx > 0 {
            output.push("".to_string());
            output.push("...".dimmed().to_string());
            output.push("".to_string());
        }

        for op in group {
            for change in diff.iter_changes(op) {
                let code = change.value().trim_end();
                let line_num = match change.tag() {
                    ChangeTag::Delete => format!("{:<4}", change.old_index().map_or(0, |i| i + 1)),
                    ChangeTag::Insert => {
                        format!("{:<4}", change.new_index().map_or(0, |i| i + 1))
                    }
                    ChangeTag::Equal => {
                        format!("{:<4}", change.old_index().map_or(0, |i| i + 1))
                    }
                };
                let dim_num = format!("\x1b[2m{}\x1b[22m", line_num);

                let s: String = match change.tag() {
                    ChangeTag::Delete => {
                        let highlighted =
                            highlight_line_with_bg(code, DELETE_BG, &mut hl_delete, syntax_set);
                        format!(
                            "{}\x1b[48;2;{};{};{}m- \x1b[0m{}",
                            dim_num, DELETE_BG.r, DELETE_BG.g, DELETE_BG.b, highlighted
                        )
                    }
                    ChangeTag::Insert => {
                        let highlighted =
                            highlight_line_with_bg(code, INSERT_BG, &mut hl_insert, syntax_set);
                        format!(
                            "{}\x1b[48;2;{};{};{}m+ \x1b[0m{}",
                            dim_num, INSERT_BG.r, INSERT_BG.g, INSERT_BG.b, highlighted
                        )
                    }
                    ChangeTag::Equal => {
                        let ranges: Vec<(Style, &str)> = hl_equal
                            .highlight_line(code, syntax_set)
                            .unwrap_or_default();
                        let mut line = format!("{}  ", dim_num);
                        for (style, text) in &ranges {
                            let fg = style.foreground;
                            line.push_str(&format!(
                                "\x1b[38;2;{};{};{}m{}\x1b[0m",
                                fg.r, fg.g, fg.b, text
                            ));
                        }
                        line
                    }
                };

                output.push(s);
            }
        }
    }

    output.join("\n")
}

pub fn generate_compact_diff(original: &str, modified: &str, file_path: &str) -> String {
    generate_contextual_diff(original, modified, 2, file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_diff() {
        let original = "line 1\nline 2\nline 3\n";
        let modified = "line 1\nline 2 modified\nline 3\n";

        let diff = generate_compact_diff(original, modified, "test.txt");
        assert!(diff.contains("line 2"));
    }

    #[test]
    fn test_multiple_changes() {
        let original = "var x = 1;\nvar y = 2;\nvar z = 3;\n";
        let modified = "const x = 1;\nconst y = 2;\nconst z = 3;\n";

        let diff = generate_compact_diff(original, modified, "test.js");
        assert!(diff.contains("-"));
        assert!(diff.contains("+"));
    }
}
