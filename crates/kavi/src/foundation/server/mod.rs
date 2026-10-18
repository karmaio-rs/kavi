//! Caller-driven application servers and local connection supervision helpers.
//!
//! Serving is limited to plain HTTP/1 behind a reverse proxy. Public connections,
//! TLS termination, and client-facing HTTP/2 belong to that proxy.
//!
//! The web application owns listeners, its accept loop, signal handling, runtime,
//! and dependency shutdown. These types neither bind sockets nor start an accept
//! loop. Explicitly drive each Connection, or register it with Connections and
//! drive that set. Stop admission and await shutdown before closing services.

mod connections;
pub mod http1;

pub use connections::{Completion, ConnectionId, Connections};
pub use http1::{Connection, Http1Server};
pub use vakya::connection::{ConnectionControl, ConnectionOutcome};
pub use vakya::error::Error as ConnectionError;
