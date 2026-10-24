//! Infers the bounds a handler's return type needs.
//!
//! A handler's return value is converted through `IntoResponse`. When the
//! return type mentions one of the function's own type parameters, the
//! conversion may need a bound on the complete return type, rather than on
//! the parameters contained inside a response wrapper.

use syn::{
    ReturnType, Type,
    visit::{self, Visit},
};

/// Tracks generic parameters and syntax that cannot appear in an adapter bound.
struct ReturnTypeUse<'a> {
    parameters: &'a [String],
    generic: bool,
    opaque: bool,
    borrowed: bool,
}

impl<'ast> Visit<'ast> for ReturnTypeUse<'_> {
    fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
        if let Some(segment) = path.path.segments.first() {
            self.generic |= self.parameters.iter().any(|parameter| segment.ident == parameter);
        }
        visit::visit_type_path(self, path);
    }

    fn visit_type_impl_trait(&mut self, ty: &'ast syn::TypeImplTrait) {
        self.opaque = true;
        visit::visit_type_impl_trait(self, ty);
    }

    fn visit_lifetime(&mut self, lifetime: &'ast syn::Lifetime) {
        self.borrowed |= lifetime.ident != "static";
    }

    fn visit_type_reference(&mut self, ty: &'ast syn::TypeReference) {
        self.borrowed |= ty.lifetime.is_none();
        visit::visit_type_reference(self, ty);
    }
}

/// Returns the complete return type when it needs a response bound.
///
/// Opaque types carry their own bounds and cannot appear in a where-clause.
/// Types borrowing from a call are checked against the author's bounds inside
/// the method, where those lifetimes are in scope.
pub(super) fn bound(output: &ReturnType, parameters: &[String]) -> Option<Type> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    let mut usage = ReturnTypeUse {
        parameters,
        generic: false,
        opaque: false,
        borrowed: false,
    };
    usage.visit_type(ty);
    (usage.generic && !usage.opaque && !usage.borrowed).then(|| ty.as_ref().clone())
}
