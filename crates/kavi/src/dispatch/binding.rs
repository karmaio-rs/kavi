use std::rc::Rc;

use super::{Controller, DispatchFuture, ErasedController, ErasedMiddleware, Middleware, Next};
use crate::http::Request;

/// Registration retains the component; compilation supplies its dependency owner once.
pub(crate) type ControllerFactory<S> = Box<dyn FnOnce(Rc<S>) -> Box<dyn ErasedController>>;
/// Middleware uses the same deferred ownership contract as controllers.
pub(crate) type MiddlewareFactory<S> = Box<dyn FnOnce(Rc<S>) -> Box<dyn ErasedMiddleware>>;

pub(crate) fn defer_controller<T: Controller<S>, S: 'static>(handler: T) -> ControllerFactory<S> {
    Box::new(move |dependencies| Box::new(Bound { handler, dependencies }))
}

pub(crate) fn defer_middleware<T: Middleware<S>, S: 'static>(handler: T) -> MiddlewareFactory<S> {
    Box::new(move |dependencies| Box::new(Bound { handler, dependencies }))
}

struct Bound<T, S> {
    handler: T,
    dependencies: Rc<S>,
}

impl<T: Controller<S>, S> ErasedController for Bound<T, S> {
    #[inline]
    fn call<'r>(&'r self, request: &'r mut Request) -> DispatchFuture<'r> {
        Box::pin(self.handler.handle(request, &self.dependencies))
    }
}

impl<T: Middleware<S>, S> ErasedMiddleware for Bound<T, S> {
    #[inline]
    fn call<'r>(&'r self, request: &'r mut Request, next: Next<'r>) -> DispatchFuture<'r> {
        Box::pin(self.handler.handle(request, next, &self.dependencies))
    }
}
