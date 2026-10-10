use std::fmt::{self, Display, Formatter};

type BoxError = Box<dyn std::error::Error>;

/// Typed failures from multipart extraction, field reads, and upload writes.
///
/// Converting into [`crate::Error`] assigns 415 to unsupported content types,
/// 413 to limit violations, 400 to malformed input, and 500
/// to body-source, destination-write, or already-terminated parser failures.
/// Body sources need not be `Send` or `Sync`.
#[non_exhaustive]
#[derive(Debug)]
pub enum MultipartError {
    /// A previous parser failure prevents further reads.
    Terminated,

    /// The multipart body contains more fields than configured.
    FieldCountExceeded {
        /// The configured maximum number of fields.
        limit: usize,
    },

    /// A boundary candidate contains more than 64 KiB of transport padding.
    BoundaryPaddingSizeExceeded {
        /// The maximum number of padding bytes accepted for a candidate.
        limit: usize,
    },

    /// Writing a field to its destination failed.
    Write(std::io::Error),
    /// An unknown field is detected when multipart
    /// parser field constraints are added.
    UnknownField { field_name: Option<String> },

    /// The field data is found incomplete.
    IncompleteFieldData { field_name: Option<String> },

    /// Couldn't read the field headers completely.
    IncompleteHeaders,

    /// Failed to read headers.
    ReadHeaderFailed(httparse::Error),

    /// Failed to decode the field's raw header name to
    /// [`HeaderName`](http::header::HeaderName) type.
    DecodeHeaderName { name: String, cause: BoxError },

    /// Failed to decode the field's raw header value to
    /// [`HeaderValue`](http::header::HeaderValue) type.
    DecodeHeaderValue { value: Vec<u8>, cause: BoxError },

    /// Multipart stream is incomplete.
    IncompleteStream,

    /// The incoming field size exceeded the maximum limit.
    FieldSizeExceeded { limit: u64, field_name: Option<String> },

    /// The incoming stream size exceeded the maximum limit.
    StreamSizeExceeded { limit: u64 },

    /// The data before the first multipart boundary exceeded the maximum
    /// limit.
    PreambleSizeExceeded { limit: u64 },

    /// The incoming field headers exceeded the maximum limit.
    HeadersSizeExceeded { limit: u64 },

    /// Stream read failed.
    StreamReadFailed(BoxError),

    /// The `Content-Type` header is not `multipart/form-data`.
    NoMultipart,

    /// Failed to convert the `Content-Type` to [`mime::Mime`] type.
    DecodeContentType(mime::FromStrError),

    /// No boundary found in `Content-Type` header.
    NoBoundary,

    /// The boundary parameter is outside the allowed multipart boundary syntax.
    InvalidBoundary { boundary: String },
}

impl Display for MultipartError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminated => write!(f, "multipart parser terminated after a previous failure"),
            Self::FieldCountExceeded { limit } => write!(f, "multipart field count exceeded limit: {limit}"),
            Self::BoundaryPaddingSizeExceeded { limit } => {
                write!(f, "boundary transport padding exceeded limit: {limit} bytes")
            }
            Self::Write(_) => write!(f, "failed to write multipart field"),
            Self::UnknownField { field_name } => {
                let name = field_name.as_deref().unwrap_or("<unknown>");
                write!(f, "unknown field received: {name:?}")
            }
            Self::IncompleteFieldData { field_name } => {
                let name = field_name.as_deref().unwrap_or("<unknown>");
                write!(f, "field {name:?} received with incomplete data")
            }
            Self::DecodeHeaderName { name, .. } => {
                write!(f, "failed to decode field's raw header name: {name:?}")
            }
            Self::DecodeHeaderValue { .. } => {
                write!(f, "failed to decode field's raw header value")
            }
            Self::FieldSizeExceeded { limit, field_name } => {
                let name = field_name.as_deref().unwrap_or("<unknown>");
                write!(f, "field {name:?} exceeded the size limit: {limit} bytes")
            }
            Self::StreamSizeExceeded { limit } => {
                write!(f, "stream size exceeded limit: {limit} bytes")
            }
            Self::PreambleSizeExceeded { limit } => {
                write!(f, "multipart preamble exceeded limit: {limit} bytes")
            }
            Self::HeadersSizeExceeded { limit } => {
                write!(f, "field headers exceeded limit: {limit} bytes")
            }
            Self::ReadHeaderFailed(_) => write!(f, "failed to read headers"),
            Self::StreamReadFailed(_) => write!(f, "failed to read stream"),
            Self::DecodeContentType(_) => write!(f, "failed to decode Content-Type"),
            Self::IncompleteHeaders => write!(f, "failed to read field complete headers"),
            Self::IncompleteStream => write!(f, "incomplete multipart stream"),
            Self::NoMultipart => write!(f, "Content-Type is not multipart/form-data"),
            Self::NoBoundary => write!(f, "multipart boundary not found in Content-Type"),
            Self::InvalidBoundary { boundary } => {
                write!(f, "invalid multipart boundary: {boundary:?}")
            }
        }
    }
}

impl std::error::Error for MultipartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BoundaryPaddingSizeExceeded { .. } => None,
            Self::Write(error) => Some(error),
            Self::Terminated | Self::FieldCountExceeded { .. } => None,
            Self::ReadHeaderFailed(e) => Some(e),
            Self::DecodeHeaderName { cause, .. } => Some(cause.as_ref()),
            Self::DecodeHeaderValue { cause, .. } => Some(cause.as_ref()),
            Self::StreamReadFailed(e) => Some(e.as_ref()),
            Self::DecodeContentType(e) => Some(e),
            Self::UnknownField { .. }
            | Self::IncompleteFieldData { .. }
            | Self::IncompleteHeaders
            | Self::IncompleteStream
            | Self::FieldSizeExceeded { .. }
            | Self::StreamSizeExceeded { .. }
            | Self::PreambleSizeExceeded { .. }
            | Self::HeadersSizeExceeded { .. }
            | Self::NoMultipart
            | Self::NoBoundary
            | Self::InvalidBoundary { .. } => None,
        }
    }
}
