use std::collections::HashSet;

use super::size_limit::SizeLimit;

/// Represents some rules to be applied on the stream and field's content size
/// to prevent `DoS` attacks.
///
/// It's recommended to add some rules on field (specially text field) size to
/// avoid potential `DoS` attacks from attackers running the server out of memory.
/// This type provides some API to apply constraints on very granular level to
/// make `multipart/form-data` safe. By default, it does not apply any
/// constraint.
///
#[derive(Debug)]
#[must_use]
pub struct Constraints {
    pub(crate) field_count: usize,
    pub(crate) size_limit: SizeLimit,
    pub(crate) allowed_fields: Option<HashSet<String>>,
}

impl Constraints {
    /// Creates a set of rules with default behaviour.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies rules on field's content length.
    pub fn size_limit(self, size_limit: SizeLimit) -> Self {
        Self {
            size_limit,
            field_count: self.field_count,
            allowed_fields: self.allowed_fields,
        }
    }

    /// Sets the maximum number of fields, including empty fields.
    pub fn field_count(mut self, limit: usize) -> Self {
        self.field_count = limit;
        self
    }

    /// Specify which fields should be allowed, for any unknown field, the
    /// [`next_field`](super::Multipart::next_field) will throw an error.
    #[allow(dead_code)] // Retained from Multra for future internal configuration.
    pub fn allowed_fields<N: Into<String>>(self, allowed_fields: Vec<N>) -> Self {
        let allowed_fields = allowed_fields.into_iter().map(Into::into).collect();

        Self {
            size_limit: self.size_limit,
            field_count: self.field_count,
            allowed_fields: Some(allowed_fields),
        }
    }

    pub(crate) fn is_it_allowed(&self, field: Option<&str>) -> bool {
        self.allowed_fields
            .as_ref()
            .is_none_or(|allowed_fields| field.is_some_and(|field| allowed_fields.contains(field)))
    }
}

impl Default for Constraints {
    fn default() -> Self {
        Self {
            size_limit: SizeLimit::default(),
            allowed_fields: None,
            field_count: usize::MAX,
        }
    }
}
