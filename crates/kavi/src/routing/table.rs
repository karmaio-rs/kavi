use std::{collections::HashMap, str::Utf8Error};

use super::{RouteInfo, UrlError, matcher::Matcher};
use crate::{
    dispatch::{ErasedController, ErasedMiddleware},
    http::{Method, RoutingInfo, Uri},
};

pub(super) struct Endpoint {
    pub(super) controller: Box<dyn ErasedController>,
    pub(super) chain: Vec<usize>,
}

pub(crate) struct SelectedEndpoint<'a> {
    pub(crate) controller: &'a dyn ErasedController,
    pub(crate) chain: &'a [usize],
    pub(crate) metadata: Option<RoutingInfo>,
}

/// Route-recognition failures; Application chooses their HTTP responses.
pub(crate) enum RecognitionError {
    InvalidCapture(Utf8Error),
    MethodNotAllowed(Vec<Method>),
    NotFound,
}

/// Immutable method indexes, route metadata, and owned dispatch endpoints.
///
/// The application asks the table to recognize the effective method and URI after
/// application middleware. It then runs the selected middleware and controller.
/// The table never invokes handlers, renders errors, or finalizes responses.
/// Changing the request in route middleware does not select another route.
/// Matching is case-sensitive, ignores the query, and preserves
/// repeated and trailing slashes.
///
/// Captures are percent-decoded once when evaluating structural candidates.
/// Constraint failures allow backtracking. Encoded slashes do not
/// change the matched path structure; malformed percent escapes remain literal.
/// Invalid decoded UTF-8 returns a routing failure converted to a 400 by Application.
/// Recognition returns metadata only for successfully selected routes with valid
/// captures. The application installs it on the request and clears metadata from
/// earlier passes.
/// Unchanged captures borrow an owned URI snapshot through byte ranges, so
/// middleware URI changes preserve their original values. Names and route labels
/// share one request-owned buffer. Other-method probes build no routing metadata.
///
/// Unmatched paths select the root fallback or return a routing failure for a 404.
/// When other methods match, recognition returns a failure for a 405 with a sorted
/// `Allow` header, including implicit HEAD. OPTIONS requires explicit registration. Table
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

    pub(crate) fn middleware(&self) -> &[Box<dyn ErasedMiddleware>] {
        &self.middleware
    }

    pub(crate) fn error_chain(&self) -> &[usize] {
        &self.table_chain
    }

    pub(crate) fn recognize(
        &self,
        method_to_match: &Method,
        uri: &Uri,
    ) -> Result<SelectedEndpoint<'_>, RecognitionError> {
        let path = uri.path();

        if path.starts_with('/') {
            if let Some(paths) = self.paths.get(method_to_match)
                && let Some(matched) = paths.at(path, &self.routes).map_err(RecognitionError::InvalidCapture)?
            {
                let info = &self.routes[matched.id];
                let endpoint = &self.endpoints[matched.id];
                let metadata = RoutingInfo::new(
                    uri,
                    info.path(),
                    info.name.as_deref(),
                    info.pattern.captures.iter().map(|capture| capture.name.as_str()),
                    matched.params,
                );
                return Ok(SelectedEndpoint {
                    controller: &*endpoint.controller,
                    chain: &endpoint.chain,
                    metadata: Some(metadata),
                });
            }

            let mut allowed = Vec::new();
            for (method, paths) in &self.paths {
                if method == method_to_match {
                    continue;
                }
                if paths
                    .matches(path, &self.routes)
                    .map_err(RecognitionError::InvalidCapture)?
                {
                    allowed.push(method.clone());
                }
            }

            if !allowed.is_empty() {
                allowed.sort_unstable_by(|a, b| a.as_str().cmp(b.as_str()));
                return Err(RecognitionError::MethodNotAllowed(allowed));
            }
        }

        match &self.fallback {
            Some(endpoint) => Ok(SelectedEndpoint {
                controller: &*endpoint.controller,
                chain: &endpoint.chain,
                metadata: None,
            }),
            None => Err(RecognitionError::NotFound),
        }
    }
}
