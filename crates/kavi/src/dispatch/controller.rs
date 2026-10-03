use std::ops::AsyncFn;

use crate::{
    Error,
    http::{Request, Response},
};

use super::DispatchFuture;

/// Handles one routed request.
///
/// Async functions and reusable async closures automatically implement this trait
/// when they accept one [`Request`] and return [`Result<Response, Error>`].
/// Use response helpers such as [`text`](crate::http::response::text) and
/// [`json`](crate::http::response::json) to build the response. Convert application
/// errors into [`Error`] inside the controller.
///
/// The controller must own its dependencies or borrow them for `'static`, but
/// each call's future may borrow the controller itself. Neither the controller
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
pub trait Controller: 'static {
    /// Handles the owned request and returns a concrete HTTP response.
    ///
    /// # Errors
    ///
    /// Returns the controller's framework error without rendering it.
    async fn handle(&self, request: Request) -> Result<Response, Error>;
}

impl<F> Controller for F
where
    F: AsyncFn(Request) -> Result<Response, Error> + 'static,
{
    #[inline]
    async fn handle(&self, request: Request) -> Result<Response, Error> {
        self(request).await
    }
}

/// Object-compatible controller calls for heterogeneous routing storage.
///
/// The blanket implementation boxes each call's future while preserving its
/// borrow of the controller. The future need not be `Send` or `'static`.
pub(crate) trait ErasedController {
    fn call(&self, request: Request) -> DispatchFuture<'_>;
}

impl<C: Controller> ErasedController for C {
    #[inline]
    fn call(&self, request: Request) -> DispatchFuture<'_> {
        Box::pin(Controller::handle(self, request))
    }
}
