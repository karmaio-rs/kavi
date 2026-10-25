//! Caller-driven application servers and local connection supervision helpers.
//!
//! Serving is limited to plain HTTP/1 behind a reverse proxy. Public connections,
//! TLS termination, and client-facing HTTP/2 belong to that proxy.
//!
//! The web application owns listener binding, signal policy, its runtime, and
//! dependency shutdown. [`Http1Server::serve`] optionally drives acceptance and
//! bounded local connection supervision on that runtime. Await [`Serve::run_until`]
//! before closing application services. [`ShutdownSignal`] supplies an optional
//! Ctrl-C/SIGTERM trigger; the runner also accepts application-defined futures.
//!
//! Custom accept loops can drive individual connections or register them with
//! [`Connections`]. In either case, stop admission and await shutdown before
//! closing services. Dropping a runner or connection set does not await I/O.
//!
//! ```no_run
//! use std::io;
//! use karmaio::net::tcp::TcpListener;
//! use kavi::{Application, foundation::server::{Http1Server, ShutdownSignal}};
//!
//! async fn serve(application: Application) -> io::Result<()> {
//!     let stop = ShutdownSignal::new()?;
//!     let listener = TcpListener::bind("127.0.0.1:8080".parse().unwrap())?;
//!     Http1Server::new().serve(listener, application).run_until(stop.wait()).await
//! }
//! ```

mod connections;
pub mod http1;
mod serve;
mod shutdown;

pub use connections::{Completion, ConnectionId, Connections};
pub use http1::{Connection, Http1Server};
pub use serve::{ConnectionResult, Serve};
pub use shutdown::ShutdownSignal;
pub use vakya::connection::{ConnectionControl, ConnectionOutcome};
pub use vakya::error::Error as ConnectionError;
