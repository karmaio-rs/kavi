use crate::{
    Error,
    dispatch::{Middleware, Next, Provide},
    http::{
        Request, Response,
        cookies::{self, Processor},
    },
};

/// Emits queued response cookies for one scope.
///
/// The processor comes from the dependency root, as [`crate::http::cookies::Processor`].
/// Register this outside every component that queues cookies in the scope. A root
/// that does not provide that processor cannot register it. Nested registration
/// emits once: the inner middleware consumes the queue, and the outer one finds
/// nothing left. An invalid generated header discards that queue and returns an
/// empty 500.
///
/// Handlers obtain a [`crate::http::cookies::CookieJar`] through
/// [`crate::http::RequestExt::cookies`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CookieMiddleware;

impl<S> Middleware<S> for CookieMiddleware
where
    S: Provide<Processor>,
{
    async fn handle(&self, request: &mut Request, next: Next<'_>, dependencies: &S) -> Response {
        cookies::mark_enabled(request);
        let mut response = next.run(request).await;
        if let Err(error) = cookies::emit_cookies(&mut response, dependencies.provide()) {
            return Error::internal(error).into_response();
        }
        response
    }
}
