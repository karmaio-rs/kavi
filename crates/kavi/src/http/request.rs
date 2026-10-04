use http::uri::Scheme;
use serde::de::DeserializeOwned;
use std::{net::SocketAddr, ops::Range};

use crate::routing::{Param, RouteParams};

use super::{
    Request,
    cookies::{self, CookieError, RequestCookies},
    extract::{FromRequestBody, FromRequestHead, ParamError, QueryError},
};

/// Transport-level connection metadata.
///
/// Inserted into [`http::Extensions`] by the Vakya → Kavi boundary adapter.
/// Accessed via [`RequestExt`] methods.
#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    /// The remote peer's socket address.
    pub remote_addr: SocketAddr,
    /// The local socket address.
    pub local_addr: SocketAddr,
}

/// Request routing metadata inserted before route middleware and the controller.
///
/// Captures borrow an owned snapshot of the original URI when decoding is
/// unnecessary. Encoded captures own their decoded strings. Route labels share
/// one owned buffer. Changing the request URI does not change these parameters.
/// Cloning copies labels and decoded strings and clones the URI snapshot.
#[derive(Clone, Debug)]
pub struct RoutingInfo {
    uri: Option<super::Uri>,
    labels: String,
    path: Range<usize>,
    name: Option<Range<usize>>,
    params: Vec<Param>,
}

impl RoutingInfo {
    pub(crate) fn new<'a>(
        uri: &super::Uri,
        path: &str,
        name: Option<&str>,
        names: impl ExactSizeIterator<Item = &'a str> + Clone,
        mut params: Vec<Param>,
    ) -> Self {
        debug_assert_eq!(names.len(), params.len());
        let uri = params.iter().any(Param::borrows_path).then(|| uri.clone());
        let capacity = path.len() + name.map_or(0, str::len) + names.clone().map(str::len).sum::<usize>();
        let mut labels = String::with_capacity(capacity);
        labels.push_str(path);
        let path = 0..labels.len();
        let name = name.map(|name| {
            let start = labels.len();
            labels.push_str(name);
            start..labels.len()
        });
        for (name, param) in names.zip(&mut params) {
            let start = labels.len();
            labels.push_str(name);
            param.name = start..labels.len();
        }
        Self {
            uri,
            labels,
            path,
            name,
            params,
        }
    }

    /// Returns decoded path captures in recognition order.
    #[inline]
    pub fn params(&self) -> RouteParams<'_> {
        RouteParams::new(
            self.uri.as_ref().map_or("", super::Uri::path),
            &self.labels,
            &self.params,
        )
    }

    /// Returns the full matched path pattern.
    #[inline]
    pub fn matched_path(&self) -> &str {
        &self.labels[self.path.clone()]
    }

    /// Returns the fully qualified route name, when assigned.
    #[inline]
    pub fn route_name(&self) -> Option<&str> {
        self.name.as_ref().map(|range| &self.labels[range.clone()])
    }

    /// Returns the value of a single path parameter.
    ///
    /// # Errors
    ///
    /// Returns [`ParamError::Missing`] if no parameter with this name was captured,
    /// or [`ParamError::Deserialize`] if conversion into `T` fails.
    pub fn param<T: DeserializeOwned>(&self, name: &str) -> Result<T, ParamError> {
        self.params()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
            .ok_or_else(|| ParamError::Missing { name: name.to_owned() })
            .and_then(|v| {
                T::deserialize(crate::support::serde::Scalar(v)).map_err(|e| ParamError::Deserialize {
                    name: name.to_owned(),
                    source: e,
                })
            })
    }
}

/// Request-specific metadata and extraction helpers for the standard HTTP request.
///
/// Import this trait alongside [`Request`] to access connection and routing metadata, parse query parameters, and run extractors.
#[allow(async_fn_in_trait)]
pub trait RequestExt {
    /// Returns the remote peer's socket address, if known.
    fn remote_addr(&self) -> Option<&SocketAddr>;

    /// Returns the local socket address, if known.
    fn local_addr(&self) -> Option<&SocketAddr>;

    /// Returns the connection scheme, if known.
    fn scheme(&self) -> Option<&Scheme>;

    /// Returns the matched route pattern after recognition.
    fn matched_route(&self) -> Option<&str>;

    /// Returns the fully qualified name of the selected route, if named.
    fn route_name(&self) -> Option<&str>;

    /// Returns the route parameters decoded exactly once by the router.
    ///
    /// Iterate borrowed pairs directly with `for (name, value) in request.route_params()`.
    /// Values remain those selected from the original URI, even after URI changes.
    fn route_params(&self) -> RouteParams<'_>;

    /// Deserializes a captured path parameter by name.
    ///
    /// # Errors
    /// Returns [`ParamError::Missing`] for an absent parameter, or
    /// [`ParamError::Deserialize`] when conversion fails.
    fn param<T: DeserializeOwned>(&self, name: &str) -> Result<T, ParamError>;

    /// Deserializes the query string into a typed value.
    ///
    /// # Errors
    /// Returns [`QueryError`] when deserialization fails.
    fn query<T: DeserializeOwned>(&self) -> Result<T, QueryError>;

    /// Parses incoming cookies using the request's configured cookie processor.
    ///
    /// The collection borrows the request headers where possible and preserves
    /// duplicate names. A [`cookies::Processor`] in request extensions overrides
    /// Biscotti's default processor. Outgoing cookies are held separately.
    ///
    /// # Errors
    /// Returns [`CookieError`] when a header is unreadable or cookie parsing,
    /// decoding, or cryptographic processing fails.
    fn cookies(&self) -> Result<RequestCookies<'_>, CookieError>;

    /// Runs an asynchronous metadata extractor.
    ///
    /// The request is borrowed immutably. Its head, extensions, and body remain
    /// in place through success, error, unwinding, and cancellation.
    ///
    /// # Errors
    /// Returns the error produced by the extractor.
    async fn extract_head<E: FromRequestHead>(&self) -> Result<E, E::Error>;

    /// Runs a body extractor.
    ///
    /// When first polled, this replaces the body with an empty body and transfers
    /// the remaining body to the extractor. The head and extensions stay in place
    /// on success, error, unwinding, and cancellation. The consumed body is never
    /// replayed. Dropping the future before polling leaves the request intact.
    ///
    /// # Errors
    /// Returns the error produced by the extractor.
    async fn extract_body<E: FromRequestBody>(&mut self) -> Result<E, E::Error>;
}

impl RequestExt for Request {
    #[inline]
    fn remote_addr(&self) -> Option<&SocketAddr> {
        self.extensions().get::<ConnectionInfo>().map(|info| &info.remote_addr)
    }

    #[inline]
    fn local_addr(&self) -> Option<&SocketAddr> {
        self.extensions().get::<ConnectionInfo>().map(|info| &info.local_addr)
    }

    #[inline]
    fn scheme(&self) -> Option<&Scheme> {
        self.uri().scheme()
    }

    #[inline]
    fn matched_route(&self) -> Option<&str> {
        self.extensions().get::<RoutingInfo>().map(RoutingInfo::matched_path)
    }

    #[inline]
    fn route_name(&self) -> Option<&str> {
        self.extensions().get::<RoutingInfo>().and_then(RoutingInfo::route_name)
    }

    #[inline]
    fn route_params(&self) -> RouteParams<'_> {
        self.extensions()
            .get::<RoutingInfo>()
            .map(RoutingInfo::params)
            .unwrap_or_default()
    }

    #[inline]
    fn param<T: DeserializeOwned>(&self, name: &str) -> Result<T, ParamError> {
        self.extensions()
            .get::<RoutingInfo>()
            .ok_or_else(|| ParamError::Missing { name: name.to_owned() })
            .and_then(|info| info.param(name))
    }

    #[inline]
    fn query<T: DeserializeOwned>(&self) -> Result<T, QueryError> {
        serde_qs::from_str(self.uri().query().unwrap_or("")).map_err(QueryError::from)
    }

    #[inline]
    fn cookies(&self) -> Result<RequestCookies<'_>, CookieError> {
        cookies::from_headers(self)
    }

    #[inline]
    async fn extract_head<E: FromRequestHead>(&self) -> Result<E, E::Error> {
        E::from_head(self).await
    }

    #[inline]
    async fn extract_body<E: FromRequestBody>(&mut self) -> Result<E, E::Error> {
        let body = std::mem::take(self.body_mut());
        E::from_body(self, body).await
    }
}
