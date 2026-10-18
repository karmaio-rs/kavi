use crate::{
    Error,
    dispatch::{Middleware, Next},
    http::{Method, Request, Response, StatusCode, header::HeaderName},
};

/// Removes trailing slashes before routing, preserving the root and query string.
///
/// This rewrites rather than redirects. Interior repeated slashes and encoded
/// path bytes are preserved. Register it on Application, before route selection.
#[derive(Clone, Copy, Default)]
pub struct NormalizePath;

impl<S> Middleware<S> for NormalizePath {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        if let Err(error) = normalize(request) {
            return error.into_response();
        }
        next.run(request).await
    }
}

fn normalize(request: &mut Request) -> Result<(), Error> {
    let path = request.uri().path();
    let normalized = path.trim_end_matches('/');
    let normalized = if normalized.is_empty() { "/" } else { normalized };
    if path.starts_with('/') && normalized != path {
        let mut target = normalized.to_owned();
        if let Some(query) = request.uri().query() {
            target.push('?');
            target.push_str(query);
        }
        let mut parts = request.uri().clone().into_parts();
        parts.path_and_query = Some(target.parse().map_err(Error::internal)?);
        *request.uri_mut() = crate::http::Uri::from_parts(parts).map_err(Error::internal)?;
    }
    Ok(())
}

/// Overrides POST routing from one explicit header, without reading the body.
///
/// Only PUT, PATCH, and DELETE are accepted. Other incoming methods are unchanged.
/// Ambiguous or invalid override values produce a 400 response. Browser form-body
/// overrides are excluded because they require bounded buffering and replay.
pub struct MethodOverride {
    header: HeaderName,
}

impl MethodOverride {
    /// Selects the header carrying the method override.
    pub fn new(header: HeaderName) -> Self {
        Self { header }
    }
}

impl Default for MethodOverride {
    fn default() -> Self {
        Self::new(HeaderName::from_static("x-http-method-override"))
    }
}

impl<S> Middleware<S> for MethodOverride {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        if request.method() == Method::POST {
            let mut values = request.headers().get_all(&self.header).iter();
            if let Some(value) = values.next() {
                if values.next().is_some() {
                    return Error::status(StatusCode::BAD_REQUEST).into_response();
                }
                let method = match Method::from_bytes(value.as_bytes()) {
                    Ok(method) => method,
                    Err(error) => return Error::with_status(StatusCode::BAD_REQUEST, error).into_response(),
                };
                if !matches!(method, Method::PUT | Method::PATCH | Method::DELETE) {
                    return Error::status(StatusCode::BAD_REQUEST).into_response();
                }
                *request.method_mut() = method;
            }
        }
        next.run(request).await
    }
}
