use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::{ToTokens, quote_spanned};
use syn::{Data, DeriveInput, Index, LitStr, Member, parse::Parser, parse_quote, spanned::Spanned};

use crate::attributes;

pub(super) fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let mut options = input.attrs.iter().filter(|attr| attr.path().is_ident("dependencies"));
    let args = options
        .next()
        .map(|attr| attr.meta.require_list().map(|list| list.tokens.clone()))
        .transpose()?
        .unwrap_or_default();
    if let Some(attr) = options.next() {
        return Err(syn::Error::new_spanned(attr, "dependencies may only be specified once"));
    }
    let mut path = None;
    let mut root = false;
    syn::meta::parser(|meta| {
        if meta.path.is_ident("crate") {
            if path.is_some() {
                return Err(meta.error("crate may only be specified once"));
            }
            path = Some(meta.value()?.parse::<LitStr>()?.parse::<syn::Path>()?);
        } else if meta.path.is_ident("root") {
            if root {
                return Err(meta.error("root may only be specified once"));
            }
            root = true;
        } else {
            return Err(meta.error("expected `root` or `crate = \"framework_alias\"`"));
        }
        Ok(())
    })
    .parse2(args)?;
    let path = path.unwrap_or_else(|| parse_quote!(::kavi));
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, "Dependencies requires a struct"));
    };
    let name = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let mut types = HashMap::new();
    let mut output = TokenStream::new();

    if root {
        output.extend(quote_spanned! {name.span()=>
            #[automatically_derived]
            impl #impl_generics #path::dispatch::Provide<Self> for #name #type_generics #where_clause {
                #[inline]
                fn provide(&self) -> &Self {
                    self
                }
            }
        });
    }

    for (index, field) in data.fields.iter().enumerate() {
        let mut skip = false;
        let mut directives = field.attrs.iter().filter(|attr| attr.path().is_ident("dependency"));
        if let Some(attr) = directives.next() {
            if let Some(duplicate) = directives.next() {
                return Err(syn::Error::new_spanned(
                    duplicate,
                    "dependency may only be specified once",
                ));
            }
            attr.parse_nested_meta(|meta| {
                if !meta.path.is_ident("skip") {
                    return Err(meta.error("expected `skip`"));
                }
                if skip {
                    return Err(meta.error("skip may only be specified once"));
                }
                skip = true;
                Ok(())
            })?;
            if !skip {
                return Err(syn::Error::new_spanned(attr, "expected `dependency(skip)`"));
            }
        }
        if skip {
            continue;
        }
        let ty = &field.ty;
        let member = field
            .ident
            .clone()
            .map_or_else(|| Member::Unnamed(Index::from(index)), Member::Named);
        let field_name = member.to_token_stream().to_string();
        if let Some((first, first_name)) =
            types.insert(ty.to_token_stream().to_string(), (ty.span(), field_name.clone()))
        {
            let mut error = syn::Error::new_spanned(
                ty,
                format!(
                    "fields `{first_name}` and `{field_name}` expose the same dependency type; use distinct newtypes or `#[dependency(skip)]`"
                ),
            );
            error.combine(syn::Error::new(first, "first exposed here"));
            return Err(error);
        }
        let configuration = attributes::configuration(&field.attrs)?;
        output.extend(quote_spanned! {ty.span()=>
            #(#configuration)*
            #[automatically_derived]
            impl #impl_generics #path::dispatch::Provide<#ty> for #name #type_generics #where_clause {
                #[inline]
                fn provide(&self) -> &#ty {
                    &self.#member
                }
            }
        });
    }
    Ok(output)
}
