//! Controllers and middleware for owned requests and worker-local execution.
//!
//! A [`Controller`] handles one owned request. A [`Middleware`] can modify the
//! request before passing it to [`Next::run`], inspect the returned response or
//! error, or return early to short-circuit the chain. Each continuation ends in
//! a selected controller or routing error, even when no
//! middleware remains.
//!
//! Middleware and controllers may borrow their captured dependencies during a
//! call. Their futures need not be `Send`, and errors remain fallible until an
//! explicit rendering boundary converts them into HTTP responses.
//!
//! Use [`macro@controller`] and [`macro@middleware`] to declare borrowed or
//! explicitly cloned application dependencies as function parameters.
//! Registration checks provider bounds, and router compilation binds handlers to a shared dependency root
//! through [`Provide`]. [`Controller`] and
//! [`Middleware`] are the invocation contracts implemented by those macros.

mod binding;
mod controller;
mod dependencies;
mod middleware;
mod next;

use std::{future::Future, pin::Pin};

use crate::{Error, http::Response};

pub use controller::Controller;
pub use dependencies::Provide;
pub use kavi_macros::{controller, middleware};
pub use middleware::Middleware;
pub use next::Next;

pub(crate) use binding::{ControllerFactory, MiddlewareFactory, defer_controller, defer_middleware};
pub(crate) use controller::ErasedController;
pub(crate) use middleware::ErasedMiddleware;
pub(crate) use next::Terminal;

/// A borrowed local future used only at dispatch's dynamic storage boundary.
pub(crate) type DispatchFuture<'a> = Pin<Box<dyn Future<Output = Result<Response, Error>> + 'a>>;
