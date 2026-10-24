//! Core building blocks for the Kavi web framework on Karmaio.
//!
//! HTTP messages, extractors, and response types live in [`http`].
//! [`routing`] selects paths and methods; [`Application`] executes [`dispatch`] controllers and middleware.
//! [`foundation::server`] adapts accepted transports to the application.
//! [`Error`] provides the shared framework error contract.
//!
//! A controller returns a response-convertible value, including `Result<T, E>`
//! when both branches implement [`http::IntoResponse`]. The adapter converts it
//! before returning through middleware, so application errors retain their own
//! status, headers, and body:
//!
//! ```rust
//! use kavi::{Error, controller};
//! use kavi::http::{Request, RequestExt};
//!
//! #[controller]
//! async fn show(request: &mut Request) -> Result<String, Error> {
//!     let id = request.param::<u64>("id")?;
//!     Ok(format!("user {id}"))
//! }
//! ```
//!
//! The application panic hook is separate from returned errors. [`Application`]
//! contains a panic, reports it to [`ErrorHandler`], and renders a generic 500 so the caller
//! never sees the panic's message. Replace that rendering with
//! [`ApplicationBuilder::error_handler`].

#![forbid(unsafe_code)]

pub mod dispatch;
pub mod foundation;
pub mod http;
pub mod routing;
pub mod support;

pub use foundation::Error;
pub use foundation::{Application, ApplicationBuilder, DefaultErrorHandler, ErrorHandler, PanicError};
pub use kavi_macros::{Dependencies, controller, middleware};
