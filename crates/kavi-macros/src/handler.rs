use proc_macro2::TokenStream;
use quote::{ToTokens, quote, quote_spanned};
use syn::{
    Attribute, BoundLifetimes, FnArg, GenericParam, ItemFn, Lifetime, ParenthesizedGenericArguments, TraitBound, Type,
    TypeBareFn, TypeImplTrait, TypeInfer, TypeReference, parse_quote,
    spanned::Spanned,
    visit::{self, Visit},
};

use crate::{attributes, generics};

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Controller,
    Middleware,
}

pub(super) fn expand(args: TokenStream, mut function: ItemFn, kind: Kind) -> syn::Result<TokenStream> {
    let path = attributes::crate_path(args)?;
    let signature = &function.sig;
    if signature.asyncness.is_none() {
        return Err(syn::Error::new_spanned(signature, "handler functions must be async"));
    }
    if let Some(qualifier) = signature.constness {
        return Err(syn::Error::new_spanned(qualifier, "handler functions cannot be const"));
    }
    if let Some(qualifier) = signature.unsafety {
        return Err(syn::Error::new_spanned(qualifier, "handler functions must be safe"));
    }
    if let Some(abi) = &signature.abi {
        return Err(syn::Error::new_spanned(
            abi,
            "handler functions cannot declare an extern ABI",
        ));
    }
    if signature.variadic.is_some() {
        return Err(syn::Error::new_spanned(
            signature,
            "handler functions cannot be variadic",
        ));
    }
    let leading = match kind {
        Kind::Controller => 1,
        Kind::Middleware => 2,
    };
    if signature.inputs.len() < leading {
        let message = match kind {
            Kind::Controller => "a controller requires an owned Request parameter",
            Kind::Middleware => "middleware requires Request and Next parameters",
        };
        return Err(syn::Error::new_spanned(signature, message));
    }

    for (index, argument) in signature.inputs.iter().enumerate() {
        let FnArg::Typed(argument) = argument else {
            return Err(syn::Error::new_spanned(
                argument,
                "apply this attribute to a function without a self parameter",
            ));
        };
        if clone_dependency(&argument.attrs)? && index < leading {
            return Err(syn::Error::new_spanned(
                argument,
                "Request and Next parameters cannot have a dependency directive",
            ));
        }
    }

    // Resolve one conditional parameter before generating bounds or forwarding
    // arguments. Rust expands only the enabled branch, including when several
    // dependencies have independent conditions.
    for (index, argument) in signature.inputs.iter().enumerate() {
        let FnArg::Typed(argument) = argument else {
            continue;
        };
        let mut attrs = argument.attrs.clone();
        let Some(condition) = attributes::take_configuration(&mut attrs)? else {
            continue;
        };
        if index < leading {
            return Err(syn::Error::new_spanned(
                argument,
                "Request and Next parameters cannot be conditionally compiled",
            ));
        }
        let mut enabled = function.clone();
        let FnArg::Typed(argument) = &mut enabled.sig.inputs[index] else {
            unreachable!("matched typed parameter")
        };
        argument.attrs = attrs;
        let mut disabled = function.clone();
        disabled.sig.inputs = disabled
            .sig
            .inputs
            .into_iter()
            .enumerate()
            .filter_map(|(position, argument)| (position != index).then_some(argument))
            .collect();
        let attribute = match kind {
            Kind::Controller => quote!(controller),
            Kind::Middleware => quote!(middleware),
        };
        let crate_name = path.to_token_stream().to_string();
        return Ok(quote! {
            #[cfg(#condition)]
            #[#path::#attribute(crate = #crate_name)]
            #enabled

            #[cfg(not(#condition))]
            #[#path::#attribute(crate = #crate_name)]
            #disabled
        });
    }

    let mut names = generics::Names::new(&function);
    let name = signature.ident.clone();
    let encoded_name = name
        .to_string()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let state = names.fresh(&format!("__KaviDependenciesF{encoded_name}"));
    let request = names.fresh(&format!("__kavi_f{encoded_name}_request"));
    let dependencies = names.fresh(&format!("__kavi_f{encoded_name}_dependencies"));
    let next = names.fresh(&format!("__kavi_f{encoded_name}_next"));
    let invoke_name = names.fresh(&format!("__kavi_f{encoded_name}_call"));
    let adapter = names.fresh(&format!("__KaviHandlerF{encoded_name}"));
    let (retained, call_generics) = generics::split(&signature.generics)?;
    let mut requirements = Vec::new();
    let mut arguments = Vec::new();
    for (index, argument) in signature.inputs.iter().enumerate() {
        let FnArg::Typed(argument) = argument else {
            return Err(syn::Error::new_spanned(
                argument,
                "apply this attribute to a function without a self parameter",
            ));
        };
        if index < leading {
            continue;
        }
        let clone = clone_dependency(&argument.attrs)?;
        let ty = match ungroup_type(&argument.ty) {
            Type::Reference(reference) => {
                if reference.mutability.is_some() {
                    return Err(syn::Error::new_spanned(
                        reference,
                        "application dependencies cannot be mutable references",
                    ));
                }
                if clone {
                    return Err(syn::Error::new_spanned(
                        argument,
                        "`dependency(clone)` requires an owned parameter; borrow with &T without this directive",
                    ));
                }
                reference.elem.as_ref()
            }
            _ if clone => argument.ty.as_ref(),
            ty => {
                return Err(syn::Error::new_spanned(
                    ty,
                    "owned application dependencies require `#[dependency(clone)]`; otherwise borrow as &T",
                ));
            }
        };
        let mut validation = DependencyType::default();
        validation.visit_type(ty);
        if let Some(error) = validation.error {
            return Err(error);
        }
        requirements.push(quote_spanned!(ty.span()=> #state: #path::dispatch::Provide<#ty>));
        let provided = quote_spanned!(ty.span()=> <#state as #path::dispatch::Provide<#ty>>::provide(#dependencies));
        if clone {
            requirements.push(quote_spanned!(ty.span()=> #ty: ::core::clone::Clone));
            arguments.push(quote_spanned!(ty.span()=> ::core::clone::Clone::clone(#provided)));
        } else {
            arguments.push(provided);
        }
    }

    let visibility = function.vis.clone();
    let configuration = attributes::configuration(&function.attrs)?;
    let public_attributes = attributes::take_matching(&mut function.attrs, &["doc", "deprecated"])?;
    let lint_attributes = attributes::take_matching(&mut function.attrs.clone(), &["allow", "warn", "deny", "forbid"])?;
    for argument in &mut function.sig.inputs {
        let FnArg::Typed(argument) = argument else {
            unreachable!("validated function parameter")
        };
        argument
            .attrs
            .retain(|attribute| !attribute.path().is_ident("dependency"));
    }
    let mut call = function.sig.clone();
    call.ident = parse_quote!(call);
    call.generics = call_generics;
    let mut parameters = Vec::new();
    for (index, argument) in call.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = argument else {
            unreachable!("validated function parameter")
        };
        let parameter = names.fresh(&format!("__kavi_f{encoded_name}_argument_{index}"));
        argument.pat = parse_quote!(#parameter);
        parameters.push(parameter);
    }
    call.inputs.insert(0, parse_quote!(&self));
    let mut invoke = call.clone();
    invoke.ident = invoke_name.clone();
    function.vis = syn::Visibility::Inherited;

    let type_parameters = retained
        .type_params()
        .map(|parameter| &parameter.ident)
        .collect::<Vec<_>>();
    let forwarded = retained
        .params
        .iter()
        .map(|parameter| match parameter {
            GenericParam::Type(parameter) => &parameter.ident,
            GenericParam::Const(parameter) => &parameter.ident,
            GenericParam::Lifetime(_) => unreachable!("only retained parameters"),
        })
        .collect::<Vec<_>>();
    let turbofish = (!forwarded.is_empty()).then(|| quote!(::<#(#forwarded),*>));
    let (impl_generics, type_generics, where_clause) = retained.split_for_impl();
    let mut bound_generics = retained.clone();
    bound_generics.params.push(parse_quote!(#state));
    for parameter in &type_parameters {
        bound_generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#parameter: 'static));
    }
    for requirement in requirements {
        bound_generics
            .make_where_clause()
            .predicates
            .push(syn::parse2(requirement)?);
    }
    let (bound_impl_generics, _, bound_where_clause) = bound_generics.split_for_impl();

    let (handler_trait, continuation, call_continuation) = match kind {
        Kind::Controller => (quote!(Controller), quote!(), quote!()),
        Kind::Middleware => (
            quote!(Middleware),
            quote!(#next: #path::dispatch::Next<'_>,),
            quote!(#next,),
        ),
    };

    Ok(quote! {
        #(#configuration)*
        #(#lint_attributes)*
        #[doc(hidden)]
        #visibility struct #adapter #retained #where_clause {
            _private: ::core::marker::PhantomData<fn() -> (#(*const #type_parameters,)*)>,
        }

        #(#configuration)*
        #(#lint_attributes)*
        #(#public_attributes)*
        #[inline]
        #[must_use]
        #visibility const fn #name #retained () -> #adapter #type_generics #where_clause {
            #adapter { _private: ::core::marker::PhantomData }
        }

        #(#configuration)*
        impl #impl_generics #adapter #type_generics #where_clause {
            #(#lint_attributes)*
            #(#public_attributes)*
            #visibility #call {
                self.#invoke_name(#(#parameters),*).await
            }

            #invoke {
                // Preserve the original function's parameter patterns and attributes.
                #function
                #name #turbofish (#(#parameters),*).await
            }
        }

        #(#configuration)*
        #[automatically_derived]
        impl #bound_impl_generics #path::dispatch::#handler_trait<#state> for #adapter #type_generics
            #bound_where_clause
        {
            #[inline]
            async fn handle(
                &self,
                #request: #path::http::Request,
                #continuation
                #dependencies: &#state,
            ) -> ::core::result::Result<#path::http::Response, #path::Error> {
                self.#invoke_name(#request, #call_continuation #(#arguments),*).await
            }
        }
    })
}

fn ungroup_type(ty: &Type) -> &Type {
    match ty {
        Type::Group(group) => ungroup_type(&group.elem),
        Type::Paren(paren) => ungroup_type(&paren.elem),
        _ => ty,
    }
}

/// Clone mode must be unconditional even when the parameter itself is conditional.
fn clone_dependency(attrs: &[Attribute]) -> syn::Result<bool> {
    let mut conditional = attrs.to_vec();
    let directives = attributes::take_matching(&mut conditional, &["dependency"])?;
    let mut clone = false;
    for directive in directives {
        if !directive.path().is_ident("dependency") {
            return Err(syn::Error::new_spanned(
                directive,
                "dependency directives cannot be conditional; conditionally compile the entire parameter instead",
            ));
        }
        if clone {
            return Err(syn::Error::new_spanned(
                directive,
                "dependency may only be specified once",
            ));
        }
        directive.parse_nested_meta(|meta| {
            if !meta.path.is_ident("clone") {
                return Err(meta.error("expected `clone`"));
            }
            if clone {
                return Err(meta.error("clone may only be specified once"));
            }
            clone = true;
            Ok(())
        })?;
        if !clone {
            return Err(syn::Error::new_spanned(directive, "expected `dependency(clone)`"));
        }
    }
    Ok(clone)
}

/// Checks types before they are copied into provider bounds outside the function.
#[derive(Default)]
struct DependencyType {
    error: Option<syn::Error>,
    lifetimes: Vec<String>,
    elision: usize,
}

impl DependencyType {
    fn bind_lifetimes(&mut self, lifetimes: &Option<BoundLifetimes>) -> usize {
        let previous = self.lifetimes.len();
        if let Some(lifetimes) = lifetimes {
            self.lifetimes
                .extend(lifetimes.lifetimes.iter().filter_map(|parameter| {
                    let GenericParam::Lifetime(parameter) = parameter else {
                        return None;
                    };
                    Some(parameter.lifetime.ident.to_string())
                }));
        }
        previous
    }

    fn reject(&mut self, tokens: impl ToTokens, message: &str) {
        if self.error.is_none() {
            self.error = Some(syn::Error::new_spanned(tokens, message));
        }
    }
}

impl<'a> Visit<'a> for DependencyType {
    fn visit_type_impl_trait(&mut self, ty: &'a TypeImplTrait) {
        self.reject(
            ty,
            "application dependencies require a concrete type; use a named type or dyn Trait instead of impl Trait",
        );
    }

    fn visit_type_infer(&mut self, ty: &'a TypeInfer) {
        self.reject(
            ty,
            "application dependency types cannot contain inferred types; specify a concrete type",
        );
    }

    fn visit_lifetime(&mut self, lifetime: &'a Lifetime) {
        if lifetime.ident != "static" && !self.lifetimes.iter().any(|name| lifetime.ident == name) {
            self.reject(lifetime, "application dependency types cannot contain inferred or unbound lifetimes; use 'static or a higher-ranked bound");
        }
    }

    fn visit_type_reference(&mut self, ty: &'a TypeReference) {
        if ty.lifetime.is_none() && self.elision == 0 {
            self.reject(
                ty,
                "references inside application dependency types require an explicit 'static or higher-ranked lifetime",
            );
        }
        visit::visit_type_reference(self, ty);
    }

    fn visit_type_bare_fn(&mut self, ty: &'a TypeBareFn) {
        let previous = self.bind_lifetimes(&ty.lifetimes);
        self.elision += 1;
        visit::visit_type_bare_fn(self, ty);
        self.elision -= 1;
        self.lifetimes.truncate(previous);
    }

    fn visit_trait_bound(&mut self, bound: &'a TraitBound) {
        let previous = self.bind_lifetimes(&bound.lifetimes);
        visit::visit_trait_bound(self, bound);
        self.lifetimes.truncate(previous);
    }

    fn visit_parenthesized_generic_arguments(&mut self, arguments: &'a ParenthesizedGenericArguments) {
        self.elision += 1;
        visit::visit_parenthesized_generic_arguments(self, arguments);
        self.elision -= 1;
    }
}
