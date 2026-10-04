use std::panic::Location;

use super::Pattern;
use crate::{
    dispatch::{ErasedController, ErasedMiddleware, Middleware},
    http::Method,
};

pub(super) enum DeclarationPattern {
    Text(String),
    Typed(Pattern),
}

/// A registered route, borrowed from its router for further configuration.
///
/// One controller serves every explicitly registered method. Middleware applies
/// to the whole declaration, including implicit HEAD for GET. Neither the
/// controller nor middleware is cloned or required to be `Send` or `Sync`.
pub struct Route {
    pub(super) pattern: DeclarationPattern,
    pub(super) constraints: Vec<(String, String)>,
    pub(super) methods: Vec<Method>,
    pub(super) controller: Box<dyn ErasedController>,
    pub(super) middleware: Vec<Box<dyn ErasedMiddleware>>,
    pub(super) name: Option<String>,
    pub(super) location: &'static Location<'static>,
}

impl Route {
    /// Assigns a name, qualified by the containing groups' name prefixes.
    ///
    /// The final name must be nonempty and unique across all route declarations.
    /// Names support generation through the compiled routing table.
    /// Calling this again replaces the previously assigned name.
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = Some(name.into());
        self
    }

    /// Sets or replaces a capture's full-value Rust regex, including inherited captures.
    ///
    /// Unknown captures and invalid expressions fail in [`super::Router::build`].
    pub fn constraint(&mut self, name: impl Into<String>, regex: impl Into<String>) -> &mut Self {
        set_constraint(&mut self.constraints, name.into(), regex.into());
        self
    }

    /// Adds middleware around this route in registration order.
    ///
    /// It runs after table and group middleware, only when this route is selected
    /// and its captures have been decoded successfully.
    pub fn middleware<M: Middleware>(&mut self, middleware: M) -> &mut Self {
        self.middleware.push(Box::new(middleware));
        self
    }
}

/// Read-only metadata for one compiled route declaration.
///
/// A multi-method registration has one catalog entry. Implicit HEAD and the root
/// fallback are not separate catalog entries. Executable objects remain private.
#[derive(Debug)]
pub struct RouteInfo {
    pub(super) pattern: Pattern,
    pub(super) methods: Vec<Method>,
    pub(super) name: Option<String>,
    pub(super) location: &'static Location<'static>,
}

impl RouteInfo {
    /// Returns the URI-ready matching pattern, including group prefixes.
    ///
    #[inline]
    pub fn path(&self) -> &str {
        self.pattern.as_str()
    }

    /// Returns the full mounted pattern and its final constraints.
    #[inline]
    pub fn pattern(&self) -> &Pattern {
        &self.pattern
    }

    /// Returns explicitly registered methods in registration order.
    #[inline]
    pub fn methods(&self) -> &[Method] {
        &self.methods
    }

    /// Returns the fully qualified route name, when assigned.
    #[inline]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the source location where the route was registered.
    #[inline]
    pub fn location(&self) -> &'static Location<'static> {
        self.location
    }
}

pub(super) fn set_constraint(constraints: &mut Vec<(String, String)>, name: String, regex: String) {
    if let Some((_, prior)) = constraints.iter_mut().find(|(prior, _)| *prior == name) {
        *prior = regex;
    } else {
        constraints.push((name, regex));
    }
}
