use std::{convert::Infallible, error::Error as StdError, fmt, io};

use vakya::body::CollectError;

use crate::http::{
    Body, Response, StatusCode,
    extract::{BodyError, FormError, JsonError, ParamError, PathError, QueryError, TextError},
};

/// A dispatch failure with an HTTP status and an optional typed source.
///
/// Errors remain fallible through middleware; use [`Self::into_response`] at an
/// explicit error-rendering boundary. The default renderer produces an empty
/// response and never exposes source details to a client.
///
/// Sources need not be `Send` or `Sync`, matching Karmaio's local execution model.
/// Formatting reports the HTTP status; inspect [`StdError::source`] or
/// [`Self::downcast_ref`] for the underlying details.
///
/// ```text
/// use kavi::Error;
///
/// let error = Error::internal(std::io::Error::other("private database detail"));
/// let response = error.into_response();
/// ```
#[derive(thiserror::Error)]
#[error("HTTP dispatch failed: {status}")]
pub struct Error {
    status: StatusCode,
    #[source]
    source: Option<Box<dyn StdError + 'static>>,
    headers: Box<http::HeaderMap>,
}

impl Error {
    /// Creates an intentional HTTP failure without an underlying source.
    ///
    /// The supplied status is retained as-is for the final rendering boundary.
    #[inline]
    pub fn status(status: StatusCode) -> Self {
        Self {
            status,
            source: None,
            headers: Box::default(),
        }
    }

    /// Wraps an internal failure with a 500 status and retains its typed source.
    #[inline]
    pub fn internal<E: StdError + 'static>(source: E) -> Self {
        Self::with_status(StatusCode::INTERNAL_SERVER_ERROR, source)
    }

    /// Retains a typed source with an explicitly chosen HTTP status.
    #[inline]
    pub fn with_status<E: StdError + 'static>(status: StatusCode, source: E) -> Self {
        Self {
            status,
            source: Some(Box::new(source)),
            headers: Box::default(),
        }
    }

    /// Borrows response headers retained for the error renderer.
    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }

    /// Mutates headers such as `WWW-Authenticate` or `Retry-After`.
    pub fn headers_mut(&mut self) -> &mut http::HeaderMap {
        &mut self.headers
    }

    /// Returns the HTTP status assigned to this failure.
    #[inline]
    pub fn status_code(&self) -> StatusCode {
        self.status
    }

    /// Returns the immediately wrapped source if it has the requested type.
    ///
    /// This does not search deeper entries in the source chain.
    #[inline]
    pub fn downcast_ref<E: StdError + 'static>(&self) -> Option<&E> {
        self.source.as_deref()?.downcast_ref()
    }

    /// Explicitly renders this failure as an empty response with its HTTP status.
    ///
    /// Call this after middleware has had an opportunity to inspect or recover
    /// the error. Applications can instead implement their own rendering policy.
    #[inline]
    pub fn into_response(self) -> Response {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = self.status;
        *response.headers_mut() = *self.headers;
        response
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("status", &self.status)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl From<io::Error> for Error {
    #[inline]
    fn from(source: io::Error) -> Self {
        Self::internal(source)
    }
}

impl From<Infallible> for Error {
    #[inline]
    fn from(source: Infallible) -> Self {
        match source {}
    }
}

// Body sources may be custom producers or transport failures. Keep their default
// conservative until the application/server boundary defines transport policy.
fn collection_status(source: &CollectError<vakya::error::Error>) -> StatusCode {
    match source {
        CollectError::LimitExceeded { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl From<CollectError<vakya::error::Error>> for Error {
    #[inline]
    fn from(source: CollectError<vakya::error::Error>) -> Self {
        Self::with_status(collection_status(&source), source)
    }
}

impl From<JsonError> for Error {
    #[inline]
    fn from(source: JsonError) -> Self {
        let status = match &source {
            JsonError::WrongContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            JsonError::Deserialize(_) => StatusCode::BAD_REQUEST,
            JsonError::Body(error) => collection_status(error),
        };
        Self::with_status(status, source)
    }
}

impl From<FormError> for Error {
    #[inline]
    fn from(source: FormError) -> Self {
        let status = match &source {
            FormError::WrongContentType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            FormError::Deserialize(_) => StatusCode::BAD_REQUEST,
            FormError::Body(error) => collection_status(error),
        };
        Self::with_status(status, source)
    }
}

impl From<BodyError> for Error {
    #[inline]
    fn from(source: BodyError) -> Self {
        let BodyError::Collect(error) = &source;
        Self::with_status(collection_status(error), source)
    }
}

impl From<TextError> for Error {
    #[inline]
    fn from(source: TextError) -> Self {
        let status = match &source {
            TextError::Utf8(_) => StatusCode::BAD_REQUEST,
            TextError::Body(error) => collection_status(error),
        };
        Self::with_status(status, source)
    }
}

impl From<PathError> for Error {
    #[inline]
    fn from(source: PathError) -> Self {
        let status = match &source {
            PathError::Missing => StatusCode::INTERNAL_SERVER_ERROR,
            PathError::Deserialize(_) => StatusCode::BAD_REQUEST,
        };
        Self::with_status(status, source)
    }
}

impl From<ParamError> for Error {
    #[inline]
    fn from(source: ParamError) -> Self {
        let status = match &source {
            ParamError::Missing { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            ParamError::Deserialize { .. } => StatusCode::BAD_REQUEST,
        };
        Self::with_status(status, source)
    }
}

impl From<QueryError> for Error {
    #[inline]
    fn from(source: QueryError) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, source)
    }
}

impl From<vakya::error::Error> for Error {
    fn from(source: vakya::error::Error) -> Self {
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        //TODO: Add middleware limit error here later
        Self::with_status(status, source)
    }
}
