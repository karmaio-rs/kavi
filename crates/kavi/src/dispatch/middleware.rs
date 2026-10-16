use std::ops::AsyncFn;

use crate::{
    Error,
    http::{Request, Response},
};

use super::{DispatchFuture, Next};

/// Surrounds the remaining middleware and controller with before-and-after logic.
///
/// The [`crate::middleware`] macro implements this trait with [`super::Provide`]
/// bounds for its application dependency parameters. Call the generated factory
/// when registering middleware, as in `router.middleware(authenticate())`.
/// Routers check provider bounds at registration and bind middleware to the shared
/// dependency owner when building the routing table. Application services remain
/// outside the request.
/// Shared references may remain borrowed across `.await`. An owned dependency
/// marked `#[dependency(clone)]` clones the provided value per invocation, allowing
/// that handle to outlive dispatch. The generated `call` method instead accepts
/// the original explicit arguments and performs no projection or cloning.
///
/// Call [`Next::run`] to transfer the owned request to the remainder at most once,
/// or return directly to short-circuit. The continuation always includes a
/// terminal controller or routing error. Async functions
/// and reusable async closures automatically implement this trait for any `S` when they
/// accept a [`Request`] and a [`Next`] and return
/// [`Result<Response, Error>`], ignoring the supplied dependency root.
///
/// A middleware instance must own its captures or borrow them for `'static`.
/// Each call's future may borrow the instance, dependency root, and continuation,
/// including across downstream execution. Calls use `&self`,
/// so an instance may serve interleaved requests; request-local data can be passed
/// to downstream middleware and the controller through request extensions.
/// Neither middleware nor its futures are required to be `Send` or `Sync`.
///
/// Code after `next.run(request).await` runs when the remainder returns a response
/// or error, before a streaming response body has necessarily been sent. Using
/// `next.run(request).await?` returns early on error, skipping subsequent code.
/// Match the result to inspect or recover errors. Code after the await is not
/// guaranteed to run if the dispatch future is dropped or panics.
///
/// Handwritten implementations receive the dependency root as the final argument.
/// They can attach request-local data before invoking the continuation and modify
/// response headers when it returns. They need no provider projections unless
/// their implementation chooses to use them.
#[allow(async_fn_in_trait)]
pub trait Middleware<S = ()>: 'static {
    /// Handles the owned request and may invoke the remainder once or return early.
    ///
    /// # Errors
    ///
    /// May return its own framework error or propagate one from [`Next::run`].
    async fn handle(&self, request: Request, next: Next<'_>, dependencies: &S) -> Result<Response, Error>;
}

impl<F, S> Middleware<S> for F
where
    F: for<'a> AsyncFn(Request, Next<'a>) -> Result<Response, Error> + 'static,
{
    #[inline]
    async fn handle(&self, request: Request, next: Next<'_>, _: &S) -> Result<Response, Error> {
        self(request, next).await
    }
}

/// Object-compatible middleware calls for heterogeneous routing storage.
///
/// Bound middleware retains its dependency owner and boxes each call's future,
/// which may borrow the middleware, dependencies, and continuation without being
/// `Send` or `'static`.
pub(crate) trait ErasedMiddleware {
    fn call<'a>(&'a self, request: Request, next: Next<'a>) -> DispatchFuture<'a>;
}
