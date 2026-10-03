use std::ops::AsyncFn;

use crate::{
    Error,
    http::{Request, Response},
};

use super::{DispatchFuture, Next};

/// Surrounds the remaining middleware and controller with before-and-after logic.
///
/// Call [`Next::run`] to transfer the owned request to the remainder at most once,
/// or return directly to short-circuit. The continuation always includes a
/// terminal controller. Async functions and reusable async closures automatically
/// implement this trait when they accept a [`Request`] and a [`Next`] and return
/// [`Result<Response, Error>`].
///
/// A middleware instance must own its dependencies or borrow them for `'static`.
/// Each call's future may borrow the instance and continuation. Calls use `&self`,
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
/// ```
/// use kavi::{
///     Error,
///     dispatch::{Middleware, Next},
///     http::{HeaderValue, Request, Response},
/// };
///
/// struct RequestId;
/// impl Middleware for RequestId {
///     async fn handle(&self, mut request: Request, next: Next<'_>) -> Result<Response, Error> {
///         request.extensions_mut().insert(String::from("request-123"));
///         let mut response = next.run(request).await?;
///         response.headers_mut().insert("x-request-id", HeaderValue::from_static("request-123"));
///         Ok(response)
///     }
/// }
/// ```
#[allow(async_fn_in_trait)]
pub trait Middleware: 'static {
    /// Handles the owned request and may invoke the remainder once or return early.
    ///
    /// # Errors
    ///
    /// May return its own framework error or propagate one from [`Next::run`].
    async fn handle(&self, request: Request, next: Next<'_>) -> Result<Response, Error>;
}

impl<F> Middleware for F
where
    F: for<'a> AsyncFn(Request, Next<'a>) -> Result<Response, Error> + 'static,
{
    #[inline]
    async fn handle(&self, request: Request, next: Next<'_>) -> Result<Response, Error> {
        self(request, next).await
    }
}

/// Object-compatible middleware calls for heterogeneous routing storage.
///
/// The blanket implementation boxes each call's future, which may borrow both
/// the middleware and continuation without being `Send` or `'static`.
pub(crate) trait ErasedMiddleware {
    fn call<'a>(&'a self, request: Request, next: Next<'a>) -> DispatchFuture<'a>;
}

impl<M: Middleware> ErasedMiddleware for M {
    #[inline]
    fn call<'a>(&'a self, request: Request, next: Next<'a>) -> DispatchFuture<'a> {
        Box::pin(Middleware::handle(self, request, next))
    }
}
