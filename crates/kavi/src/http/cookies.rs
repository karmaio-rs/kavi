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
//! Available with the `cookies` feature. Keep the configured processor in the
//! typed dependency root and pass it to [`super::RequestExt::cookies`] for lazy
//! parsing. Register [`crate::http::middleware::CookieMiddleware`] to emit queued
//! response cookies, or use [`super::ResponseExt::set_cookie`] to append them
//! immediately. Neither the processor nor parsed cookies live in request
//! extensions. Application and HTTP servers perform no cookie processing.

pub use biscotti::{
    Expiration, Key, Processor, ProcessorConfig, RemovalCookie, RequestCookie, RequestCookies, ResponseCookie,
    ResponseCookieId, ResponseCookies, SameSite, config, errors, request, response, time,
};

use super::Request;
use http::{header::COOKIE, header::ToStrError};

/// Encodes and validates all generated cookies before appending any headers.
///
/// Called by CookieMiddleware after its downstream chain returns. The response's
/// store is consumed, so nested middleware cannot emit the same queue twice.
pub(crate) fn emit(
    response: &mut super::Response,
    processor: &Processor,
) -> Result<(), http::header::InvalidHeaderValue> {
    let Some(cookies) = response.extensions_mut().remove::<ResponseCookies<'static>>() else {
        return Ok(());
    };
    let values = cookies
        .header_values(processor)
        .map(|value| http::HeaderValue::from_str(&value))
        .collect::<Result<Vec<_>, _>>()?;
    for value in values {
        response.headers_mut().append(http::header::SET_COOKIE, value);
    }
    Ok(())
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

/// Parses every Cookie header using the supplied processor.
///
/// The resulting collection borrows the header strings where possible. All
/// values with the same name are retained in header order; [`RequestCookies::get`]
/// returns the first, while [`RequestCookies::get_all`] exposes all of them.
/// No Cookie headers yields an empty collection. Request headers are unchanged.
/// Signing and encryption use the supplied configuration.
///
/// # Errors
///
/// Returns [`CookieError::Header`] for an unreadable header or
/// [`CookieError::Parse`] for malformed syntax, invalid decoded UTF-8, or failed
/// cryptographic verification. No partially parsed collection is returned.
pub fn from_headers<'a>(request: &'a Request, processor: &Processor) -> Result<RequestCookies<'a>, CookieError> {
    let mut cookies = RequestCookies::new();
    for value in request.headers().get_all(COOKIE) {
        cookies.extend_from_header(value.to_str()?, processor)?;
    }
    Ok(cookies)
}
