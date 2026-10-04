use crate::dispatch::{ErasedMiddleware, Middleware};

/// Path, name, and middleware configuration shared by a group of routes.
///
/// Nested groups concatenate their prefixes and inherit outer middleware.
/// Middleware wraps every route in the group and is owned once, even when the
/// group contains multiple routes or methods. Group prefixes may contain ordinary
/// parameters, but cannot contain catch-all parameters.
#[must_use]
pub struct Group {
    pub(super) prefix: String,
    pub(super) name_prefix: String,
    pub(super) middleware: Vec<Box<dyn ErasedMiddleware>>,
    pub(super) constraints: Vec<(String, String)>,
}

impl Group {
    /// Creates a group with an absolute path prefix; `/` is a neutral prefix.
    ///
    /// Unicode literals are encoded during [`super::Router::build`].
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            name_prefix: String::new(),
            middleware: Vec::new(),
            constraints: Vec::new(),
        }
    }

    /// Sets the literal prefix prepended to names assigned within this group.
    ///
    /// Separators such as `.` must be supplied explicitly. Unnamed routes stay unnamed.
    pub fn name_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_prefix = prefix.into();
        self
    }

    /// Sets or replaces a constraint on a capture in the composed group prefix.
    ///
    /// It applies to descendants and may be replaced by a nested group or route.
    /// Unknown captures and invalid expressions fail in [`super::Router::build`].
    pub fn constraint(mut self, name: impl Into<String>, regex: impl Into<String>) -> Self {
        super::route::set_constraint(&mut self.constraints, name.into(), regex.into());
        self
    }

    /// Adds middleware in registration order, before nested group and route middleware.
    pub fn middleware<M: Middleware>(mut self, middleware: M) -> Self {
        self.middleware.push(Box::new(middleware));
        self
    }
}
