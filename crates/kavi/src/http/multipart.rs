//! Streaming multipart form extraction and caller-owned upload destinations.
//!
//! Extract [`Multipart`] through [`super::RequestExt::extract_body`].
//! Pass [`MultipartLimits`] to [`super::RequestExt::extract_body_with`] to configure
//! field checks explicitly. Use [`crate::http::middleware::BodyLimit`] independently
//! to enforce total payload bytes, including multipart framing.
//! Failures remain typed as [`MultipartError`] and convert into [`crate::Error`].
//!

use crate::http::{Body, Request, extract::FromRequestBody};
use bytes::Bytes;

mod error;
mod parser;

pub use error::MultipartError;

/// Limits applied to multipart extraction, including skipped fields.
///
/// Keep these parser settings in application configuration and pass them directly
/// to extraction. They do not implement middleware and are never installed in
/// request extensions. Total payload enforcement belongs to
/// [`crate::http::middleware::BodyLimit`]; without it there is no total payload ceiling.
/// Defaults are 2 MiB per field and 128 fields.
/// Preambles are limited to 32 KiB and field headers to 64 KiB and 32 headers;
/// transport padding on a boundary is limited to 64 KiB. The parser stops at the
/// closing delimiter without draining the remaining HTTP body; unread epilogue
/// bytes are not counted.
#[derive(Clone, Copy, Debug)]
pub struct MultipartLimits {
    /// Maximum bytes in one field, including files.
    pub field_bytes: usize,
    /// Maximum number of fields, including empty fields.
    pub fields: usize,
}

impl Default for MultipartLimits {
    #[inline]
    fn default() -> Self {
        Self {
            field_bytes: super::body::DEFAULT_BODY_LIMIT,
            fields: 128,
        }
    }
}

/// A streaming multipart request body.
///
/// Only one field can be borrowed at a time. Dropping a field skips its remaining
/// bytes when the next field is requested. Limits also cover skipped fields.
/// Incoming filenames are metadata; they are never used as filesystem paths.
/// Dropping this extractor releases its body without replaying or draining it.
/// Extraction validates the content type and boundary without reading body frames.
///
/// # Errors
///
/// Extraction returns [`MultipartError::NoMultipart`] or
/// [`MultipartError::DecodeContentType`] for an unsupported content type, and
/// [`MultipartError::NoBoundary`] or [`MultipartError::InvalidBoundary`] for a
/// missing or invalid boundary. Read errors are returned as fields are consumed.
pub struct Multipart {
    parser: parser::Multipart,
}

impl<'r, 's> FromRequestBody<'r, 's> for Multipart {
    type Error = MultipartError;

    async fn from_body(request: &'r Request, body: Body, _: &'s ()) -> Result<Self, Self::Error> {
        Self::from_body(request, body, &MultipartLimits::default()).await
    }
}

impl<'r, 's> FromRequestBody<'r, 's, MultipartLimits> for Multipart {
    type Error = MultipartError;

    async fn from_body(request: &'r Request, body: Body, limits: &'s MultipartLimits) -> Result<Self, Self::Error> {
        let content_type = request
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let boundary = parser::parse_boundary(content_type)?;
        let constraints = parser::Constraints::new()
            .field_count(limits.fields)
            .size_limit(parser::SizeLimit::new().per_field(limits.field_bytes as u64));

        Ok(Self {
            parser: parser::Multipart::with_constraints(body, boundary, constraints),
        })
    }
}

impl Multipart {
    /// Advances to the next field, consuming a dropped previous field if necessary.
    ///
    /// # Errors
    ///
    /// Returns a typed parsing, body-read, or limit error. A parser failure is
    /// terminal; subsequent reads return [`MultipartError::Terminated`].
    pub async fn next_field(&mut self) -> Result<Option<Field<'_>>, MultipartError> {
        Ok(self.parser.next_field().await?.map(|field| Field { field }))
    }
}

/// One borrowed multipart field with streaming and bounded collection operations.
pub struct Field<'a> {
    field: parser::Field<'a>,
}

impl Field<'_> {
    /// Returns the declared field name.
    pub fn name(&self) -> Option<&str> {
        self.field.name()
    }

    /// Returns an untrusted original filename, if supplied.
    pub fn file_name(&self) -> Option<&str> {
        self.field.file_name()
    }

    /// Returns parsed field headers.
    pub fn headers(&self) -> &http::HeaderMap {
        self.field.headers()
    }

    /// Returns the declared content type, if present and valid.
    pub fn content_type(&self) -> Option<&mime::Mime> {
        self.field.content_type()
    }

    /// Returns this field's zero-based position in the multipart stream.
    pub fn index(&self) -> usize {
        self.field.index()
    }

    /// Reads the next field chunk without collecting the entire upload.
    ///
    /// # Errors
    ///
    /// Returns a parsing, body-read, or limit error. Parser failures are terminal.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, MultipartError> {
        self.field.chunk().await
    }

    /// Collects a field within its parser limit and any enforced payload ceiling.
    ///
    /// # Errors
    ///
    /// Returns a parsing, body-read, or limit error.
    pub async fn bytes(self) -> Result<Bytes, MultipartError> {
        self.field.bytes().await
    }

    /// Decodes the remaining field data using its charset, defaulting to UTF-8.
    ///
    /// BOM sniffing is enabled and malformed sequences are replaced with the Unicode replacement character.
    ///
    /// # Errors
    ///
    /// Returns a parsing, body-read, or limit error.
    pub async fn text(self) -> Result<String, MultipartError> {
        self.text_with_charset("utf-8").await
    }

    /// Decodes the remaining field data with a caller-supplied default charset.
    ///
    /// A field's declared `charset` takes precedence over `default_encoding`.
    /// Unknown labels fall back to UTF-8. BOM sniffing is enabled and malformed
    /// sequences are replaced, matching Multra's decoding behavior.
    ///
    /// # Errors
    ///
    /// Returns a parsing, body-read, or limit error.
    pub async fn text_with_charset(self, default_encoding: &str) -> Result<String, MultipartError> {
        let label = self
            .content_type()
            .and_then(|mime| mime.get_param(mime::CHARSET))
            .map_or(default_encoding, |charset| charset.as_str());
        let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);

        let bytes = self.bytes().await?;

        Ok(encoding.decode(&bytes).0.into_owned())
    }

    /// Streams this field to a completion-based writer, returning bytes written.
    ///
    /// The caller owns the destination and cleanup policy. A failed or canceled
    /// upload can leave partial output; this method does not commit a file.
    ///
    /// # Errors
    ///
    /// Returns a read or limit error, or [`MultipartError::Write`] if writing fails.
    pub async fn write_to<W: karmaio::io::AsyncWrite>(mut self, writer: &mut W) -> Result<u64, MultipartError> {
        use karmaio::io::AsyncWriteExt;
        let mut total = 0;
        let mut budget = crate::support::future::WorkBudget::new();

        loop {
            budget.step().await;
            let Some(bytes) = self.chunk().await? else {
                break;
            };

            let (result, _) = writer.write_all(bytes).await.into_parts();

            total += result.map_err(MultipartError::Write)? as u64;
        }

        Ok(total)
    }
}
