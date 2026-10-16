//! HTTP messages, streaming bodies, typed extraction, and response construction.
//!
//! [`Request`] and [`Response`] use Kavi's canonical [`Body`]. Import
//! [`RequestExt`] alongside [`Request`] to access metadata and run extractors.
//! [`extract`] contains extractor traits, values, and their typed errors;
//! [`response`] contains helpers for constructing responses. [`cookies`]
//! provides Biscotti's server-side cookie API. [`ResponseExt`] provides access
//! to the outgoing cookie store; streaming messages use [`Body`].
//! [`multipart`] provides streaming form uploads with typed errors and limits.
//! Extractors accept explicit service contexts and can return borrowed metadata
//! or services. [`Request`] remains the standard HTTP request alias. Its extensions
//! store compatible metadata; a separate store for worker-local request resources
//! is deferred.
//! [`from_vakya`] captures socket and origin metadata;
//! [`TrustedProxies`] explicitly establishes which forwarding peers are trusted.
//! Metadata is read through [`RequestExt`]; its storage type is internal. Capture
//! synthetic requests with [`RequestExt::capture_metadata`] before rewriting or
//! use [`TrustedProxies`] middleware. Routing does not capture or validate origins.

mod body;
pub mod cookies;
pub mod extract;
mod metadata;
pub mod multipart;
pub mod response;

mod request;

pub use http::{Extensions, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version};
#[doc(no_inline)]
pub use http::{header, uri};

/// An HTTP request with Kavi's canonical streaming body.
pub type Request = http::Request<Body>;

/// An HTTP response with Kavi's canonical streaming body.
pub type Response = http::Response<Body>;

pub use http::request::{Builder as RequestBuilder, Parts as RequestParts};
pub use http::response::{Builder as ResponseBuilder, Parts as ResponseParts};

pub use crate::routing::RouteParams;
pub use body::Body;
pub use metadata::{MetadataError, TrustedProxies};
pub use request::{ConnectionInfo, RequestExt, RoutingInfo};
pub use response::ResponseExt;

/// Converts a Vakya request and captures its transport and origin metadata.
///
/// The serving boundary supplies facts from the accepted socket and transport.
/// Metadata is captured before pre-routing middleware. Transport controls,
/// connection supervision, error rendering and timeout enforcement remain the
/// serving boundary's responsibility.
///
/// # Errors
/// Returns [`MetadataError`] for ambiguous or malformed origin metadata.
// TODO: Remove this later and fold it into the app server
pub fn from_vakya(
    request: http::Request<vakya::body::Incoming>,
    connection: ConnectionInfo,
) -> Result<Request, MetadataError> {
    let mut request = request.map(Body::from_incoming);
    request.extensions_mut().insert(connection);
    request.capture_metadata()?;
    Ok(request)
}
