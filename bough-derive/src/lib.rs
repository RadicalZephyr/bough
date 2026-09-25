// SPDX-License-Identifier: MPL-2.0

//! # Status
//!
//! This is the API skeleton: every public signature, with `todo!()` bodies.
//! The examples in the documentation compile against it, and the guarantees
//! the RFDs make are fixed by `compile_fail` doc tests. The engine lands
//! behind these signatures one increment at a time.
//!
//! # Derive macros for bough.
//!
//! `#[derive(Trace)]` implements `bough::Trace` for a struct or an enum by
//! tracing every field, and `#[trace(skip)]` leaves a field out. The `bough`
//! crate re-exports it with its `derive` feature, so it is written
//! `#[derive(bough::Trace)]` or, with the trait imported, `#[derive(Trace)]`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;

use syn::{DeriveInput, parse_macro_input};

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

fn expand(_input: DeriveInput) -> syn::Result<TokenStream2> {
    todo!("expand #[derive(Trace)]")
}
