use std::{cell::Cell, num::NonZeroUsize, rc::Rc};

use crate::{
    Error,
    dispatch::{Middleware, Next},
    http::{
        Request, Response, StatusCode,
        body::{BodyLimitExceeded, DEFAULT_BODY_LIMIT},
        header,
    },
};

/// Enforces a hard payload-byte ceiling, including for custom streaming consumers.
///
/// Known oversized bodies are rejected before dispatch, using the body's lower
/// size bound or a decimal Content-Length when Transfer-Encoding is absent.
/// Unknown-length bodies are checked as they are consumed, preserving streaming.
/// A read overflow returns a typed BodyLimitExceeded source to its consumer.
/// Controllers handle that failure and choose a response; middleware does not
/// override the response after downstream returns.
/// Controller work can begin before an unknown-length overflow is detected.
///
/// Configuration is captured when this middleware is registered. It is never
/// installed in request extensions. Parser and collection budgets are supplied
/// independently to extractors. Registrations tighten the same body ceiling and
/// count bytes already consumed without adding producer wrappers.
/// Unread bytes are not drained by this middleware; the server owns connection
/// reuse policy. Trailer bytes are covered by transport limits, not this counter.
/// A body forwarded as the response is checked when transmitted; a late failure
/// belongs to the connection and cannot replace already-sent response headers.
#[derive(Clone, Copy, Debug)]
pub struct BodyLimit(pub usize);

impl Default for BodyLimit {
    #[inline]
    fn default() -> Self {
        Self(DEFAULT_BODY_LIMIT)
    }
}

impl<S> Middleware<S> for BodyLimit {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        if declared_too_large(request, self.0) {
            return Error::from(BodyLimitExceeded { limit: self.0 }).into_response();
        }
        if let Err(error) = request.body_mut().limit(self.0) {
            return Error::from(error).into_response();
        }
        next.run(request).await
    }
}

fn declared_too_large(request: &Request, limit: usize) -> bool {
    if request.headers().contains_key(header::TRANSFER_ENCODING) {
        return false;
    }
    // Protocol framing validation belongs to the server. A usable decimal length
    // also allows early rejection of synthetic bodies without useful size hints.
    request.headers().get_all(header::CONTENT_LENGTH).iter().any(|value| {
        let bytes = value.as_bytes();
        !bytes.is_empty()
            && bytes.iter().all(u8::is_ascii_digit)
            && value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .is_none_or(|length| length > limit as u64)
    })
}

/// Bounds simultaneous dispatches on one worker, rejecting overload with 503.
///
/// Clones share the same counter. No requests are queued. The permit is released
/// on success, failure, unwinding, or cancellation of dispatch; a returned
/// streaming response does not retain it. Use server connection admission to
/// bound ongoing network activity.
#[derive(Clone)]
pub struct ConcurrencyLimit {
    active: Rc<Cell<usize>>,
    limit: NonZeroUsize,
}

impl ConcurrencyLimit {
    /// Creates a worker-local dispatch limit with no waiting queue.
    pub fn new(limit: NonZeroUsize) -> Self {
        Self {
            active: Rc::new(Cell::new(0)),
            limit,
        }
    }
    /// Returns dispatches currently holding a permit.
    pub fn active(&self) -> usize {
        self.active.get()
    }
}

struct Permit<'a>(&'a Cell<usize>);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl<S> Middleware<S> for ConcurrencyLimit {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        if self.active.get() >= self.limit.get() {
            return Error::status(StatusCode::SERVICE_UNAVAILABLE).into_response();
        }
        self.active.set(self.active.get() + 1);
        let _permit = Permit(&self.active);
        next.run(request).await
    }
}
