//! Streaming multipart form extraction and caller-owned upload destinations.
//!
//! Extract [`Multipart`] through [`super::RequestExt::extract_body`].
//! [`MultipartLimits`] can be installed as middleware or in request extensions.
//! Failures remain typed as [`MultipartError`] and convert into [`crate::Error`].
//!
//! ```no_run
//! use kavi::{Error, http::{Request, RequestExt, Response, multipart::Multipart}};
//!
//! async fn upload(mut request: Request) -> Result<Response, Error> {
//!     let mut multipart = request.extract_body::<Multipart>().await?;
//!     while let Some(mut field) = multipart.next_field().await? {
//!         while let Some(bytes) = field.chunk().await? {
//!             // Process each chunk without collecting the entire upload.
//!         }
//!     }
//!     Ok(Response::default())
//! }
//! ```

use crate::{
    Error,
    dispatch::{Middleware, Next},
    http::{Body, Request, Response, extract::FromRequestBody},
};
use bytes::Bytes;

mod error;
mod parser;

pub use error::MultipartError;

/// Limits applied to multipart extraction, including skipped fields.
///
/// Install this value as middleware or insert it in request extensions before
/// extraction. Defaults are 2 MiB for parser-consumed body bytes, 2 MiB per field,
/// and 128 fields. Preambles are limited to 32 KiB and field headers to 64 KiB
/// and 32 headers; transport padding on a boundary is limited to 64 KiB. The parser stops at the closing delimiter without draining
/// the remaining HTTP body; unread epilogue bytes are not counted.
#[derive(Clone, Copy, Debug)]
pub struct MultipartLimits {
    /// Maximum body bytes read by the parser, including multipart framing.
    pub body_bytes: usize,
    /// Maximum bytes in one field, including files.
    pub field_bytes: usize,
    /// Maximum number of fields, including empty fields.
    pub fields: usize,
}

impl Default for MultipartLimits {
    #[inline]
    fn default() -> Self {
        Self {
            body_bytes: super::body::DEFAULT_BODY_LIMIT,
            field_bytes: super::body::DEFAULT_BODY_LIMIT,
            fields: 128,
        }
    }
}

impl<S> Middleware<S> for MultipartLimits {
    #[inline]
    async fn handle(&self, mut request: Request, next: Next<'_>, _: &S) -> Result<Response, Error> {
        request.extensions_mut().insert(*self);
        next.run(request).await
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

impl<'r, 's, S: ?Sized> FromRequestBody<'r, 's, S> for Multipart {
    type Error = MultipartError;

    async fn from_body(request: &'r Request, body: Body, _: &'s S) -> Result<Self, Self::Error> {
        let content_type = request
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let boundary = parser::parse_boundary(content_type)?;
        let limits = request
            .extensions()
            .get::<MultipartLimits>()
            .copied()
            .unwrap_or_default();
        let constraints = parser::Constraints::new().field_count(limits.fields).size_limit(
            parser::SizeLimit::new()
                .whole_stream(limits.body_bytes as u64)
                .per_field(limits.field_bytes as u64),
        );

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

    /// Collects a field within the configured field and total limits.
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
