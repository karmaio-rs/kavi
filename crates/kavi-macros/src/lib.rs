//! Typed routes, application dependencies, and handler adapters for Kavi.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use syn::{DeriveInput, ItemFn, parse_macro_input};

mod attributes;
mod dependencies;
mod generics;
mod handler;
mod typed_path;

/// Exposes a struct's fields as borrowed application dependencies by exact type.
///
/// Use `#[dependency(skip)]` on fields that should not be injectable. Exposed
/// types must be distinct; use newtypes to distinguish service roles. No field
/// is cloned and no `Send` or `Sync` bounds are added. Type aliases that resolve
/// to the same type, or generic fields whose types can overlap, are rejected by
/// Rust's overlapping implementation checks. Existing generic bounds are preserved.
/// `#[dependencies(crate = "framework_alias")]` supports a renamed Kavi crate.
/// Add `#[dependencies(root)]` to also implement `Provide<Self>` for borrowing
/// the entire context; the default exposes fields only. Options can be combined.
/// A whole-root projection participates in the same coherence checks as fields.
///
/// Field projection is shallow and exact: an `Rc<Client>` field provides
/// `&Rc<Client>`, and an `Option<Client>` field provides `&Option<Client>`.
/// Implement `Provide<T>` manually for nested fields or trait-object views;
/// mark any derived field with a conflicting projection as skipped. Projections
/// do not initialize services, perform conversions, or load configuration.
#[proc_macro_derive(Dependencies, attributes(dependencies, dependency))]
pub fn derive_dependencies(input: TokenStream) -> TokenStream {
    dependencies::expand(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generates an automatically bound controller from a free async function.
///
/// The first parameter is a mutable Request reference; subsequent parameters are
/// shared references to application dependencies, or owned dependencies annotated
/// with `#[dependency(clone)]`. Each owned binding requires `Clone` and clones the
/// provided value once per invocation. Mutable dependency references and cloning
/// borrowed parameters are rejected. The function must return Kavi's Response
/// directly, handling business and extraction errors locally. The original name
/// becomes a zero-argument factory returning a zero-sized controller: register it
/// with `router.get(path, show())`. The returned controller's `call` method accepts the original arguments
/// for direct invocation as `show().call(request, &service).await`. Calling the
/// factory does not execute the handler or allocate. Application dependencies are
/// checked at registration and never stored in the request.
/// Direct `call` invocations receive dependencies from the caller and do not
/// project or clone them. The adapter's generated type name is an implementation
/// detail; use the factory rather than naming or constructing that type.
///
/// Type and const parameters belong to the factory and adapter; select them with
/// the factory's turbofish when they cannot be inferred. Borrow lifetimes belong
/// to `call`, which preserves lifetime-outlives bounds. Type and const declaration
/// bounds, and type-directed predicates, cannot depend on these free borrow
/// lifetimes; explicitly higher-ranked predicates are supported. Retained type
/// parameters must be `'static` when the adapter is registered. No `Send`, `Sync`,
/// or implicit `Clone` bounds are imposed.
/// Generic parameters cannot be conditionally removed; conditionally compile the
/// entire function instead.
///
/// Dependency parameters support `cfg` and `cfg_attr`; the Request parameter must
/// always be present. Disabled dependencies are omitted from both binding and `call`.
/// Their provider and clone bounds are also omitted. The clone directive itself
/// must be unconditional; conditional directives inside `cfg_attr` are rejected.
/// Dependency types must be concrete. `impl Trait`, inferred types, and inferred
/// lifetimes within dependency types are rejected; nested references require
/// explicit lifetimes, except where Rust permits higher-ranked function elision.
/// A dependency's outer shared reference may use a named call-borrow lifetime;
/// the provided target type cannot contain that free lifetime.
/// Documentation and deprecation attributes, including through `cfg_attr`, apply
/// to the public factory and `call` method. Lint levels also apply to those public
/// functions; `expect` and other behavior attributes remain on the original
/// nested function. Its body retains the original parameter patterns, generics,
/// lexical module, and bare recursive calls. Function availability propagates to
/// every generated item, including through nested `cfg_attr`.
/// `#[controller(crate = "framework_alias")]` supports a renamed Kavi crate.
#[proc_macro_attribute]
pub fn controller(args: TokenStream, input: TokenStream) -> TokenStream {
    handler::expand(
        args.into(),
        parse_macro_input!(input as ItemFn),
        handler::Kind::Controller,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// Generates automatically bound middleware from a free async function.
///
/// Parameters begin with a mutable Request reference and a consuming Next,
/// followed by shared references or explicitly cloned owned application dependencies.
/// The return contract, generic support, generated factory, and `call` method match
/// [`controller`]. Register the returned
/// middleware with `router.middleware(authenticate())`. Dependencies may remain
/// borrowed across downstream execution. Dependency parameters may be conditionally compiled;
/// Request and Next must always be present. `#[middleware(crate = "framework_alias")]`
/// supports a renamed Kavi crate.
#[proc_macro_attribute]
pub fn middleware(args: TokenStream, input: TokenStream) -> TokenStream {
    handler::expand(
        args.into(),
        parse_macro_input!(input as ItemFn),
        handler::Kind::Middleware,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

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
/// The generated head extractor ignores its supplied context and works with both
/// unit-context and service-aware extraction calls. Captures remain owned values.
#[proc_macro_derive(TypedPath, attributes(typed_path))]
pub fn derive_typed_path(input: TokenStream) -> TokenStream {
    typed_path::expand(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
