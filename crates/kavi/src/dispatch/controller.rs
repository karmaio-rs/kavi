use std::ops::AsyncFn;

use crate::http::{Request, Response};

use super::DispatchFuture;

/// Handles a borrowed request using application dependencies of type `S`.
///
/// The application retains ownership of the request throughout dispatch. A
/// controller may read or modify its head and consume its body through
/// [`crate::http::RequestExt`]. It returns an owned response whose body may
/// outlive dispatch; that body cannot borrow the request.
///
/// The [`crate::controller`] macro binds borrowed dependency parameters through
/// [`super::Provide`]. Call the generated factory when registering a controller.
/// Dependencies remain outside the request and can be borrowed across await
/// points. `#[dependency(clone)]` explicitly acquires an owned dependency handle.
///
/// Business and extraction failures must be handled by the controller and mapped
/// to an appropriate response. No error is propagated through the dispatch chain.
/// Fallible response helpers and extractors retain their typed error APIs.
///
/// Async functions and reusable async closures accepting `&mut Request` and
/// returning Response implement this trait automatically, ignoring `S`.
/// Controllers own their captures or borrow them for `'static`; call futures may
/// borrow the controller, request, and services. Neither needs to be Send or Sync.
#[allow(async_fn_in_trait)]
pub trait Controller<S = ()>: 'static {
    /// Handles the request and returns a complete response head and owned body.
    async fn handle(&self, request: &mut Request, dependencies: &S) -> Response;
}

impl<F, S> Controller<S> for F
where
    F: for<'r> AsyncFn(&'r mut Request) -> Response + 'static,
{
    #[inline]
    async fn handle(&self, request: &mut Request, _: &S) -> Response {
        self(request).await
    }
}

/// Object-compatible calls whose futures borrow the request and bound owner.
pub(crate) trait ErasedController {
    fn call<'r>(&'r self, request: &'r mut Request) -> DispatchFuture<'r>;
}
