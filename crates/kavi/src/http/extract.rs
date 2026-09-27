//! Typed extraction from request metadata and bodies.
//!
//! [`FromRequestHead`] borrows a request without consuming its body.
//! [`FromRequestBody`] receives the remaining body by ownership. Invoke them
//! through [`super::RequestExt::extract_head`] and [`super::RequestExt::extract_body`].
//! Extractor failures remain typed and can be converted into [`crate::Error`].

use serde::de::DeserializeOwned;

use super::{
    Body, HeaderMap, Request, RoutingInfo,
    body::{CollectError, DEFAULT_BODY_LIMIT, Error as BodySourceError},
};

/// Extracts and deserializes a JSON request body.
///
/// Checks that the `Content-Type` header is `application/json` before consuming the body.
/// The body is collected and deserialized into `T`.
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

impl<T: DeserializeOwned> FromRequestBody for Json<T> {
    type Error = JsonError;

    async fn from_body(request: &Request, body: Body) -> Result<Self, Self::Error> {
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

        // Collect body with the default limit
        // TODO: Make this dynamic from config later
        let collected = body.collect(DEFAULT_BODY_LIMIT).await?;
        let value = serde_json::from_slice(collected.bytes())?;
        Ok(Json(value))
    }
}

/// Extracts and deserializes a form-urlencoded request body.
///
/// Checks that the `Content-Type` header is `application/x-www-form-urlencoded` before consuming the body.
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

impl<T: DeserializeOwned> FromRequestBody for Form<T> {
    type Error = FormError;

    async fn from_body(request: &Request, body: Body) -> Result<Self, Self::Error> {
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

        let collected = body.collect(DEFAULT_BODY_LIMIT).await?;
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

impl<T: DeserializeOwned> FromRequestHead for Path<T> {
    type Error = PathError;

    #[inline]
    async fn from_head(request: &Request) -> Result<Self, Self::Error> {
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

impl<T: DeserializeOwned> FromRequestHead for Query<T> {
    type Error = QueryError;

    #[inline]
    async fn from_head(request: &Request) -> Result<Self, Self::Error> {
        let query = request.uri().query().unwrap_or("");
        let value = serde_qs::from_str(query)?;
        Ok(Query(value))
    }
}

/// Extracts the raw request body as [`bytes::Bytes`].
///
/// This consumes the remaining body. Subsequent body extraction sees an empty
/// body unless application code explicitly replaces it.
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

impl FromRequestBody for Bytes {
    type Error = BodyError;

    async fn from_body(_request: &Request, body: Body) -> Result<Self, Self::Error> {
        let collected = body.collect(DEFAULT_BODY_LIMIT).await?;
        Ok(Bytes(collected.bytes().clone()))
    }
}

/// Extracts the request body as a UTF-8 [`String`].
///
/// This consumes the remaining body. Subsequent body extraction sees an empty
/// body unless application code explicitly replaces it.
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

impl FromRequestBody for Text {
    type Error = TextError;

    async fn from_body(_request: &Request, body: Body) -> Result<Self, Self::Error> {
        let collected = body.collect(DEFAULT_BODY_LIMIT).await?;
        let s = std::str::from_utf8(collected.bytes())?;
        Ok(Text(s.to_owned()))
    }
}

/// Extracts the raw [`HeaderMap`] from the request.
///
/// This clones the headers; use [`Request::headers`] to borrow them directly.
pub struct RawHeaders(pub HeaderMap);

impl FromRequestHead for RawHeaders {
    type Error = std::convert::Infallible;

    #[inline]
    async fn from_head(request: &Request) -> Result<Self, Self::Error> {
        Ok(Self(request.headers().clone()))
    }
}

/// Extracts typed data from request metadata without consuming the body.
///
/// This trait is implemented by extractors that do **not** consume the body.
/// Extraction may await I/O, such as authentication or tenant lookup,
/// while borrowing the request immutably and preserving the body.
///
/// Built-in implementations include [`Query`], [`Path`], and [`RawHeaders`].
/// Read borrowed cookies through [`super::RequestExt::cookies`].
/// JSON extraction consumes the body instead.
/// See [`FromRequestBody`] for body-consuming extractors.
///
/// # Implementing
///
/// ```text
/// use kavi::http::extract::FromRequestHead;
///
/// struct MyExtractor;
///
/// impl FromRequestHead for MyExtractor {
///     type Error = std::convert::Infallible;
///
///     async fn from_head(request: &kavi::http::Request) -> Result<Self, Self::Error> {
///         // Read request metadata without consuming its body.
///         Ok(MyExtractor)
///     }
/// }
/// ```
#[allow(async_fn_in_trait)]
pub trait FromRequestHead: Sized {
    /// The error type returned when extraction fails.
    type Error;

    /// Extracts the value while borrowing the request immutably. The body is preserved.
    async fn from_head(request: &Request) -> Result<Self, Self::Error>;
}

/// Extracts typed data from the request body.
///
/// This trait is implemented by extractors that **consume** the body.
/// The remaining body is passed by ownership. A subsequent extraction sees an
/// empty body unless middleware or application code has explicitly replaced it.
/// The request is borrowed immutably for metadata.
/// When invoked through [`super::RequestExt::extract_body`], the request body has already been replaced with an empty body.
///
/// Built-in implementations include [`Json`], [`Form`], [`Bytes`], and [`Text`].
///
/// # Implementing
///
/// ```text
/// use kavi::http::{Body, extract::FromRequestBody};
///
/// struct MyExtractor;
///
/// impl FromRequestBody for MyExtractor {
///     type Error = kavi::http::body::CollectError<kavi::http::body::Error>;
///
///     async fn from_body(request: &kavi::http::Request, body: Body) -> Result<Self, Self::Error> {
///         let collected = body.collect(1024 * 1024).await?;
///         // ... process collected.bytes()
///         Ok(MyExtractor)
///     }
/// }
/// ```
#[allow(async_fn_in_trait)]
pub trait FromRequestBody: Sized {
    /// The error type returned when extraction fails.
    type Error;

    /// Extracts the value from the request body.
    ///
    /// The separately owned body is consumed by this call.
    /// The borrowed request retains its head and extensions throughout extraction.
    /// Trailers are available via the [`vakya::body::Collected`] result.
    async fn from_body(request: &Request, body: Body) -> Result<Self, Self::Error>;
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
