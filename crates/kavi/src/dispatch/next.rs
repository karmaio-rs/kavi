use crate::{
    Error,
    http::{Request, Response},
};

use super::{ErasedController, ErasedMiddleware};

/// The remaining middleware and its guaranteed terminal controller.
///
/// Each invoked middleware wraps the remainder: it runs before downstream
/// middleware and the controller, then resumes when they return. Returning early
/// skips the remainder; propagating an error with `?` skips subsequent code in
/// that middleware.
///
/// [`Self::run`] consumes this continuation, which cannot be cloned or copied.
/// Returning without calling it short-circuits the chain. Construction belongs
/// to the framework. The continuation borrows its middleware and terminal
/// controller for `'a`; it does not own or clone them.
///
/// ```compile_fail
/// use kavi::{Error, dispatch::Next, http::{Body, Request, Response}};
///
/// async fn twice(next: Next<'_>, request: Request) -> Result<Response, Error> {
///     next.run(request).await?;
///     next.run(Request::new(Body::empty())).await // `next` was moved.
/// }
/// ```
pub struct Next<'a> {
    middleware: &'a [Box<dyn ErasedMiddleware>],
    controller: &'a dyn ErasedController,
}

impl<'a> Next<'a> {
    /// Borrows middleware in execution order and its required terminal controller.
    ///
    /// An empty middleware slice dispatches directly to the controller.
    pub(crate) fn new(middleware: &'a [Box<dyn ErasedMiddleware>], controller: &'a dyn ErasedController) -> Self {
        Self { middleware, controller }
    }

    /// Executes the remainder once, forwarding ownership of the request.
    ///
    /// Invokes the next middleware, or the terminal controller if none remains.
    /// Completion means a response or error was produced, not that a streaming
    /// response body has finished sending.
    ///
    /// # Errors
    ///
    /// Returns middleware or controller errors without rendering them.
    pub async fn run(self, request: Request) -> Result<Response, Error> {
        match self.middleware.split_first() {
            Some((middleware, remaining)) => middleware.call(request, Self::new(remaining, self.controller)).await,
            None => self.controller.call(request).await,
        }
    }
}
