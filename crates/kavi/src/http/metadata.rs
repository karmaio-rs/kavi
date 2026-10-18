//! Transport facts and explicitly trusted request-origin metadata.
//!
//! Capture metadata before rewriting a request. Forwarded values never replace
//! socket addresses or the transport scheme. Proxy headers are ignored unless a
//! policy explicitly trusts the actual peer. Host values remain client input;
//! applications requiring a canonical origin must validate allowed authorities.

use std::{borrow::Cow, net::IpAddr};

use super::{
    ConnectionInfo, Request, Uri, header,
    uri::{Authority, Scheme},
};

/// Invalid request-origin or trusted proxy metadata.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// A host or forwarded authority is malformed or contains user information.
    #[error("invalid request authority")]
    Authority,
    /// Multiple Host fields make the request authority ambiguous.
    #[error("multiple Host headers")]
    MultipleHosts,
    /// A supplied scheme is neither HTTP nor HTTPS.
    #[error("request scheme must be http or https")]
    Scheme,
    /// A trusted Forwarded field has invalid or ambiguous syntax.
    #[error("invalid Forwarded header")]
    Forwarded,
}

/// A snapshot of the original target and effective client-facing origin.
///
/// The boundary captures this before pre-routing middleware. Effective values
/// initially come from the transport and request target/Host. Only an explicit
/// [`crate::http::middleware::TrustedProxies`] changes them. Socket facts remain in [`ConnectionInfo`].
#[derive(Clone, Debug)]
pub(crate) struct RequestMetadata {
    original_uri: Uri,
    scheme: Option<Scheme>,
    authority: Option<Authority>,
    client_ip: Option<IpAddr>,
}

impl RequestMetadata {
    /// Captures metadata without trusting any forwarding headers.
    ///
    /// Absolute-form target authority takes precedence over Host. Missing
    /// authority is retained as unknown for synthetic requests and special
    /// targets. This does not enforce the transport's HTTP version rules.
    ///
    /// # Errors
    /// Rejects malformed authorities, repeated Host fields and non-HTTP schemes.
    pub(crate) fn capture(request: &Request) -> Result<Self, MetadataError> {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let host = hosts
            .next()
            .map(|value| authority(value.to_str().map_err(|_| MetadataError::Authority)?))
            .transpose()?;
        if hosts.next().is_some() {
            return Err(MetadataError::MultipleHosts);
        }
        let authority = match request.uri().authority() {
            Some(value) => Some(authority(value.as_str())?),
            None => host,
        };
        let connection = request.extensions().get::<ConnectionInfo>();
        let scheme = request
            .uri()
            .scheme()
            .or_else(|| connection.map(|info| &info.scheme))
            .map(|value| scheme(value.as_str()))
            .transpose()?;
        Ok(Self {
            original_uri: request.uri().clone(),
            scheme,
            authority,
            client_ip: connection.map(|info| info.remote_addr.ip()),
        })
    }

    /// Returns the target captured before middleware rewriting.
    #[inline]
    pub(super) fn original_uri(&self) -> &Uri {
        &self.original_uri
    }

    /// Returns the effective HTTP or HTTPS scheme, if established.
    #[inline]
    pub(super) fn scheme(&self) -> Option<&Scheme> {
        self.scheme.as_ref()
    }

    /// Returns the effective authority, including its explicit port.
    #[inline]
    pub(super) fn authority(&self) -> Option<&Authority> {
        self.authority.as_ref()
    }

    /// Returns the first client address outside the explicitly trusted proxy chain.
    #[inline]
    pub(super) fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    /// Returns an HTTP(S) origin with a root path when both components are known.
    #[inline]
    pub(super) fn origin(&self) -> Option<Uri> {
        Uri::builder()
            .scheme(self.scheme.clone()?)
            .authority(self.authority.clone()?)
            .path_and_query("/")
            .build()
            .ok()
    }
}

/// Parses trusted Forwarded fields and atomically updates effective request metadata.
pub(crate) fn apply_forwarded(request: &mut Request, trusted: &impl Fn(IpAddr) -> bool) -> Result<(), MetadataError> {
    let mut metadata = request
        .extensions()
        .get::<RequestMetadata>()
        .cloned()
        .map_or_else(|| RequestMetadata::capture(request), Ok)?;
    let Some(connection) = request.extensions().get::<ConnectionInfo>() else {
        request.extensions_mut().insert(metadata);
        return Ok(());
    };
    let mut peer = connection.remote_addr.ip();
    if !trusted(peer) {
        request.extensions_mut().insert(metadata);
        return Ok(());
    }
    let mut elements = Vec::new();
    for value in request.headers().get_all("forwarded") {
        let value = value.to_str().map_err(|_| MetadataError::Forwarded)?;
        elements.extend(split(value, b',')?);
    }
    if elements.is_empty() {
        request.extensions_mut().insert(metadata);
        return Ok(());
    }
    for element in elements.into_iter().rev() {
        if !trusted(peer) {
            break;
        }
        let hop = Hop::parse(element)?;
        metadata.scheme = hop.scheme;
        metadata.authority = hop.authority;
        metadata.client_ip = hop.client_ip;
        let Some(client) = hop.client_ip else {
            break;
        };
        peer = client;
    }
    request.extensions_mut().insert(metadata);
    Ok(())
}

struct Hop {
    client_ip: Option<IpAddr>,
    scheme: Option<Scheme>,
    authority: Option<Authority>,
}

impl Hop {
    fn parse(element: &str) -> Result<Self, MetadataError> {
        let mut hop = Self {
            client_ip: None,
            scheme: None,
            authority: None,
        };
        let mut seen = Vec::new();
        for pair in split(element, b';')? {
            let (name, value) = pair.trim().split_once('=').ok_or(MetadataError::Forwarded)?;
            if name.is_empty()
                || !name.bytes().all(token)
                || seen.iter().any(|previous: &&str| previous.eq_ignore_ascii_case(name))
            {
                return Err(MetadataError::Forwarded);
            }
            seen.push(name);
            let value = unquote(value)?;
            if name.eq_ignore_ascii_case("for") {
                hop.client_ip = node(&value)?;
            } else if name.eq_ignore_ascii_case("proto") {
                hop.scheme = Some(scheme(&value)?);
            } else if name.eq_ignore_ascii_case("host") {
                hop.authority = Some(authority(&value)?);
            }
        }
        Ok(hop)
    }
}

fn scheme(value: &str) -> Result<Scheme, MetadataError> {
    if value.eq_ignore_ascii_case("http") {
        Ok(Scheme::HTTP)
    } else if value.eq_ignore_ascii_case("https") {
        Ok(Scheme::HTTPS)
    } else {
        Err(MetadataError::Scheme)
    }
}

fn authority(value: &str) -> Result<Authority, MetadataError> {
    let authority: Authority = value.parse().map_err(|_| MetadataError::Authority)?;
    let suffix = value.strip_prefix(authority.host()).ok_or(MetadataError::Authority)?;
    let invalid_port = !suffix.is_empty()
        && suffix
            .strip_prefix(':')
            .is_none_or(|port| !port.is_empty() && port.parse::<u16>().is_err());
    if authority.host().is_empty() || value.contains('@') || invalid_port {
        return Err(MetadataError::Authority);
    }
    Ok(authority)
}

fn node(value: &str) -> Result<Option<IpAddr>, MetadataError> {
    let (host, port) = if value.starts_with('[') {
        let close = value.find(']').ok_or(MetadataError::Forwarded)?;
        (&value[..=close], &value[close + 1..])
    } else {
        let end = value.find(':').unwrap_or(value.len());
        (&value[..end], &value[end..])
    };
    if !port.is_empty() {
        let port = port.strip_prefix(':').ok_or(MetadataError::Forwarded)?;
        if !obfuscated(port) && port.parse::<u16>().is_err() {
            return Err(MetadataError::Forwarded);
        }
    }
    if host.eq_ignore_ascii_case("unknown") || obfuscated(host) {
        return Ok(None);
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return Ok(Some(ip.into()));
    }
    if host.starts_with('[') && host.ends_with(']') {
        return host[1..host.len() - 1]
            .parse::<std::net::Ipv6Addr>()
            .map(|ip| Some(ip.into()))
            .map_err(|_| MetadataError::Forwarded);
    }
    Err(MetadataError::Forwarded)
}

fn obfuscated(value: &str) -> bool {
    value.strip_prefix('_').is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    })
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// Splits delimiters outside quoted strings, rejecting malformed escaping.
fn split(value: &str, delimiter: u8) -> Result<Vec<&str>, MetadataError> {
    let mut pieces = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (index, byte) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == delimiter {
            let piece = value[start..index].trim();
            if piece.is_empty() {
                return Err(MetadataError::Forwarded);
            }
            pieces.push(piece);
            start = index + 1;
        }
    }
    if quoted || escaped || value[start..].trim().is_empty() {
        return Err(MetadataError::Forwarded);
    }
    pieces.push(value[start..].trim());
    Ok(pieces)
}

fn unquote(value: &str) -> Result<Cow<'_, str>, MetadataError> {
    if !value.starts_with('"') {
        return if !value.is_empty() && value.bytes().all(token) {
            Ok(Cow::Borrowed(value))
        } else {
            Err(MetadataError::Forwarded)
        };
    }
    let Some(value) = value.strip_prefix('"').and_then(|value| value.strip_suffix('"')) else {
        return Err(MetadataError::Forwarded);
    };
    let mut output = String::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        let escaped = byte == b'\\';
        let byte = if escaped {
            bytes.next().ok_or(MetadataError::Forwarded)?
        } else {
            byte
        };
        if !(byte == b'\t' || (32..=126).contains(&byte)) || (!escaped && byte == b'"') {
            return Err(MetadataError::Forwarded);
        }
        output.push(char::from(byte));
    }
    Ok(Cow::Owned(output))
}
