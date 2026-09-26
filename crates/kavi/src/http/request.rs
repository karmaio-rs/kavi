use http::uri::Scheme;
use serde::de::DeserializeOwned;
use std::net::SocketAddr;

use super::{
    Request,
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
/// The pattern, name, and decoded captures are owned by this request.
/// Cloning the metadata copies those values.
#[derive(Clone, Debug)]
pub struct RoutingInfo {
    path: String,
    name: Option<String>,
    params: Vec<(String, String)>,
}

impl RoutingInfo {
    pub(crate) fn new(path: String, name: Option<String>, params: Vec<(String, String)>) -> Self {
        Self { path, name, params }
    }

    /// Returns decoded path captures in recognition order.
    pub fn params(&self) -> &[(String, String)] {
        &self.params
    }

    /// Returns the full matched path pattern.
    pub fn matched_path(&self) -> &str {
        &self.path
    }

    /// Returns the fully qualified route name, when assigned.
    pub fn route_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the value of a single path parameter.
    ///
    /// # Errors
    ///
    /// Returns [`ParamError::Missing`] if no parameter with this name was captured,
    /// or [`ParamError::Deserialize`] if conversion into `T` fails.
    pub fn param<T: DeserializeOwned>(&self, name: &str) -> Result<T, ParamError> {
        self.params()
            .iter()
            .find(|(k, _)| k == name)
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
    fn route_params(&self) -> &[(String, String)];

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
    fn route_params(&self) -> &[(String, String)] {
        self.extensions()
            .get::<RoutingInfo>()
            .map(RoutingInfo::params)
            .unwrap_or(&[])
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
    async fn extract_head<E: FromRequestHead>(&self) -> Result<E, E::Error> {
        E::from_head(self).await
    }

    #[inline]
    async fn extract_body<E: FromRequestBody>(&mut self) -> Result<E, E::Error> {
        let body = std::mem::take(self.body_mut());
        E::from_body(self, body).await
    }
}
