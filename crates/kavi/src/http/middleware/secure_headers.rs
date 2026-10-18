use crate::{
    dispatch::{Middleware, Next},
    http::{HeaderName, HeaderValue, Request, Response, header},
};

/// The supported X-Content-Type-Options policy.
#[derive(Clone, Copy, Debug)]
pub enum ContentTypeOptions {
    /// Prevents browsers from overriding declared content types by sniffing.
    NoSniff,
}

/// The supported X-Frame-Options policies.
#[derive(Clone, Copy, Debug)]
pub enum FrameOptions {
    /// Disallows framing by any origin.
    Deny,
    /// Allows framing only by the same origin.
    SameOrigin,
}

/// A standardized Referrer-Policy token.
#[derive(Clone, Copy, Debug)]
pub enum ReferrerPolicy {
    /// Omits the referrer on all requests.
    NoReferrer,
    /// Sends the full referrer except when downgrading transport security.
    NoReferrerWhenDowngrade,
    /// Sends the full referrer only to the same origin.
    SameOrigin,
    /// Sends only the origin.
    Origin,
    /// Sends the full referrer to the same origin and the origin elsewhere.
    OriginWhenCrossOrigin,
    /// Sends only the origin, except when downgrading transport security.
    StrictOrigin,
    /// Sends the full referrer to the same origin and the origin elsewhere,
    /// except when downgrading transport security.
    StrictOriginWhenCrossOrigin,
    /// Sends the full referrer, including across transport-security downgrades.
    UnsafeUrl,
}

impl ReferrerPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoReferrer => "no-referrer",
            Self::NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
            Self::SameOrigin => "same-origin",
            Self::Origin => "origin",
            Self::OriginWhenCrossOrigin => "origin-when-cross-origin",
            Self::StrictOrigin => "strict-origin",
            Self::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
            Self::UnsafeUrl => "unsafe-url",
        }
    }
}

/// Applies prevalidated security headers after downstream middleware returns.
///
/// The explicitly installed default sets X-Content-Type-Options to `nosniff`,
/// Referrer-Policy to `strict-origin-when-cross-origin`, and X-Frame-Options to
/// `SAMEORIGIN`. Change or omit the frame policy when cross-origin embedding is
/// intended. No CSP, permissions, isolation, or transport policy is implicit.
/// HSTS belongs at the TLS-terminating reverse proxy in this release.
///
/// Configured values replace all downstream instances. Omitted headers retain
/// downstream values; removed headers are stripped. For route-specific CSP,
/// omit global management so intentional downstream policies are preserved.
/// Register outside Cors, limits, and other short-circuiting middleware to cover
/// their responses. Response bodies and unrelated headers are untouched.
#[derive(Clone, Debug)]
pub struct SecureHeaders {
    operations: Vec<(HeaderName, Option<HeaderValue>)>,
}

/// Configures an immutable security policy, starting from the default preset.
///
/// The last operation for a name wins. This is a bounded security-header policy,
/// not a generic header-rewriting facility. Build rejects unsupported names and
/// values before serving.
#[derive(Debug)]
#[must_use]
pub struct SecureHeadersBuilder {
    operations: Vec<(HeaderName, Operation)>,
}

#[derive(Debug)]
enum Operation {
    Set(HeaderValue),
    Remove,
    Omit,
}

/// Invalid security-header configuration.
#[derive(Debug, thiserror::Error)]
pub enum SecurityHeadersConfigError {
    /// The requested header or operation is outside this policy's scope.
    #[error("unsupported security-header operation for {0}")]
    UnsupportedHeader(HeaderName),
    /// The value is non-ASCII or an unsupported default-header policy token.
    #[error("invalid security-header value for {0}")]
    InvalidValue(HeaderName),
}

impl Default for SecureHeaders {
    fn default() -> Self {
        Self {
            operations: preset().into_iter().map(|(name, value)| (name, Some(value))).collect(),
        }
    }
}

impl SecureHeaders {
    /// Begins configuration with the three-header default preset.
    pub fn builder() -> SecureHeadersBuilder {
        SecureHeadersBuilder {
            operations: preset()
                .into_iter()
                .map(|(name, value)| (name, Operation::Set(value)))
                .collect(),
        }
    }
}

impl SecureHeadersBuilder {
    /// Sets the typed X-Content-Type-Options policy.
    pub fn content_type_options(self, policy: ContentTypeOptions) -> Self {
        let ContentTypeOptions::NoSniff = policy;
        self.set(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"))
    }

    /// Sets the typed Referrer-Policy policy.
    pub fn referrer_policy(self, policy: ReferrerPolicy) -> Self {
        self.set(header::REFERRER_POLICY, HeaderValue::from_static(policy.as_str()))
    }

    /// Sets DENY or SAMEORIGIN. Obsolete ALLOW-FROM is unsupported.
    pub fn frame_options(self, policy: FrameOptions) -> Self {
        let value = match policy {
            FrameOptions::Deny => "DENY",
            FrameOptions::SameOrigin => "SAMEORIGIN",
        };
        self.set(header::X_FRAME_OPTIONS, HeaderValue::from_static(value))
    }

    /// Authoritatively replaces a supported security header.
    ///
    /// Supports the three preset headers, Content-Security-Policy and its
    /// Report-Only form, Permissions-Policy, and Cross-Origin-Resource/Opener/
    /// Embedder-Policy. Values must be ASCII. Preset headers accept their
    /// standardized tokens; Referrer-Policy also accepts comma-separated fallback
    /// tokens. Other policies receive header-syntax validation only: the caller
    /// owns their meaning. In particular, this API does not parse CSP directives
    /// or generate nonces. HSTS, CORS, and HTTP message framing are unsupported.
    pub fn set(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.replace(name, Operation::Set(value));
        self
    }

    /// Strips all instances of a supported security header, Server, or X-Powered-By.
    ///
    /// This affects Kavi's response; a reverse proxy can add its own headers.
    pub fn remove(mut self, name: HeaderName) -> Self {
        self.replace(name, Operation::Remove);
        self
    }

    /// Stops managing a supported header, preserving downstream values.
    ///
    /// Accepts the same names as remove. Use this to disable a preset or leave
    /// route-specific CSP policies untouched. This does not strip a header.
    pub fn omit(mut self, name: HeaderName) -> Self {
        self.replace(name, Operation::Omit);
        self
    }

    /// Validates and compiles the configured operations.
    ///
    /// # Errors
    /// Returns unsupported names/operations or invalid policy values.
    pub fn build(self) -> Result<SecureHeaders, SecurityHeadersConfigError> {
        let mut operations = Vec::with_capacity(self.operations.len());
        for (name, operation) in self.operations {
            let supported = supported_set(&name)
                || (!matches!(operation, Operation::Set(_)) && matches!(name.as_str(), "server" | "x-powered-by"));
            if !supported {
                return Err(SecurityHeadersConfigError::UnsupportedHeader(name));
            }
            match operation {
                Operation::Set(value) => {
                    if !valid_value(&name, &value) {
                        return Err(SecurityHeadersConfigError::InvalidValue(name));
                    }
                    operations.push((name, Some(value)));
                }
                Operation::Remove => operations.push((name, None)),
                Operation::Omit => {}
            }
        }
        Ok(SecureHeaders { operations })
    }

    fn replace(&mut self, name: HeaderName, operation: Operation) {
        if let Some((_, current)) = self.operations.iter_mut().find(|(key, _)| key == name) {
            *current = operation;
        } else {
            self.operations.push((name, operation));
        }
    }
}

impl<S> Middleware<S> for SecureHeaders {
    async fn handle(&self, request: &mut Request, next: Next<'_>, _: &S) -> Response {
        let mut response = next.run(request).await;
        for (name, value) in &self.operations {
            match value {
                Some(value) => {
                    response.headers_mut().insert(name.clone(), value.clone());
                }
                None => {
                    response.headers_mut().remove(name);
                }
            }
        }
        response
    }
}

fn supported_set(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "x-content-type-options"
            | "referrer-policy"
            | "x-frame-options"
            | "content-security-policy"
            | "content-security-policy-report-only"
            | "permissions-policy"
            | "cross-origin-resource-policy"
            | "cross-origin-opener-policy"
            | "cross-origin-embedder-policy"
    )
}

fn preset() -> [(HeaderName, HeaderValue); 3] {
    [
        (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN")),
    ]
}

fn valid_value(name: &HeaderName, value: &HeaderValue) -> bool {
    let Ok(value) = value.to_str() else { return false };
    match name.as_str() {
        "x-content-type-options" => value.eq_ignore_ascii_case("nosniff"),
        "x-frame-options" => value.eq_ignore_ascii_case("DENY") || value.eq_ignore_ascii_case("SAMEORIGIN"),
        "referrer-policy" => value.split(',').all(|token| {
            matches!(
                token.trim_matches([' ', '\t']),
                "no-referrer"
                    | "no-referrer-when-downgrade"
                    | "same-origin"
                    | "origin"
                    | "origin-when-cross-origin"
                    | "strict-origin"
                    | "strict-origin-when-cross-origin"
                    | "unsafe-url"
            )
        }),
        _ => true,
    }
}
