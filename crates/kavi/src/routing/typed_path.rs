use std::fmt;

use crate::http::Uri;

use super::{
    Pattern, UrlError,
    pattern::{absolute_url, with_prefix},
};

/// An optional typed route value with encoded formatting and fallible generation.
///
/// Derive with `#[derive(TypedPath)]` and `#[typed_path("/users/{id}")]`.
/// Named fields match captures exactly; tuple fields follow declaration order;
/// unit structs declare static paths. Extraction uses capture names, so group
/// captures do not shift tuple fields, and never consumes the request body.
/// `Display` and `DeserializeOwned` bounds are inferred independently.
///
/// Field constraints use `#[typed_path(constraint = r"[0-9]+")]`. Generation
/// checks the type's intrinsic constraints; named generation checks the full
/// mounted route's final constraints, including builder overrides.
///
/// `Display` encodes values without validating emptiness or constraints.
/// Formatting streams fields without allocating intermediate strings or
/// initializing the cached pattern. Generation reuses one field buffer.
/// Ordinary values encode `/` and `%`; catch-alls preserve `/` and encode `%`.
/// All generation methods validate bindings and return errors. No method checks
/// whether a competing route would take precedence over the generated URI.
///
/// Manual implementations must supply a cached pattern and generate through it.
pub trait TypedPath: fmt::Display {
    /// The declaration with URI-ready literals and unfilled captures.
    const PATH: &'static str;

    /// Returns the type's cached pattern and intrinsic constraints.
    fn pattern() -> &'static Pattern;

    /// Generates a URI from the type's fields using its intrinsic constraints.
    ///
    /// # Errors
    /// Returns formatting, empty-value, constraint, or URI construction errors.
    fn to_uri(&self) -> Result<Uri, UrlError>;

    /// Generates a path below a concrete, URI-ready absolute prefix.
    ///
    /// `/` is neutral; a route at `/` denotes the prefix base. One slash joins
    /// each boundary; other repeated and trailing slashes remain significant.
    /// Prefixes contain concrete values, not declarations or group captures.
    ///
    /// # Errors
    /// Returns generation errors or an invalid-prefix error.
    fn to_uri_with_prefix(&self, prefix: &str) -> Result<Uri, UrlError> {
        with_prefix(self.to_uri()?, prefix)
    }

    /// Generates an absolute URL using an explicit HTTP(S) origin.
    ///
    /// Request headers are never consulted. The origin must exclude user
    /// information, query, and a non-root path.
    ///
    /// # Errors
    /// Returns generation, origin, or URI construction errors.
    fn to_url(&self, origin: &Uri) -> Result<Uri, UrlError> {
        absolute_url(self.to_uri()?, origin)
    }

    /// Generates an absolute URL with an explicit concrete path prefix.
    ///
    /// # Errors
    /// Returns generation, prefix, origin, or URI construction errors.
    fn to_url_with_prefix(&self, prefix: &str, origin: &Uri) -> Result<Uri, UrlError> {
        absolute_url(self.to_uri_with_prefix(prefix)?, origin)
    }
}

/// Minimal support for generated code in downstream crates.
#[doc(hidden)]
pub mod __private {
    use std::fmt;

    use super::Pattern;
    pub use serde::de::DeserializeOwned;

    /// Constructs the cached pattern from parts validated by the derive.
    pub fn pattern(parts: &[(&str, Option<bool>)], constraints: &[(&str, &str)]) -> Pattern {
        let mut pattern = Pattern::from_parts(parts.iter().copied()).expect("derive validated route parts");
        for &(name, regex) in constraints {
            pattern
                .set_constraint(name, regex)
                .expect("derive validated route constraints");
        }
        pattern
    }

    /// Generates from fields in capture-slot order using one reusable formatting buffer.
    pub fn to_uri(pattern: &Pattern, values: &[&dyn fmt::Display]) -> Result<crate::http::Uri, super::UrlError> {
        pattern.to_uri_values(values)
    }

    /// Streams an encoded field without allocating or applying generation validation.
    pub fn write_parameter(
        formatter: &mut fmt::Formatter<'_>,
        value: &(impl fmt::Display + ?Sized),
        catch_all: bool,
    ) -> fmt::Result {
        super::super::pattern::write_parameter(formatter, value, catch_all)
    }
}
