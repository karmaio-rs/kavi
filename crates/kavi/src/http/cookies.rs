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
//! application-configured [`Processor`] for incoming and outgoing cookies.
//!
//! [`super::RequestExt::cookies`] uses a [`Processor`] from request extensions,
//! or a shared default processor when
//! none is installed. The application boundary can install its configured
//! processor with `request.extensions_mut().insert(processor.clone())`.
//! Controllers do not need to pass configuration when reading cookies.
//!
//! ```text
//! let cookies = request.cookies()?;
//! ```

pub use biscotti::{
    Expiration, Key, Processor, ProcessorConfig, RemovalCookie, RequestCookie, RequestCookies, ResponseCookie,
    ResponseCookieId, ResponseCookies, SameSite, config, errors, request, response, time,
};

use super::Request;
use http::{header::COOKIE, header::ToStrError};
use std::sync::OnceLock;

/// Resolves the request's processor without rebuilding the default per request.
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
    let mut cookies = RequestCookies::new();
    let processor = processor(request);
    for value in request.headers().get_all(COOKIE) {
        cookies.extend_from_header(value.to_str()?, processor)?;
    }
    Ok(cookies)
}
