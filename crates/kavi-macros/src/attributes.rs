use proc_macro2::TokenStream;
use syn::{Attribute, LitStr, Meta, Path, Token, parse::Parser, parse_quote, punctuated::Punctuated};

pub(super) fn crate_path(args: TokenStream) -> syn::Result<Path> {
    let mut path = None;
    syn::meta::parser(|meta| {
        if !meta.path.is_ident("crate") {
            return Err(meta.error("expected `crate = \"framework_alias\"`"));
        }
        if path.is_some() {
            return Err(meta.error("crate may only be specified once"));
        }
        path = Some(meta.value()?.parse::<LitStr>()?.parse()?);
        Ok(())
    })
    .parse2(args)?;
    Ok(path.unwrap_or_else(|| parse_quote!(::kavi)))
}

/// Copies only conditional compilation, leaving other attributes on the original item.
pub(super) fn configuration(attrs: &[Attribute]) -> syn::Result<Vec<Attribute>> {
    attrs
        .iter()
        .filter_map(|attr| match split_configuration(&attr.meta) {
            Ok((Some(condition), _)) => Some(Ok(parse_quote!(#[cfg(#condition)]))),
            Ok((None, _)) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

/// Removes conditional compilation attributes and returns their combined predicate.
pub(super) fn take_configuration(attrs: &mut Vec<Attribute>) -> syn::Result<Option<Meta>> {
    let mut conditions = Vec::new();
    let mut retained = Vec::new();
    for attr in attrs.iter() {
        let (condition, remainder) = split_configuration(&attr.meta)?;
        conditions.extend(condition);
        if let Some(meta) = remainder {
            let mut attr = attr.clone();
            attr.meta = meta;
            retained.push(attr);
        }
    }
    *attrs = retained;
    Ok((!conditions.is_empty()).then(|| parse_quote!(all(#(#conditions),*))))
}

/// Moves selected attributes while preserving their conditional compilation wrappers.
pub(super) fn take_matching(attrs: &mut Vec<Attribute>, names: &[&str]) -> syn::Result<Vec<Attribute>> {
    let mut selected = Vec::new();
    let mut retained = Vec::new();
    for attr in attrs.iter() {
        let (matching, remainder) = split_matching(&attr.meta, names)?;
        if let Some(meta) = matching {
            let mut attr = attr.clone();
            attr.meta = meta;
            selected.push(attr);
        }
        if let Some(meta) = remainder {
            let mut attr = attr.clone();
            attr.meta = meta;
            retained.push(attr);
        }
    }
    *attrs = retained;
    Ok(selected)
}

fn split_matching(meta: &Meta, names: &[&str]) -> syn::Result<(Option<Meta>, Option<Meta>)> {
    if names.iter().any(|name| meta.path().is_ident(name)) {
        return Ok((Some(meta.clone()), None));
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok((None, Some(meta.clone())));
    }
    let arguments = meta
        .require_list()?
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    let mut arguments = arguments.into_iter();
    let Some(condition) = arguments.next() else {
        return Err(syn::Error::new_spanned(meta, "cfg_attr requires a condition"));
    };
    let mut selected = Vec::new();
    let mut retained = Vec::new();
    for argument in arguments {
        let (matching, remainder) = split_matching(&argument, names)?;
        selected.extend(matching);
        retained.extend(remainder);
    }
    Ok((
        (!selected.is_empty()).then(|| parse_quote!(cfg_attr(#condition, #(#selected),*))),
        (!retained.is_empty()).then(|| parse_quote!(cfg_attr(#condition, #(#retained),*))),
    ))
}

/// Separates the condition under which an item exists from its other attributes.
fn split_configuration(meta: &Meta) -> syn::Result<(Option<Meta>, Option<Meta>)> {
    if meta.path().is_ident("cfg") {
        return Ok((Some(meta.require_list()?.parse_args()?), None));
    }
    if !meta.path().is_ident("cfg_attr") {
        return Ok((None, Some(meta.clone())));
    }
    let arguments = meta
        .require_list()?
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    let mut arguments = arguments.into_iter();
    let Some(condition) = arguments.next() else {
        return Err(syn::Error::new_spanned(meta, "cfg_attr requires a condition"));
    };
    let mut conditions = Vec::new();
    let mut retained = Vec::new();
    for argument in arguments {
        let (condition, remainder) = split_configuration(&argument)?;
        conditions.extend(condition);
        retained.extend(remainder);
    }
    Ok((
        (!conditions.is_empty()).then(|| parse_quote!(any(not(#condition), all(#(#conditions),*)))),
        (!retained.is_empty()).then(|| parse_quote!(cfg_attr(#condition, #(#retained),*))),
    ))
}
