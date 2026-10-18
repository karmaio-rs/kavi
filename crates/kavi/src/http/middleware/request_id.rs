use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    dispatch::{Middleware, Next},
    http::{HeaderValue, Request, Response},
};

/// Establishes an X-Request-Id and propagates it onto the downstream response.
///
/// Incoming IDs are replaced by default. Generated IDs combine process, time,
/// and counter values; they are correlation labels, not security tokens. The
/// policy retains its chosen ID locally across the downstream await. Register
/// it on Application to cover routing errors and early framework rejections.
#[derive(Clone, Copy, Default)]
pub struct RequestId {
    trust_incoming: bool,
}

impl RequestId {
    /// Selects whether one nonempty textual incoming ID of at most 128 bytes is accepted.
    pub fn trust_incoming(mut self, enabled: bool) -> Self {
        self.trust_incoming = enabled;
        self
    }
}

impl<S> Middleware<S> for RequestId {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        let mut values = request.headers().get_all("x-request-id").iter();
        let valid = self.trust_incoming
            && values
                .next()
                .is_some_and(|value| !value.is_empty() && value.as_bytes().len() <= 128 && value.to_str().is_ok())
            && values.next().is_none();
        if !valid {
            request.headers_mut().insert("x-request-id", generate_id());
        }
        let id = request.headers()["x-request-id"].clone();
        let mut response = next.run(request).await;
        response.headers_mut().entry("x-request-id").or_insert(id);
        response
    }
}

fn generate_id() -> HeaderValue {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = format!(
        "{:x}-{time:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    HeaderValue::try_from(id).expect("hexadecimal request ID")
}
