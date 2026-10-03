//! Core building blocks for the Kavi web framework on Karmaio.
//!
//! HTTP messages, extractors, and response helpers live in [`http`].
//! [`Error`] and [`Result`] provide the shared framework error contract.

#![forbid(unsafe_code)]

pub mod dispatch;
pub mod foundation;
pub mod http;
pub mod support;

pub use foundation::Error;
