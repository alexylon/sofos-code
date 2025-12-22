use crate::error::{Result, SofosError};
use crate::error_ext::ResultExt;
use crate::tools::permissions::{CommandPermission, PermissionManager};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::path::PathBuf;

const MAX_IMAGE_SIZE: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Webp,
}

impl ImageFormat {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
            "png" => Some(ImageFormat::Png),
            "gif" => Some(ImageFormat::Gif),
            "webp" => Some(ImageFormat::Webp),
            _ => None,
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Png => "image/png",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

pub fn detect_image_reference(text: &str) -> Option<ImageReference> {
    let trimmed = text.trim();

    // Strip common trailing punctuation that might be attached to paths in sentences
    let cleaned = trimmed.trim_end_matches(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'));

    if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
        if is_image_url(cleaned) {
            return Some(ImageReference::WebUrl(cleaned.to_string()));
        }
    }

    if has_image_extension(cleaned) {
        return Some(ImageReference::LocalPath(cleaned.to_string()));
    }

    None
}

#[derive(Debug, Clone)]
pub enum ImageReference {
    WebUrl(String),
    LocalPath(String),
}

fn is_image_url(url: &str) -> bool {
    let lower = url.to_lowercase();

    if let Some(path_part) = url.split('?').next() {
        if has_image_extension(path_part) {
            return true;
        }
    }

    // Known image hosting domains (URLs from these are likely images even without extension)
    let image_hosts = [
        "imgur.com",
        "i.imgur.com",
        "images.unsplash.com",
        "upload.wikimedia.org",
        "raw.githubusercontent.com",
        "pbs.twimg.com",
        "cdn.discordapp.com",
    ];

    for host in &image_hosts {
        if lower.contains(host) {
            return true;
        }
    }

    false
}

fn has_image_extension(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
}

pub struct ImageLoader {
    workspace: PathBuf,
    permission_manager: PermissionManager,
}

impl ImageLoader {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let permission_manager = PermissionManager::new(workspace.clone())?;

        Ok(Self {
            workspace,
            permission_manager,
        })
    }

    pub fn load_local_image(&self, path: &str) -> Result<ImageSource> {
        let full_path = if path.starts_with('/') || path.starts_with('~') {
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
                    ".sofos/config.local.toml or ~/.sofos/config.toml".to_string()
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

        let is_explicit_allow = self
            .permission_manager
            .is_read_explicit_allow_both_forms(path, canonical_str);

        if !is_inside_workspace && !is_explicit_allow {
            return Err(SofosError::ToolExecution(format!(
                "Image '{}' is outside workspace and not explicitly allowed\n\
                 Hint: Add Read({}) to 'allow' list in .sofos/config.local.toml",
                path, path
            )));
        }

        let metadata = std::fs::metadata(&canonical)
            .with_context(|| format!("Failed to read image metadata: {}", path))?;

        if metadata.len() > MAX_IMAGE_SIZE {
            return Err(SofosError::ToolExecution(format!(
                "Image too large: {} (max: {} MB)",
                path,
                MAX_IMAGE_SIZE / (1024 * 1024)
            )));
        }

        let extension = canonical.extension().and_then(|e| e.to_str()).unwrap_or("");

        let format = ImageFormat::from_extension(extension).ok_or_else(|| {
            SofosError::ToolExecution(format!(
                "Unsupported image format: {}. Supported formats: JPEG, PNG, GIF, WebP",
                extension
            ))
        })?;

        let image_data = std::fs::read(&canonical)
            .with_context(|| format!("Failed to read image file: {}", path))?;

        let base64_data = STANDARD.encode(&image_data);

        Ok(ImageSource::Base64 {
            media_type: format.mime_type().to_string(),
            data: base64_data,
        })
    }

    /// Claude API fetches URLs directly, so we just validate and pass through
    pub fn prepare_web_image(&self, url: &str) -> Result<ImageSource> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(SofosError::ToolExecution(format!(
                "Invalid image URL: {}. Must start with http:// or https://",
                url
            )));
        }

        Ok(ImageSource::Url {
            url: url.to_string(),
        })
    }

    pub fn load_image(&self, reference: &ImageReference) -> Result<ImageSource> {
        match reference {
            ImageReference::LocalPath(path) => self.load_local_image(path),
            ImageReference::WebUrl(url) => self.prepare_web_image(url),
        }
    }
}

/// Returns (remaining_text, image_references) after extracting image paths/URLs from input
pub fn extract_image_references(input: &str) -> (String, Vec<ImageReference>) {
    let mut remaining_text = String::new();
    let mut references = Vec::new();

    for word in input.split_whitespace() {
        if let Some(reference) = detect_image_reference(word) {
            references.push(reference);
        } else {
            if !remaining_text.is_empty() {
                remaining_text.push(' ');
            }
            remaining_text.push_str(word);
        }
    }

    (remaining_text, references)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_web_url() {
        assert!(matches!(
            detect_image_reference("https://example.com/image.png"),
            Some(ImageReference::WebUrl(_))
        ));
        assert!(matches!(
            detect_image_reference("http://example.com/photo.jpg"),
            Some(ImageReference::WebUrl(_))
        ));
        assert!(matches!(
            detect_image_reference("https://i.imgur.com/abc123"),
            Some(ImageReference::WebUrl(_))
        ));
    }

    #[test]
    fn test_detect_local_path() {
        assert!(matches!(
            detect_image_reference("./screenshot.png"),
            Some(ImageReference::LocalPath(_))
        ));
        assert!(matches!(
            detect_image_reference("images/photo.jpeg"),
            Some(ImageReference::LocalPath(_))
        ));
        assert!(matches!(
            detect_image_reference("/home/user/image.webp"),
            Some(ImageReference::LocalPath(_))
        ));
    }

    #[test]
    fn test_detect_non_image() {
        assert!(detect_image_reference("hello world").is_none());
        assert!(detect_image_reference("https://example.com/page").is_none());
        assert!(detect_image_reference("document.pdf").is_none());
    }

    #[test]
    fn test_extract_image_references() {
        let (text, refs) = extract_image_references(
            "describe this image.png and this https://example.com/photo.jpg please",
        );
        assert_eq!(text, "describe this and this please");
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_extract_absolute_path_with_colon() {
        // Test case: "what do you see on this image: /Users/alex/test/images/test.jpg"
        let (text, refs) = extract_image_references(
            "what do you see on this image: /Users/alex/test/images/test.jpg",
        );
        assert_eq!(refs.len(), 1, "Should detect 1 image reference");
        assert!(
            matches!(&refs[0], ImageReference::LocalPath(p) if p == "/Users/alex/test/images/test.jpg")
        );
        assert_eq!(text, "what do you see on this image:");
    }

    #[test]
    fn test_extract_various_formats() {
        // Test with absolute paths
        let (_, refs) = extract_image_references("check /path/to/image.png please");
        assert_eq!(refs.len(), 1);

        // Test with tilde paths
        let (_, refs) = extract_image_references("view ~/photos/test.jpg");
        assert_eq!(refs.len(), 1);

        // Test without any text, just path
        let (text, refs) = extract_image_references("/Users/test/photo.jpg");
        assert_eq!(refs.len(), 1);
        assert_eq!(text, "");

        // Test with trailing punctuation (common in sentences)
        let (_, refs) = extract_image_references("look at this: /path/to/image.jpg.");
        assert_eq!(
            refs.len(),
            1,
            "Should detect image even with trailing period"
        );

        // Test with comma
        let (_, refs) = extract_image_references("files: image.png, other.txt");
        assert_eq!(refs.len(), 1, "Should detect image before comma");

        // Test exact user case: relative path after colon
        let (text, refs) =
            extract_image_references("what do you in in this image: images/test_image.png");
        assert_eq!(
            refs.len(),
            1,
            "Should detect relative image path after colon"
        );
        assert!(matches!(&refs[0], ImageReference::LocalPath(p) if p == "images/test_image.png"));
        assert_eq!(text, "what do you in in this image:");
    }

    #[test]
    fn test_image_format_from_extension() {
        assert_eq!(ImageFormat::from_extension("jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_extension("JPEG"), Some(ImageFormat::Jpeg));
        assert_eq!(ImageFormat::from_extension("png"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::from_extension("gif"), Some(ImageFormat::Gif));
        assert_eq!(ImageFormat::from_extension("webp"), Some(ImageFormat::Webp));
        assert_eq!(ImageFormat::from_extension("pdf"), None);
    }

    #[test]
    fn test_image_format_mime_type() {
        assert_eq!(ImageFormat::Jpeg.mime_type(), "image/jpeg");
        assert_eq!(ImageFormat::Png.mime_type(), "image/png");
        assert_eq!(ImageFormat::Gif.mime_type(), "image/gif");
        assert_eq!(ImageFormat::Webp.mime_type(), "image/webp");
    }
}
