use std::collections::HashMap;

use super::{RouteInfo, UrlError, matcher::Matcher};
use crate::{
    Error,
    dispatch::{ErasedController, ErasedMiddleware, Next, Terminal},
    http::{Body, HeaderValue, Method, Request, Response, RoutingInfo, StatusCode, Uri, header},
};

pub(super) struct Endpoint {
    pub(super) controller: Box<dyn ErasedController>,
    pub(super) chain: Vec<usize>,
}

/// Immutable method indexes, route metadata, and owned dispatch endpoints.
///
/// Calls borrow the table and may run concurrently on a local worker. Recognition
/// uses the original method and URI path before middleware; changing either in
/// middleware does not select another route. Routing never consumes or replays
/// the request body. Matching is case-sensitive, ignores the query, and preserves
/// repeated and trailing slashes.
///
/// Captures are percent-decoded once when evaluating structural candidates.
/// Constraint failures allow backtracking. Encoded slashes do not
/// change the matched path structure; malformed percent escapes remain literal.
/// Invalid decoded UTF-8 returns a fallible 400. Routing metadata is present only
/// for successfully selected routes with valid captures; misses and errors clear
/// metadata from any earlier pass.
/// Unchanged captures borrow an owned URI snapshot through byte ranges, so
/// middleware URI changes preserve their original values. Names and route labels
/// share one request-owned buffer. Other-method probes build no routing metadata.
///
/// Unmatched paths invoke the root fallback or return a fallible 404. When other
/// methods match, dispatch returns a fallible 405 with a sorted `Allow` header,
/// including implicit HEAD. OPTIONS requires explicit registration. Table
/// middleware wraps every outcome; group and route middleware wrap selected routes.
/// Merged-router middleware wraps its imported endpoints and imported fallback.
/// Middleware is registered on [`super::Router`] before building the table. Each
/// endpoint and fallback has one complete compiled chain. General routing errors
/// use the outer table's middleware chain.
#[must_use]
pub struct RoutingTable {
    pub(super) paths: HashMap<Method, Matcher>,
    pub(super) routes: Vec<RouteInfo>,
    pub(super) names: HashMap<String, usize>,
    pub(super) endpoints: Vec<Endpoint>,
    pub(super) middleware: Vec<Box<dyn ErasedMiddleware>>,
    pub(super) table_chain: Vec<usize>,
    pub(super) fallback: Option<Endpoint>,
}

impl RoutingTable {
    /// Returns route declarations in registration order, excluding the root fallback.
    #[inline]
    pub fn routes(&self) -> &[RouteInfo] {
        &self.routes
    }

    /// Generates a path using the full mounted pattern of a named route.
    ///
    /// # Errors
    /// Returns an error for an unknown name or invalid capture bindings.
    pub fn path_for(&self, name: &str, params: &[(&str, &str)]) -> Result<Uri, UrlError> {
        self.named_pattern(name)?.to_uri(params)
    }

    /// Generates a named route's absolute URL with an explicit HTTP(S) origin.
    ///
    /// # Errors
    /// Returns name, binding, origin, or URI construction errors.
    pub fn url_for(&self, name: &str, params: &[(&str, &str)], origin: &Uri) -> Result<Uri, UrlError> {
        self.named_pattern(name)?.to_url(params, origin)
    }

    fn named_pattern(&self, name: &str) -> Result<&super::Pattern, UrlError> {
        self.names
            .get(name)
            .map(|&id| &self.routes[id].pattern)
            .ok_or_else(|| UrlError::UnknownRoute { name: name.to_owned() })
    }

    /// Recognizes and dispatches one owned request through a consuming middleware chain.
    ///
    /// HEAD keeps its method for middleware and controllers. Successful responses
    /// lose their body after table middleware returns; a known body size supplies
    /// Content-Length when absent and permitted by the status. An outer rendering
    /// or transport boundary must enforce HEAD semantics if it renders an error
    /// or replaces the response.
    ///
    /// # Errors
    /// Returns 400 for invalid decoded capture UTF-8, 404 for unmatched paths
    /// without a fallback, 405 for unsupported methods, or middleware/controller errors.
    pub async fn handle(&self, mut request: Request) -> Result<Response, Error> {
        let head = request.method() == Method::HEAD;
        let (chain, terminal) = match self.select(&mut request) {
            Ok((chain, controller)) => (chain, Terminal::Controller(controller)),
            Err(error) => (self.table_chain.as_slice(), Terminal::Error(error)),
        };
        let mut response = Next::new(&self.middleware, chain, terminal).run(request).await?;

        if head {
            let status = response.status();
            if !status.is_informational()
                && status != StatusCode::NO_CONTENT
                && status != StatusCode::NOT_MODIFIED
                && !response.headers().contains_key(header::CONTENT_LENGTH)
                && !response.headers().contains_key(header::TRANSFER_ENCODING)
                && let Some(length) = response.body().size_hint().exact()
            {
                response
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            }
            *response.body_mut() = Body::empty();
        }

        Ok(response)
    }

    fn select(&self, request: &mut Request) -> Result<(&[usize], &dyn ErasedController), Error> {
        request.extensions_mut().remove::<RoutingInfo>();
        let path = request.uri().path();

        if path.starts_with('/') {
            if let Some(paths) = self.paths.get(request.method())
                && let Some(matched) = paths
                    .at(path, &self.routes)
                    .map_err(|source| Error::with_status(StatusCode::BAD_REQUEST, source))?
            {
                let info = &self.routes[matched.id];
                let endpoint = &self.endpoints[matched.id];
                let metadata = RoutingInfo::new(
                    request.uri(),
                    info.path(),
                    info.name.as_deref(),
                    info.pattern.captures.iter().map(|capture| capture.name.as_str()),
                    matched.params,
                );
                request.extensions_mut().insert(metadata);
                return Ok((&endpoint.chain, &*endpoint.controller));
            }

            let mut allowed = Vec::new();
            for (method, paths) in &self.paths {
                if method == request.method() {
                    continue;
                }
                if paths
                    .matches(path, &self.routes)
                    .map_err(|source| Error::with_status(StatusCode::BAD_REQUEST, source))?
                {
                    allowed.push(method.as_str());
                }
            }

            if !allowed.is_empty() {
                allowed.sort_unstable();
                let mut error = Error::status(StatusCode::METHOD_NOT_ALLOWED);
                error.headers_mut().insert(
                    header::ALLOW,
                    HeaderValue::try_from(allowed.join(", ")).expect("HTTP methods are valid header tokens"),
                );
                return Err(error);
            }
        }

        match &self.fallback {
            Some(endpoint) => Ok((&endpoint.chain, &*endpoint.controller)),
            None => Err(Error::status(StatusCode::NOT_FOUND)),
        }
    }
}
