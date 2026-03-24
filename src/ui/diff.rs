use colored::Colorize;
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, Style, ThemeSet};
use syntect::parsing::SyntaxSet;

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

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();
    let theme = &theme_set.themes["base16-ocean.dark"];

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
                            highlight_line_with_bg(code, DELETE_BG, &mut hl_delete, &syntax_set);
                        format!(
                            "{}\x1b[48;2;{};{};{}m- \x1b[0m{}",
                            dim_num, DELETE_BG.r, DELETE_BG.g, DELETE_BG.b, highlighted
                        )
                    }
                    ChangeTag::Insert => {
                        let highlighted =
                            highlight_line_with_bg(code, INSERT_BG, &mut hl_insert, &syntax_set);
                        format!(
                            "{}\x1b[48;2;{};{};{}m+ \x1b[0m{}",
                            dim_num, INSERT_BG.r, INSERT_BG.g, INSERT_BG.b, highlighted
                        )
                    }
                    ChangeTag::Equal => {
                        let ranges: Vec<(Style, &str)> = hl_equal
                            .highlight_line(code, &syntax_set)
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
