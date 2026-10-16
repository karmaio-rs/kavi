use std::{collections::HashSet, fmt::Write};

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use proc_macro2::TokenStream;
use quote::quote;
use regex_syntax::hir::{Hir, Look};
use syn::{Data, DeriveInput, Fields, Index, LitStr, Member, Path, Token, ext::IdentExt, parse_quote};

use crate::generics::Names;

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

enum Part {
    Literal(String),
    Capture { name: String, catch_all: bool },
}

pub(super) fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let mut names = Names::new(&input);
    let request_lifetime = names.fresh_lifetime("__kavi_request_lifetime");
    let context_lifetime = names.fresh_lifetime("__kavi_context_lifetime");
    let context = names.fresh("__KaviExtractContext");
    let request = names.fresh("__kavi_request");
    let mut attributes = input.attrs.iter().filter(|attr| attr.path().is_ident("typed_path"));
    let attr = attributes
        .next()
        .ok_or_else(|| syn::Error::new_spanned(&input.ident, "expected #[typed_path(\"/path/{capture}\")]"))?;
    if let Some(attr) = attributes.next() {
        return Err(syn::Error::new_spanned(attr, "typed_path may only be specified once"));
    }
    let (path, crate_path) = attr.parse_args_with(|args: syn::parse::ParseStream<'_>| {
        let path = args.parse::<LitStr>()?;
        let mut crate_path: Path = parse_quote!(::kavi);
        if !args.is_empty() {
            args.parse::<Token![,]>()?;
            if !args.is_empty() {
                args.parse::<Token![crate]>()?;
                args.parse::<Token![=]>()?;
                crate_path = args.parse::<LitStr>()?.parse()?;
                if !args.is_empty() {
                    args.parse::<Token![,]>()?;
                }
            }
        }
        Ok((path, crate_path))
    })?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(&input.ident, "TypedPath requires a struct"));
    };
    let parts = parse_pattern(&path)?;
    let captures: Vec<_> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Capture { name, .. } => Some(name.as_str()),
            Part::Literal(_) => None,
        })
        .collect();
    let fields = &data.fields;
    if fields.len() != captures.len() {
        return Err(syn::Error::new_spanned(
            fields,
            "fields must match the path captures exactly",
        ));
    }
    let members: Vec<_> = match fields {
        Fields::Named(fields) => captures
            .iter()
            .map(|name| {
                fields
                    .named
                    .iter()
                    .find(|field| field.ident.as_ref().is_some_and(|ident| ident.unraw() == *name))
                    .map(|field| Member::Named(field.ident.clone().expect("named field")))
                    .ok_or_else(|| syn::Error::new(path.span(), format!("capture {name:?} has no matching field")))
            })
            .collect::<syn::Result<_>>()?,
        Fields::Unnamed(_) => (0..captures.len()).map(|i| Member::Unnamed(Index::from(i))).collect(),
        Fields::Unit => Vec::new(),
    };
    let field_types: Vec<_> = members
        .iter()
        .map(|member| match member {
            Member::Named(name) => {
                &fields
                    .iter()
                    .find(|field| field.ident.as_ref() == Some(name))
                    .expect("matched field")
                    .ty
            }
            Member::Unnamed(index) => &fields.iter().nth(index.index as usize).expect("matched field").ty,
        })
        .collect();

    let mut pattern = String::new();
    let mut metadata = Vec::new();
    let mut constraints = Vec::new();
    let mut display = Vec::new();
    let mut capture = 0;
    for part in &parts {
        match part {
            Part::Literal(literal) => {
                let encoded = encode_literal(literal);
                pattern.push_str(&encoded);
                metadata.push(quote!((#literal, ::std::option::Option::None)));
                display.push(quote!(formatter.write_str(#encoded)?;));
            }
            Part::Capture { name, catch_all } => {
                pattern.push('{');
                if *catch_all {
                    pattern.push('*');
                }
                for c in name.chars() {
                    if matches!(c, '{' | '}') {
                        pattern.push(c);
                    }
                    pattern.push(c);
                }
                pattern.push('}');
                let member = &members[capture];
                metadata.push(quote!((#name, ::std::option::Option::Some(#catch_all))));
                display.push(quote!(
                    #crate_path::routing::__private::write_parameter(formatter, &self.#member, #catch_all)?;
                ));
                let field = match member {
                    Member::Named(name) => fields
                        .iter()
                        .find(|field| field.ident.as_ref() == Some(name))
                        .expect("named field"),
                    Member::Unnamed(index) => fields.iter().nth(index.index as usize).expect("tuple field"),
                };
                let mut attributes = field.attrs.iter().filter(|attr| attr.path().is_ident("typed_path"));
                if let Some(attr) = attributes.next() {
                    let expression = attr.parse_args_with(|args: syn::parse::ParseStream<'_>| {
                        let key: syn::Ident = args.parse()?;
                        if key != "constraint" {
                            return Err(syn::Error::new_spanned(key, "expected constraint"));
                        }
                        args.parse::<Token![=]>()?;
                        let expression: LitStr = args.parse()?;
                        if !args.is_empty() {
                            args.parse::<Token![,]>()?;
                        }
                        Ok(expression)
                    })?;
                    validate_constraint(&expression)?;
                    if let Some(attr) = attributes.next() {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "a field constraint may only be specified once",
                        ));
                    }
                    constraints.push(quote!((#name, #expression)));
                }
                capture += 1;
            }
        }
    }
    let values = captures
        .iter()
        .zip(&field_types)
        .map(|(name, ty)| quote!(#crate_path::http::RequestExt::param::<#ty>(#request, #name)?))
        .collect::<Vec<_>>();
    let construct = match fields {
        Fields::Named(_) => quote!(Self { #(#members: #values),* }),
        Fields::Unnamed(_) => quote!(Self(#(#values),*)),
        Fields::Unit => quote!(Self),
    };
    let (request, error) = if captures.is_empty() {
        (quote!(_), quote!(::std::convert::Infallible))
    } else {
        (quote!(#request), quote!(#crate_path::http::extract::ParamError))
    };
    let mut display_generics = input.generics.clone();
    let mut extract_generics = input.generics.clone();
    extract_generics.params.insert(0, parse_quote!(#context_lifetime));
    extract_generics.params.insert(0, parse_quote!(#request_lifetime));
    extract_generics.params.push(parse_quote!(#context: ?Sized));
    for ty in field_types {
        display_generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: ::std::fmt::Display));
        extract_generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: #crate_path::routing::__private::DeserializeOwned));
    }
    // Referencing the field reference also supports a generic unsized last field.
    let parameters = members
        .iter()
        .map(|member| quote!(&(&self.#member) as &dyn ::std::fmt::Display))
        .collect::<Vec<_>>();
    let name = &input.ident;
    let (display_impl, type_generics, display_where) = display_generics.split_for_impl();
    let (extract_impl, _, extract_where) = extract_generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #display_impl #crate_path::routing::TypedPath for #name #type_generics #display_where {
            const PATH: &'static str = #pattern;

            #[inline]
            fn pattern() -> &'static #crate_path::routing::Pattern {
                static PATTERN: ::std::sync::LazyLock<#crate_path::routing::Pattern> = ::std::sync::LazyLock::new(|| {
                    #crate_path::routing::__private::pattern(&[#(#metadata),*], &[#(#constraints),*])
                });
                &PATTERN
            }

            fn to_uri(&self) -> ::std::result::Result<#crate_path::http::Uri, #crate_path::routing::UrlError> {
                #crate_path::routing::__private::to_uri(
                    <Self as #crate_path::routing::TypedPath>::pattern(),
                    &[#(#parameters),*],
                )
            }
        }

        #[automatically_derived]
        impl #display_impl ::std::fmt::Display for #name #type_generics #display_where {
            #[inline]
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                #(#display)*
                ::std::result::Result::Ok(())
            }
        }

        #[automatically_derived]
        impl #extract_impl #crate_path::http::extract::FromRequestHead<#request_lifetime, #context_lifetime, #context>
            for #name #type_generics #extract_where
        {
            type Error = #error;

            #[inline]
            async fn from_head(
                #request: &#request_lifetime #crate_path::http::Request,
                _: &#context_lifetime #context,
            ) -> ::std::result::Result<Self, Self::Error> {
                ::std::result::Result::Ok(#construct)
            }
        }
    })
}

fn validate_constraint(expression: &LitStr) -> syn::Result<()> {
    let error = |error: &dyn std::fmt::Display| syn::Error::new(expression.span(), error.to_string());
    let parsed = regex_syntax::parse(&expression.value()).map_err(|source| error(&source))?;
    let anchored = Hir::concat(vec![Hir::look(Look::Start), parsed, Hir::look(Look::End)]);
    regex::Regex::new(&anchored.to_string()).map_err(|source| error(&source))?;
    Ok(())
}

fn parse_pattern(path: &LitStr) -> syn::Result<Vec<Part>> {
    let source = path.value();
    let error = |message| syn::Error::new(path.span(), message);
    if !source.starts_with('/') || source.contains(['?', '#']) {
        return Err(error("a typed path must start with '/' and exclude query and fragment"));
    }
    let mut raw = source.chars().peekable();
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
    let mut names = HashSet::new();
    let mut segment_capture = false;
    while let Some((c, escaped)) = chars.next() {
        if c == '}' && !escaped {
            return Err(error("unmatched closing brace in typed path"));
        }
        if c != '{' || escaped {
            if c == '/' {
                segment_capture = false;
            }
            literal.push(c);
            continue;
        }
        if segment_capture {
            return Err(error("only one capture is allowed per path segment"));
        }
        segment_capture = true;
        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(&mut literal)));
        }
        let catch_all = chars.peek() == Some(&('*', false));
        if catch_all {
            chars.next();
        }
        let mut name = String::new();
        loop {
            let Some((c, escaped)) = chars.next() else {
                return Err(error("missing closing brace in typed path capture"));
            };
            if c == '}' && !escaped {
                break;
            }
            if matches!(c, '/' | '*') || (c == '{' && !escaped) {
                return Err(error("invalid character in capture name"));
            }
            name.push(c);
        }
        if name.is_empty() {
            return Err(error("capture names must not be empty"));
        }
        if !names.insert(name.clone()) {
            return Err(error("capture names must be unique"));
        }
        if catch_all && chars.peek().is_some() {
            return Err(error("a catch-all capture must be at the end of the path"));
        }
        parts.push(Part::Capture { name, catch_all });
    }
    if !literal.is_empty() {
        parts.push(Part::Literal(literal));
    }
    Ok(parts)
}

// PATH is a compile-time constant; runtime formatting delegates to Kavi's Pattern.
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
