use std::ops::AsyncFn;

use crate::http::{IntoResponse, Request, Response};

use super::DispatchFuture;

/// Handles a borrowed request using application dependencies of type `S`.
///
/// The application retains ownership of the request throughout dispatch. A
/// controller may read or modify its head and consume its body through
/// [`crate::http::RequestExt`]. It returns an owned response whose body may
/// outlive dispatch; that body cannot borrow the request.
///
/// A handler may return any [`IntoResponse`] value, including `Result<T, E>`
/// when both branches implement that trait. The adapter converts the selected
/// branch before returning through middleware. Application errors choose their
/// own representation; returning `Err` does not invoke the panic handler.
///
/// ```rust
/// use kavi::{Error, controller};
/// use kavi::http::{Request, RequestExt, response::JsonResponse};
///
/// #[controller]
/// async fn show(request: &mut Request) -> Result<JsonResponse<u64>, Error> {
///     let id = request.param::<u64>("id")?;
///     Ok(JsonResponse(id))
/// }
/// ```
///
/// The [`crate::controller`] macro binds borrowed dependency parameters through
/// [`super::Provide`]. Call the generated factory when registering a controller.
/// Dependencies remain outside the request and can be borrowed across await
/// points. `#[dependency(clone)]` explicitly acquires an owned dependency handle.
///
/// Async functions and reusable async closures accepting `&mut Request` and
/// returning an [`IntoResponse`] implement this trait automatically, ignoring
/// `S`. Controllers own their captures or borrow them for `'static`; call
/// futures may borrow the controller, request, and services. Neither needs to
/// be Send or Sync.
#[allow(async_fn_in_trait)]
pub trait Controller<S = ()>: 'static {
    /// Handles the request and returns a complete response head and owned body.
    async fn handle(&self, request: &mut Request, dependencies: &S) -> Response;
}

impl<F, S, R> Controller<S> for F
where
    F: for<'r> AsyncFn(&'r mut Request) -> R + 'static,
    R: IntoResponse + 'static,
{
    #[inline]
    async fn handle(&self, request: &mut Request, _: &S) -> Response {
        self(request).await.into_response()
    }
}

/// Object-compatible calls whose futures borrow the request and bound owner.
pub(crate) trait ErasedController {
    fn call<'r>(&'r self, request: &'r mut Request) -> DispatchFuture<'r>;
}
