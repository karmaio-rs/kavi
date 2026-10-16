//! Server-side cookies backed by Biscotti.
//!
//! Incoming [`RequestCookies`] and outgoing [`ResponseCookies`] are separate
//! collections. Request cookies preserve duplicate names; response cookies are
//! identified by name, path, and domain. Biscotti's API, including configuration,
//! cryptographic keys, rotation rules, iterators, and time types, is re-exported
//! here without changing its processing behavior.
//!
//! [`ProcessorConfig::default`] enables percent-encoding and decoding. Signing
//! and encryption are opt-in through [`config::CryptoRule`]. Use the same
//! configured [`Processor`] for incoming and outgoing cookies.
//!
//! [`super::RequestExt::cookies`] uses a [`Processor`] from request extensions,
//! or a shared default when none is installed. Cookie middleware can own the
//! configured processor, install it in each request's extensions, and use the
//! same policy to process outgoing [`ResponseCookies`]. Controllers read cookies
//! through the request without receiving configuration as an application dependency.
//! Applications requiring signing or encryption must install their processor;
//! the default only performs percent-encoding and decoding.

pub use biscotti::{
    Expiration, Key, Processor, ProcessorConfig, RemovalCookie, RequestCookie, RequestCookies, ResponseCookie,
    ResponseCookieId, ResponseCookies, SameSite, config, errors, request, response, time,
};

use super::Request;
use http::{header::COOKIE, header::ToStrError};
use std::sync::OnceLock;

/// Resolves the request's effective cookie policy without rebuilding the default.
pub(crate) fn processor(request: &Request) -> &Processor {
    static DEFAULT_PROCESSOR: OnceLock<Processor> = OnceLock::new();
    request
        .extensions()
        .get::<Processor>()
        .unwrap_or_else(|| DEFAULT_PROCESSOR.get_or_init(|| ProcessorConfig::default().into()))
}

/// A failure while reading and processing incoming request cookies.
#[derive(Debug, thiserror::Error)]
pub enum CookieError {
    /// A Cookie header contains bytes that cannot be read as header text.
    #[error("invalid Cookie header")]
    Header(#[from] ToStrError),
    /// Cookie syntax, percent-decoding, or cryptographic processing failed.
    #[error("failed to process request cookies")]
    Parse(#[from] errors::ParseError),
}

/// Parses every Cookie header using the request's configured or default processor.
///
/// The resulting collection borrows the header strings where possible. All
/// values with the same name are retained in header order; [`RequestCookies::get`]
/// returns the first, while [`RequestCookies::get_all`] exposes all of them.
/// No Cookie headers yields an empty collection. Request headers are unchanged.
///
/// # Errors
///
/// Returns [`CookieError::Header`] for an unreadable header or
/// [`CookieError::Parse`] for malformed syntax, invalid decoded UTF-8, or failed
/// cryptographic verification. No partially parsed collection is returned.
pub fn from_headers(request: &Request) -> Result<RequestCookies<'_>, CookieError> {
    from_headers_with(request, processor(request))
}

/// Parses every Cookie header using an explicitly supplied processor.
///
/// The collection borrows the request's headers, not the processor. Signing and
/// encryption use the supplied configuration. This low-level override does not
/// change the processor in request extensions or the outgoing cookie policy.
///
/// # Errors
/// Returns [`CookieError`] for unreadable headers or failed cookie processing.
pub fn from_headers_with<'a>(request: &'a Request, processor: &Processor) -> Result<RequestCookies<'a>, CookieError> {
    let mut cookies = RequestCookies::new();
    for value in request.headers().get_all(COOKIE) {
        cookies.extend_from_header(value.to_str()?, processor)?;
    }
    Ok(cookies)
}
