//! Response types and conversion into concrete HTTP responses.
//!
//! [`IntoResponse`] converts a value into a response, so a controller can
//! return a [`String`], a [`StatusCode`], a [`JsonResponse`] value, or any type the
//! application implements it for.
//!
//! [`TextResponse`], [`HtmlResponse`], and [`BinaryResponse`] select a media type
//! for an owned or streaming body. [`JsonResponse`] serializes application data;
//! [`JsonResponse::try_into_response`] exposes serialization failures when needed.
//! [`Redirect`] validates its location during construction.

use std::{borrow::Cow, convert::Infallible};

use http::{
    HeaderValue, StatusCode,
    header::{CONTENT_TYPE, LOCATION},
};

use crate::{
    Error,
    http::{Body, Response},
};

/// The media type for a plain-text body.
const TEXT: &str = "text/plain; charset=utf-8";
/// The media type for an HTML body.
const HTML: &str = "text/html; charset=utf-8";
/// The media type for a JSON body.
const JSON: &str = "application/json";
/// The media type for an opaque binary body.
const BINARY: &str = "application/octet-stream";

/// Converts a value into an HTTP response.
///
/// Implement this for your own types to return them directly from a
/// controller. `Result<T, E>` also implements this trait when both branches
/// implement it. The selected branch becomes a response at the handler boundary,
/// so outer middleware always receives a concrete response. Application errors
/// keep their own conversion and are not forced through [`Error`].
///
/// A `(StatusCode, T)` overrides the converted response's status, except when
/// JSON serialization failed. That conversion failure retains its 500 status,
/// including through nested status tuples.
///
/// ```rust
/// use kavi::http::{Response, response::IntoResponse};
///
/// struct Greeting(&'static str);
///
/// impl IntoResponse for Greeting {
///     fn into_response(self) -> Response {
///         self.0.into_response()
///     }
/// }
/// ```
///
/// A custom error need not implement `std::error::Error` or convert into
/// [`Error`]. It only needs to describe its HTTP response:
///
/// ```rust
/// use kavi::http::{IntoResponse, Response, StatusCode, response::JsonResponse};
///
/// enum LookupError { Missing }
///
/// impl IntoResponse for LookupError {
///     fn into_response(self) -> Response {
///         (StatusCode::NOT_FOUND, "user not found").into_response()
///     }
/// }
///
/// let result: Result<JsonResponse<u64>, LookupError> = Err(LookupError::Missing);
/// assert_eq!(result.into_response().status(), StatusCode::NOT_FOUND);
/// ```
pub trait IntoResponse {
    /// Converts this value into a complete response.
    fn into_response(self) -> Response;
}

impl<T: IntoResponse, E: IntoResponse> IntoResponse for Result<T, E> {
    /// Converts the selected branch without changing its status, headers, or body.
    #[inline]
    fn into_response(self) -> Response {
        match self {
            Ok(value) => value.into_response(),
            Err(error) => error.into_response(),
        }
    }
}

impl IntoResponse for Error {
    /// Renders a framework error using its default JSON representation.
    #[inline]
    fn into_response(self) -> Response {
        Error::into_response(self)
    }
}

impl IntoResponse for Infallible {
    #[inline]
    fn into_response(self) -> Response {
        match self {}
    }
}

/// Keeps a failed conversion from being relabeled by a surrounding status tuple.
#[derive(Clone, Copy, Debug)]
struct ConversionFailure;

/// A plain-text response with `text/plain; charset=utf-8` and a 200 status.
///
/// The value may be any owned or streaming body implementing `Into<Body>`.
/// Use a status tuple to select another status.
///
/// ```rust
/// use kavi::http::{IntoResponse, StatusCode, response::TextResponse};
///
/// let response = (StatusCode::ACCEPTED, TextResponse("queued")).into_response();
/// assert_eq!(response.status(), StatusCode::ACCEPTED);
/// ```
#[derive(Clone, Debug)]
pub struct TextResponse<B>(pub B);

/// An HTML response with `text/html; charset=utf-8` and a 200 status.
///
/// Pass already-rendered HTML as any owned or streaming body implementing
/// `Into<Body>`. This type does not render templates or escape HTML.
///
/// ```rust
/// use kavi::http::{IntoResponse, header, response::HtmlResponse};
///
/// let response = HtmlResponse("<h1>Hello</h1>").into_response();
/// assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
/// ```
#[derive(Clone, Debug)]
pub struct HtmlResponse<B>(pub B);

/// A binary response with `application/octet-stream` and a 200 status.
///
/// The value may be any owned or streaming body implementing `Into<Body>`.
#[derive(Clone, Debug)]
pub struct BinaryResponse<B>(pub B);

/// A JSON response with `application/json` and a 200 status.
///
/// Serialization happens during conversion, so the value may borrow application
/// data while the returned response body owns its encoded bytes. This type is
/// separate from the request-body extractor [`super::extract::Json`].
///
/// [`IntoResponse`] renders serialization failures as a generic 500. Status
/// tuples preserve that failure status. Use [`Self::try_into_response`] to
/// inspect or report the error locally instead.
///
/// ```rust
/// use kavi::http::{IntoResponse, StatusCode, response::JsonResponse};
///
/// let response = (StatusCode::CREATED, JsonResponse(vec![1, 2, 3])).into_response();
/// assert_eq!(response.status(), StatusCode::CREATED);
/// ```
#[derive(Clone, Debug)]
pub struct JsonResponse<T>(pub T);

impl<T: serde::Serialize> JsonResponse<T> {
    /// Serializes the value into an owned response body.
    ///
    /// # Errors
    /// Returns an internal framework error retaining the [`serde_json::Error`]
    /// source when serialization fails.
    pub fn try_into_response(self) -> Result<Response, Error> {
        let bytes = serde_json::to_vec(&self.0).map_err(Error::internal)?;
        Ok(with_content_type(Body::from(bytes), JSON))
    }
}

/// A response with no content.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoContent;

/// A redirect to another location.
///
/// The location is validated as a header value when the redirect is built, so
/// construction reports an invalid location as [`Error`].
#[derive(Clone, Debug)]
pub struct Redirect {
    location: HeaderValue,
    status: StatusCode,
}

impl Redirect {
    /// Builds a `302 Found` redirect.
    ///
    /// # Errors
    /// Returns an internal framework error if the location is not a valid
    /// header value.
    #[inline]
    pub fn to(location: impl AsRef<str>) -> Result<Self, Error> {
        Self::with_status(StatusCode::FOUND, location)
    }

    /// Builds a `303 See Other` redirect.
    ///
    /// # Errors
    /// Returns an internal framework error if the location is not a valid
    /// header value.
    #[inline]
    pub fn see_other(location: impl AsRef<str>) -> Result<Self, Error> {
        Self::with_status(StatusCode::SEE_OTHER, location)
    }

    /// Builds a `307 Temporary Redirect` redirect, which preserves the method.
    ///
    /// # Errors
    /// Returns an internal framework error if the location is not a valid
    /// header value.
    #[inline]
    pub fn temporary(location: impl AsRef<str>) -> Result<Self, Error> {
        Self::with_status(StatusCode::TEMPORARY_REDIRECT, location)
    }

    /// Builds a `308 Permanent Redirect` redirect, which preserves the method.
    ///
    /// For `301 Moved Permanently`, use [`Self::with_status`] with
    /// [`StatusCode::MOVED_PERMANENTLY`].
    ///
    /// # Errors
    /// Returns an internal framework error if the location is not a valid
    /// header value.
    #[inline]
    pub fn permanent(location: impl AsRef<str>) -> Result<Self, Error> {
        Self::with_status(StatusCode::PERMANENT_REDIRECT, location)
    }

    /// Builds a redirect with an explicit status.
    ///
    /// # Errors
    /// Returns an internal framework error if the location is not a valid
    /// header value.
    #[inline]
    pub fn with_status(status: StatusCode, location: impl AsRef<str>) -> Result<Self, Error> {
        let location = HeaderValue::from_str(location.as_ref()).map_err(Error::internal)?;
        Ok(Self { location, status })
    }

    /// Returns the validated `Location` header value.
    #[inline]
    pub fn location(&self) -> &HeaderValue {
        &self.location
    }

    /// Returns the redirect status.
    #[inline]
    pub fn status_code(&self) -> StatusCode {
        self.status
    }
}

fn with_content_type(body: Body, content_type: &'static str) -> Response {
    build(body, StatusCode::OK, Some(content_type))
}

fn build(body: Body, status: StatusCode, content_type: Option<&'static str>) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    strip_forbidden_content_type(&mut response);
    response
}

/// Removes a content type from statuses that must not carry one.
///
/// RFC 9110 forbids a `Content-Type` on informational and `204` responses.
/// `205` and `304` are treated the same way
fn strip_forbidden_content_type(response: &mut Response) {
    if forbids_content_type(response.status()) {
        response.headers_mut().remove(CONTENT_TYPE);
    }
}

/// Returns whether a status forbids a content type.
///
/// RFC 9110 forbids a `Content-Type` on informational and `204` responses.
/// `205` and `304` are treated the same way
pub(crate) fn forbids_content_type(status: StatusCode) -> bool {
    status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::RESET_CONTENT
        || status == StatusCode::NOT_MODIFIED
}

impl IntoResponse for Response {
    #[inline]
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for String {
    #[inline]
    fn into_response(self) -> Response {
        TextResponse(self).into_response()
    }
}

impl IntoResponse for Cow<'static, str> {
    #[inline]
    fn into_response(self) -> Response {
        match self {
            Cow::Borrowed(value) => TextResponse(value).into_response(),
            Cow::Owned(value) => TextResponse(value).into_response(),
        }
    }
}

impl IntoResponse for &'static str {
    #[inline]
    fn into_response(self) -> Response {
        TextResponse(self).into_response()
    }
}

impl IntoResponse for bytes::Bytes {
    #[inline]
    fn into_response(self) -> Response {
        BinaryResponse(self).into_response()
    }
}

impl IntoResponse for Vec<u8> {
    #[inline]
    fn into_response(self) -> Response {
        BinaryResponse(self).into_response()
    }
}

impl IntoResponse for Body {
    /// Returns the body unchanged and sets no content type.
    #[inline]
    fn into_response(self) -> Response {
        Response::new(self)
    }
}

impl IntoResponse for () {
    #[inline]
    fn into_response(self) -> Response {
        build(Body::empty(), StatusCode::OK, Some(HTML))
    }
}

impl IntoResponse for StatusCode {
    #[inline]
    fn into_response(self) -> Response {
        build(Body::empty(), self, Some(HTML))
    }
}

impl IntoResponse for NoContent {
    #[inline]
    fn into_response(self) -> Response {
        build(Body::empty(), StatusCode::NO_CONTENT, Some(HTML))
    }
}

impl IntoResponse for Redirect {
    #[inline]
    fn into_response(self) -> Response {
        let mut response = build(Body::empty(), self.status, Some(HTML));
        response.headers_mut().insert(LOCATION, self.location);
        response
    }
}

impl<B: Into<Body>> IntoResponse for TextResponse<B> {
    #[inline]
    fn into_response(self) -> Response {
        with_content_type(self.0.into(), TEXT)
    }
}

impl<B: Into<Body>> IntoResponse for HtmlResponse<B> {
    #[inline]
    fn into_response(self) -> Response {
        with_content_type(self.0.into(), HTML)
    }
}

impl<B: Into<Body>> IntoResponse for BinaryResponse<B> {
    #[inline]
    fn into_response(self) -> Response {
        with_content_type(self.0.into(), BINARY)
    }
}

impl<T: serde::Serialize> IntoResponse for JsonResponse<T> {
    fn into_response(self) -> Response {
        match self.try_into_response() {
            Ok(response) => response,
            Err(error) => {
                let mut response = error.into_response();
                response.extensions_mut().insert(ConversionFailure);
                response
            }
        }
    }
}

impl<T: IntoResponse> IntoResponse for (StatusCode, T) {
    #[inline]
    fn into_response(self) -> Response {
        let (status, value) = self;
        let mut response = value.into_response();
        if response.extensions().get::<ConversionFailure>().is_none() {
            *response.status_mut() = status;
        }
        strip_forbidden_content_type(&mut response);
        response
    }
}
