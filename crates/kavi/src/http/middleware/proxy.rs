use std::net::IpAddr;

use crate::{
    dispatch::{Middleware, Next},
    http::{MetadataError, Request, Response, apply_forwarded},
};

/// Applies RFC 7239 Forwarded metadata through an explicitly trusted proxy chain.
///
/// `F` decides which peer IP addresses are trusted. No proxies are implicitly
/// trusted, including loopback. Traversal starts at the socket peer and follows
/// rightmost elements until the first untrusted, unknown, or obfuscated node.
/// Values to its left cannot affect the origin. Each trusted proxy must append or
/// replace its own element correctly and protect its connection to Kavi.
/// Nonstandard X-Forwarded fields are ignored because they cannot be correlated
/// reliably with RFC 7239 elements. Register before routing and origin consumers.
/// Socket facts and the request URI remain unchanged.
pub struct TrustedProxies<F> {
    trusted: F,
}

impl<F: Fn(IpAddr) -> bool> TrustedProxies<F> {
    /// Creates a policy using an application-selected peer trust predicate.
    #[inline]
    pub fn new(trusted: F) -> Self {
        Self { trusted }
    }

    /// Applies trusted forwarding metadata without running a middleware chain.
    ///
    /// Captures initial origin metadata when absent. An unknown or untrusted peer
    /// causes forwarding headers to be ignored. Each accepted element supplies its
    /// own scheme and authority; omitted values stay unknown. Parsing failure
    /// leaves existing metadata unchanged. Read results through RequestExt.
    /// Capture original metadata before request rewriting.
    ///
    /// # Errors
    /// Returns MetadataError for malformed trusted fields or an invalid base origin.
    #[inline]
    pub fn apply(&self, request: &mut Request) -> Result<(), MetadataError> {
        apply_forwarded(request, &self.trusted)
    }
}

impl<F: Fn(IpAddr) -> bool + 'static, S> Middleware<S> for TrustedProxies<F> {
    #[inline]
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        if let Err(error) = self.apply(request) {
            return crate::Error::from(error).into_response();
        }
        next.run(request).await
    }
}
