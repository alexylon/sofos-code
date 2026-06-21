use crate::config::config_files_hint;
use crate::error::{Result, ResultExt, SofosError};
use crate::tools::permissions::{CommandPermission, PermissionManager};
use crate::tools::utils::{is_absolute_or_tilde, is_http_url};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{
    DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat as ImageCrateFormat,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const MAX_IMAGE_SIZE_MB: u64 = 20;
pub const MAX_IMAGE_SIZE_BYTES: u64 = MAX_IMAGE_SIZE_MB * 1024 * 1024;

/// Long-side pixel bound used when resizing an image before sending it
/// to the model. Larger images get scaled proportionally to fit.
pub const MAX_PROMPT_IMAGE_DIMENSION: u32 = 2048;

/// JPEG quality used when re-encoding a resized image.
const JPEG_QUALITY: u8 = 85;

/// Human-readable list of source formats called out in the tool schema.
pub const SUPPORTED_FORMATS_HUMAN_LIST: &str = "JPEG, PNG, GIF, and WebP";

#[derive(Debug, Clone)]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// Bytes of an image ready to send to the model, with its MIME type.
#[derive(Debug, Clone)]
pub struct EncodedImage {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// Decode, optionally resize within `MAX_PROMPT_IMAGE_DIMENSION`, and
/// return bytes ready for the model. Small images in a supported format
/// pass through unchanged.
pub fn encode_image_for_prompt(bytes: Vec<u8>) -> Result<EncodedImage> {
    let detected = image::guess_format(&bytes).ok();

    let decoded = decode_with_orientation(&bytes)?;
    let (width, height) = decoded.dimensions();

    let within_bound = width <= MAX_PROMPT_IMAGE_DIMENSION && height <= MAX_PROMPT_IMAGE_DIMENSION;
    let passthrough_format = detected.filter(|f| is_passthrough_format(*f));

    if within_bound {
        if let Some(format) = passthrough_format {
            return Ok(EncodedImage {
                bytes,
                mime: mime_for_image_format(format).to_string(),
            });
        }
        let (encoded_bytes, format) = encode_image_to_bytes(&decoded, ImageCrateFormat::Png)?;
        return Ok(EncodedImage {
            bytes: encoded_bytes,
            mime: mime_for_image_format(format).to_string(),
        });
    }

    let resized = decoded.resize(
        MAX_PROMPT_IMAGE_DIMENSION,
        MAX_PROMPT_IMAGE_DIMENSION,
        image::imageops::FilterType::Triangle,
    );
    let target = passthrough_format.unwrap_or(ImageCrateFormat::Png);
    let (encoded_bytes, format) = encode_image_to_bytes(&resized, target)?;
    Ok(EncodedImage {
        bytes: encoded_bytes,
        mime: mime_for_image_format(format).to_string(),
    })
}

/// Decode image bytes, applying any EXIF orientation. A resize +
/// re-encode strips EXIF, so the orientation is baked into the pixels.
fn decode_with_orientation(bytes: &[u8]) -> Result<DynamicImage> {
    fn decode_err<E: std::fmt::Display>(e: E) -> SofosError {
        SofosError::ToolExecution(format!(
            "Failed to decode image: {e}. Supported: {SUPPORTED_FORMATS_HUMAN_LIST}."
        ))
    }

    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(decode_err)?
        .into_decoder()
        .map_err(decode_err)?;
    let orientation = decoder.orientation().map_err(decode_err)?;
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(decode_err)?;
    decoded.apply_orientation(orientation);
    Ok(decoded)
}

fn is_passthrough_format(format: ImageCrateFormat) -> bool {
    matches!(
        format,
        ImageCrateFormat::Png | ImageCrateFormat::Jpeg | ImageCrateFormat::WebP
    )
}

fn mime_for_image_format(format: ImageCrateFormat) -> &'static str {
    match format {
        ImageCrateFormat::Jpeg => "image/jpeg",
        ImageCrateFormat::Gif => "image/gif",
        ImageCrateFormat::WebP => "image/webp",
        _ => "image/png",
    }
}

/// Re-encode `image`. Non-(PNG/JPEG/WebP) targets fall back to PNG.
fn encode_image_to_bytes(
    image: &DynamicImage,
    preferred: ImageCrateFormat,
) -> Result<(Vec<u8>, ImageCrateFormat)> {
    let target = match preferred {
        ImageCrateFormat::Jpeg => ImageCrateFormat::Jpeg,
        ImageCrateFormat::WebP => ImageCrateFormat::WebP,
        _ => ImageCrateFormat::Png,
    };

    let mut buffer = Vec::new();
    match target {
        ImageCrateFormat::Png => write_rgba_image(
            image,
            image::codecs::png::PngEncoder::new(&mut buffer),
            "PNG",
        )?,
        ImageCrateFormat::Jpeg => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, JPEG_QUALITY)
                .encode_image(image)
                .map_err(|e| SofosError::ToolExecution(format!("JPEG encode failed: {e}")))?
        }
        ImageCrateFormat::WebP => write_rgba_image(
            image,
            image::codecs::webp::WebPEncoder::new_lossless(&mut buffer),
            "WebP",
        )?,
        _ => unreachable!("target is always one of PNG/JPEG/WebP"),
    }
    Ok((buffer, target))
}

/// Shared rgba-encode path for the PNG and WebP encoders.
fn write_rgba_image<E: ImageEncoder>(
    image: &DynamicImage,
    encoder: E,
    format_name: &str,
) -> Result<()> {
    let rgba = image.to_rgba8();
    encoder
        .write_image(
            rgba.as_raw(),
            image.width(),
            image.height(),
            image::ColorType::Rgba8.into(),
        )
        .map_err(|e| SofosError::ToolExecution(format!("{format_name} encode failed: {e}")))
}

pub struct ImageLoader {
    workspace: PathBuf,
    permission_manager: PermissionManager,
    interactive: bool,
    read_path_session_allowed: Arc<Mutex<HashSet<String>>>,
    read_path_session_denied: Arc<Mutex<HashSet<String>>>,
}

impl ImageLoader {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        // Canonicalise so workspace and file paths compare in the same shape.
        let canonical_workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
        let permission_manager = PermissionManager::new(canonical_workspace.clone())?;

        Ok(Self {
            workspace: canonical_workspace,
            permission_manager,
            interactive: false,
            read_path_session_allowed: Arc::new(Mutex::new(HashSet::new())),
            read_path_session_denied: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// Share the executor's read-permission session caches so a single
    /// "Allow Read access?" answer covers both `read_file` and image loads.
    pub fn install_read_path_session(
        &mut self,
        interactive: bool,
        allowed: Arc<Mutex<HashSet<String>>>,
        denied: Arc<Mutex<HashSet<String>>>,
    ) {
        self.interactive = interactive;
        self.read_path_session_allowed = allowed;
        self.read_path_session_denied = denied;
    }

    pub fn load_local_image(&self, path: &str) -> Result<ImageSource> {
        let full_path = if is_absolute_or_tilde(path) {
            PathBuf::from(PermissionManager::expand_tilde_pub(path))
        } else {
            self.workspace.join(path)
        };

        let canonical = std::fs::canonicalize(&full_path)
            .with_context(|| format!("Image not found: '{}'. Make sure the file exists.", path))?;

        let is_inside_workspace = canonical.starts_with(&self.workspace);
        let canonical_str = canonical.to_str().unwrap_or(path);

        let (perm_original, matched_rule_original) = self
            .permission_manager
            .check_read_permission_with_source(path);
        let (perm_canonical, matched_rule_canonical) = self
            .permission_manager
            .check_read_permission_with_source(canonical_str);

        let (final_perm, matched_rule) = if perm_original == CommandPermission::Denied {
            (perm_original, matched_rule_original)
        } else if perm_canonical == CommandPermission::Denied {
            (perm_canonical, matched_rule_canonical)
        } else {
            (CommandPermission::Allowed, None)
        };

        match final_perm {
            CommandPermission::Denied => {
                let config_source = if let Some(ref rule) = matched_rule {
                    self.permission_manager.get_rule_source(rule)
                } else {
                    config_files_hint()
                };
                return Err(SofosError::ToolExecution(format!(
                    "Read access denied for image '{}'\n\
                     Hint: Blocked by deny rule in {}",
                    path, config_source
                )));
            }
            CommandPermission::Ask => {
                return Err(SofosError::ToolExecution(format!(
                    "Image path '{}' is in 'ask' list\n\
                     Hint: 'ask' only works for Bash commands. Use 'allow' or 'deny' for image access.",
                    path
                )));
            }
            CommandPermission::Allowed => {}
        }

        // Use ONLY canonical (symlink-resolved) path for permission checks
        let is_explicit_allow = self
            .permission_manager
            .is_read_explicit_allow(canonical_str);

        if !is_inside_workspace && !is_explicit_allow {
            self.ask_external_read_access(&canonical, canonical_str)?;
        }

        let metadata = std::fs::metadata(&canonical)
            .with_context(|| format!("Failed to read image metadata: {}", path))?;

        if metadata.len() > MAX_IMAGE_SIZE_BYTES {
            return Err(SofosError::ToolExecution(format!(
                "Image too large: {} (max: {} MB)",
                path, MAX_IMAGE_SIZE_MB
            )));
        }

        let raw_bytes = std::fs::read(&canonical)
            .with_context(|| format!("Failed to read image file: {}", path))?;

        let encoded = encode_image_for_prompt(raw_bytes)?;
        let base64_data = STANDARD.encode(&encoded.bytes);

        Ok(ImageSource::Base64 {
            media_type: encoded.mime,
            data: base64_data,
        })
    }

    /// Claude API fetches URLs directly, so we just validate and pass through
    pub fn prepare_web_image(&self, url: &str) -> Result<ImageSource> {
        if !is_http_url(url) {
            return Err(SofosError::ToolExecution(format!(
                "Invalid image URL: {}. Must start with http:// or https://",
                url
            )));
        }

        Ok(ImageSource::Url {
            url: url.to_string(),
        })
    }

    fn ask_external_read_access(&self, canonical: &Path, canonical_str: &str) -> Result<()> {
        let grant_dir = crate::tools::permissions::grant_dir_for_path(canonical);
        crate::tools::permissions::check_external_path_session_access(
            &self.workspace,
            "Read",
            canonical_str,
            grant_dir,
            self.interactive,
            &self.read_path_session_allowed,
            &self.read_path_session_denied,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageBuffer;
    use image::Rgba;
    use std::io::Cursor;

    fn png_bytes(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        encode_fixture(width, height, rgba, ImageCrateFormat::Png)
    }

    fn encode_fixture(width: u32, height: u32, rgba: [u8; 4], format: ImageCrateFormat) -> Vec<u8> {
        let image = ImageBuffer::from_pixel(width, height, Rgba(rgba));
        let mut cursor = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut cursor, format)
            .expect("encode fixture");
        cursor.into_inner()
    }

    #[test]
    fn passthroughs_small_png() {
        let bytes = png_bytes(64, 32, [10, 20, 30, 255]);
        let encoded = encode_image_for_prompt(bytes.clone()).expect("encode");
        assert_eq!(encoded.mime, "image/png");
        assert_eq!(
            encoded.bytes, bytes,
            "small image should pass through unchanged"
        );
    }

    #[test]
    fn resizes_wide_image_to_bound() {
        let bytes = png_bytes(4096, 2048, [200, 10, 10, 255]);
        let encoded = encode_image_for_prompt(bytes).expect("encode");
        let decoded = image::load_from_memory(&encoded.bytes).expect("decode resized");
        let (w, h) = decoded.dimensions();
        assert!(w <= MAX_PROMPT_IMAGE_DIMENSION && h <= MAX_PROMPT_IMAGE_DIMENSION);
        assert_eq!((w, h), (MAX_PROMPT_IMAGE_DIMENSION, 1024));
    }

    #[test]
    fn resizes_tall_image_proportionally() {
        let bytes = png_bytes(1024, 4096, [50, 60, 70, 255]);
        let encoded = encode_image_for_prompt(bytes).expect("encode");
        let decoded = image::load_from_memory(&encoded.bytes).expect("decode resized");
        assert_eq!(decoded.dimensions(), (512, MAX_PROMPT_IMAGE_DIMENSION));
    }

    #[test]
    fn reencodes_small_gif_as_png() {
        let bytes = encode_fixture(32, 32, [100, 150, 200, 255], ImageCrateFormat::Gif);
        let encoded = encode_image_for_prompt(bytes).expect("encode");
        assert_eq!(
            encoded.mime, "image/png",
            "GIF input should be re-encoded as PNG to avoid the animated-GIF case"
        );
        let decoded = image::load_from_memory(&encoded.bytes).expect("decode output");
        assert_eq!(decoded.dimensions(), (32, 32));
    }

    #[test]
    fn keeps_jpeg_format_after_resize() {
        let bytes = encode_fixture(4096, 2048, [200, 50, 50, 255], ImageCrateFormat::Jpeg);
        let encoded = encode_image_for_prompt(bytes).expect("encode");
        assert_eq!(
            encoded.mime, "image/jpeg",
            "JPEG source should stay JPEG after resize"
        );
        let decoded = image::load_from_memory(&encoded.bytes).expect("decode resized");
        let (w, h) = decoded.dimensions();
        assert!(w <= MAX_PROMPT_IMAGE_DIMENSION && h <= MAX_PROMPT_IMAGE_DIMENSION);
        assert_eq!((w, h), (MAX_PROMPT_IMAGE_DIMENSION, 1024));
    }

    #[test]
    fn rejects_non_image_bytes() {
        let err = encode_image_for_prompt(b"not an image".to_vec())
            .expect_err("non-image bytes must error");
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("decode") || msg.to_lowercase().contains("supported"),
            "error should explain the failure; got: {msg}"
        );
    }

    /// Build a JPEG carrying an EXIF orientation tag, so the decode
    /// path under test has a real orientation to read.
    fn jpeg_with_exif_orientation(
        width: u32,
        height: u32,
        rgba: [u8; 4],
        orientation: u16,
    ) -> Vec<u8> {
        let base = encode_fixture(width, height, rgba, ImageCrateFormat::Jpeg);
        assert!(
            base.starts_with(&[0xFF, 0xD8]),
            "JPEG must open with the start-of-image marker"
        );

        // EXIF holds the orientation in a little-endian TIFF block: a
        // byte-order marker, then a directory with one entry.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]); // "II" marker + magic 42
        tiff.extend_from_slice(&8u32.to_le_bytes()); // byte offset of the directory
        tiff.extend_from_slice(&1u16.to_le_bytes()); // entry count
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // orientation tag id
        tiff.extend_from_slice(&3u16.to_le_bytes()); // field type: 16-bit unsigned
        tiff.extend_from_slice(&1u32.to_le_bytes()); // value count
        tiff.extend_from_slice(&u32::from(orientation).to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // no following directory

        let mut body = b"Exif\0\0".to_vec();
        body.extend_from_slice(&tiff);

        let mut app1 = vec![0xFF, 0xE1]; // EXIF metadata marker
        // The segment length counts its own two length bytes.
        let segment_len = (2 + body.len()) as u16;
        app1.extend_from_slice(&segment_len.to_be_bytes());
        app1.extend_from_slice(&body);

        let mut out = Vec::with_capacity(base.len() + app1.len());
        out.extend_from_slice(&base[..2]);
        out.extend_from_slice(&app1);
        out.extend_from_slice(&base[2..]);
        out
    }

    #[test]
    fn applies_exif_orientation_when_decoding() {
        // A wide JPEG tagged "rotate 90°" must come back with its
        // dimensions swapped — the orientation is applied before the
        // resize + re-encode that would otherwise strip the EXIF tag.
        let bytes = jpeg_with_exif_orientation(8, 4, [180, 60, 60, 255], 6);
        let decoded = decode_with_orientation(&bytes).expect("decode oriented JPEG");
        assert_eq!(decoded.dimensions(), (4, 8));
    }

    #[test]
    fn identity_exif_orientation_keeps_dimensions() {
        let bytes = jpeg_with_exif_orientation(8, 4, [180, 60, 60, 255], 1);
        let decoded = decode_with_orientation(&bytes).expect("decode JPEG");
        assert_eq!(decoded.dimensions(), (8, 4));
    }
}
