//! Response-only controllers and middleware borrowing application-owned requests.
//!
//! A Controller handles `&mut Request` and returns an owned Response. Middleware
//! can modify the request before lending it to [`Next::run`], then inspect the
//! request and response afterward. Early responses skip the remaining chain.
//! Business and extraction errors are handled inside the controller or middleware.
//!
//! Request data remains on the request; Next stores only the remaining call
//! structure. Typed application dependencies are bound separately through Provide.
//! Calls can borrow the request, handler, and services across await points without
//! Send or Sync requirements.
//!
//! Use [`macro@controller`] and [`macro@middleware`] to declare borrowed or
//! explicitly cloned application dependencies as function parameters. Router
//! compilation binds all handlers to one shared dependency owner.

mod binding;
mod controller;
mod dependencies;
mod middleware;
mod next;

use std::{future::Future, pin::Pin};

use crate::http::Response;

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
pub(crate) type DispatchFuture<'a> = Pin<Box<dyn Future<Output = Response> + 'a>>;
