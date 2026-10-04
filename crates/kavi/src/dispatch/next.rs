use crate::{
    Error,
    http::{Request, Response},
};

use super::{ErasedController, ErasedMiddleware};

/// The remaining middleware and its selected terminal outcome.
///
/// Each invoked middleware wraps the remainder: it runs before downstream
/// middleware and the controller or routing error, then resumes when they
/// return. Returning early skips the remainder; propagating an error with `?`
/// skips subsequent code in that middleware.
///
/// [`Self::run`] consumes this continuation, which cannot be cloned or copied.
/// Returning without calling it short-circuits the chain. Construction belongs
/// to the framework. The continuation borrows its middleware and selected
/// controller for `'a`. A routing error is owned and delivered at most once.
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
    chain: &'a [usize],
    terminal: Terminal<'a>,
}

/// The controller or routing error at the end of a middleware chain.
pub(crate) enum Terminal<'a> {
    Controller(&'a dyn ErasedController),
    Error(Error),
}

impl<'a> Next<'a> {
    /// Borrows an owned middleware store and one complete compiled chain.
    ///
    /// The chain contains middleware in execution order, regardless of its scope.
    /// Identifiers share instances across endpoints without cloning them.
    pub(crate) fn new(middleware: &'a [Box<dyn ErasedMiddleware>], chain: &'a [usize], terminal: Terminal<'a>) -> Self {
        Self {
            middleware,
            chain,
            terminal,
        }
    }

    /// Executes the remainder once, forwarding ownership of the request.
    ///
    /// Invokes the next middleware, or delivers the terminal outcome if none remains.
    /// Completion means a response or error was produced, not that a streaming
    /// response body has finished sending.
    ///
    /// # Errors
    ///
    /// Returns routing, middleware, or controller errors without rendering them.
    pub async fn run(mut self, request: Request) -> Result<Response, Error> {
        if let Some((&id, remaining)) = self.chain.split_first() {
            self.chain = remaining;
            return self.middleware[id].call(request, self).await;
        }
        match self.terminal {
            Terminal::Controller(controller) => controller.call(request).await,
            Terminal::Error(error) => Err(error),
        }
    }
}
