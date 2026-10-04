use std::{
    collections::HashSet,
    fmt::{self, Write},
};

use percent_encoding::{AsciiSet, CONTROLS, NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use regex_syntax::hir::{Hir, Look};

use super::UrlError;
use crate::http::Uri;

const LITERAL: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'\\')
    .add(b'%');
const PARAMETER: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
const CATCH_ALL: &AsciiSet = &PARAMETER.remove(b'/');

/// Invalid route syntax or a constraint that cannot be attached to a capture.
#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    /// Declarations must be absolute paths without a query or fragment.
    #[error("a route declaration must start with '/' and exclude query and fragment")]
    InvalidPath,
    /// Braces do not delimit a well-formed, nonempty capture.
    #[error("malformed capture or unmatched brace")]
    InvalidCapture,
    /// A capture name occurs more than once, including across groups.
    #[error("duplicate capture {name:?}")]
    DuplicateCapture { name: String },
    /// Only one capture is allowed within a path segment.
    #[error("only one capture is allowed per path segment")]
    MultipleCaptures,
    /// A catch-all must consume the final part of the path.
    #[error("a catch-all must be the final part of the path")]
    NonterminalCatchAll,
    /// A constraint references no capture in the pattern.
    #[error("unknown capture {name:?}")]
    UnknownCapture { name: String },
    /// A full-value Rust regex could not be compiled.
    #[error("invalid constraint for {name:?}: {source}")]
    InvalidConstraint {
        name: String,
        #[source]
        source: regex::Error,
    },
}

/// A parsed route declaration that can generate URIs without a router.
///
/// Supports `/users/{id}`, `/images/{file}.png`, and `/files/{*rest}`.
/// Escape literal braces with `{{` and `}}`. Unicode literals are encoded;
/// existing valid percent escapes are preserved, including their case.
/// Recognition compares encoded literal text without URI normalization.
#[derive(Clone, Debug)]
pub struct Pattern {
    text: String,
    parts: Vec<Part>,
    pub(super) captures: Vec<Capture>,
}

#[derive(Clone, Debug)]
enum Part {
    Literal(String),
    Capture(usize),
}

#[derive(Clone, Debug)]
pub(super) struct Capture {
    pub(super) name: String,
    catch_all: bool,
    constraint: Option<Regex>,
    expression: Option<String>,
}

/// Structural tokens contain no capture names or constraints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Token {
    Byte(u8),
    Parameter,
    CatchAll,
}

impl Token {
    pub(super) fn is_capture(self) -> bool {
        !matches!(self, Self::Byte(_))
    }
}

impl Pattern {
    /// Parses and encodes a declaration independently of any routing table.
    ///
    /// # Errors
    /// Returns an error for invalid paths, malformed or duplicate captures,
    /// multiple captures in one segment, or nonterminal catch-alls.
    pub fn parse(declaration: &str) -> Result<Self, PatternError> {
        if !declaration.starts_with('/') || declaration.contains(['?', '#']) {
            return Err(PatternError::InvalidPath);
        }
        let mut raw = declaration.chars().peekable();
        let mut chars = std::iter::from_fn(|| {
            let c = raw.next()?;
            let escaped = matches!(c, '{' | '}') && raw.peek() == Some(&c);
            if escaped {
                raw.next();
            }
            Some((c, escaped))
        })
        .peekable();
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut captured = false;
        while let Some((c, escaped)) = chars.next() {
            if c == '}' && !escaped {
                return Err(PatternError::InvalidCapture);
            }
            if c != '{' || escaped {
                if c == '/' {
                    captured = false;
                }
                literal.push(c);
                continue;
            }
            if captured {
                return Err(PatternError::MultipleCaptures);
            }
            captured = true;
            if !literal.is_empty() {
                parts.push((std::mem::take(&mut literal), None));
            }
            let catch_all = chars.peek() == Some(&('*', false));
            if catch_all {
                chars.next();
            }
            let mut name = String::new();
            loop {
                let Some((c, escaped)) = chars.next() else {
                    return Err(PatternError::InvalidCapture);
                };
                if c == '}' && !escaped {
                    break;
                }
                if matches!(c, '/' | '*') || (c == '{' && !escaped) {
                    return Err(PatternError::InvalidCapture);
                }
                name.push(c);
            }
            if name.is_empty() {
                return Err(PatternError::InvalidCapture);
            }
            if catch_all && chars.peek().is_some() {
                return Err(PatternError::NonterminalCatchAll);
            }
            parts.push((name, Some(catch_all)));
        }
        if !literal.is_empty() {
            parts.push((literal, None));
        }
        Self::from_parts(parts.iter().map(|(text, capture)| (text.as_str(), *capture)))
    }

    // The derive emits raw literal/capture parts, never a second runtime template.
    pub(super) fn from_parts<'a>(
        parts: impl IntoIterator<Item = (&'a str, Option<bool>)>,
    ) -> Result<Self, PatternError> {
        let mut pattern = Self {
            text: String::new(),
            parts: Vec::new(),
            captures: Vec::new(),
        };
        let mut names = HashSet::new();
        for (text, capture) in parts {
            if let Some(catch_all) = capture {
                if !names.insert(text) {
                    return Err(PatternError::DuplicateCapture { name: text.to_owned() });
                }
                let slot = pattern.captures.len();
                pattern.captures.push(Capture {
                    name: text.to_owned(),
                    catch_all,
                    constraint: None,
                    expression: None,
                });
                pattern.parts.push(Part::Capture(slot));
            } else {
                pattern.push_part(Part::Literal(encode_literal(text)));
            }
        }
        pattern.rebuild_text();
        Ok(pattern)
    }

    /// Returns the canonical declaration with encoded literals and named captures.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Sets or replaces a capture's full-value Rust regex.
    ///
    /// # Errors
    /// Returns an error for an unknown name or invalid regex. Look-around and
    /// backreferences are unsupported. Constraints are not inferred from types.
    pub fn constraint(mut self, name: &str, regex: &str) -> Result<Self, PatternError> {
        self.set_constraint(name, regex)?;
        Ok(self)
    }

    pub(super) fn set_constraint(&mut self, name: &str, expression: &str) -> Result<(), PatternError> {
        let capture = self
            .captures
            .iter_mut()
            .find(|capture| capture.name == name)
            .ok_or_else(|| PatternError::UnknownCapture { name: name.to_owned() })?;
        if capture.expression.as_deref() == Some(expression) {
            return Ok(());
        }
        let regex = compile_constraint(expression).map_err(|source| PatternError::InvalidConstraint {
            name: name.to_owned(),
            source,
        })?;
        capture.constraint = Some(regex);
        capture.expression = Some(expression.to_owned());
        Ok(())
    }

    /// Generates a path from exactly one nonempty binding per capture.
    ///
    /// Values are decoded application strings. Ordinary captures encode slashes
    /// and percent signs; catch-alls preserve slashes and encode percent signs.
    /// No query is generated and no routing table is consulted.
    ///
    /// # Errors
    /// Returns an error for missing, duplicate, unknown, empty, or constraint-invalid
    /// bindings, or an invalid generated URI.
    pub fn to_uri(&self, params: &[(&str, &str)]) -> Result<Uri, UrlError> {
        for (i, &(name, _)) in params.iter().enumerate() {
            if params[..i].iter().any(|&(prior, _)| prior == name) {
                return Err(UrlError::DuplicateParameter { name: name.to_owned() });
            }
            if !self.captures.iter().any(|capture| capture.name == name) {
                return Err(UrlError::UnknownParameter { name: name.to_owned() });
            }
        }
        let mut path = String::with_capacity(self.text.len());
        for part in &self.parts {
            match part {
                Part::Literal(literal) => path.push_str(literal),
                Part::Capture(slot) => {
                    let capture = &self.captures[*slot];
                    let value = params
                        .iter()
                        .find(|&&(name, _)| name == capture.name)
                        .map(|&(_, value)| value)
                        .ok_or_else(|| UrlError::MissingParameter {
                            name: capture.name.clone(),
                        })?;
                    capture.validate(value)?;
                    write_parameter(&mut path, value, capture.catch_all)?;
                }
            }
        }
        Ok(Uri::try_from(path)?)
    }

    /// Generates an absolute URL from bindings and an explicit HTTP(S) origin.
    ///
    /// # Errors
    /// Returns binding errors from [`Self::to_uri`] or an invalid origin or URI.
    pub fn to_url(&self, params: &[(&str, &str)], origin: &Uri) -> Result<Uri, UrlError> {
        absolute_url(self.to_uri(params)?, origin)
    }

    // The derive supplies fields in capture-slot order, so named binding lookup
    // is unnecessary. One scratch buffer serves every field's validation.
    pub(super) fn to_uri_values(&self, values: &[&dyn fmt::Display]) -> Result<Uri, UrlError> {
        let mut path = String::with_capacity(self.text.len());
        let mut value = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(literal) => path.push_str(literal),
                Part::Capture(slot) => {
                    value.clear();
                    write!(value, "{}", values[*slot])?;
                    let capture = &self.captures[*slot];
                    capture.validate(&value)?;
                    write_parameter(&mut path, &value, capture.catch_all)?;
                }
            }
        }
        Ok(Uri::try_from(path)?)
    }

    pub(super) fn tokens(&self) -> Vec<Token> {
        let mut tokens = Vec::new();
        for part in &self.parts {
            match part {
                Part::Literal(literal) => tokens.extend(literal.bytes().map(Token::Byte)),
                Part::Capture(slot) => tokens.push(if self.captures[*slot].catch_all {
                    Token::CatchAll
                } else {
                    Token::Parameter
                }),
            }
        }
        tokens
    }

    pub(super) fn has_catch_all(&self) -> bool {
        self.captures.iter().any(|capture| capture.catch_all)
    }

    pub(super) fn constraint_count(&self) -> usize {
        self.captures
            .iter()
            .filter(|capture| capture.constraint.is_some())
            .count()
    }

    pub(super) fn same_constraints(&self, other: &Self) -> bool {
        self.captures
            .iter()
            .map(|c| &c.expression)
            .eq(other.captures.iter().map(|c| &c.expression))
    }

    // Validate before moving anything so callers retain the original declaration
    // for diagnostics. The child is already owned by the compiler.
    pub(super) fn prepend(&mut self, prefix: &Self) -> Result<(), PatternError> {
        if prefix.text == "/" {
            return Ok(());
        }
        for capture in &self.captures {
            if prefix.captures.iter().any(|prior| prior.name == capture.name) {
                return Err(PatternError::DuplicateCapture {
                    name: capture.name.clone(),
                });
            }
        }
        let mut joined = prefix.clone();
        if let Some(Part::Literal(literal)) = joined.parts.last_mut()
            && literal.ends_with('/')
        {
            literal.pop();
        }
        if self.text != "/" {
            if let Some(Part::Literal(literal)) = self.parts.first_mut() {
                literal.remove(0);
            }
            joined.push_part(Part::Literal("/".to_owned()));
            let base = joined.captures.len();
            joined.captures.append(&mut self.captures);
            for part in self.parts.drain(..) {
                joined.push_part(match part {
                    Part::Capture(slot) => Part::Capture(base + slot),
                    part => part,
                });
            }
        }
        joined.rebuild_text();
        *self = joined;
        Ok(())
    }

    fn push_part(&mut self, part: Part) {
        if let Part::Literal(literal) = &part {
            if literal.is_empty() {
                return;
            }
            if let Some(Part::Literal(prior)) = self.parts.last_mut() {
                prior.push_str(literal);
                return;
            }
        }
        self.parts.push(part);
    }

    fn rebuild_text(&mut self) {
        self.text.clear();
        for part in &self.parts {
            match part {
                Part::Literal(literal) => self.text.push_str(literal),
                Part::Capture(slot) => {
                    let capture = &self.captures[*slot];
                    self.text.push('{');
                    if capture.catch_all {
                        self.text.push('*');
                    }
                    for c in capture.name.chars() {
                        if matches!(c, '{' | '}') {
                            self.text.push(c);
                        }
                        self.text.push(c);
                    }
                    self.text.push('}');
                }
            }
        }
    }
}

impl Capture {
    pub(super) fn matches(&self, value: &str) -> bool {
        self.constraint
            .as_ref()
            .is_none_or(|constraint| constraint.is_match(value))
    }

    fn validate(&self, value: &str) -> Result<(), UrlError> {
        if value.is_empty() {
            return Err(UrlError::EmptyParameter {
                name: self.name.clone(),
            });
        }
        if !self.matches(value) {
            return Err(UrlError::ConstraintMismatch {
                name: self.name.clone(),
                constraint: self.expression.clone().expect("a failed constraint has an expression"),
            });
        }
        Ok(())
    }
}

fn compile_constraint(expression: &str) -> Result<Regex, regex::Error> {
    // Parsing resolves flags and comments before anchors are added. Appending a
    // textual wrapper would let a trailing verbose-mode comment consume it.
    let expression = regex_syntax::parse(expression).map_err(|error| regex::Error::Syntax(error.to_string()))?;
    let anchored = Hir::concat(vec![Hir::look(Look::Start), expression, Hir::look(Look::End)]);
    Regex::new(&anchored.to_string())
}

pub(super) fn write_parameter(
    output: &mut impl fmt::Write,
    value: &(impl fmt::Display + ?Sized),
    catch_all: bool,
) -> fmt::Result {
    let set = if catch_all { CATCH_ALL } else { PARAMETER };
    fmt::write(&mut EncodingWriter { output, set }, format_args!("{value}"))
}

struct EncodingWriter<'a, W> {
    output: &'a mut W,
    set: &'static AsciiSet,
}

impl<W: fmt::Write> fmt::Write for EncodingWriter<'_, W> {
    #[inline]
    fn write_str(&mut self, value: &str) -> fmt::Result {
        write!(self.output, "{}", utf8_percent_encode(value, self.set))
    }
}

fn encode_literal(literal: &str) -> String {
    let mut output = String::new();
    let bytes = literal.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'%' && bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit() {
            write!(output, "{}", utf8_percent_encode(&literal[start..i], LITERAL)).expect("writing to a String");
            output.push_str(&literal[i..i + 3]);
            i += 3;
            start = i;
        } else {
            i += 1;
        }
    }
    write!(output, "{}", utf8_percent_encode(&literal[start..], LITERAL)).expect("writing to a String");
    output
}

pub(super) fn absolute_url(path: Uri, origin: &Uri) -> Result<Uri, UrlError> {
    if !matches!(origin.scheme_str(), Some("http" | "https"))
        || !origin
            .authority()
            .is_some_and(|authority| !authority.host().is_empty() && !authority.as_str().contains('@'))
        || origin.query().is_some()
        || !matches!(origin.path(), "" | "/")
    {
        return Err(UrlError::InvalidOrigin { origin: origin.clone() });
    }
    let mut parts = origin.clone().into_parts();
    parts.path_and_query = path.into_parts().path_and_query;
    Ok(Uri::from_parts(parts)?)
}

pub(super) fn with_prefix(path: Uri, prefix: &str) -> Result<Uri, UrlError> {
    if prefix == "/" {
        return Ok(path);
    }
    let invalid = || UrlError::InvalidPrefix {
        prefix: prefix.to_owned(),
    };
    if !prefix.starts_with('/') || !prefix.is_ascii() || prefix.contains(['#', '{', '}']) {
        return Err(invalid());
    }
    let parsed = prefix.parse::<Uri>().map_err(|_| invalid())?;
    if parsed.scheme().is_some() || parsed.authority().is_some() || parsed.query().is_some() {
        return Err(invalid());
    }
    let joined_prefix = prefix.strip_suffix('/').unwrap_or(prefix);
    if path.path() == "/" {
        return if joined_prefix.len() == prefix.len() {
            Ok(parsed)
        } else {
            Ok(joined_prefix.parse()?)
        };
    }
    Ok(Uri::try_from(format!(
        "{joined_prefix}/{}",
        path.path().strip_prefix('/').expect("generated paths are absolute")
    ))?)
}
