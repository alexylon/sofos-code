use base64::{engine::general_purpose::STANDARD, Engine};

#[derive(Clone)]
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

    Some(PastedImage {
        media_type: "image/png".to_string(),
        base64_data: STANDARD.encode(&buf),
    })
}
