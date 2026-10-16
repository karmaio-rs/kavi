use std::collections::HashSet;

use proc_macro2::{Ident, Span, TokenStream, TokenTree};
use quote::{ToTokens, format_ident};
use syn::{
    BoundLifetimes, GenericParam, Generics, Lifetime, PredicateType, TraitBound, TypeBareFn, WherePredicate,
    visit::{self, Visit},
};

use crate::attributes;

/// Separates retained type parameters from lifetimes belonging to one invocation.
pub(super) fn split(generics: &Generics) -> syn::Result<(Generics, Generics)> {
    let free = generics
        .lifetimes()
        .map(|parameter| parameter.lifetime.ident.to_string())
        .collect();
    let mut validation = FreeLifetimes {
        free,
        bound: Vec::new(),
        error: None,
    };
    let mut retained = generics.clone();
    retained.params.clear();
    retained.where_clause = None;
    let mut call = retained.clone();

    for parameter in &generics.params {
        let attrs = match parameter {
            GenericParam::Lifetime(parameter) => &parameter.attrs,
            GenericParam::Type(parameter) => &parameter.attrs,
            GenericParam::Const(parameter) => &parameter.attrs,
        };
        if !attributes::configuration(attrs)?.is_empty() {
            return Err(syn::Error::new_spanned(
                parameter,
                "generic parameters cannot be conditionally removed; conditionally compile the entire handler instead",
            ));
        }
        match parameter {
            GenericParam::Lifetime(_) => call.params.push(parameter.clone()),
            GenericParam::Type(_) | GenericParam::Const(_) => {
                validation.visit_generic_param(parameter);
                retained.params.push(parameter.clone());
            }
        }
    }
    if let Some(clause) = &generics.where_clause {
        for predicate in &clause.predicates {
            match predicate {
                WherePredicate::Lifetime(_) => call.make_where_clause().predicates.push(predicate.clone()),
                _ => {
                    validation.visit_where_predicate(predicate);
                    retained.make_where_clause().predicates.push(predicate.clone());
                }
            }
        }
    }
    if let Some(error) = validation.error {
        return Err(error);
    }
    Ok((retained, call))
}

struct FreeLifetimes {
    free: HashSet<String>,
    bound: Vec<String>,
    error: Option<syn::Error>,
}

impl FreeLifetimes {
    fn bind(&mut self, lifetimes: &Option<BoundLifetimes>) -> usize {
        let previous = self.bound.len();
        if let Some(lifetimes) = lifetimes {
            self.bound.extend(lifetimes.lifetimes.iter().filter_map(|parameter| {
                let GenericParam::Lifetime(parameter) = parameter else {
                    return None;
                };
                Some(parameter.lifetime.ident.to_string())
            }));
        }
        previous
    }
}

impl<'a> Visit<'a> for FreeLifetimes {
    fn visit_lifetime(&mut self, lifetime: &'a Lifetime) {
        if self.free.contains(&lifetime.ident.to_string())
            && !self.bound.iter().any(|bound| lifetime.ident == bound)
            && self.error.is_none()
        {
            self.error = Some(syn::Error::new_spanned(
                lifetime,
                "type and const bounds cannot depend on a call-borrow lifetime; use an explicit higher-ranked bound",
            ));
        }
    }

    fn visit_predicate_type(&mut self, predicate: &'a PredicateType) {
        let previous = self.bind(&predicate.lifetimes);
        visit::visit_predicate_type(self, predicate);
        self.bound.truncate(previous);
    }

    fn visit_trait_bound(&mut self, bound: &'a TraitBound) {
        let previous = self.bind(&bound.lifetimes);
        visit::visit_trait_bound(self, bound);
        self.bound.truncate(previous);
    }

    fn visit_type_bare_fn(&mut self, ty: &'a TypeBareFn) {
        let previous = self.bind(&ty.lifetimes);
        visit::visit_type_bare_fn(self, ty);
        self.bound.truncate(previous);
    }
}

/// Avoids shadowing identifiers already present in the original signature or body.
pub(super) struct Names(HashSet<String>);

impl Names {
    pub(super) fn new(tokens: impl ToTokens) -> Self {
        let mut names = Self(HashSet::new());
        names.collect(tokens.into_token_stream());
        names
    }

    fn collect(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Ident(ident) => {
                    self.0.insert(ident.to_string().trim_start_matches("r#").to_owned());
                }
                TokenTree::Group(group) => self.collect(group.stream()),
                _ => {}
            }
        }
    }

    pub(super) fn fresh(&mut self, base: &str) -> Ident {
        let mut name = base.to_owned();
        while !self.0.insert(name.clone()) {
            name.push('_');
        }
        format_ident!("{name}", span = Span::mixed_site())
    }

    pub(super) fn fresh_lifetime(&mut self, base: &str) -> Lifetime {
        Lifetime::new(&format!("'{}", self.fresh(base)), Span::mixed_site())
    }
}
