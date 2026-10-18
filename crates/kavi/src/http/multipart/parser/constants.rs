use std::borrow::Cow;

use encoding_rs::Encoding;

pub const DEFAULT_PER_FIELD_SIZE_LIMIT: u64 = u64::MAX;
pub const DEFAULT_HEADERS_SIZE_LIMIT: u64 = 64 * 1024;

// Default limit for preamble bytes (data before the first boundary). Applied
// by the parser and configurable through `SizeLimit::preamble`.
pub const DEFAULT_PREAMBLE_SIZE_LIMIT: u64 = 32 * 1024;

pub const MAX_HEADERS: usize = 32;
pub const MAX_BOUNDARY_PADDING: usize = 64 * 1024;
pub const BOUNDARY_EXT: &str = "--";
#[allow(dead_code)]
pub const LF: &str = "\n";
pub const CRLF: &str = "\r\n";
pub const CRLF_CRLF: &str = "\r\n\r\n";

#[derive(PartialEq, Eq)]
pub enum ContentDispositionAttr {
    Name,
    FileName,
}

fn trim_ascii_ws_start(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or_else(|| &bytes[bytes.len()..], |i| &bytes[i..])
}

fn trim_ascii_ws_then(bytes: &[u8], char: u8) -> Option<&[u8]> {
    match trim_ascii_ws_start(bytes) {
        [first, rest @ ..] if *first == char => Some(rest),
        _ => None,
    }
}

fn trim_ascii_ws_end(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(&bytes[..0], |i| &bytes[..=i])
}

fn skip_to_next_parameter(header: &[u8], index: &mut usize) {
    // Start before a value's opening quote. A quoted value is one parameter,
    // even if it contains semicolons, fake parameter names, or escaped quotes.
    let mut quoted = false;
    while *index < header.len() {
        match header[*index] {
            b'\\' if quoted => {
                *index += 1;
                if *index < header.len() {
                    *index += 1;
                }
                continue;
            }
            b'"' => quoted = !quoted,
            b';' if !quoted => {
                *index += 1;
                return;
            }
            _ => {}
        }
        *index += 1;
    }
}

fn skip_ascii_ws(header: &[u8], index: &mut usize) {
    while *index < header.len() && header[*index].is_ascii_whitespace() {
        *index += 1;
    }
}

fn parse_quoted_value(mut header: &[u8]) -> Option<(&[u8], bool)> {
    header = trim_ascii_ws_then(header, b'"')?;
    let start = 0;
    let (mut index, mut escaped) = (start, false);

    while index < header.len() {
        if header[index] == b'"' {
            let mut backslashes = 0;
            let mut cursor = index;
            while cursor > start && header[cursor - 1] == b'\\' {
                backslashes += 1;
                cursor -= 1;
            }

            if backslashes % 2 == 0 {
                return Some((&header[..index], escaped));
            }

            escaped = true;
        }

        index += 1;
    }

    None
}

fn parse_unquoted_value(header: &[u8]) -> &[u8] {
    let value = trim_ascii_ws_start(header);
    trim_ascii_ws_end(&value[..memchr::memchr(b';', value).unwrap_or(value.len())])
}

fn decode_percent_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.contains(&b'%') {
        return Some(bytes.to_vec());
    }

    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = bytes.get(index + 1)?;
            let lo = bytes.get(index + 2)?;
            let hex = [*hi, *lo];
            decoded.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    Some(decoded)
}

fn decode_value(bytes: &[u8], is_escaped: bool, decode_percent_encoding: bool) -> Option<Cow<'_, str>> {
    if decode_percent_encoding
        && bytes.contains(&b'%')
        && let Some(decoded) = decode_percent_bytes(bytes)
    {
        let value = String::from_utf8(decoded).ok()?;
        return if is_escaped {
            Some(value.replace(r#"\""#, "\"").into())
        } else {
            Some(value.into())
        };
    }

    let value = std::str::from_utf8(bytes).ok()?;
    if is_escaped {
        Some(value.replace(r#"\""#, "\"").into())
    } else {
        Some(value.into())
    }
}

fn decode_extended_value(bytes: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(bytes).ok()?;
    let mut parts = value.splitn(3, '\'');
    let charset = parts.next()?;
    let _language = parts.next()?;
    let encoded = parts.next()?;

    let encoding = Encoding::for_label(charset.as_bytes())?;
    let decoded = decode_percent_bytes(encoded.as_bytes())?;
    let (text, _, had_errors) = encoding.decode(&decoded);
    if had_errors {
        return None;
    }

    Some(text.into_owned())
}

impl ContentDispositionAttr {
    /// Extract `ContentDisposition` Attribute from header.
    ///
    /// Some older clients may not quote the name or filename, so we allow them.
    /// Legacy percent-encoding in `filename` is decoded before returning;
    /// percent signs in field names are preserved literally.
    pub fn extract_from<'h>(&self, header: &'h [u8]) -> Option<Cow<'h, str>> {
        if self == &Self::FileName
            && let Some(value) = self.extract_extended_from(header)
        {
            return Some(value);
        }

        let prefix = match self {
            Self::Name => &b"name"[..],
            Self::FileName => &b"filename"[..],
        };
        let mut index = 0;

        while index < header.len() {
            skip_to_next_parameter(header, &mut index);
            skip_ascii_ws(header, &mut index);
            if index >= header.len() {
                break;
            }

            let key_start = index;
            while index < header.len()
                && !header[index].is_ascii_whitespace()
                && header[index] != b'='
                && header[index] != b';'
            {
                index += 1;
            }

            let key = &header[key_start..index];
            skip_ascii_ws(header, &mut index);
            if index >= header.len() || header[index] != b'=' {
                continue;
            }

            index += 1;
            let rest = &header[index..];
            let (bytes, is_escaped) = if let Some((value, escaped)) = parse_quoted_value(rest) {
                (value, escaped)
            } else {
                (parse_unquoted_value(rest), false)
            };

            if key.eq_ignore_ascii_case(prefix) {
                return decode_value(bytes, is_escaped, matches!(self, Self::FileName));
            }
        }

        None
    }

    fn extract_extended_from<'h>(&self, header: &'h [u8]) -> Option<Cow<'h, str>> {
        let prefix = match self {
            Self::Name => return None,
            Self::FileName => &b"filename*"[..],
        };
        let mut index = 0;

        while index < header.len() {
            skip_to_next_parameter(header, &mut index);
            skip_ascii_ws(header, &mut index);
            if index >= header.len() {
                break;
            }

            let key_start = index;
            while index < header.len()
                && !header[index].is_ascii_whitespace()
                && header[index] != b'='
                && header[index] != b';'
            {
                index += 1;
            }

            let key = &header[key_start..index];
            skip_ascii_ws(header, &mut index);
            if index >= header.len() || header[index] != b'=' {
                continue;
            }

            index += 1;
            if key.eq_ignore_ascii_case(prefix) {
                let value = parse_unquoted_value(&header[index..]);
                return Some(decode_extended_value(value)?.into());
            }
        }

        None
    }
}
