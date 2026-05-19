use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Clone, Debug)]
pub struct PastedImage {
    pub media_type: String,
    pub base64_data: String,
}

/// Maximum number of pasted images a single submission can carry.
/// Anchored to the size of the Unicode circled-number range
/// (`①` through `⑳`) used to mark each image inline.
pub const MAX_PASTED_IMAGES_PER_MESSAGE: usize = 20;

/// Map a paste index (0-based) to a circled number character that the
/// submission parser can later recover. Returns `None` past
/// `MAX_PASTED_IMAGES_PER_MESSAGE`; the caller is expected to reject
/// the paste with a visible warning rather than emit a marker we
/// could not strip back out.
pub fn marker_for_index(n: usize) -> Option<char> {
    if n < MAX_PASTED_IMAGES_PER_MESSAGE {
        char::from_u32(0x2460 + n as u32)
    } else {
        None
    }
}

fn index_from_char(c: char) -> Option<usize> {
    let val = c as u32;
    if (0x2460..=0x2473).contains(&val) {
        Some((val - 0x2460) as usize)
    } else {
        None
    }
}

/// Strip circled-number markers from input. Returns (cleaned_text, image_indices).
/// Each index maps to the corresponding image in the pasted images Vec.
pub fn strip_paste_markers(input: &str) -> (String, Vec<usize>) {
    let mut indices = Vec::new();
    let mut cleaned = String::new();

    for c in input.chars() {
        if let Some(idx) = index_from_char(c) {
            indices.push(idx);
        } else {
            cleaned.push(c);
        }
    }

    (cleaned.trim().to_string(), indices)
}

/// Matches the 20 MB cap Anthropic imposes on base64-encoded image
/// bodies in the Messages API.
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 20 * 1024 * 1024;

pub fn get_clipboard_image() -> Option<PastedImage> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;

    if image.width == 0 || image.height == 0 {
        return None;
    }

    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&image.bytes).ok()?;
    }

    // Binary cap is 3/4 of the API limit because base64 inflates by
    // ~33% — without it the binary check is dead and the post-encode
    // check is the effective gate.
    let binary_cap = MAX_CLIPBOARD_IMAGE_BYTES * 3 / 4;
    if buf.len() > binary_cap {
        tracing::warn!(
            bytes = buf.len(),
            limit = binary_cap,
            "dropping oversized clipboard image"
        );
        return None;
    }

    let base64_data = STANDARD.encode(&buf);
    if base64_data.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        tracing::warn!(
            bytes = base64_data.len(),
            limit = MAX_CLIPBOARD_IMAGE_BYTES,
            "dropping clipboard image whose base64 form exceeds the API limit"
        );
        return None;
    }

    Some(PastedImage {
        media_type: "image/png".to_string(),
        base64_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_for_first_index_returns_circled_one() {
        assert_eq!(marker_for_index(0), Some('\u{2460}'));
    }

    #[test]
    fn marker_for_last_supported_index_returns_circled_twenty() {
        assert_eq!(
            marker_for_index(MAX_PASTED_IMAGES_PER_MESSAGE - 1),
            Some('\u{2473}')
        );
    }

    #[test]
    fn marker_past_supported_range_returns_none() {
        assert_eq!(marker_for_index(MAX_PASTED_IMAGES_PER_MESSAGE), None);
        assert_eq!(marker_for_index(100), None);
    }

    #[test]
    fn strip_paste_markers_round_trips_indices_and_text() {
        let input = format!(
            "look at {} and {}",
            marker_for_index(0).unwrap(),
            marker_for_index(2).unwrap()
        );
        let (text, indices) = strip_paste_markers(&input);
        assert_eq!(text, "look at  and");
        assert_eq!(indices, vec![0, 2]);
    }
}
