use std::{any::Any, convert::Infallible, error::Error as StdError, fmt, io};

use http::{
    HeaderValue, StatusCode,
    header::{ALLOW, CONTENT_TYPE},
};
use vakya::body::CollectError;

use crate::http::{
    Body, MetadataError, Request, Response,
    body::BodyLimitExceeded,
    extract::{BodyError, FormError, JsonError, ParamError, PathError, QueryError, TextError},
    multipart::MultipartError,
    response::forbids_content_type,
};

#[cfg(feature = "cookies")]
use crate::http::cookies::CookieError;

/// A failure with an HTTP status, response headers, and an optional typed source.
///
/// Fallible helpers and extractors can return typed errors. Controllers and
/// middleware may handle these locally or propagate them with `?` in a handler
/// returning `Result<T, Error>`. Use
/// [`Self::into_response`] for a JSON response with the assigned status and
/// headers, or inspect the source and construct an application-specific response.
/// The explicit public message is rendered at any status. Without one, client
/// errors expose the source's display text; server errors use the status reason.
/// Statuses without content produce an empty response.
///
/// Sources need not be `Send` or `Sync`, matching Karmaio's local execution model.
/// Formatting reports the HTTP status; inspect [`StdError::source`] or
/// [`Self::downcast_ref`] for the underlying details.
///
#[derive(thiserror::Error)]
#[error("HTTP request failed: {status}")]
pub struct Error {
    status: StatusCode,
    /// A client-facing message. It is rendered for every status.
    message: Option<Box<str>>,
    #[source]
    source: Option<Box<dyn StdError + 'static>>,
    headers: Box<http::HeaderMap>,
}

/// The body rendered for an unhandled error.
#[derive(serde::Serialize)]
struct ErrorBody<'a> {
    message: &'a str,
}

impl Error {
    /// Creates an intentional HTTP failure without an underlying source.
    ///
    /// The supplied status is retained as-is when converted to a response.
    #[inline]
    pub fn status(status: StatusCode) -> Self {
        Self {
            status,
            message: None,
            source: None,
            headers: Box::default(),
        }
    }

    /// Creates an intentional HTTP failure with a client-facing message.
    ///
    /// The message is rendered for every status, so it must not contain
    /// information the client should not receive.
    #[inline]
    pub fn message(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: Some(message.into().into_boxed_str()),
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
            message: None,
            source: Some(Box::new(source)),
            headers: Box::default(),
        }
    }

    /// Retains a typed source and a client-facing message.
    #[inline]
    pub fn with_message<E: StdError + 'static>(status: StatusCode, message: impl Into<String>, source: E) -> Self {
        Self {
            status,
            message: Some(message.into().into_boxed_str()),
            source: Some(Box::new(source)),
            headers: Box::default(),
        }
    }

    /// Creates a `404 Not Found` failure.
    #[inline]
    pub fn not_found() -> Self {
        Self::status(StatusCode::NOT_FOUND)
    }

    /// Creates a `400 Bad Request` failure with a client-facing message.
    #[inline]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::message(StatusCode::BAD_REQUEST, message)
    }

    /// Creates a `401 Unauthorized` failure.
    ///
    /// Add a `WWW-Authenticate` header through [`Self::headers_mut`] to
    /// describe the expected credentials.
    #[inline]
    pub fn unauthorized() -> Self {
        Self::status(StatusCode::UNAUTHORIZED)
    }

    /// Creates a `403 Forbidden` failure.
    #[inline]
    pub fn forbidden() -> Self {
        Self::status(StatusCode::FORBIDDEN)
    }

    /// Creates a `409 Conflict` failure with a client-facing message.
    #[inline]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::message(StatusCode::CONFLICT, message)
    }

    /// Creates a `422 Unprocessable Entity` failure with a client-facing message.
    #[inline]
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::message(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    /// Creates a `429 Too Many Requests` failure.
    ///
    /// Add a `Retry-After` header through [`Self::headers_mut`] to describe
    /// when the client may retry.
    #[inline]
    pub fn too_many_requests() -> Self {
        Self::status(StatusCode::TOO_MANY_REQUESTS)
    }

    /// Creates a `405 Method Not Allowed` failure listing the allowed methods.
    ///
    /// The list is published in the `Allow` header.
    pub fn method_not_allowed(allowed: impl IntoIterator<Item = crate::http::Method>) -> Self {
        let methods = allowed
            .into_iter()
            .map(|method| method.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        let mut error = Self::status(StatusCode::METHOD_NOT_ALLOWED);
        if let Ok(value) = HeaderValue::from_str(&methods) {
            error.headers_mut().insert(ALLOW, value);
        }
        error
    }

    /// Borrows headers retained for response conversion.
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

    /// Returns the client-facing message, when one was assigned.
    ///
    /// A failure without an explicit message renders the source's display
    /// text for a client error, and the canonical reason for any other status.
    #[inline]
    pub fn public_message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Returns the immediately wrapped source if it has the requested type.
    ///
    /// This does not search deeper entries in the source chain.
    #[inline]
    pub fn downcast_ref<E: StdError + 'static>(&self) -> Option<&E> {
        self.source.as_deref()?.downcast_ref()
    }

    /// Explicitly renders this failure as a response with its HTTP status.
    ///
    /// The body is a JSON object with a single `message` field. Call this where
    /// the failure is handled. Outer middleware receives the resulting response
    /// and can inspect or replace it normally.
    #[inline]
    pub fn into_response(self) -> Response {
        self.to_response()
    }

    /// Renders this failure as a response with its HTTP status.
    ///
    /// The body is a JSON object with a single `message` field. The message is
    /// the explicit public message when one was assigned, otherwise the
    /// source's display text for a client error, otherwise the canonical
    /// reason for the status. A server error never exposes its source.
    pub fn to_response(&self) -> Response {
        let status = self.status;
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status;
        *response.headers_mut() = self.headers.as_ref().clone();
        if !forbids_content_type(status) {
            let message = self.resolve_message();
            let body = serde_json::to_string(&ErrorBody { message: &message })
                .unwrap_or_else(|_| String::from("{\"message\":\"Internal Server Error\"}"));
            *response.body_mut() = Body::from(body);
            if !response.headers().contains_key(CONTENT_TYPE) {
                response
                    .headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            }
        }
        response
    }

    /// Resolves the client-facing text for this failure.
    fn resolve_message(&self) -> String {
        if let Some(message) = &self.message {
            return message.to_string();
        }
        // A client error is actionable, so its source explains what was wrong.
        // A server error must never expose an internal source.
        if self.status.is_client_error()
            && let Some(source) = &self.source
        {
            return source.to_string();
        }
        self.status.canonical_reason().unwrap_or("Error").to_string()
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("status", &self.status)
            .field("message", &self.message)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

/// Observes and renders a panic that escapes a controller or middleware.
///
/// Handler results are converted into responses at their controller or
/// middleware boundary, using the selected branch's `IntoResponse` implementation.
/// Returned application errors do not reach this hook. A panic is a separate
/// defect: the framework catches it at the application boundary and renders a
/// generic 500.
///
/// The framework reports the panic once, then renders it. Both hooks receive
/// the request, so an implementation can correlate the failure with a request
/// ID. What is rendered must not describe the panic; [`PanicError`] carries the
/// message for observation only.
///
/// Because the panic unwinds the middleware chain, middleware awaiting
/// downstream does not resume and cannot decorate the rendered response.
/// Branding the 500 belongs in this handler instead.
///
/// The default implementation reports nothing and renders the standard JSON
/// body with `500 Internal Server Error`.
///
/// ```rust,ignore
/// use kavi::{Error, ErrorHandler, PanicError};
/// use kavi::http::{Request, Response};
///
/// struct Logged;
///
/// impl ErrorHandler for Logged {
///     fn report(&self, error: &Error, _request: &Request) {
///         if let Some(panic) = error.downcast_ref::<PanicError>() {
///             eprintln!("panic: {}", panic.message());
///         }
///     }
/// }
/// ```
pub trait ErrorHandler {
    /// Observes a caught panic before it is rendered.
    ///
    /// The default implementation does nothing.
    fn report(&self, error: &Error, request: &Request) {
        let _ = (error, request);
    }

    /// Renders a caught panic into a response.
    ///
    /// The default implementation renders the standard JSON body.
    fn render(&self, error: &Error, request: &Request) -> Response {
        let _ = request;
        error.to_response()
    }
}

/// The panic that escaped a controller or middleware.
///
/// It is the source of the [`Error`] passed to [`ErrorHandler::report`], so a
/// report can log what the panic said without the caller ever seeing it. The
/// default rendering never exposes it, because a server error renders the
/// status reason instead.
#[derive(Debug)]
pub struct PanicError {
    message: String,
}

impl PanicError {
    /// Captures the message from a panic payload.
    ///
    /// A panic carries either a `&str`, a `String`, or an arbitrary value. The
    /// last case has no message, and a report records that instead.
    pub fn from_payload(payload: &(dyn Any + Send)) -> Self {
        let message = if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_owned()
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            String::from("a panic occurred without a message")
        };
        Self { message }
    }

    /// Returns the message the panic carried.
    #[inline]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PanicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for PanicError {}

/// The standard error handler.
///
/// It reports nothing and renders the standard JSON body.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultErrorHandler;

impl ErrorHandler for DefaultErrorHandler {}

impl From<io::Error> for Error {
    #[inline]
    fn from(source: io::Error) -> Self {
        Self::internal(source)
    }
}

impl From<MetadataError> for Error {
    #[inline]
    fn from(source: MetadataError) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, source)
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
        CollectError::Body(error) if body_limit_source(error) => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl From<BodyLimitExceeded> for Error {
    #[inline]
    fn from(source: BodyLimitExceeded) -> Self {
        Self::with_status(StatusCode::PAYLOAD_TOO_LARGE, source)
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

impl From<MultipartError> for Error {
    #[inline]
    fn from(source: MultipartError) -> Self {
        let status = if body_limit_source(&source) {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            match &source {
                MultipartError::NoMultipart | MultipartError::DecodeContentType(_) => {
                    StatusCode::UNSUPPORTED_MEDIA_TYPE
                }
                MultipartError::FieldSizeExceeded { .. }
                | MultipartError::PreambleSizeExceeded { .. }
                | MultipartError::HeadersSizeExceeded { .. }
                | MultipartError::BoundaryPaddingSizeExceeded { .. }
                | MultipartError::FieldCountExceeded { .. }
                | MultipartError::ReadHeaderFailed(httparse::Error::TooManyHeaders) => StatusCode::PAYLOAD_TOO_LARGE,
                MultipartError::StreamReadFailed(_) | MultipartError::Write(_) | MultipartError::Terminated => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                _ => StatusCode::BAD_REQUEST,
            }
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

#[cfg(feature = "cookies")]
impl From<CookieError> for Error {
    #[inline]
    fn from(source: CookieError) -> Self {
        let status = match &source {
            CookieError::Disabled => StatusCode::INTERNAL_SERVER_ERROR,
            CookieError::Header(_) | CookieError::Parse(_) => StatusCode::BAD_REQUEST,
        };
        Self::with_status(status, source)
    }
}

impl From<vakya::error::Error> for Error {
    fn from(source: vakya::error::Error) -> Self {
        let status = StatusCode::INTERNAL_SERVER_ERROR;
        let status = if body_limit_source(&source) {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            status
        };
        Self::with_status(status, source)
    }
}

fn body_limit_source(error: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.is::<BodyLimitExceeded>() {
            return true;
        }
        current = match error.downcast_ref::<io::Error>().and_then(io::Error::get_ref) {
            Some(inner) => Some(inner as &(dyn StdError + 'static)),
            None => error.source(),
        };
    }
    false
}
