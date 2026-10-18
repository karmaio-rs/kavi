use std::{
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::{Duration, Instant},
};

use karmaio::runtime::{CancellationSource, FutureExt};

use crate::{
    Error,
    dispatch::{Middleware, Next},
    http::{Request, Response, StatusCode, body::IncomingAbort},
};

/// The request deadline expired and downstream dispatch finished cleanup.
#[derive(Debug, thiserror::Error)]
#[error("request dispatch deadline expired")]
pub struct RequestTimedOut;

/// Cancels downstream operations, awaits cleanup, then returns a 504 response.
///
/// The timer covers dispatch and downstream middleware cleanup, not transmission
/// of the returned body. Direct Karmaio I/O inherits a local cancellation scope.
/// At the deadline, existing operations are cancelled, then downstream is polled
/// outside this scope so cleanup can perform new I/O. Outer scopes still apply.
/// The server also supplies a dedicated incoming-body cancellation capability on
/// the request, allowing passive upload waits to finish through body wrappers.
/// No request data or cancellation controls are stored in Next or application
/// services. Neither a task nor its cleanup is detached.
///
/// Cancellation is cooperative. Noncooperative futures and separately spawned
/// tasks can delay completion indefinitely. Alternative body sources must arrange
/// their own cancellation cooperation. Successful response bodies retain the
/// scope owner for previously submitted I/O, without applying it to later reads
/// or extending the timer to transmission. Connection abort can drop dispatch
/// before cleanup completes. Register on Application to cover routing and its middleware.
#[derive(Clone, Copy, Debug)]
pub struct RequestTimeout(pub Duration);

impl<S> Middleware<S> for RequestTimeout {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        let Some(deadline) = Instant::now().checked_add(self.0) else {
            return Error::internal(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unrepresentable request timeout",
            ))
            .into_response();
        };
        let incoming = request.extensions().get::<IncomingAbort>().cloned();
        let source = CancellationSource::new();
        let mut call = pin!(next.run(request));
        let mut timer = pin!(karmaio::time::sleep_until(deadline));
        let result = {
            let mut scoped = pin!(call.as_mut().with_cancellation(source.token()));
            poll_fn(|cx| {
                if let Poll::Ready(response) = scoped.as_mut().poll(cx) {
                    return Poll::Ready(Some(response));
                }
                if timer.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(None);
                }
                Poll::Pending
            })
            .await
        };
        match result {
            Some(mut response) => {
                response.body_mut().retain_dispatch_scope(source);
                response
            }
            None => {
                source.cancel();
                if let Some(incoming) = incoming {
                    incoming.cancel();
                }
                let _ = call.await;
                Error::with_status(StatusCode::GATEWAY_TIMEOUT, RequestTimedOut).into_response()
            }
        }
    }
}
