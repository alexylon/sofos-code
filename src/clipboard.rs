#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Clone, Debug)]
pub struct PastedImage {
    pub media_type: String,
    pub base64_data: String,
}

/// Map paste index (0-based) to a circled number character: ①②③...⑳
pub fn marker_for_index(n: usize) -> char {
    if n < 20 {
        char::from_u32(0x2460 + n as u32).unwrap_or('*')
    } else {
        '*'
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

/// Ceiling on pasted clipboard images. Matches the 20 MB cap Anthropic
/// imposes on base64-encoded image bodies in the Messages API — a
/// larger screenshot would just get rejected at request time with a
/// confusing 400. Checked on both the raw PNG buffer (encoder output)
/// and the encoded base64 so a huge image never makes it into the
/// conversation state.
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

    if buf.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        tracing::warn!(
            bytes = buf.len(),
            limit = MAX_CLIPBOARD_IMAGE_BYTES,
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
