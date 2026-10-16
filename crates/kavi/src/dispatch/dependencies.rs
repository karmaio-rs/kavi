//! Typed access to application dependencies during dispatch.
//!
//! The application initializes and owns its dependency root. Route declarations
//! check provider bounds before compilation binds controllers and middleware to
//! one shared `Rc<S>` through [`crate::routing::Router::build_with_dependencies`].
//! The application can retain that owner for jobs, tasks, and resource shutdown.
//! Runtime-bound services must be constructed on their worker. Individual fields
//! may contain cross-thread handles when the application requires them.
//! Injection borrows existing values by default. An owned macro parameter marked
//! `#[dependency(clone)]` explicitly clones its provided value per invocation.
//! Shared fields such as `Rc<T>` are otherwise exposed as `&Rc<T>`. Owned handles
//! can be moved into streaming bodies or worker-local tasks when work must outlive
//! an invocation. Direct adapter calls receive their arguments without injection.
//! Shut down asynchronous resources only after their in-flight work settles;
//! dropping the dependency
//! owner does not perform an asynchronous shutdown.
//!
//! Configuration is ordinary typed data in the root, initialized and validated
//! by the application before routing is built. Projection neither loads nor
//! validates it. Optional dependencies use `Option<T>` fields and parameters;
//! obtaining an `Option<&T>` or applying defaults happens inside the handler.
//! Request-local values remain in request extensions and the extractor system.
//! [`crate::http::RequestExt::extract_head_with`] and
//! [`crate::http::RequestExt::extract_body_with`] accept an explicit borrowed
//! service or provider root. Their outputs can own acquired resources or borrow
//! that context independently of the request. HTTP extension insertion still
//! requires `Clone`, `Send`, `Sync`, and `'static`; service-aware extraction does
//! not add local request storage or define transaction completion policies.

/// Borrows an application dependency by its exact type.
///
/// Implementations should return an existing value without performing service
/// discovery or initialization. Derive [`crate::Dependencies`] to expose struct
/// fields automatically. Use distinct newtypes for different roles of the same
/// underlying service. Neither providers nor dependencies need to be `Send`,
/// `Sync`, or `Clone`.
/// The derive is shallow: a field provides exactly its declared type. Nested
/// services and unsized trait-object views can be exposed with manual projections
/// returning references. Generic projections must satisfy Rust's coherence rules;
/// distinct type parameters and aliases do not guarantee distinct service types.
///
/// A root can implement `Provide<Self>` manually, or opt in through the derive's
/// `#[dependencies(root)]` option, to borrow the entire application context.
/// The derive's default exposes only fields. Providers do not need `'static` for
/// standalone borrowing; a dependency root stored in routing must be `'static`.
#[diagnostic::on_unimplemented(message = "`{Self}` does not provide the application dependency `{T}`")]
pub trait Provide<T: ?Sized> {
    /// Returns the dependency borrowed for the duration of this access.
    fn provide(&self) -> &T;
}
