//! Typed extraction from request metadata and bodies.
//!
//! [`FromRequestHead`] borrows a request without consuming its body.
//! [`FromRequestBody`] receives the remaining body by ownership. Invoke them
//! through [`super::RequestExt::extract_head`] and [`super::RequestExt::extract_body`].
//! Extractor failures remain typed and can be converted into [`crate::Error`].
//! Service-aware calls use [`super::RequestExt::extract_head_with`] and
//! [`super::RequestExt::extract_body_with`] with an explicit borrowed service or
//! dependency root. Extractors may await resource acquisition or validation.
//! Neither the context nor the extractor future needs to be `Send` or `Sync`.
//!
//! Request and context lifetimes are independent. Outputs may borrow metadata
//! from the request, services from the context, or both. A request borrow prevents
//! moving or mutating the request while that borrow remains in use. Independent
//! lifetimes let context-only outputs avoid prolonging the request borrow;
//! relationships imposed by custom implementations are still enforced by Rust.
//! Body extractors
//! may borrow metadata or context, but must own data from their consumed body.
//!
//! Buffered body extractors accept [`BufferedBodyLimit`] as their explicit context;
//! streaming multipart extraction accepts [`super::multipart::MultipartLimits`].
//! Unit context uses fixed defaults. Application services and configuration stay
//! outside the request; these extractors do not inspect extensions for limits.
//!
//! Streaming form uploads use [`super::multipart::Multipart`].

use serde::de::DeserializeOwned;

use crate::dispatch::Provide;

use super::{
    Body, HeaderMap, Request, RoutingInfo,
    body::{BufferedBodyLimit, CollectError, Error as BodySourceError},
};

/// Extracts and deserializes a JSON request body.
///
/// Checks that the `Content-Type` header is `application/json` before consuming the body.
/// The body is collected and deserialized into `T`.
/// Pass [`BufferedBodyLimit`] to explicit-context extraction to configure the
/// collection budget; unit context uses 2 MiB.
/// Use [`crate::http::response::json`] to build a JSON response.
///
/// # Errors
///
/// Returns [`JsonError::WrongContentType`] if the content type is not JSON,
/// [`JsonError::Body`] if body collection fails, or
/// [`JsonError::Deserialize`] if deserialization fails.
pub struct Json<T>(pub T);

/// Errors produced by the [`Json`] extractor.
#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    /// The request content type is not `application/json`.
    #[error("expected application/json content type")]
    WrongContentType,
    /// The body could not be collected.
    #[error("failed to collect body: {0}")]
    Body(#[from] CollectError<BodySourceError>),
    /// The body could not be deserialized into the target type.
    #[error("failed to deserialize JSON: {0}")]
    Deserialize(#[from] serde_json::Error),
}

impl<'r, 's, T: DeserializeOwned> FromRequestBody<'r, 's> for Json<T> {
    type Error = JsonError;

    #[inline]
    async fn from_body(request: &'r Request, body: Body, _: &'s ()) -> Result<Self, Self::Error> {
        Self::from_body(request, body, &BufferedBodyLimit::default()).await
    }
}

impl<'r, 's, T: DeserializeOwned> FromRequestBody<'r, 's, BufferedBodyLimit> for Json<T> {
    type Error = JsonError;

    async fn from_body(request: &'r Request, body: Body, limit: &'s BufferedBodyLimit) -> Result<Self, Self::Error> {
        // Check content type
        let content_type = request
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !content_type.parse::<mime::Mime>().is_ok_and(|m| {
            m.type_() == mime::APPLICATION && (m.subtype() == mime::JSON || m.suffix() == Some(mime::JSON))
        }) {
            return Err(JsonError::WrongContentType);
        }

        // Collection budgets remain subject to independently enforced stream ceilings.
        let collected = body.collect(limit.0).await?;
        let value = serde_json::from_slice(collected.bytes())?;
        Ok(Json(value))
    }
}

/// Extracts and deserializes a form-urlencoded request body.
///
/// Checks that the `Content-Type` header is `application/x-www-form-urlencoded` before consuming the body.
/// Pass [`BufferedBodyLimit`] to explicit-context extraction to configure the
/// collection budget; unit context uses 2 MiB.
///
/// # Errors
///
/// Returns [`FormError::WrongContentType`] if the content type is not form,
/// [`FormError::Body`] if body collection fails, or
/// [`FormError::Deserialize`] if deserialization fails.
pub struct Form<T>(pub T);

/// Errors produced by the [`Form`] extractor.
#[derive(Debug, thiserror::Error)]
pub enum FormError {
    /// The request content type is not `application/x-www-form-urlencoded`.
    #[error("expected application/x-www-form-urlencoded content type")]
    WrongContentType,
    /// The body could not be collected.
    #[error("failed to collect body: {0}")]
    Body(#[from] CollectError<BodySourceError>),
    /// The body could not be deserialized into the target type.
    #[error("failed to deserialize form: {0}")]
    Deserialize(#[from] serde_qs::Error),
}

impl<'r, 's, T: DeserializeOwned> FromRequestBody<'r, 's> for Form<T> {
    type Error = FormError;

    #[inline]
    async fn from_body(request: &'r Request, body: Body, _: &'s ()) -> Result<Self, Self::Error> {
        Self::from_body(request, body, &BufferedBodyLimit::default()).await
    }
}

impl<'r, 's, T: DeserializeOwned> FromRequestBody<'r, 's, BufferedBodyLimit> for Form<T> {
    type Error = FormError;

    async fn from_body(request: &'r Request, body: Body, limit: &'s BufferedBodyLimit) -> Result<Self, Self::Error> {
        let content_type = request
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !content_type
            .parse::<mime::Mime>()
            .is_ok_and(|m| m.essence_str() == "application/x-www-form-urlencoded")
        {
            return Err(FormError::WrongContentType);
        }

        let collected = body.collect(limit.0).await?;
        let value = serde_qs::from_bytes(collected.bytes())?;
        Ok(Form(value))
    }
}

/// Extracts typed path parameters from the matched route.
///
/// The router populates the path parameters before the controller is called.
/// This extractor deserializes them into `T`.
///
/// Supports structs and maps by parameter name, tuples and sequences in capture
/// order, and scalar values when exactly one parameter was captured. Values are
/// decoded by the router and are never interpreted as query-string syntax.
///
/// # Errors
///
/// Returns [`PathError::Missing`] if routing metadata is absent, or
/// [`PathError::Deserialize`] if deserialization fails.
pub struct Path<T>(pub T);

/// Errors produced by the [`Path`] extractor.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The request has not been routed.
    #[error("routing metadata is missing")]
    Missing,
    /// The path parameters could not be deserialized.
    #[error("failed to deserialize path parameters: {0}")]
    Deserialize(#[from] serde::de::value::Error),
}

impl<'r, 's, T: DeserializeOwned, S: ?Sized> FromRequestHead<'r, 's, S> for Path<T> {
    type Error = PathError;

    #[inline]
    async fn from_head(request: &'r Request, _: &'s S) -> Result<Self, Self::Error> {
        let params = request.extensions().get::<RoutingInfo>().ok_or(PathError::Missing)?;
        let value = T::deserialize(crate::support::serde::Pairs(params.params()))?;
        Ok(Path(value))
    }
}

/// Extracts and deserializes query string parameters.
///
/// Uses `serde_qs` for deserialization, which supports nested structures.
///
/// # Errors
///
/// Returns [`QueryError`] if deserialization fails.
pub struct Query<T>(pub T);

/// Errors produced by the [`Query`] extractor.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The query string could not be deserialized.
    #[error("failed to deserialize query string: {0}")]
    Deserialize(#[from] serde_qs::Error),
}

impl<'r, 's, T: DeserializeOwned, S: ?Sized> FromRequestHead<'r, 's, S> for Query<T> {
    type Error = QueryError;

    #[inline]
    async fn from_head(request: &'r Request, _: &'s S) -> Result<Self, Self::Error> {
        let query = request.uri().query().unwrap_or("");
        let value = serde_qs::from_str(query)?;
        Ok(Query(value))
    }
}

/// Extracts the raw request body as [`bytes::Bytes`].
///
/// This consumes the remaining body. Subsequent body extraction sees an empty
/// body unless application code explicitly replaces it.
/// Pass [`BufferedBodyLimit`] to explicit-context extraction to configure the
/// collection budget; unit context uses 2 MiB.
///
/// # Errors
///
/// Returns [`BodyError`] if body collection fails.
pub struct Bytes(pub bytes::Bytes);

/// Errors produced by the [`Bytes`] extractor.
#[derive(Debug, thiserror::Error)]
pub enum BodyError {
    /// The body could not be collected.
    #[error("failed to collect body: {0}")]
    Collect(#[from] CollectError<BodySourceError>),
}

impl<'r, 's> FromRequestBody<'r, 's> for Bytes {
    type Error = BodyError;

    #[inline]
    async fn from_body(request: &'r Request, body: Body, _: &'s ()) -> Result<Self, Self::Error> {
        Self::from_body(request, body, &BufferedBodyLimit::default()).await
    }
}

impl<'r, 's> FromRequestBody<'r, 's, BufferedBodyLimit> for Bytes {
    type Error = BodyError;

    async fn from_body(_: &'r Request, body: Body, limit: &'s BufferedBodyLimit) -> Result<Self, Self::Error> {
        let collected = body.collect(limit.0).await?;
        Ok(Bytes(collected.into_parts().0))
    }
}

/// Extracts the request body as a UTF-8 [`String`].
///
/// This consumes the remaining body. Subsequent body extraction sees an empty
/// body unless application code explicitly replaces it.
/// Pass [`BufferedBodyLimit`] to explicit-context extraction to configure the
/// collection budget; unit context uses 2 MiB.
///
/// # Errors
///
/// Returns [`TextError::Body`] if body collection fails,
/// or [`TextError::Utf8`] if the body is not valid UTF-8.
pub struct Text(pub String);

/// Errors produced by the [`Text`] extractor.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// The body could not be collected.
    #[error("failed to collect body: {0}")]
    Body(#[from] CollectError<BodySourceError>),
    /// The body is not valid UTF-8.
    #[error("body is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

impl<'r, 's> FromRequestBody<'r, 's> for Text {
    type Error = TextError;

    #[inline]
    async fn from_body(request: &'r Request, body: Body, _: &'s ()) -> Result<Self, Self::Error> {
        Self::from_body(request, body, &BufferedBodyLimit::default()).await
    }
}

impl<'r, 's> FromRequestBody<'r, 's, BufferedBodyLimit> for Text {
    type Error = TextError;

    async fn from_body(_: &'r Request, body: Body, limit: &'s BufferedBodyLimit) -> Result<Self, Self::Error> {
        let collected = body.collect(limit.0).await?;
        let bytes = collected.into_parts().0;
        let text = String::from_utf8(Vec::from(bytes)).map_err(|error| error.utf8_error())?;
        Ok(Text(text))
    }
}

/// Extracts the raw [`HeaderMap`] from the request.
///
/// The default `RawHeaders` clones the headers. `RawHeaders<&HeaderMap>` borrows
/// them for the request lifetime without cloning. Both ignore the supplied context.
pub struct RawHeaders<T = HeaderMap>(pub T);

impl<'r, 's, S: ?Sized> FromRequestHead<'r, 's, S> for RawHeaders {
    type Error = std::convert::Infallible;

    #[inline]
    async fn from_head(request: &'r Request, _: &'s S) -> Result<Self, Self::Error> {
        Ok(Self(request.headers().clone()))
    }
}

impl<'r, 's, S: ?Sized> FromRequestHead<'r, 's, S> for RawHeaders<&'r HeaderMap> {
    type Error = std::convert::Infallible;

    #[inline]
    async fn from_head(request: &'r Request, _: &'s S) -> Result<Self, Self::Error> {
        Ok(Self(request.headers()))
    }
}

/// Borrows an existing service from an explicitly supplied dependency root.
///
/// Uses [`Provide`] to project the exact type `T`, including unsized trait-object
/// views. This requires neither cloning nor thread-safe services. The result
/// borrows only the supplied context and does not retain the extractor's request borrow.
/// Acquisition and validation belong in custom service-aware extractors.
pub struct Dependency<'s, T: ?Sized>(pub &'s T);

impl<'r, 's, T: ?Sized, S: Provide<T> + ?Sized> FromRequestHead<'r, 's, S> for Dependency<'s, T> {
    type Error = std::convert::Infallible;

    #[inline]
    async fn from_head(_: &'r Request, context: &'s S) -> Result<Self, Self::Error> {
        Ok(Self(context.provide()))
    }
}

/// Extracts typed data from request metadata without consuming the body.
///
/// This trait is implemented by extractors that do **not** consume the body.
/// Extraction may await I/O, such as authentication or tenant lookup,
/// while borrowing the request immutably and preserving the body.
///
/// Built-in implementations include [`Query`], [`Path`], [`RawHeaders`], and [`Dependency`].
/// With the `cookies` feature, cookie jars are also available as head extractors.
/// JSON extraction consumes the body instead.
/// See [`FromRequestBody`] for body-consuming extractors.
///
/// `S` is an explicitly supplied context, defaulting to `()`. It may be a narrow
/// service, an unsized interface, or a root satisfying [`Provide`] bounds. The
/// context is borrowed for `'s`, independently of the request's `'r` lifetime.
/// Implementations can return values borrowing either input and can await I/O.
/// Stateless implementations can be generic over any `S` and ignore it. No
/// `Clone`, `Send`, `Sync`, or `'static` bounds are required by this contract.
///
/// [`super::RequestExt::extract_head`] supplies unit context;
/// [`super::RequestExt::extract_head_with`] supplies the caller's explicit context.
/// Context values are never installed in request extensions by extraction.
#[allow(async_fn_in_trait)]
pub trait FromRequestHead<'r, 's, S: ?Sized = ()>: Sized {
    /// The error type returned when extraction fails.
    type Error;

    /// Extracts the value while borrowing the request immutably. The body is preserved.
    async fn from_head(request: &'r Request, context: &'s S) -> Result<Self, Self::Error>;
}

/// Extracts typed data from the request body.
///
/// This trait is implemented by extractors that **consume** the body.
/// The remaining body is passed by ownership. A subsequent extraction sees an
/// empty body unless middleware or application code has explicitly replaced it.
/// The request is borrowed immutably for metadata.
/// When invoked through [`super::RequestExt::extract_body`], the request body has already been replaced with an empty body.
///
/// Built-in implementations include [`Json`], [`Form`], [`Bytes`], [`Text`], and [`super::multipart::Multipart`].
///
/// `S` and the independent request and context lifetimes follow [`FromRequestHead`].
/// Outputs may borrow request metadata or context; data read from the separately
/// owned body must remain owned by the output. Extraction does not lend buffers
/// from a collected body. Implementations can await service access and body I/O
/// without `Clone`, `Send`, `Sync`, or `'static` bounds.
///
/// [`super::RequestExt::extract_body`] supplies unit context;
/// [`super::RequestExt::extract_body_with`] supplies the caller's explicit context.
#[allow(async_fn_in_trait)]
pub trait FromRequestBody<'r, 's, S: ?Sized = ()>: Sized {
    /// The error type returned when extraction fails.
    type Error;

    /// Extracts the value from the request body.
    ///
    /// The separately owned body is consumed by this call.
    /// The borrowed request retains its head and extensions throughout extraction.
    /// Trailers are available via the [`vakya::body::Collected`] result.
    async fn from_body(request: &'r Request, body: Body, context: &'s S) -> Result<Self, Self::Error>;
}

/// Errors produced when extracting a typed path parameter.
#[derive(Debug, thiserror::Error)]
pub enum ParamError {
    /// The router did not capture a parameter with this name.
    #[error("missing path parameter: {name}")]
    Missing {
        /// The parameter name that was requested.
        name: String,
    },
    /// The parameter value could not be deserialized into the target type.
    #[error("failed to deserialize path parameter {name}: {source}")]
    Deserialize {
        /// The parameter name being deserialized.
        name: String,
        /// The deserialization error.
        source: serde::de::value::Error,
    },
}
