use std::{borrow::Cow, iter::FusedIterator, ops::Range, slice, str::Utf8Error};

/// Borrowed decoded route parameters in capture order.
///
/// Values requiring no decoding borrow the original URI snapshot. Other values
/// borrow decoded request-owned strings. Iteration performs no decoding or allocation.
#[derive(Clone, Debug)]
pub struct RouteParams<'a> {
    path: &'a str,
    labels: &'a str,
    params: slice::Iter<'a, Param>,
}

impl<'a> RouteParams<'a> {
    pub(crate) fn new(path: &'a str, labels: &'a str, params: &'a [Param]) -> Self {
        Self {
            path,
            labels,
            params: params.iter(),
        }
    }
}

impl Default for RouteParams<'_> {
    #[inline]
    fn default() -> Self {
        Self::new("", "", &[])
    }
}

impl<'a> Iterator for RouteParams<'a> {
    type Item = (&'a str, &'a str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.params
            .next()
            .map(|param| (&self.labels[param.name.clone()], param.value(self.path)))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.params.size_hint()
    }
}

impl DoubleEndedIterator for RouteParams<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.params
            .next_back()
            .map(|param| (&self.labels[param.name.clone()], param.value(self.path)))
    }
}

impl ExactSizeIterator for RouteParams<'_> {}
impl FusedIterator for RouteParams<'_> {}

#[derive(Clone, Debug)]
pub(crate) struct Param {
    // Assigned only after an endpoint wins, when its names enter request metadata.
    pub(crate) name: Range<usize>,
    value: Value,
}

#[derive(Clone, Debug)]
enum Value {
    Path(Range<usize>),
    Decoded(String),
}

impl Param {
    pub(super) fn decode(path: &str, range: Range<usize>) -> Result<Self, Utf8Error> {
        let value = match percent_encoding::percent_decode_str(&path[range.clone()]).decode_utf8()? {
            Cow::Borrowed(_) => Value::Path(range),
            Cow::Owned(value) => Value::Decoded(value),
        };
        Ok(Self { name: 0..0, value })
    }

    #[inline]
    pub(crate) fn borrows_path(&self) -> bool {
        matches!(self.value, Value::Path(_))
    }

    #[inline]
    pub(super) fn value<'a>(&'a self, path: &'a str) -> &'a str {
        match &self.value {
            Value::Path(range) => &path[range.clone()],
            Value::Decoded(value) => value,
        }
    }
}
