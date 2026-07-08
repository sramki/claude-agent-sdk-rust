//! Typed builders for multimodal user input (text + images).
//!
//! **Beyond-parity extension.** The Python SDK v0.2.110 has no typed image
//! block — images ride in as raw content dicts. This adds ergonomic, validated
//! constructors that serialize to the exact wire content blocks the `claude`
//! CLI accepts, for use with [`Prompt::Messages`](crate::Prompt). Sending images
//! already works via raw JSON; this just makes it type-safe and fails fast on a
//! bad media type or oversized payload.
//!
//! ```no_run
//! use claude_agent_sdk_rs::{query, ClaudeAgentOptions, Prompt};
//! use claude_agent_sdk_rs::input::{user_message, UserContentBlock};
//!
//! # async fn run(png_base64: String) -> claude_agent_sdk_rs::Result<()> {
//! let msg = user_message([
//!     UserContentBlock::text("What is in this image?"),
//!     UserContentBlock::image_base64("image/png", png_base64)?,
//! ]);
//! let _stream = query(Prompt::Messages(vec![msg]), ClaudeAgentOptions::default()).await?;
//! # Ok(()) }
//! ```

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Image MIME types Claude accepts for vision input.
pub const SUPPORTED_IMAGE_MEDIA_TYPES: [&str; 4] =
    ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Client-side sanity cap on base64 image-payload length (15 MiB). This is a
/// guard against obviously-oversized input, not an API guarantee — the CLI /
/// model may impose stricter limits.
pub const MAX_IMAGE_BASE64_LEN: usize = 15 * 1024 * 1024;

/// Document (attachment) MIME types Claude accepts for input.
pub const SUPPORTED_DOCUMENT_MEDIA_TYPES: [&str; 2] = ["application/pdf", "text/plain"];

/// Client-side sanity cap on base64 document-payload length (40 MiB). Sanity
/// guard only; the CLI / model may impose stricter limits.
pub const MAX_DOCUMENT_BASE64_LEN: usize = 40 * 1024 * 1024;

/// The source of an [`UserContentBlock::Image`] block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// Base64-encoded image data (no `data:` URI prefix).
    Base64 {
        /// MIME type, one of [`SUPPORTED_IMAGE_MEDIA_TYPES`].
        media_type: String,
        /// Base64 payload.
        data: String,
    },
    /// A URL Claude fetches.
    Url {
        /// The image URL.
        url: String,
    },
}

/// The source of an [`UserContentBlock::Document`] block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentSource {
    /// Base64-encoded document data (no `data:` URI prefix).
    Base64 {
        /// MIME type, one of [`SUPPORTED_DOCUMENT_MEDIA_TYPES`].
        media_type: String,
        /// Base64 payload.
        data: String,
    },
    /// A URL Claude fetches.
    Url {
        /// The document URL.
        url: String,
    },
}

/// A block of user input content. Build a sequence and serialize it with
/// [`user_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserContentBlock {
    /// A text block.
    Text(String),
    /// An image block.
    Image(ImageSource),
    /// A document block (e.g. a PDF).
    Document(DocumentSource),
}

impl UserContentBlock {
    /// A text block.
    pub fn text(text: impl Into<String>) -> Self {
        UserContentBlock::Text(text.into())
    }

    /// A base64 image block. Validates `media_type` against
    /// [`SUPPORTED_IMAGE_MEDIA_TYPES`] and `data` against
    /// [`MAX_IMAGE_BASE64_LEN`]; returns [`Error::Invalid`] on either.
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Result<Self> {
        let media_type = media_type.into();
        if !SUPPORTED_IMAGE_MEDIA_TYPES.contains(&media_type.as_str()) {
            return Err(Error::Invalid(format!(
                "unsupported image media type {media_type:?}; expected one of {SUPPORTED_IMAGE_MEDIA_TYPES:?}"
            )));
        }
        let data = data.into();
        if data.len() > MAX_IMAGE_BASE64_LEN {
            return Err(Error::Invalid(format!(
                "base64 image data is {} bytes, over the {MAX_IMAGE_BASE64_LEN}-byte limit",
                data.len()
            )));
        }
        Ok(UserContentBlock::Image(ImageSource::Base64 { media_type, data }))
    }

    /// A URL image block (Claude fetches the URL). Errors on an empty URL.
    pub fn image_url(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        if url.trim().is_empty() {
            return Err(Error::Invalid("image url must be non-empty".into()));
        }
        Ok(UserContentBlock::Image(ImageSource::Url { url }))
    }

    /// A base64 document block (e.g. a PDF). Validates `media_type` against
    /// [`SUPPORTED_DOCUMENT_MEDIA_TYPES`] and `data` against
    /// [`MAX_DOCUMENT_BASE64_LEN`].
    pub fn document_base64(media_type: impl Into<String>, data: impl Into<String>) -> Result<Self> {
        let media_type = media_type.into();
        if !SUPPORTED_DOCUMENT_MEDIA_TYPES.contains(&media_type.as_str()) {
            return Err(Error::Invalid(format!(
                "unsupported document media type {media_type:?}; expected one of {SUPPORTED_DOCUMENT_MEDIA_TYPES:?}"
            )));
        }
        let data = data.into();
        if data.len() > MAX_DOCUMENT_BASE64_LEN {
            return Err(Error::Invalid(format!(
                "base64 document data is {} bytes, over the {MAX_DOCUMENT_BASE64_LEN}-byte limit",
                data.len()
            )));
        }
        Ok(UserContentBlock::Document(DocumentSource::Base64 { media_type, data }))
    }

    /// A URL document block (Claude fetches the URL). Errors on an empty URL.
    pub fn document_url(url: impl Into<String>) -> Result<Self> {
        let url = url.into();
        if url.trim().is_empty() {
            return Err(Error::Invalid("document url must be non-empty".into()));
        }
        Ok(UserContentBlock::Document(DocumentSource::Url { url }))
    }

    /// Serializes to the wire content block the CLI accepts.
    pub fn to_wire(&self) -> Value {
        match self {
            UserContentBlock::Text(text) => json!({"type": "text", "text": text}),
            UserContentBlock::Image(source) => {
                let source = match source {
                    ImageSource::Base64 { media_type, data } => {
                        json!({"type": "base64", "media_type": media_type, "data": data})
                    }
                    ImageSource::Url { url } => json!({"type": "url", "url": url}),
                };
                json!({"type": "image", "source": source})
            }
            UserContentBlock::Document(source) => {
                let source = match source {
                    DocumentSource::Base64 { media_type, data } => {
                        json!({"type": "base64", "media_type": media_type, "data": data})
                    }
                    DocumentSource::Url { url } => json!({"type": "url", "url": url}),
                };
                json!({"type": "document", "source": source})
            }
        }
    }
}

/// Builds a `user` input message from content blocks, ready for
/// [`Prompt::Messages`](crate::Prompt). `session_id` is injected by the runtime
/// when absent, so it is omitted here.
pub fn user_message(blocks: impl IntoIterator<Item = UserContentBlock>) -> Value {
    let content: Vec<Value> = blocks.into_iter().map(|b| b.to_wire()).collect();
    json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_wire() {
        assert_eq!(
            UserContentBlock::text("hi").to_wire(),
            json!({"type": "text", "text": "hi"})
        );
    }

    #[test]
    fn image_base64_valid_wire() {
        let b = UserContentBlock::image_base64("image/png", "aGVsbG8=").unwrap();
        assert_eq!(
            b.to_wire(),
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}})
        );
    }

    #[test]
    fn image_url_valid_wire() {
        let b = UserContentBlock::image_url("https://ex.com/cat.png").unwrap();
        assert_eq!(
            b.to_wire(),
            json!({"type": "image", "source": {"type": "url", "url": "https://ex.com/cat.png"}})
        );
    }

    #[test]
    fn image_base64_rejects_bad_media_type() {
        let err = UserContentBlock::image_base64("image/bmp", "x").unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.contains("image/bmp")));
    }

    #[test]
    fn image_base64_rejects_oversized() {
        let big = "a".repeat(MAX_IMAGE_BASE64_LEN + 1);
        let err = UserContentBlock::image_base64("image/png", big).unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.contains("over the")));
    }

    #[test]
    fn image_url_rejects_empty() {
        assert!(UserContentBlock::image_url("   ").is_err());
    }

    #[test]
    fn document_base64_and_url_wire() {
        let pdf = UserContentBlock::document_base64("application/pdf", "JVBERi0=").unwrap();
        assert_eq!(
            pdf.to_wire(),
            json!({"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBERi0="}})
        );
        let url = UserContentBlock::document_url("https://ex.com/a.pdf").unwrap();
        assert_eq!(
            url.to_wire(),
            json!({"type": "document", "source": {"type": "url", "url": "https://ex.com/a.pdf"}})
        );
    }

    #[test]
    fn document_rejects_bad_media_type_and_empty_url() {
        assert!(matches!(
            UserContentBlock::document_base64("application/zip", "x").unwrap_err(),
            Error::Invalid(m) if m.contains("application/zip")
        ));
        assert!(UserContentBlock::document_url("").is_err());
    }

    #[test]
    fn user_message_shape() {
        let msg = user_message([
            UserContentBlock::text("look:"),
            UserContentBlock::image_base64("image/jpeg", "Zm9v").unwrap(),
        ]);
        assert_eq!(msg["type"], "user");
        assert_eq!(msg["message"]["role"], "user");
        let content = msg["message"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert!(msg["parent_tool_use_id"].is_null());
    }
}
