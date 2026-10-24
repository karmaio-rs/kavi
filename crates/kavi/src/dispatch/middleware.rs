use std::ops::AsyncFn;

use crate::http::{IntoResponse, Request, Response};

use super::{DispatchFuture, Next};

/// Surrounds the remaining middleware and controller with before-and-after logic.
///
/// Middleware temporarily lends its mutable request borrow to [`Next::run`].
/// After that await completes it can inspect the request again and modify the
/// owned response. Returning without invoking Next short-circuits the chain.
/// The continuation is consumed at most once and contains no request data.
///
/// Downstream handler results are converted before returning through
/// [`Next::run`], so middleware receives a concrete response for both success
/// and application rejection. A middleware function may itself return
/// `Result<T, E>` when both branches implement [`IntoResponse`]; outer middleware
/// then receives its converted response as well.
///
/// The [`macro@crate::middleware`] macro binds application dependencies through
/// [`super::Provide`]. Dependencies remain separate from the request. Borrowed
/// dependencies can be used across await points; `#[dependency(clone)]` explicitly
/// acquires an owned handle. Register the generated factory with the router or
/// application builder.
///
/// Every completed call returns Response, including rejections and business
/// failures. Use a response-convertible error type to propagate failures with `?`.
/// Outer middleware receives responses for routing misses and framework checks as
/// well. Returning a response does not mean its streaming body was transmitted.
/// Code after the downstream await does not run if dispatch panics or is dropped.
///
/// Async functions and reusable async closures accepting `&mut Request` and Next
/// that return an [`IntoResponse`] implement this trait automatically. Instances
/// own their captures or borrow them for `'static`; call futures may borrow the
/// request and dependencies. Neither instances nor their futures need to be
/// Send or Sync.
#[allow(async_fn_in_trait)]
pub trait Middleware<S = ()>: 'static {
    /// Borrows the request and invokes the remainder once or returns an early response.
    async fn handle(&self, request: &mut Request, next: Next<'_>, dependencies: &S) -> Response;
}

impl<F, S, R> Middleware<S> for F
where
    F: for<'r, 'n> AsyncFn(&'r mut Request, Next<'n>) -> R + 'static,
    R: IntoResponse + 'static,
{
    #[inline]
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        self(request, next).await.into_response()
    }
}

/// Object-compatible middleware calls borrowing the request and bound owner.
pub(crate) trait ErasedMiddleware {
    fn call<'r>(&'r self, request: &'r mut Request, next: Next<'r>) -> DispatchFuture<'r>;
}
