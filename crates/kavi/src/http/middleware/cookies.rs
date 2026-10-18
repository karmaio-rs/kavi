use std::rc::Rc;

use crate::{
    Error,
    dispatch::{Middleware, Next},
    http::{
        Request, Response,
        cookies::{self, Processor, ProcessorConfig},
    },
};

/// Emits queued response cookies using an explicitly configured processor.
///
/// Available with the `cookies` feature. Register outside all cookie-producing
/// handlers and middleware in this scope. Responses that short-circuit before
/// reaching this middleware are not processed. Incoming parsing stays lazy and
/// explicit through RequestExt::cookies; no configuration is stored on requests.
///
/// The processor can be shared with the application dependency root through Rc.
/// Each returned response owns its pending cookies. Emission consumes that queue,
/// validates every generated header before appending any, and preserves existing
/// Set-Cookie fields. Invalid output returns an empty 500 response through outer
/// middleware. Application finalizes original HEAD semantics after the chain.
#[derive(Clone)]
pub struct CookieMiddleware {
    processor: Rc<Processor>,
}

impl CookieMiddleware {
    /// Captures an owned or shared processor without cloning it per request.
    #[inline]
    pub fn new(processor: impl Into<Rc<Processor>>) -> Self {
        Self {
            processor: processor.into(),
        }
    }
}

impl Default for CookieMiddleware {
    /// Enables ordinary cookie encoding without signing or encryption.
    #[inline]
    fn default() -> Self {
        Self::new(Processor::from(ProcessorConfig::default()))
    }
}

impl<S> Middleware<S> for CookieMiddleware {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        let mut response = next.run(request).await;
        if let Err(error) = cookies::emit(&mut response, &self.processor) {
            return Error::internal(error).into_response();
        }
        response
    }
}
