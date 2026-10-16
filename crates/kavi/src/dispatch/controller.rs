use std::ops::AsyncFn;

use crate::{
    Error,
    http::{Request, Response},
};

use super::DispatchFuture;

/// Handles one routed request with application dependencies of type `S`.
///
/// The [`crate::controller`] macro implements this trait with a
/// [`super::Provide`] bound for each dependency parameter. Call the generated
/// factory when registering a controller, as in `router.get(path, show())`.
/// Routers check those bounds at registration and bind the shared dependency
/// owner when building the routing table. Dependencies stay outside the request
/// and may be borrowed across `.await`.
/// An owned dependency marked `#[dependency(clone)]` is explicitly cloned for
/// each invocation; borrowed parameters do not require `Clone`. Generic adapters
/// retain type and const parameters, while borrow lifetimes belong to each call.
/// The generated adapter's `call` method accepts explicit arguments without
/// provider access or cloning, so it can also be invoked directly.
///
/// Async functions and reusable async closures automatically implement this trait
/// for any `S` when they accept one [`Request`] and return [`Result<Response, Error>`],
/// ignoring the supplied dependency root.
/// Use response helpers such as [`text`](crate::http::response::text) and
/// [`json`](crate::http::response::json) to build the response. Convert application
/// errors into [`Error`] inside the controller.
///
/// The controller must own its captures or borrow them for `'static`, but
/// each call's future may borrow both the controller and `S`. Neither the controller
/// nor its futures are required to be `Send` or `Sync`. Calls use `&self`, so an
/// instance may serve interleaved requests.
///
/// Errors remain fallible so outer middleware can inspect or recover them before
/// an explicit rendering boundary calls [`Error::into_response`] or an
/// application-specific renderer. Returning a streaming response does not wait
/// for its body to be sent.
///
/// ```
/// use kavi::{
///     Error,
///     http::{Request, Response, response::{json, text}},
/// };
///
/// async fn health(_: Request) -> Result<Response, Error> {
///     Ok(text("ok"))
/// }
///
/// async fn details(_: Request) -> Result<Response, Error> {
///     json(true)
/// }
/// ```
#[allow(async_fn_in_trait)]
pub trait Controller<S = ()>: 'static {
    /// Handles the owned request and returns a concrete HTTP response.
    ///
    /// # Errors
    ///
    /// Returns the controller's framework error without rendering it.
    async fn handle(&self, request: Request, dependencies: &S) -> Result<Response, Error>;
}

impl<F, S> Controller<S> for F
where
    F: AsyncFn(Request) -> Result<Response, Error> + 'static,
{
    #[inline]
    async fn handle(&self, request: Request, _: &S) -> Result<Response, Error> {
        self(request).await
    }
}

/// Object-compatible controller calls for heterogeneous routing storage.
///
/// Bound controllers retain their dependency owner and box each call's borrowed
/// future. The future need not be `Send` or `'static`.
pub(crate) trait ErasedController {
    fn call(&self, request: Request) -> DispatchFuture<'_>;
}
