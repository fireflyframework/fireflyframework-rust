// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The `#[transactional]` attribute — Spring's `@Transactional`.
//!
//! Wraps an `async fn`'s body in
//! [`firefly_transactional::transactional`], so the function runs inside a
//! transaction governed by the registered
//! [`TransactionManager`](firefly_transactional::TransactionManager): commit on
//! `Ok`, roll back on `Err`. The function's error type must implement
//! `From<firefly_transactional::TxError>` so begin/commit failures surface
//! through the normal `?` path.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemFn, ReturnType};

use crate::common::facade_from_override;

/// Per-call `#[transactional(...)]` options.
#[derive(Default)]
struct TxAttr {
    krate: Option<String>,
    propagation: Option<String>,
    isolation: Option<String>,
    read_only: bool,
    timeout_ms: Option<u64>,
}

/// Expands `#[transactional]` / `#[transactional(propagation = "...", …)]` on an
/// `async fn`.
pub(crate) fn transactional_impl(args: TokenStream, mut func: ItemFn) -> syn::Result<TokenStream> {
    let attr = parse_attr(args)?;
    let facade = facade_from_override(&attr.krate)?;
    let tx = facade.transactional();

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[transactional] requires an `async fn` — the transaction boundary is awaited",
        ));
    }

    let propagation = propagation_tokens(&tx, attr.propagation.as_deref())?;
    let isolation = isolation_tokens(&tx, attr.isolation.as_deref())?;
    let read_only = attr.read_only;
    let timeout = match attr.timeout_ms {
        Some(ms) => quote!(::core::option::Option::Some(::core::time::Duration::from_millis(#ms))),
        None => quote!(::core::option::Option::None),
    };

    // Take the original body and wrap it. The original block becomes the
    // operation closure; the function body becomes the orchestrator call.
    let block = &func.block;
    let output = &func.sig.output;
    // The closure's async block must return the same `Result<R, E>` the fn does;
    // an explicit return type is required so the bound `E: From<TxError>` holds.
    if matches!(output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[transactional] async fn must return a `Result<T, E>` where `E: From<\
             firefly_transactional::TxError>`",
        ));
    }

    let new_block = quote! {
        {
            #tx::transactional(
                #tx::TxOptions {
                    propagation: #propagation,
                    isolation: #isolation,
                    read_only: #read_only,
                    timeout: #timeout,
                },
                move || async move #block,
            )
            .await
        }
    };
    func.block = syn::parse2(new_block)?;

    Ok(quote!(#func))
}

fn propagation_tokens(tx: &TokenStream, value: Option<&str>) -> syn::Result<TokenStream> {
    let variant = match value.map(normalize).as_deref() {
        None | Some("required") => "Required",
        Some("requiresnew") | Some("requires_new") => "RequiresNew",
        Some("nested") => "Nested",
        Some("supports") => "Supports",
        Some("notsupported") | Some("not_supported") => "NotSupported",
        Some("mandatory") => "Mandatory",
        Some("never") => "Never",
        Some(other) => {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "unknown propagation {other:?}; use required, requires_new, nested, supports, \
                     not_supported, mandatory, or never"
                ),
            ))
        }
    };
    let ident = syn::Ident::new(variant, proc_macro2::Span::call_site());
    Ok(quote!(#tx::Propagation::#ident))
}

fn isolation_tokens(tx: &TokenStream, value: Option<&str>) -> syn::Result<TokenStream> {
    let variant = match value.map(normalize).as_deref() {
        None | Some("default") => "Default",
        Some("readuncommitted") | Some("read_uncommitted") => "ReadUncommitted",
        Some("readcommitted") | Some("read_committed") => "ReadCommitted",
        Some("repeatableread") | Some("repeatable_read") => "RepeatableRead",
        Some("serializable") => "Serializable",
        Some(other) => {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "unknown isolation {other:?}; use default, read_uncommitted, read_committed, \
                     repeatable_read, or serializable"
                ),
            ))
        }
    };
    let ident = syn::Ident::new(variant, proc_macro2::Span::call_site());
    Ok(quote!(#tx::Isolation::#ident))
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase().replace('-', "_")
}

fn parse_attr(args: TokenStream) -> syn::Result<TxAttr> {
    let mut attr = TxAttr::default();
    if args.is_empty() {
        return Ok(attr);
    }
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("crate") {
            attr.krate = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("propagation") {
            attr.propagation = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("isolation") {
            attr.isolation = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("read_only") {
            // Bare `read_only` or `read_only = true/false`.
            if meta.input.peek(syn::Token![=]) {
                attr.read_only = meta.value()?.parse::<syn::LitBool>()?.value();
            } else {
                attr.read_only = true;
            }
        } else if meta.path.is_ident("timeout_ms") {
            attr.timeout_ms = Some(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?);
        } else {
            return Err(meta.error(
                "unknown #[transactional] argument; use propagation, isolation, read_only, \
                 timeout_ms, or crate",
            ));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, args)?;
    Ok(attr)
}
