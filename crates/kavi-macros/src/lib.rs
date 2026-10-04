//! Derives for Kavi's typed route declarations.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod typed_path;

/// Derives a typed route pattern, encoded path formatting, and metadata extraction.
///
/// Declare the path with `#[typed_path("/users/{id}")]`. Named fields must match
/// captures exactly; tuple fields follow capture order; unit structs have no
/// captures. An optional `crate = "framework_alias"` selects a renamed Kavi crate.
///
/// Malformed captures, duplicate names, mismatched fields, multiple captures
/// within a segment, and nonterminal catch-alls produce compilation errors.
/// Field constraints use `#[typed_path(constraint = r"[0-9]+")]`; invalid regexes
/// also produce compilation errors. Literal Unicode and escaped braces are encoded
/// during expansion; captured values are encoded when formatted.
#[proc_macro_derive(TypedPath, attributes(typed_path))]
pub fn derive_typed_path(input: TokenStream) -> TokenStream {
    typed_path::expand(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
