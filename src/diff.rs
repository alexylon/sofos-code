use colored::Colorize;
use similar::{ChangeTag, TextDiff};

/// Generate a contextual diff display showing only changed blocks
/// Returns a formatted string with colored additions (blue +) and deletions (red -)
pub fn generate_contextual_diff(original: &str, modified: &str, context_lines: usize) -> String {
    let diff = TextDiff::from_lines(original, modified);
    let mut output = Vec::new();

    for (idx, group) in diff.grouped_ops(context_lines).iter().enumerate() {
        if idx > 0 {
            // Add separator between hunks
            output.push("".to_string());
            output.push("...".dimmed().to_string());
            output.push("".to_string());
        }

        for op in group {
            for change in diff.iter_changes(op) {
                let s: String = match change.tag() {
                    ChangeTag::Delete => {
                        let line = format!("- {}", change.value().trim_end());
                        line.on_red().black().to_string()
                    }
                    ChangeTag::Insert => {
                        let line = format!("+ {}", change.value().trim_end());
                        line.on_blue().black().to_string()
                    }
                    ChangeTag::Equal => {
                        let line = format!("  {}", change.value().trim_end());
                        line.normal().to_string()
                    }
                };

                output.push(s);
            }
        }
    }

    output.join("\n")
}

/// Generate a compact diff showing only changed lines with minimal context
pub fn generate_compact_diff(original: &str, modified: &str) -> String {
    generate_contextual_diff(original, modified, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_diff() {
        let original = "line 1\nline 2\nline 3\n";
        let modified = "line 1\nline 2 modified\nline 3\n";

        let diff = generate_compact_diff(original, modified);
        assert!(diff.contains("line 2"));
    }

    #[test]
    fn test_multiple_changes() {
        let original = "var x = 1;\nvar y = 2;\nvar z = 3;\n";
        let modified = "const x = 1;\nconst y = 2;\nconst z = 3;\n";

        let diff = generate_compact_diff(original, modified);
        assert!(diff.contains("-"));
        assert!(diff.contains("+"));
    }
}
