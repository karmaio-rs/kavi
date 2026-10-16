use http::uri::Scheme;
use serde::de::DeserializeOwned;
use std::{
    net::{IpAddr, SocketAddr},
    ops::Range,
};

use crate::routing::{Param, RouteParams};

use super::{
    MetadataError, Request, Uri,
    cookies::{self, CookieError, RequestCookies},
    extract::{FromRequestBody, FromRequestHead, ParamError, QueryError},
    metadata::RequestMetadata,
};

/// Transport-level connection metadata.
///
/// Supplied from the accepted socket by [`super::from_vakya`] and retained in
/// [`http::Extensions`]. Proxy metadata does not change these transport facts.
/// Accessed via [`RequestExt`] methods. The required `scheme` field must reflect
/// the actual transport rather than a request URI or forwarding header.
#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    /// The remote peer's socket address.
    pub remote_addr: SocketAddr,
    /// The local socket address.
    pub local_addr: SocketAddr,
    /// The actual accepted transport's scheme, independent of proxy headers.
    pub scheme: Scheme,
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
/// Service-aware extraction receives a separate explicit context. HTTP extensions
/// remain the request-local metadata store and retain their upstream insertion
/// bounds. Worker-local resources can be acquired as extractor outputs; separate
/// local request storage is deferred.
/// The HTTP boundary captures origin metadata before rewriting. Synthetic
/// requests can call [`Self::capture_metadata`] explicitly; routing alone does
/// not initialize it. Effective metadata storage remains internal.
#[allow(async_fn_in_trait)]
pub trait RequestExt {
    /// Returns the remote peer's socket address, if known.
    fn remote_addr(&self) -> Option<&SocketAddr>;

    /// Returns the local socket address, if known.
    fn local_addr(&self) -> Option<&SocketAddr>;

    /// Returns the effective scheme, honoring explicitly trusted captured metadata.
    ///
    /// Before capture, uses the target's scheme or the actual transport scheme.
    /// Forwarding headers alone never affect this result.
    fn scheme(&self) -> Option<&Scheme>;

    /// Returns the actual transport's scheme without proxy overrides.
    fn transport_scheme(&self) -> Option<&Scheme>;

    /// Returns the effective authority from captured metadata, or the target URI.
    fn authority(&self) -> Option<&http::uri::Authority>;

    /// Returns the original target captured before rewriting, or the current URI if uncaptured.
    fn original_uri(&self) -> &Uri;

    /// Returns the effective client IP, or the actual socket peer when uncaptured.
    fn client_ip(&self) -> Option<IpAddr>;

    /// Returns the captured HTTP(S) origin, if scheme and authority are both known.
    fn origin(&self) -> Option<Uri>;

    /// Captures the original URI and initial origin once, before request rewriting.
    ///
    /// Existing metadata, including trusted proxy updates, is preserved.
    /// This does not initialize application state or per-request controls.
    /// [`super::from_vakya`] captures it at the transport boundary;
    /// [`super::TrustedProxies`] middleware captures it when missing. Bare routing
    /// leaves capture and validation to the caller or relevant middleware.
    ///
    /// # Errors
    /// Returns [`MetadataError`] for ambiguous or malformed origin metadata.
    fn capture_metadata(&mut self) -> Result<(), MetadataError>;

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

    /// Parses incoming cookies using the request's configured or default processor.
    ///
    /// The collection borrows the request headers where possible and preserves
    /// duplicate names. Middleware can install a [`cookies::Processor`] in request
    /// extensions to establish the effective cookie policy. Outgoing cookies are
    /// held separately and must be processed with that same policy.
    ///
    /// # Errors
    /// Returns [`CookieError`] when a header is unreadable or cookie parsing,
    /// decoding, or cryptographic processing fails.
    fn cookies(&self) -> Result<RequestCookies<'_>, CookieError>;

    /// Runs an asynchronous metadata extractor.
    ///
    /// The request is borrowed immutably. Its head, extensions, and body remain
    /// in place through success, error, unwinding, and cancellation.
    /// Outputs may borrow request metadata. The context is a static unit value.
    ///
    /// # Errors
    /// Returns the error produced by the extractor.
    async fn extract_head<'r, E>(&'r self) -> Result<E, E::Error>
    where
        E: FromRequestHead<'r, 'static>;

    /// Runs an asynchronous metadata extractor with an explicit service or context.
    ///
    /// The context stays outside the request. Outputs may borrow request metadata,
    /// the context, or both. Independent lifetimes let context-only outputs avoid
    /// prolonging the request borrow; Rust enforces any relationships imposed by
    /// the extractor. The head, extensions, and body remain in place on
    /// success, error, unwinding, and cancellation. Neither context nor future
    /// requires `Clone`, `Send`, `Sync`, or `'static`.
    ///
    /// # Errors
    /// Returns the error produced by the extractor.
    async fn extract_head_with<'r, 's, E, S>(&'r self, context: &'s S) -> Result<E, E::Error>
    where
        E: FromRequestHead<'r, 's, S>,
        S: ?Sized;

    /// Runs a body extractor.
    ///
    /// When first polled, this replaces the body with an empty body and transfers
    /// the remaining body to the extractor. The head and extensions stay in place
    /// on success, error, unwinding, and cancellation. The consumed body is never
    /// replayed. Dropping the future before polling leaves the request intact.
    /// Outputs may borrow request metadata. The context is a static unit value.
    ///
    /// # Errors
    /// Returns the error produced by the extractor.
    async fn extract_body<'r, E>(&'r mut self) -> Result<E, E::Error>
    where
        E: FromRequestBody<'r, 'static>;

    /// Runs a body extractor with an explicit service or context.
    ///
    /// Body ownership and cancellation follow [`Self::extract_body`]. The context
    /// stays outside the request and is borrowed independently of its metadata.
    /// Outputs may borrow metadata or context and must own their body-derived data.
    /// Neither context nor future requires `Clone`, `Send`, `Sync`, or `'static`.
    ///
    /// # Errors
    /// Returns the error produced by the extractor.
    async fn extract_body_with<'r, 's, E, S>(&'r mut self, context: &'s S) -> Result<E, E::Error>
    where
        E: FromRequestBody<'r, 's, S>,
        S: ?Sized;
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
        match self.extensions().get::<RequestMetadata>() {
            Some(metadata) => metadata.scheme(),
            None => self.uri().scheme().or_else(|| self.transport_scheme()),
        }
    }

    #[inline]
    fn transport_scheme(&self) -> Option<&Scheme> {
        self.extensions().get::<ConnectionInfo>().map(|info| &info.scheme)
    }

    #[inline]
    fn authority(&self) -> Option<&http::uri::Authority> {
        match self.extensions().get::<RequestMetadata>() {
            Some(metadata) => metadata.authority(),
            None => self.uri().authority(),
        }
    }

    #[inline]
    fn original_uri(&self) -> &Uri {
        self.extensions()
            .get::<RequestMetadata>()
            .map_or_else(|| self.uri(), RequestMetadata::original_uri)
    }

    #[inline]
    fn client_ip(&self) -> Option<IpAddr> {
        match self.extensions().get::<RequestMetadata>() {
            Some(metadata) => metadata.client_ip(),
            None => self.remote_addr().map(SocketAddr::ip),
        }
    }

    #[inline]
    fn origin(&self) -> Option<Uri> {
        self.extensions().get::<RequestMetadata>()?.origin()
    }

    // TODO: Make this automatic when building the app server
    #[inline]
    fn capture_metadata(&mut self) -> Result<(), MetadataError> {
        if self.extensions().get::<RequestMetadata>().is_none() {
            let metadata = RequestMetadata::capture(self)?;
            self.extensions_mut().insert(metadata);
        }
        Ok(())
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
    async fn extract_head<'r, E>(&'r self) -> Result<E, E::Error>
    where
        E: FromRequestHead<'r, 'static>,
    {
        self.extract_head_with::<E, _>(&()).await
    }

    #[inline]
    async fn extract_head_with<'r, 's, E, S>(&'r self, context: &'s S) -> Result<E, E::Error>
    where
        E: FromRequestHead<'r, 's, S>,
        S: ?Sized,
    {
        E::from_head(self, context).await
    }

    #[inline]
    async fn extract_body<'r, E>(&'r mut self) -> Result<E, E::Error>
    where
        E: FromRequestBody<'r, 'static>,
    {
        self.extract_body_with::<E, _>(&()).await
    }

    #[inline]
    async fn extract_body_with<'r, 's, E, S>(&'r mut self, context: &'s S) -> Result<E, E::Error>
    where
        E: FromRequestBody<'r, 's, S>,
        S: ?Sized,
    {
        let body = std::mem::take(self.body_mut());
        E::from_body(self, body, context).await
    }
}
