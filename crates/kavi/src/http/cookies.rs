//! Server-side cookies backed by Biscotti.
//!
//! [`CookieJar`] reads request cookies and queues response cookies. Register
//! [`crate::http::middleware::CookieMiddleware`] outside every cookie-producing
//! handler and middleware in the scope. The dependency root owns one [`Processor`]
//! and exposes it through [`crate::dispatch::Provide`]. The middleware borrows
//! that processor when it emits. Handlers pass the same processor to
//! [`super::RequestExt::cookies`], or extract [`CookieJar`] with
//! [`super::RequestExt::extract_head_with`] from a root that provides it.
//!
//! Incoming and outgoing cookies stay separate. Request cookies preserve
//! duplicate names. Response cookies are identified by name, path, and domain.
//! [`ProcessorConfig::default`] percent-encodes and decodes. Signing and
//! encryption are opt-in through [`config::CryptoRule`].
//!
//! Opening a jar copies the `Cookie` header values and borrows the processor.
//! Parsing stays lazy: the copied headers are processed when the jar is read,
//! so the request stays available for body extraction while the jar is open.
//! Each read parses them again; reuse one collection for multiple lookups.
//! A request that never reads cookies does not fail closed on a malformed
//! `Cookie` header. Emission validates every generated header before appending
//! any, and it preserves existing `Set-Cookie` fields. Responses that return
//! before reaching this middleware are not emitted. Application and HTTP
//! servers do not process cookies. Application finalizes original HEAD
//! semantics after the chain.

pub use biscotti::{
    Expiration, Key, Processor, ProcessorConfig, RemovalCookie, RequestCookie, RequestCookies, ResponseCookie,
    ResponseCookieId, SameSite, config, time,
};

use http::{
    HeaderValue,
    header::{COOKIE, SET_COOKIE, ToStrError},
};

use crate::dispatch::Provide;

use super::{Request, RequestExt, Response, extract::FromRequestHead};

/// Request and response cookies for one handler call.
///
/// Obtain a jar with [`RequestExt::cookies`] or [`FromRequestHead`] through
/// [`RequestExt::extract_head_with`]. Both succeed only after
/// [`crate::http::middleware::CookieMiddleware`] has marked the request.
/// The borrowed processor must be the dependency root's processor, because
/// emission uses that same value. Opening the jar copies the `Cookie` header
/// values and leaves the request free, so body extraction can run while the
/// jar is open. Parsing is deferred until a read. The copy is a snapshot:
/// later changes to the request's headers are not observed.
pub struct CookieJar<'processor> {
    processor: &'processor Processor,
    headers: Vec<HeaderValue>,
}

impl<'processor> CookieJar<'processor> {
    /// Copies the request's `Cookie` headers and borrows the application processor.
    ///
    /// # Errors
    /// Returns [`CookieError::Disabled`] when this request did not pass through
    /// [`crate::http::middleware::CookieMiddleware`].
    #[inline]
    pub(crate) fn open(request: &Request, processor: &'processor Processor) -> Result<Self, CookieError> {
        if !is_enabled(request) {
            return Err(CookieError::Disabled);
        }
        Ok(Self {
            processor,
            headers: request.headers().get_all(COOKIE).into_iter().cloned().collect(),
        })
    }

    /// Returns the first request cookie with this name.
    ///
    /// Parses every `Cookie` header and returns the first matching cookie.
    /// Every call parses the copied headers again; use
    /// [`Self::request_cookies`] when reading more than one name.
    /// The cookie borrows this jar's copied header values.
    /// [`RequestCookies::get_all`] is available through
    /// [`Self::request_cookies`] when duplicate names matter.
    ///
    /// # Errors
    /// Returns [`CookieError::Header`] for an unreadable header or
    /// [`CookieError::Parse`] for malformed syntax, invalid decoded UTF-8, or
    /// failed cryptographic verification. No partial collection is returned.
    #[inline]
    pub fn get(&self, name: &str) -> Result<Option<RequestCookie<'_>>, CookieError> {
        Ok(self.request_cookies()?.get(name))
    }

    /// Parses every `Cookie` header into a collection.
    ///
    /// Every call parses the copied headers again, including any signing or
    /// encryption verification. The collection borrows this jar's copied
    /// header values; clone values that must outlive the jar.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::get`].
    pub fn request_cookies(&self) -> Result<RequestCookies<'_>, CookieError> {
        let mut cookies = RequestCookies::new();
        for value in &self.headers {
            cookies.extend_from_header(value.to_str()?, self.processor)?;
        }
        Ok(cookies)
    }

    /// Queues one response cookie for [`crate::http::middleware::CookieMiddleware`] to emit.
    ///
    /// Accepts a response cookie or a name/value pair. A later queue with the
    /// same name, path, and domain replaces the earlier one. The cookie must
    /// own its strings or borrow `'static` data, because the queue outlives
    /// this call.
    #[inline]
    pub fn add(&self, response: &mut Response, cookie: impl Into<ResponseCookie<'static>>) {
        queue_cookie(response, cookie.into());
    }

    /// Queues a removal cookie for [`crate::http::middleware::CookieMiddleware`] to emit.
    ///
    /// The removal is identified by name, path, and domain. Pass a
    /// [`RemovalCookie`] or a [`ResponseCookieId`].
    #[inline]
    pub fn remove(&self, response: &mut Response, cookie: impl Into<RemovalCookie<'static>>) {
        self.add(response, ResponseCookie::from(cookie.into()));
    }
}

impl<'request, 'processor, S> FromRequestHead<'request, 'processor, S> for CookieJar<'processor>
where
    S: Provide<Processor> + ?Sized,
{
    type Error = CookieError;

    #[inline]
    async fn from_head(request: &'request Request, context: &'processor S) -> Result<Self, Self::Error> {
        request.cookies(context.provide())
    }
}

/// A failure while reading request cookies or opening a jar without middleware.
#[derive(Debug, thiserror::Error)]
pub enum CookieError {
    /// [`crate::http::middleware::CookieMiddleware`] did not run for this request.
    #[error("cookie middleware is not registered for this request")]
    Disabled,
    /// A `Cookie` header contains bytes that cannot be read as header text.
    #[error("invalid Cookie header")]
    Header(#[from] ToStrError),
    /// Cookie syntax, percent-decoding, or cryptographic processing failed.
    #[error("failed to process request cookies")]
    Parse(#[from] biscotti::errors::ParseError),
}

/// Records that [`crate::http::middleware::CookieMiddleware`] will emit this request's cookie queue.
#[inline]
pub(crate) fn mark_enabled(request: &mut Request) {
    request.extensions_mut().insert(CookiesEnabled);
}

/// Reports whether [`crate::http::middleware::CookieMiddleware`] marked this request.
#[inline]
fn is_enabled(request: &Request) -> bool {
    request.extensions().get::<CookiesEnabled>().is_some()
}

/// Appends one owned response cookie to the queue the cookie middleware emits.
#[inline]
fn queue_cookie(response: &mut Response, cookie: ResponseCookie<'static>) {
    response
        .extensions_mut()
        .get_or_insert_with(PendingCookies::default)
        .0
        .insert(cookie);
}

/// Emits queued response cookies and removes the queue.
///
/// Existing `Set-Cookie` fields are preserved. An invalid generated header
/// discards the queue and returns the error without appending anything.
pub(crate) fn emit_cookies(
    response: &mut Response,
    processor: &Processor,
) -> Result<(), http::header::InvalidHeaderValue> {
    let Some(pending) = response.extensions_mut().remove::<PendingCookies>() else {
        return Ok(());
    };
    let values = pending
        .0
        .header_values(processor)
        .map(|value| HeaderValue::from_str(&value))
        .collect::<Result<Vec<_>, _>>()?;
    for value in values {
        response.headers_mut().append(SET_COOKIE, value);
    }
    Ok(())
}

/// Marker recorded by [`crate::http::middleware::CookieMiddleware`].
///
/// [`http::Extensions`] stores `Clone + Send + Sync` values. This marker meets
/// that bound and carries no processor; the middleware borrows that from the
/// dependency root when it emits.
#[derive(Clone, Copy)]
struct CookiesEnabled;

/// Owns the response cookies queued for the cookie middleware.
///
/// `Clone` is required by [`http::Extensions`] and copies the queued values.
#[derive(Clone, Default)]
struct PendingCookies(biscotti::ResponseCookies<'static>);
