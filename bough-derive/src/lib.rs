//! Derive macros for bough.
//!
//! `#[derive(Trace)]` implements `bough::Trace` for a struct or an enum by
//! tracing every field, and `#[trace(skip)]` leaves a field out. The `bough`
//! crate re-exports it with its `derive` feature, so it is written
//! `#[derive(bough::Trace)]` or, with the trait imported, `#[derive(Trace)]`.

use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Ident, parse_macro_input, parse_quote};

/// Derives `bough::Trace`: the collector's walk visits every token the
/// value holds, directly or inside its fields (RFD 3).
///
/// Every field is traced, so every field's type must implement `Trace`.
/// `#[trace(skip)]` leaves a field out, for a type that cannot hold tokens
/// and has no `Trace` implementation, such as another crate's channel
/// sender. A skipped field that does hold a token hides it from the
/// collector, which may then free its node: the token's next use is a
/// stale-token error, never a read of another node, which is why `Trace` is
/// a safe trait.
///
/// A type parameter that a traced field's type names is bound by `Trace`;
/// one only skipped fields name is not bound.
#[proc_macro_derive(Trace, attributes(trace))]
pub fn derive_trace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand(mut input: DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    let mut traced_types = Vec::new();
    let body = match &input.data {
        Data::Struct(data) => {
            let (pattern, visits) = destructure(&data.fields, quote!(Self), &mut traced_types)?;
            if visits.is_empty() {
                quote! { let _ = tracer; }
            } else {
                quote! {
                    let #pattern = self;
                    #(#visits)*
                }
            }
        }
        Data::Enum(data) => {
            if data.variants.is_empty() {
                quote! { match *self {} }
            } else {
                let mut arms = Vec::new();
                let mut any = false;
                for variant in &data.variants {
                    let ident = &variant.ident;
                    let (pattern, visits) =
                        destructure(&variant.fields, quote!(Self::#ident), &mut traced_types)?;
                    any |= !visits.is_empty();
                    arms.push(quote! { #pattern => { #(#visits)* } });
                }
                let unused = (!any).then(|| quote! { let _ = tracer; });
                quote! {
                    #unused
                    match self { #(#arms)* }
                }
            }
        }
        Data::Union(data) => {
            return Err(syn::Error::new(
                data.union_token.span,
                "Trace cannot be derived for a union: which field holds the value is not \
                 known; implement Trace by hand",
            ));
        }
    };

    // Bound the type parameters the traced fields' types name.
    let bounded: Vec<Ident> = input
        .generics
        .type_params()
        .map(|param| param.ident.clone())
        .filter(|param| traced_types.iter().any(|ty| names(ty, param)))
        .collect();
    let where_clause = input.generics.make_where_clause();
    for param in &bounded {
        where_clause
            .predicates
            .push(parse_quote!(#param: ::bough::Trace));
    }
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::bough::Trace for #name #ty_generics #where_clause {
            fn trace(&self, tracer: &mut ::bough::Tracer) {
                #body
            }
        }
    })
}

/// A pattern that binds the traced fields of a struct or a variant, and a
/// visit of each, collecting the traced fields' types. The bindings are
/// named after their position, so no field name can shadow `tracer`.
fn destructure(
    fields: &Fields,
    path: TokenStream2,
    traced_types: &mut Vec<TokenStream2>,
) -> syn::Result<(TokenStream2, Vec<TokenStream2>)> {
    let mut visits = Vec::new();
    let pattern = match fields {
        Fields::Named(named) => {
            let mut bound = Vec::new();
            for (i, field) in named.named.iter().enumerate() {
                if skipped(&field.attrs)? {
                    continue;
                }
                let ident = field.ident.as_ref().expect("a named field has a name");
                let binding = format_ident!("__bough_field_{}", i);
                bound.push(quote! { #ident: #binding });
                visits.push(visit(&binding, &field.ty));
                let ty = &field.ty;
                traced_types.push(quote!(#ty));
            }
            quote! { #path { #(#bound,)* .. } }
        }
        Fields::Unnamed(unnamed) => {
            let mut bound = Vec::new();
            for (i, field) in unnamed.unnamed.iter().enumerate() {
                if skipped(&field.attrs)? {
                    bound.push(quote! { _ });
                    continue;
                }
                let binding = format_ident!("__bough_field_{}", i);
                bound.push(quote! { #binding });
                visits.push(visit(&binding, &field.ty));
                let ty = &field.ty;
                traced_types.push(quote!(#ty));
            }
            quote! { #path ( #(#bound),* ) }
        }
        Fields::Unit => quote! { #path },
    };
    Ok((pattern, visits))
}

/// The visit of one field, spanned at its type, so that a type with no
/// `Trace` is an error at the field rather than at the derive.
fn visit(binding: &Ident, ty: &syn::Type) -> TokenStream2 {
    quote_spanned! {ty.span()=>
        ::bough::Trace::trace(#binding, tracer);
    }
}

/// Whether a field carries `#[trace(skip)]`. Any other `trace` attribute is
/// an error.
fn skipped(attrs: &[syn::Attribute]) -> syn::Result<bool> {
    let mut skip = false;
    for attr in attrs {
        if !attr.path().is_ident("trace") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                skip = true;
                Ok(())
            } else {
                Err(meta.error("unknown trace attribute: the one attribute is `#[trace(skip)]`"))
            }
        })?;
    }
    Ok(skip)
}

/// Whether a type names `param` anywhere, `T`, `Vec<T>` or `T::Item`.
fn names(ty: &TokenStream2, param: &Ident) -> bool {
    ty.clone().into_iter().any(|tree| match tree {
        TokenTree::Ident(ident) => ident == *param,
        TokenTree::Group(group) => names(&group.stream(), param),
        _ => false,
    })
}
