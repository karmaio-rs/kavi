//! Vendored and adapted from salvo-rs/multra at bba21763af649a737e53eb49131b0e29cc8b1165.
//! Input uses Kavi's owned completion-native body with frame recycling.

mod buffer;
mod constants;
mod constraints;
mod content_disposition;
mod field;
mod helpers;
mod multipart;
mod size_limit;

pub(super) use super::MultipartError as Error;
pub(super) use constraints::Constraints;
pub(super) use field::Field;
pub(super) use multipart::Multipart;
pub(super) use size_limit::SizeLimit;

pub(super) type Result<T> = std::result::Result<T, Error>;

const fn is_boundary_char_no_space(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'\'' | b'(' | b')' | b'+' | b'_' | b',' | b'-' | b'.' | b'/' | b':' | b'=' | b'?'
        )
}

const fn is_boundary_char(byte: u8) -> bool {
    byte == b' ' || is_boundary_char_no_space(byte)
}

fn validate_boundary(boundary: &str) -> Result<()> {
    let bytes = boundary.as_bytes();

    if bytes.is_empty()
        || bytes.len() > 70
        || !bytes[..bytes.len() - 1].iter().copied().all(is_boundary_char)
        || !is_boundary_char_no_space(bytes[bytes.len() - 1])
    {
        return Err(Error::InvalidBoundary {
            boundary: boundary.to_owned(),
        });
    }

    Ok(())
}

/// Parses the `Content-Type` header to extract the boundary value.
///
/// # Errors
///
/// Returns an error if the value is not a valid `multipart/form-data` content
/// type, has no boundary parameter, or contains an invalid boundary.
pub fn parse_boundary<T: AsRef<str>>(content_type: T) -> Result<String> {
    let m = content_type
        .as_ref()
        .parse::<mime::Mime>()
        .map_err(Error::DecodeContentType)?;

    if !(m.type_() == mime::MULTIPART && m.subtype() == mime::FORM_DATA) {
        return Err(Error::NoMultipart);
    }

    let boundary = m.get_param(mime::BOUNDARY).ok_or(Error::NoBoundary)?;
    validate_boundary(boundary.as_str())?;

    Ok(boundary.as_str().to_owned())
}
