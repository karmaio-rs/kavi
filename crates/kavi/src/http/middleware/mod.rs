//! Built-in HTTP middleware for application and route policies.
//!
//! Implementations use the existing [`crate::dispatch::Middleware`] contract.
//!
//! Register origin and rewrite policies on Application before routing. Hard body
//! limits reject known oversized requests and tighten streaming body ceilings.
//! Parser budgets are passed explicitly to extractors. Dispatch concurrency ends
//! when a response is produced, independently of body transmission and connection admission.

#[cfg(feature = "cookies")]
mod cookies;
mod limits;
mod proxy;
mod request_id;
mod rewrite;
mod timeout;

pub use crate::http::body::BodyLimitExceeded;
#[cfg(feature = "cookies")]
pub use cookies::CookieMiddleware;
pub use limits::{BodyLimit, ConcurrencyLimit};
pub use proxy::TrustedProxies;
pub use request_id::RequestId;
pub use rewrite::{MethodOverride, NormalizePath};
pub use timeout::{RequestTimedOut, RequestTimeout};
