use std::panic::Location;

use crate::http::{Method, Uri, uri};

/// A declaration error detected before a routing table becomes available.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// A path or constraint is malformed.
    #[error("invalid route pattern {path:?} at {location}: {source}")]
    Pattern {
        path: String,
        location: &'static Location<'static>,
        #[source]
        source: super::PatternError,
    },
    /// Overlapping declarations violate the matcher's structural ambiguity rules.
    #[error("conflicting {method} pattern {path:?} at {second}, previously registered at {first}")]
    Conflict {
        path: String,
        method: Method,
        first: &'static Location<'static>,
        second: &'static Location<'static>,
    },
    /// A method was repeated in one declaration or registered twice on the same pattern.
    #[error("duplicate {method} on {path:?} at {second}, previously registered at {first}")]
    DuplicateMethod {
        path: String,
        method: Method,
        first: &'static Location<'static>,
        second: &'static Location<'static>,
    },
    /// A declaration has no registered methods.
    #[error("no methods registered on {path:?} at {location}")]
    EmptyRoute {
        path: String,
        location: &'static Location<'static>,
    },
    /// A qualified route name is empty.
    #[error("empty route name on {path:?} at {location}")]
    EmptyName {
        path: String,
        location: &'static Location<'static>,
    },
    /// A resource has no name to qualify its generated action names.
    #[error("empty resource name at {location}")]
    EmptyResourceName { location: &'static Location<'static> },
    /// A qualified name identifies more than one declaration.
    #[error("duplicate route name {name:?} at {second}, previously registered at {first}")]
    DuplicateName {
        name: String,
        first: &'static Location<'static>,
        second: &'static Location<'static>,
    },
    /// A group prefix contains a catch-all parameter.
    #[error("catch-all parameter in group prefix {path:?} at {location}")]
    CatchAllPrefix {
        path: String,
        location: &'static Location<'static>,
    },
    /// A fallback was registered within a group.
    #[error("fallback at {location} must be registered at the root")]
    NestedFallback { location: &'static Location<'static> },
    /// Multiple root fallbacks were registered, including through merging.
    #[error("duplicate fallback at {second}, previously registered at {first}")]
    DuplicateFallback {
        first: &'static Location<'static>,
        second: &'static Location<'static>,
    },
}

/// A route value, binding, prefix, origin, or generated URI is invalid.
#[derive(Debug, thiserror::Error)]
pub enum UrlError {
    /// No registered route has this name.
    #[error("unknown route name {name:?}")]
    UnknownRoute { name: String },
    /// A required capture was not supplied.
    #[error("missing route parameter {name:?}")]
    MissingParameter { name: String },
    /// A binding was supplied more than once.
    #[error("duplicate route parameter {name:?}")]
    DuplicateParameter { name: String },
    /// A binding does not identify a capture.
    #[error("unknown route parameter {name:?}")]
    UnknownParameter { name: String },
    /// A capture value is empty.
    #[error("empty route parameter {name:?}")]
    EmptyParameter { name: String },
    /// A capture does not satisfy its constraint.
    #[error("route parameter {name:?} does not satisfy {constraint:?}")]
    ConstraintMismatch { name: String, constraint: String },
    /// A concrete mount prefix is invalid.
    #[error("invalid route prefix {prefix:?}")]
    InvalidPrefix { prefix: String },
    /// A field's Display implementation failed.
    #[error("failed to format a route parameter: {0}")]
    Formatting(#[from] std::fmt::Error),
    /// The generated path is not a valid URI.
    #[error("invalid generated URI: {0}")]
    InvalidUri(#[from] uri::InvalidUri),
    /// The origin is not an HTTP(S) authority with an empty or root path and no query or user information.
    #[error("invalid URL origin {origin}")]
    InvalidOrigin { origin: Uri },
    /// The generated absolute URL has invalid URI components.
    #[error("invalid generated URL: {0}")]
    InvalidUriParts(#[from] uri::InvalidUriParts),
}
