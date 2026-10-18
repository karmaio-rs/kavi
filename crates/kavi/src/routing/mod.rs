//! Method-aware routing, constraints, grouped middleware, and URL generation.
//!
//! [`Router`] collects declarations and [`Router::build`] validates them into an
//! immutable [`RoutingTable`]. Direct method registration returns a [`Route`]
//! configuration handle. [`Group`] supplies inherited path and name
//! prefixes and capture constraints. Register group middleware inside the group
//! closure. The Kavi-owned radix matcher
//! uses structural specificity within the requested method. At an equivalent
//! structure, more constrained candidates precede plain captures; ties follow
//! registration order. Constraints inspect once-decoded capture values.
//!
//! Middleware enters in table, outer group, inner group, and route order, then
//! reaches the controller. Code after the continuation returns unwinds in reverse.
//! Group and route middleware run only on valid selected routes. Merged routers
//! preserve their middleware around imported endpoints, including their fallback.
//! The outer table's middleware also wraps misses and routing errors. Controllers
//! and middleware are owned once and may be local to a worker.
//! Register table middleware with [`Router::middleware`] before building. The
//! compiler flattens all middleware levels into one ordered chain per endpoint.
//! `Router<S>` declares the application context required by its controllers and
//! middleware. Registration checks provider bounds; [`Router::build_with_dependencies`]
//! binds one application-owned `Rc<S>` across all declarations, including groups
//! and merges. Retain that context outside routing for jobs, tasks, and resource
//! shutdown. [`Router::build`] compiles declarations requiring no dependencies.
//! The compiled table erases dependency types after binding.
//! It recognizes routes and retains their compiled registrations; it never runs
//! controllers or middleware. [`crate::Application`] owns dispatch, metadata
//! installation, framework rejection responses, and final response processing.
//! String routes and optional [`TypedPath`] values share the same runtime
//! pattern representation, encoding, and constraint semantics. Both declaration
//! styles accept Unicode literals and preserve valid percent escapes.
//! [`RouteParams`] iterates borrowed decoded captures. Unchanged values refer
//! to a snapshot of the original URI; percent-decoded values own their strings.
//! Generate full mounted URIs with [`RoutingTable::path_for`] and
//! [`RoutingTable::url_for`], or generate independently with [`Pattern`] and
//! typed values. Typed prefixes are concrete, explicit paths.
//!
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
pub(crate) use table::RecognitionError;
pub use table::RoutingTable;
pub use typed_path::TypedPath;

#[doc(hidden)]
pub use typed_path::__private;
