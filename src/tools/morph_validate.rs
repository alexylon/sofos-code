//! Sanity checks for Morph-merged file output before it's committed to
//! disk. Catches the truncated-response failure mode that produced
//! silently-corrupted files; deliberately conservative so genuine large
//! deletions still pass through.

/// Thresholds for [`validate_morph_output`]. The "stub response" check
/// only fires on files large enough that a near-empty merged output
/// is almost certainly Morph returning garbage rather than a real
/// deletion. Catching tail-truncation reliably would require language-
/// aware structural analysis; we rely on `max_tokens` / `finish_reason`
/// (upstream) and trailing-newline parity (below) for that.
const MORPH_STUB_ORIGINAL_MIN: usize = 500;
const MORPH_STUB_FLOOR_BYTES: usize = 50;
/// Mid-range bracket: when the original is between this and
/// `MORPH_STUB_ORIGINAL_MIN`, a merged output that drops below
/// 30 % of the original is almost certainly a truncated response.
/// Below this bracket the absolute floor still applies; above it
/// the legitimate "trim everything except a stub" case dominates.
const MORPH_STUB_MID_ORIGINAL_MIN: usize = 200;
const MORPH_STUB_MID_RATIO: f64 = 0.30;

/// Sanity-check a Morph-merged file against the original before committing
/// it to disk. Returns `Err(reason)` if the merge looks like a truncated
/// response (the exact failure mode that produced silently-corrupted
/// files). Conservative: we only reject patterns that have no legitimate
/// explanation, so a genuine large deletion still goes through.
pub(super) fn validate_morph_output(
    original: &str,
    merged: &str,
) -> std::result::Result<(), String> {
    if merged.trim().is_empty() {
        return Err("Morph returned an empty response".to_string());
    }

    // Reject the degenerate "Morph returned a stub" case on files large
    // enough that a <50-byte response is almost certainly a bad merge.
    // Larger stubs (50+ bytes) are allowed through so a legitimate
    // delete-everything-except-`fn main(){}` edit still goes through.
    if original.len() > MORPH_STUB_ORIGINAL_MIN && merged.len() < MORPH_STUB_FLOOR_BYTES {
        return Err(format!(
            "Morph response shrank from {} to {} bytes — likely truncated",
            original.len(),
            merged.len()
        ));
    }

    // Mid-range bracket: a 200-500-byte original whose merged form
    // collapses below 30 % was previously waved through because the
    // absolute floor (50 bytes) didn't catch it. Genuine "rewrite as a
    // tiny stub" edits in this range are rare; truncated responses
    // are common.
    if original.len() >= MORPH_STUB_MID_ORIGINAL_MIN
        && original.len() <= MORPH_STUB_ORIGINAL_MIN
        && (merged.len() as f64) < (original.len() as f64) * MORPH_STUB_MID_RATIO
    {
        return Err(format!(
            "Morph response shrank from {} to {} bytes (below {}% of original) — \
             likely truncated",
            original.len(),
            merged.len(),
            (MORPH_STUB_MID_RATIO * 100.0) as u32
        ));
    }

    // Trailing-newline parity: if the original ended with a newline and
    // the merged output doesn't, the response was cut mid-line. This is
    // a strong signal even when the byte count is plausible.
    if original.ends_with('\n') && !merged.ends_with('\n') {
        return Err(
            "Morph response is missing the trailing newline — likely truncated mid-line"
                .to_string(),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty() {
        assert!(validate_morph_output("fn main() { println!(\"hi\"); }", "").is_err());
        assert!(validate_morph_output("fn main() { println!(\"hi\"); }", "   \n  ").is_err());
    }

    #[test]
    fn rejects_dramatic_shrink() {
        // Simulate Morph returning a severely truncated response for a
        // non-trivial file — the exact corruption pattern we've seen in
        // practice. Original is >500 bytes, merged is a stub under 200.
        let original =
            "fn main() {\n".to_string() + &"    println!(\"line\");\n".repeat(40) + "}\n";
        let merged = "fn main() {\n";
        assert!(validate_morph_output(&original, merged).is_err());
    }

    #[test]
    fn accepts_reasonable_edits() {
        let original =
            "fn main() {\n".to_string() + &"    println!(\"line\");\n".repeat(40) + "}\n";
        // A realistic edit — replaces a block but keeps roughly the same size.
        let merged = "fn main() {\n".to_string() + &"    println!(\"other\");\n".repeat(40) + "}\n";
        assert!(validate_morph_output(&original, &merged).is_ok());
    }

    #[test]
    fn allows_legitimate_small_stub() {
        // User asks Morph to delete everything except a minimal `main()`.
        // Original is a large file; merged is a ~50-byte stub. It's small
        // but at or above the floor, so it must still be accepted — the
        // sanity check exists to catch garbage, not legitimate deletions.
        let original =
            "fn main() {\n".to_string() + &"    println!(\"line\");\n".repeat(40) + "}\n";
        let merged = "fn main() {\n    // trimmed down by user\n    Ok(())\n}\n";
        assert!(
            merged.len() >= 50,
            "test stub must be at or above the floor"
        );
        assert!(validate_morph_output(&original, merged).is_ok());
    }

    #[test]
    fn rejects_missing_trailing_newline() {
        // If the original ends with `\n` but the merged output doesn't,
        // the response was almost certainly cut off mid-line.
        let original = "line 1\nline 2\nline 3\n";
        let merged = "line 1\nline 2\nline";
        assert!(validate_morph_output(original, merged).is_err());
    }

    #[test]
    fn allows_no_newline_when_original_had_none() {
        // Files without a final newline (the original was that way, not
        // because of truncation) should still be accepted.
        let original = "no_trailing_newline";
        let merged = "modified_no_trailing_newline";
        assert!(validate_morph_output(original, merged).is_ok());
    }
}
