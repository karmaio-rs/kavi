//! Core building blocks for the Kavi web framework on Karmaio.
//!
//! HTTP messages, extractors, and response helpers live in [`http`].
//! [`routing`] selects paths and methods; [`Application`] executes [`dispatch`] controllers and middleware.
//! [`foundation::server`] adapts accepted transports to the application.
//! [`Error`] provides the shared framework error contract.

#![forbid(unsafe_code)]

pub mod dispatch;
pub mod foundation;
pub mod http;
pub mod routing;
pub mod support;

pub use foundation::Error;
pub use foundation::{Application, ApplicationBuilder};
pub use kavi_macros::{Dependencies, controller, middleware};
