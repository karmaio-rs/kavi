//! Application dispatch, caller-driven serving, and shared framework errors.

mod application;
mod error;
pub mod server;

pub use application::{Application, ApplicationBuilder};
pub use error::Error;
