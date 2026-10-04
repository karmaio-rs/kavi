//! Method-aware routing, constraints, grouped middleware, and URL generation.
//!
//! [`Router`] collects declarations and [`Router::build`] validates them into an
//! immutable [`RoutingTable`]. Direct method registration returns a borrowed
//! [`Route`] for configuration. [`Group`] supplies inherited path and name
//! prefixes, capture constraints, and middleware. The Kavi-owned radix matcher
//! uses structural specificity within the requested method. At an equivalent
//! structure, more constrained candidates precede plain captures; ties follow
//! registration order. Constraints inspect once-decoded capture values.
//!
//! Middleware enters in table, outer group, inner group, and route order, then
//! reaches the controller. Code after the continuation returns unwinds in reverse.
//! Group and route middleware run only on valid selected routes; table middleware
//! also wraps fallbacks, misses, and routing errors. Controllers and middleware
//! are owned once and may be local to a worker.
//! Register table middleware with [`Router::middleware`] before building. The
//! compiler flattens all middleware levels into one ordered chain per endpoint.
//! String routes and optional [`TypedPath`] values share the same runtime
//! pattern representation, encoding, and constraint semantics. Both declaration
//! styles accept Unicode literals and preserve valid percent escapes.
//! [`RouteParams`] iterates borrowed decoded captures. Unchanged values refer
//! to a snapshot of the original URI; percent-decoded values own their strings.
//! Generate full mounted URIs with [`RoutingTable::path_for`] and
//! [`RoutingTable::url_for`], or generate independently with [`Pattern`] and
//! typed values. Typed prefixes are concrete, explicit paths.
//!
//! ```text
//! use kavi::{Error, http::{Request, RequestExt, Response, response::text}};
//! use kavi::dispatch::Next;
//! use kavi::routing::{Group, Router, TypedPath};
//!
//! #[derive(TypedPath)]
//! #[typed_path("/users/{id}")]
//! struct UserPath {
//!     #[typed_path(constraint = r"[0-9]+")]
//!     id: u64,
//! }
//!
//! async fn log_route(request: Request, next: Next<'_>) -> Result<Response, Error> {
//!     eprintln!("Route: {:?}", request.route_name());
//!     next.run(request).await
//! }
//!
//! async fn show(request: Request) -> Result<Response, Error> {
//!     let UserPath { id } = request.extract_head::<UserPath>().await?;
//!     Ok(text(format!("User {id}")))
//! }
//!
//! let mut router = Router::new();
//! router.middleware(log_route);
//! router.group(Group::new("/api").name_prefix("users."), |routes| {
//!     routes.typed_get::<UserPath>(show).name("show");
//! });
//! let table = router.build()?;
//! let path = UserPath { id: 42 }.to_uri_with_prefix("/api")?;
//! let named = table.path_for("users.show", &[("id", "42")])?;
//! let url = UserPath { id: 42 }.to_url_with_prefix("/api", &"https://example.com".parse()?)?;
//! ```
//!
//! Behavioral coverage is deferred for grammar errors, literal and capture
//! encoding, capture counts beyond 25, constraint ordering and backtracking,
//! tuple extraction beneath groups, HEAD/405 behavior, middleware and fallbacks,
//! and typed/untyped generation consistency. The `typed_paths` example is
//! checked without executing it; the full coverage list is in `UPSTREAM.md`.

mod error;
mod group;
mod matcher;
mod params;
mod pattern;
mod route;
mod router;
mod table;
mod typed_path;

pub use error::{BuildError, UrlError};
pub use group::Group;
pub use kavi_macros::TypedPath;
pub(crate) use params::Param;
pub use params::RouteParams;
pub use pattern::{Pattern, PatternError};
pub use route::{Route, RouteInfo};
pub use router::Router;
pub use table::RoutingTable;
pub use typed_path::TypedPath;

#[doc(hidden)]
pub use typed_path::__private;
