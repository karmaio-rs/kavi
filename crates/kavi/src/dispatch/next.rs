use crate::http::{Request, Response};

use super::{ErasedController, ErasedMiddleware};

/// The remaining middleware chain and its selected terminal call or response.
///
/// Each middleware borrows the request, invokes the consuming continuation at
/// most once, and resumes when downstream produces an owned response. The
/// request is available again after that await completes. Returning without
/// calling run short-circuits the remainder.
///
/// Next stores only execution structure. Application services are bound to the
/// registered handlers; request metadata remains on the borrowed request.
pub struct Next<'a> {
    middleware: &'a [Box<dyn ErasedMiddleware>],
    chain: &'a [usize],
    terminal: Terminal<'a>,
}

pub(crate) enum Terminal<'a> {
    Controller(&'a dyn ErasedController),
    /// A framework rejection, such as an unmatched path or a disallowed method.
    Response(Box<Response>),
}

impl<'a> Next<'a> {
    pub(crate) fn new(middleware: &'a [Box<dyn ErasedMiddleware>], chain: &'a [usize], terminal: Terminal<'a>) -> Self {
        Self {
            middleware,
            chain,
            terminal,
        }
    }

    /// Executes the remainder once with a temporary mutable request borrow.
    ///
    /// Returns a response for every completed call. A controller always
    /// produces one after converting its selected result branch, and a framework
    /// rejection is already a response. Response body transmission
    /// occurs later under the serving connection's ownership.
    pub async fn run(mut self, request: &mut Request) -> Response {
        if let Some((&id, remaining)) = self.chain.split_first() {
            self.chain = remaining;
            return self.middleware[id].call(request, self).await;
        }
        match self.terminal {
            Terminal::Controller(controller) => controller.call(request).await,
            Terminal::Response(response) => *response,
        }
    }
}
