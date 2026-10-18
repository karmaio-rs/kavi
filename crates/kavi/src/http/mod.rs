//! HTTP messages, streaming bodies, typed extraction, and response construction.
//!
//! [`Request`] and [`Response`] use Kavi's canonical [`Body`]. Import
//! [`RequestExt`] alongside [`Request`] to access metadata and run extractors.
//! [`extract`] contains extractor traits, values, and their typed errors;
//! [`response`] contains helpers for constructing responses. The optional
//! `cookies` feature provides Biscotti cookie helpers and `ResponseExt` for
//! queued or immediate response cookies. Streaming messages use [`Body`].
//! [`multipart`] provides streaming form uploads with typed errors and limits.
//! [`middleware`] provides built-in HTTP request and response policies.
//! Extractors accept explicit service contexts and can return borrowed metadata
//! or services. [`Request`] remains the standard HTTP request alias. Its extensions
//! store compatible metadata; a separate store for worker-local request resources
//! is deferred.
//! [`crate::Application`] captures origin metadata before middleware;
//! [`crate::foundation::server`] supplies actual transport facts.
//! [`crate::http::middleware::TrustedProxies`] explicitly establishes which forwarding peers are trusted.
//! Metadata is read through [`RequestExt`]; its storage type is internal.
//! Pass synthetic requests through Application for the same initialization.
//! Routing does not capture or validate origins.

pub mod body;
#[cfg(feature = "cookies")]
pub mod cookies;
pub mod extract;
mod metadata;
pub mod middleware;
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
pub use body::{Body, BodyData};
pub use metadata::MetadataError;
pub(crate) use metadata::{RequestMetadata, apply_forwarded};
pub use request::{ConnectionInfo, RequestExt, RoutingInfo};
#[cfg(feature = "cookies")]
pub use response::ResponseExt;
