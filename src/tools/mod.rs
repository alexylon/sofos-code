pub mod bash;
pub mod child_env;
pub mod codesearch;
pub mod executor;
pub mod filesystem;
pub mod image;
pub mod morph_validate;
pub mod permissions;
pub mod plan;
pub mod resolve;
pub mod tool_name;
pub mod types;
pub mod utils;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;

pub use executor::ToolExecutor;
pub use tool_name::ToolName;

/// Header prefix emitted by `read_file` results so the model can tell
/// the wrapper apart from the file body. Kept in one place so the
/// dispatcher that produces it and the display layer that consumes
/// it agree on the exact shape.
pub const READ_FILE_HEADER: &str = "File content of";

/// Build the model-facing payload for a `read_file` call. The header
/// names the path and a blank line separates the wrapper from the
/// file body, which `read_file_body` recovers.
pub fn format_read_file_output(path: &str, content: &str) -> String {
    format!("{} '{}':\n\n{}", READ_FILE_HEADER, path, content)
}

/// Return the body portion of a `read_file` output payload, stripping
/// the header line that `format_read_file_output` prepended. Falls
/// back to the whole string if the expected separator is missing so
/// the caller never gets nothing back.
pub fn read_file_body(output: &str) -> &str {
    output
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or(output)
}

#[cfg(test)]
mod public_helpers_tests {
    use super::*;

    #[test]
    fn format_and_recover_round_trip() {
        let payload = format_read_file_output("src/main.rs", "fn main() {}\n");
        assert!(payload.starts_with(READ_FILE_HEADER));
        assert_eq!(read_file_body(&payload), "fn main() {}\n");
    }

    #[test]
    fn read_file_body_falls_back_when_separator_missing() {
        assert_eq!(read_file_body("no separator here"), "no separator here");
    }

    #[test]
    fn read_file_body_handles_empty_body() {
        let payload = format_read_file_output("empty.txt", "");
        assert_eq!(read_file_body(&payload), "");
    }
}
