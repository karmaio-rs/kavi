//! HTTP messages, streaming bodies, typed extraction, and response construction.
//!
//! [`Request`] and [`Response`] use Kavi's canonical [`Body`]. Import
//! [`RequestExt`] alongside [`Request`] to access metadata and run extractors.
//! [`extract`] contains extractor traits, values, and their typed errors;
//! [`response`] contains helpers for constructing responses. [`cookies`]
//! provides Biscotti's server-side cookie API. [`ResponseExt`] provides access
//! to the outgoing cookie store; streaming messages use [`Body`].
//! [`multipart`] provides streaming form uploads with typed errors and limits.

mod body;
pub mod cookies;
pub mod extract;
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
pub use request::{ConnectionInfo, RequestExt, RoutingInfo};
pub use response::ResponseExt;
