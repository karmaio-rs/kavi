use std::{net::Ipv6Addr, time::Duration};

use crate::{
    dispatch::{Middleware, Next},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, RequestMetadata, Response, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
            ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN, VARY,
        },
        response::empty,
    },
};

type OriginPredicate = dyn Fn(&str, &Request) -> bool;

enum Origins {
    Deny,
    Any,
    List(Vec<HeaderValue>),
    Predicate(Box<OriginPredicate>),
}

enum OriginConfig {
    Deny,
    Any,
    List(Vec<String>),
    Predicate(Box<OriginPredicate>),
}

/// A single, authoritative cross-origin policy for an application.
///
/// Register before routing and authentication. Accepted preflight returns 204
/// without dispatching; policy denial returns 403 and malformed CORS input 400.
/// Actual requests dispatch even when their origin is denied. CORS controls
/// browser response access, and does not replace authentication or CSRF policy.
/// Application metadata failures retain their original terminal response.
///
/// This policy owns the six CORS response grant fields and replaces downstream
/// grants even when no Origin is supplied. It merges Vary on every response.
/// Nested policies do not compose: outer preflight bypasses inner middleware.
/// Put RequestId and SecureHeaders outside Cors to cover its early responses.
/// Predicates needing proxy metadata or rewritten paths require those policies
/// before Cors; their early responses then bypass this policy.
pub struct Cors {
    origins: Origins,
    methods: Vec<Method>,
    headers: Vec<HeaderName>,
    method_value: Option<HeaderValue>,
    header_value: Option<HeaderValue>,
    expose_value: Option<HeaderValue>,
    credentials: bool,
    max_age: Option<HeaderValue>,
}

/// Builds a restrictive CORS policy and validates it before serving.
///
/// Defaults deny all origins, omit credentials/max-age, and use empty method and
/// header lists. GET never implicitly adds HEAD. List changes replace earlier
/// configuration and duplicate values are removed during build.
#[must_use]
pub struct CorsBuilder {
    origins: OriginConfig,
    methods: Vec<Method>,
    headers: Vec<HeaderName>,
    exposed: Vec<HeaderName>,
    credentials: bool,
    max_age: Option<Duration>,
}

/// Invalid CORS configuration, rejected before accepting requests.
#[derive(Debug, thiserror::Error)]
pub enum CorsConfigError {
    /// An entry is not a serialized ASCII origin or the literal `null`.
    #[error("invalid serialized CORS origin: {0}")]
    InvalidOrigin(String),
    /// Public wildcard grants cannot carry credentials.
    #[error("wildcard CORS origins cannot allow credentials")]
    WildcardWithCredentials,
    /// Method lists must contain finite, explicit method tokens.
    #[error("wildcard CORS methods are unsupported")]
    WildcardMethod,
    /// Request/exposed header lists must contain explicit names.
    #[error("wildcard CORS header lists are unsupported")]
    WildcardHeader,
    /// Browsers cannot expose cookie response fields to scripts.
    #[error("forbidden CORS exposed header: {0}")]
    ForbiddenExposedHeader(HeaderName),
}

impl Cors {
    /// Begins configuration with no cross-origin grants.
    pub fn builder() -> CorsBuilder {
        CorsBuilder {
            origins: OriginConfig::Deny,
            methods: Vec::new(),
            headers: Vec::new(),
            exposed: Vec::new(),
            credentials: false,
            max_age: None,
        }
    }

    fn allows_origin(&self, origin: &HeaderValue, request: &Request) -> bool {
        match &self.origins {
            Origins::Deny => false,
            Origins::Any => true,
            Origins::List(origins) => origins.contains(origin),
            Origins::Predicate(predicate) => predicate(origin.to_str().expect("validated ASCII origin"), request),
        }
    }

    fn decorate(&self, response: &mut Response, origin: Option<&HeaderValue>, preflight: bool, options: bool) {
        let headers = response.headers_mut();
        for name in [
            ACCESS_CONTROL_ALLOW_ORIGIN,
            ACCESS_CONTROL_ALLOW_CREDENTIALS,
            ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_HEADERS,
            ACCESS_CONTROL_EXPOSE_HEADERS,
            ACCESS_CONTROL_MAX_AGE,
        ] {
            headers.remove(name);
        }
        merge_vary(headers, options);
        let Some(origin) = origin else { return };
        let origin = if matches!(self.origins, Origins::Any) {
            HeaderValue::from_static("*")
        } else {
            origin.clone()
        };
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        if self.credentials {
            headers.insert(ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
        }
        if preflight {
            for (name, value) in [
                (ACCESS_CONTROL_ALLOW_METHODS, &self.method_value),
                (ACCESS_CONTROL_ALLOW_HEADERS, &self.header_value),
                (ACCESS_CONTROL_MAX_AGE, &self.max_age),
            ] {
                if let Some(value) = value {
                    headers.insert(name, value.clone());
                }
            }
        } else if let Some(value) = &self.expose_value {
            headers.insert(ACCESS_CONTROL_EXPOSE_HEADERS, value.clone());
        }
    }
}

impl CorsBuilder {
    /// Grants exact serialized origins, without case folding or URL normalization.
    ///
    /// Supply browser-serialized ASCII origins, without a trailing slash. User
    /// information, paths, queries, fragments, and origin lists are rejected.
    /// Opaque origins require an explicit `null` entry.
    pub fn origins<I, T>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<str>,
    {
        self.origins = OriginConfig::List(origins.into_iter().map(|origin| origin.as_ref().to_owned()).collect());
        self
    }

    /// Grants every valid origin using `*`, including opaque `null` origins.
    ///
    /// Build rejects this mode when credentials are enabled.
    pub fn allow_any_origin(mut self) -> Self {
        self.origins = OriginConfig::Any;
        self
    }

    /// Selects origins using a synchronous worker-local predicate.
    ///
    /// The origin is already validated ASCII, including literal `null` when
    /// supplied. Accepted origins are echoed exactly. No Send/Sync bounds or
    /// asynchronous policy lookups are imposed.
    pub fn origin_predicate(mut self, predicate: impl Fn(&str, &Request) -> bool + 'static) -> Self {
        self.origins = OriginConfig::Predicate(Box::new(predicate));
        self
    }

    /// Replaces the finite, case-sensitive preflight method allowlist.
    pub fn allow_methods(mut self, methods: impl IntoIterator<Item = Method>) -> Self {
        self.methods = methods.into_iter().collect();
        self
    }

    /// Replaces the finite preflight request-header allowlist.
    ///
    /// Authorization must be listed explicitly when needed. Names are compared
    /// case-insensitively through HeaderName; wildcard lists are unsupported.
    pub fn allow_headers(mut self, headers: impl IntoIterator<Item = HeaderName>) -> Self {
        self.headers = headers.into_iter().collect();
        self
    }

    /// Replaces the explicit response-header exposure list.
    ///
    /// Wildcards, Set-Cookie, and Set-Cookie2 are rejected at build time.
    pub fn expose_headers(mut self, headers: impl IntoIterator<Item = HeaderName>) -> Self {
        self.exposed = headers.into_iter().collect();
        self
    }

    /// Selects credentialed grants. Disabled by default.
    pub fn allow_credentials(mut self, enabled: bool) -> Self {
        self.credentials = enabled;
        self
    }

    /// Sets preflight cache lifetime in whole seconds, truncating fractions.
    ///
    /// Browsers may impose their own shorter upper bound.
    pub fn max_age(mut self, duration: Duration) -> Self {
        self.max_age = Some(duration);
        self
    }

    /// Validates and precomputes immutable response grants.
    ///
    /// # Errors
    /// Returns invalid origins, wildcard combinations, or forbidden exposure.
    pub fn build(mut self) -> Result<Cors, CorsConfigError> {
        let origins = match self.origins {
            OriginConfig::Deny => Origins::Deny,
            OriginConfig::Any if self.credentials => return Err(CorsConfigError::WildcardWithCredentials),
            OriginConfig::Any => Origins::Any,
            OriginConfig::Predicate(predicate) => Origins::Predicate(predicate),
            OriginConfig::List(origins) => {
                let mut values = Vec::with_capacity(origins.len());
                for origin in origins {
                    if !valid_origin(&origin) {
                        return Err(CorsConfigError::InvalidOrigin(origin));
                    }
                    let value = HeaderValue::try_from(origin).expect("validated ASCII origin");
                    if !values.contains(&value) {
                        values.push(value);
                    }
                }
                Origins::List(values)
            }
        };
        if self.methods.iter().any(|method| method.as_str() == "*") {
            return Err(CorsConfigError::WildcardMethod);
        }
        if self.headers.iter().chain(&self.exposed).any(|name| name == "*") {
            return Err(CorsConfigError::WildcardHeader);
        }
        if let Some(name) = self
            .exposed
            .iter()
            .find(|name| matches!(name.as_str(), "set-cookie" | "set-cookie2"))
        {
            return Err(CorsConfigError::ForbiddenExposedHeader(name.clone()));
        }
        dedup(&mut self.methods);
        dedup(&mut self.headers);
        dedup(&mut self.exposed);
        Ok(Cors {
            origins,
            method_value: list_value(self.methods.iter().map(Method::as_str)),
            header_value: list_value(self.headers.iter().map(HeaderName::as_str)),
            expose_value: list_value(self.exposed.iter().map(HeaderName::as_str)),
            methods: self.methods,
            headers: self.headers,
            credentials: self.credentials,
            max_age: self.max_age.map(|duration| HeaderValue::from(duration.as_secs())),
        })
    }
}

impl<S> Middleware<S> for Cors {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        let options = request.method() == Method::OPTIONS;
        let parsed = parse_request(request);
        // Initial metadata rejection outranks a preflight decision or parse error.
        let initialized = request.extensions().get::<RequestMetadata>().is_some();
        let allowed_origin = parsed
            .as_ref()
            .ok()
            .and_then(|parsed| parsed.origin.as_ref())
            .filter(|origin| self.allows_origin(origin, request));
        let (mut response, grant, preflight) = if !initialized {
            (next.run(request).await, allowed_origin, false)
        } else {
            match &parsed {
                Err(()) => (empty(StatusCode::BAD_REQUEST), None, false),
                Ok(parsed) => {
                    if let Some(method) = &parsed.method {
                        if allowed_origin.is_some()
                            && self.methods.contains(method)
                            && parsed.headers.iter().all(|name| self.headers.contains(name))
                        {
                            (empty(StatusCode::NO_CONTENT), allowed_origin, true)
                        } else {
                            (empty(StatusCode::FORBIDDEN), None, false)
                        }
                    } else {
                        (next.run(request).await, allowed_origin, false)
                    }
                }
            }
        };
        self.decorate(&mut response, grant, preflight, options);
        response
    }
}

struct CorsRequest {
    origin: Option<HeaderValue>,
    method: Option<Method>,
    headers: Vec<HeaderName>,
}

fn parse_request(request: &Request) -> Result<CorsRequest, ()> {
    let origin = singleton(request.headers(), ORIGIN)?;
    if let Some(origin) = origin
        && !valid_origin(origin.to_str().map_err(|_| ())?)
    {
        return Err(());
    }
    let mut parsed = CorsRequest {
        origin: origin.cloned(),
        method: None,
        headers: Vec::new(),
    };
    if parsed.origin.is_none() || request.method() != Method::OPTIONS {
        return Ok(parsed);
    }
    let Some(method) = singleton(request.headers(), ACCESS_CONTROL_REQUEST_METHOD)? else {
        return Ok(parsed);
    };
    parsed.method = Some(Method::from_bytes(method.as_bytes()).map_err(|_| ())?);
    for value in request.headers().get_all(ACCESS_CONTROL_REQUEST_HEADERS) {
        let value = value.to_str().map_err(|_| ())?.trim_matches([' ', '\t']);
        if value.is_empty() {
            continue;
        }
        for token in value.split(',') {
            let name = HeaderName::from_bytes(token.trim_matches([' ', '\t']).as_bytes()).map_err(|_| ())?;
            if !parsed.headers.contains(&name) {
                parsed.headers.push(name);
            }
        }
    }
    Ok(parsed)
}

fn singleton(headers: &HeaderMap, name: HeaderName) -> Result<Option<&HeaderValue>, ()> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(());
    }
    Ok(value)
}

// Validate an origin tuple, not a general URL. Match its serialization exactly.
fn valid_origin(origin: &str) -> bool {
    if origin == "null" {
        return true;
    }
    if !origin.is_ascii() {
        return false;
    }
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    if !scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        || authority.parse::<crate::http::uri::Authority>().is_err()
        || authority.bytes().any(|byte| {
            byte <= b' '
                || matches!(
                    byte,
                    b'@' | b'/' | b'?' | b'#' | b',' | b'%' | b'\\' | b'<' | b'>' | b'^' | b'|'
                )
        })
    {
        return false;
    }
    let (host, port) = if let Some(ip) = authority.strip_prefix('[') {
        let Some((ip, suffix)) = ip.split_once(']') else {
            return false;
        };
        if ip.parse::<Ipv6Addr>().is_err() {
            return false;
        }
        if suffix.is_empty() {
            return true;
        }
        let Some(port) = suffix.strip_prefix(':') else {
            return false;
        };
        (ip, Some(port))
    } else {
        if authority.contains(['[', ']']) {
            return false;
        }
        authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)))
    };
    !host.is_empty()
        && port.is_none_or(|port| {
            !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) && port.parse::<u16>().is_ok()
        })
}

fn dedup<T: PartialEq>(values: &mut Vec<T>) {
    let mut index = 0;
    while index < values.len() {
        if values[..index].contains(&values[index]) {
            values.remove(index);
        } else {
            index += 1;
        }
    }
}

fn list_value<'a>(values: impl Iterator<Item = &'a str>) -> Option<HeaderValue> {
    let value = values.collect::<Vec<_>>().join(", ");
    (!value.is_empty()).then(|| HeaderValue::try_from(value).expect("validated HTTP tokens"))
}

fn merge_vary(headers: &mut HeaderMap, options: bool) {
    let mut names = Vec::new();
    let mut valid = true;
    for value in headers.get_all(VARY) {
        for token in value.as_bytes().split(|byte| *byte == b',') {
            let token = token.trim_ascii();
            if token == b"*" {
                return;
            }
            if token.is_empty() {
                continue;
            }
            match HeaderName::from_bytes(token) {
                Ok(name) if !names.contains(&name) => names.push(name),
                Ok(_) => {}
                Err(_) => valid = false,
            }
        }
    }
    let additions = if options {
        &[ORIGIN, ACCESS_CONTROL_REQUEST_METHOD, ACCESS_CONTROL_REQUEST_HEADERS][..]
    } else {
        &[ORIGIN][..]
    };
    if valid {
        for name in additions {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        headers.insert(
            VARY,
            list_value(names.iter().map(HeaderName::as_str)).expect("Origin is always included"),
        );
    } else {
        // Preserve malformed downstream values rather than silently removing them.
        for name in additions {
            if !names.contains(name) {
                headers.append(VARY, HeaderValue::from_static(name.as_str()));
            }
        }
    }
}
