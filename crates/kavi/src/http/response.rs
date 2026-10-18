//! Helpers for constructing concrete HTTP responses.
//!
//! [`json`] and redirects preserve construction failures as [`crate::Error`].
//! [`html`] accepts already-rendered HTML.

use http::{
    HeaderValue, StatusCode,
    header::{CONTENT_TYPE, LOCATION},
};

use crate::{
    Error,
    http::{Body, Response},
};

#[cfg(feature = "cookies")]
use super::cookies::{Processor, ResponseCookie, ResponseCookies};

/// Response extension helpers.
///
/// Import this trait alongside [`Response`] to access queued and immediate cookie
/// operations. Available with the `cookies` feature.
#[cfg(feature = "cookies")]
pub trait ResponseExt {
    /// Borrows the outgoing store, if one has been initialized.
    ///
    /// Response construction does not serialize this store into headers.
    /// CookieMiddleware consumes this store on the way out of its downstream chain.
    /// Without that middleware, queued cookies are not emitted.
    fn cookies(&self) -> Option<&ResponseCookies<'static>>;

    /// Borrows the outgoing store, initializing an empty one when necessary.
    ///
    /// The store belongs to this response and must not be shared across requests.
    /// Register CookieMiddleware outside every component that queues cookies in
    /// this scope. Use [`Self::set_cookie`] when no emission middleware is wanted.
    fn cookies_mut(&mut self) -> &mut ResponseCookies<'static>;

    /// Processes and appends one cookie immediately, without emission middleware.
    ///
    /// Accepts a response cookie, a name/value pair, or a removal cookie. Uses
    /// the supplied processor for encoding, signing, or encryption. Existing
    /// Set-Cookie headers and the pending queue are unchanged. Repeated calls
    /// append distinct header fields rather than replacing earlier ones.
    ///
    /// # Errors
    /// Returns an invalid-header error if the processed cookie cannot be encoded
    /// as an HTTP header. On failure no header is appended.
    fn set_cookie<'c>(
        &mut self,
        cookie: impl Into<ResponseCookie<'c>>,
        processor: &Processor,
    ) -> Result<(), http::header::InvalidHeaderValue>;
}

#[cfg(feature = "cookies")]
impl ResponseExt for Response {
    #[inline]
    fn cookies(&self) -> Option<&ResponseCookies<'static>> {
        self.extensions().get()
    }

    #[inline]
    fn cookies_mut(&mut self) -> &mut ResponseCookies<'static> {
        self.extensions_mut().get_or_insert_default()
    }

    fn set_cookie<'c>(
        &mut self,
        cookie: impl Into<ResponseCookie<'c>>,
        processor: &Processor,
    ) -> Result<(), http::header::InvalidHeaderValue> {
        let value = processor.process_outgoing(cookie.into()).to_string();
        let value = HeaderValue::from_str(&value)?;
        self.headers_mut().append(http::header::SET_COOKIE, value);
        Ok(())
    }
}

/// Creates an empty response with the supplied HTTP status.
#[inline]
pub fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

/// Creates a plain-text response from an owned or streaming body.
///
/// Sets `Content-Type: text/plain; charset=utf-8` and a 200 status. Strings,
/// static string slices, bytes, and existing Kavi bodies can be passed directly.
#[inline]
pub fn text<T: Into<Body>>(value: T) -> Response {
    with_content_type(value.into(), "text/plain; charset=utf-8")
}

/// Creates an HTML response from an owned or streaming body.
///
/// Sets `Content-Type: text/html; charset=utf-8` and a 200 status. Pass rendered
/// HTML produced by application code or a template renderer.
#[inline]
pub fn html<T: Into<Body>>(value: T) -> Response {
    with_content_type(value.into(), "text/html; charset=utf-8")
}

/// Creates a binary response from an owned or streaming body.
///
/// Sets `Content-Type: application/octet-stream` and a 200 status.
#[inline]
pub fn bytes<T: Into<Body>>(value: T) -> Response {
    with_content_type(value.into(), "application/octet-stream")
}

/// Serializes a value into a JSON response with a 200 status.
///
/// Sets `Content-Type: application/json`. A borrowed serializable value may be
/// passed to avoid transferring ownership of application data.
///
/// # Errors
///
/// Returns an internal framework error retaining the [`serde_json::Error`] source
/// when serialization fails. Handle the error locally before returning a Response.
pub fn json<T: serde::Serialize>(value: T) -> Result<Response, Error> {
    let bytes = serde_json::to_vec(&value).map_err(Error::internal)?;
    Ok(with_content_type(Body::from(bytes), "application/json"))
}

/// Creates an empty 302 Found response with a validated `Location` header.
///
/// # Errors
///
/// Returns an internal framework error if the location is not a valid header value.
#[inline]
pub fn redirect<T: AsRef<str>>(location: T) -> Result<Response, Error> {
    redirect_with_status(location.as_ref(), StatusCode::FOUND)
}

/// Creates an empty 301 Moved Permanently response with a validated `Location` header.
///
/// # Errors
///
/// Returns an internal framework error if the location is not a valid header value.
#[inline]
pub fn redirect_permanent<T: AsRef<str>>(location: T) -> Result<Response, Error> {
    redirect_with_status(location.as_ref(), StatusCode::MOVED_PERMANENTLY)
}

/// Creates an empty 307 Temporary Redirect response with a validated `Location` header.
///
/// # Errors
///
/// Returns an internal framework error if the location is not a valid header value.
#[inline]
pub fn redirect_temporary<T: AsRef<str>>(location: T) -> Result<Response, Error> {
    redirect_with_status(location.as_ref(), StatusCode::TEMPORARY_REDIRECT)
}

fn with_content_type(body: Body, content_type: &'static str) -> Response {
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn redirect_with_status(location: &str, status: StatusCode) -> Result<Response, Error> {
    let location = HeaderValue::from_str(location).map_err(Error::internal)?;
    let mut response = empty(status);
    response.headers_mut().insert(LOCATION, location);
    Ok(response)
}
