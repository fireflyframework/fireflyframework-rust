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
//!
//! `#[transactional(manager = "<expr>")]` (Spring's
//! `@Transactional("txManagerBean")`) instead drives an **explicit** manager via
//! [`transactional_on`](firefly_transactional::transactional_on) — the expression
//! (e.g. `self.tx_manager()`) yields a value `m` with
//! `&m: &Arc<dyn TransactionManager>`. This keeps a multi-datasource service, or
//! a per-test-isolated one, off the process-global registry.
//!
//! `#[transactional(no_rollback_for = "<pat>", rollback_only_for = "<pat>")]`
//! control which returned errors roll back. Because `Result`'s `Err(E)` already
//! separates failure from success, the rule is expressed as an error **pattern**
//! (Spring names exception *types*) and decides, per error, whether a returned
//! `Err` rolls back or commits anyway:
//!
//! - default (no rule): roll back on **every** `Err` (Rust has no
//!   checked/unchecked split, so all errors roll back);
//! - `no_rollback_for = "P"` — **Spring's `@Transactional(noRollbackFor = …)`**:
//!   an `Err` matching pattern `P` **commits** instead of rolling back (e.g. a
//!   domain "already-applied" that should still persist its side effect);
//! - `rollback_only_for = "P"`: roll back **only** when the `Err` matches `P`,
//!   committing every other error. This is a Rust-native *restrictive* rule —
//!   deliberately **not** named `rollback_for`, because Spring's `rollbackFor`
//!   is *additive* (it widens an always-rollback set that does not exist here,
//!   since every Rust `Err` already rolls back). The distinct name keeps a
//!   Spring port from being silently inverted;
//! - with both, `no_rollback_for` wins on overlap.
//!
//! Each pattern is any Rust match pattern (no `if` guard) valid for the fn's
//! error type, alternatives included: `no_rollback_for = "Error::A | Error::B"`.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::Parser as _;
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
    /// `#[transactional(manager = "<expr>")]` — Spring's
    /// `@Transactional("txManagerBean")`: run against an **explicit**
    /// `TransactionManager` expression (e.g. `self.tx_manager()`) instead of the
    /// process-global registry. The expression must yield a value `m` such that
    /// `&m: &Arc<dyn TransactionManager>`. Use this for a multi-datasource
    /// service, or to keep per-test isolation (each instance owns its manager).
    manager: Option<String>,
    /// `#[transactional(rollback_only_for = "<pat>")]` — roll back **only** when
    /// the returned `Err` matches this pattern (commit every other error). A
    /// Rust-native *restrictive* rule: **not** Spring's `rollbackFor`, which is
    /// *additive*. Named distinctly so a Spring port is never silently inverted.
    /// A match pattern valid for the fn's error type, e.g. `"Error::Backend(_)"`.
    rollback_only_for: Option<String>,
    /// `#[transactional(no_rollback_for = "<pat>")]` — Spring's
    /// `@Transactional(noRollbackFor = …)`. An `Err` matching this pattern
    /// **commits** instead of rolling back. Wins over `rollback_only_for` on
    /// overlap.
    no_rollback_for: Option<String>,
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

    let options = quote! {
        #tx::TxOptions {
            propagation: #propagation,
            isolation: #isolation,
            read_only: #read_only,
            timeout: #timeout,
        }
    };

    // A `rollback_for` / `no_rollback_for` rule, if any, becomes a
    // `should_rollback(&E) -> bool` predicate; its presence selects the
    // `transactional_with` / `transactional_with_on` runtime variants.
    let predicate = rollback_predicate(&attr)?;

    let manager = match attr
        .manager
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(expr) => Some(syn::parse_str::<syn::Expr>(expr).map_err(|e| {
            syn::Error::new_spanned(
                &func.sig,
                format!(
                    "#[transactional] `manager` must be a Rust expression yielding a value \
                     `m` with `&m: &Arc<dyn TransactionManager>` (e.g. \
                     manager = \"self.tx_manager()\"): {e}"
                ),
            )
        })?),
        None => None,
    };

    // Four runtime entry points, picked by (explicit manager?) × (rollback rule?):
    // Spring's `@Transactional` / `@Transactional("mgr")` / `@Transactional(rollbackFor=…)`.
    let op = quote!(move || async move #block);
    let driver = match (manager, predicate) {
        (Some(manager), Some(pred)) => quote! {
            #tx::transactional_with_on(&(#manager), #options, #pred, #op).await
        },
        (Some(manager), None) => quote! {
            #tx::transactional_on(&(#manager), #options, #op).await
        },
        (None, Some(pred)) => quote! {
            #tx::transactional_with(#options, #pred, #op).await
        },
        (None, None) => quote! {
            #tx::transactional(#options, #op).await
        },
    };

    let new_block = quote! { { #driver } };
    func.block = syn::parse2(new_block)?;

    Ok(quote!(#func))
}

/// Builds the `should_rollback(&E) -> bool` predicate from the
/// `rollback_only_for` / `no_rollback_for` patterns, or `None` when neither is
/// set (the default "rollback on every `Err`" needs no predicate). Precedence:
/// `no_rollback_for` is checked first and wins; then `rollback_only_for` (when
/// set) restricts the rollback set; otherwise everything else rolls back.
fn rollback_predicate(attr: &TxAttr) -> syn::Result<Option<TokenStream>> {
    let rollback = attr
        .rollback_only_for
        .as_deref()
        .map(|s| parse_pattern(s, "rollback_only_for"))
        .transpose()?;
    let no_rollback = attr
        .no_rollback_for
        .as_deref()
        .map(|s| parse_pattern(s, "no_rollback_for"))
        .transpose()?;
    if rollback.is_none() && no_rollback.is_none() {
        return Ok(None);
    }
    // `__tx_err: &E`; match ergonomics let a value pattern match through the
    // reference, so `matches!(__tx_err, Error::Variant)` is correct.
    let no_rollback_guard = no_rollback.map(|pat| {
        quote! { if ::core::matches!(__tx_err, #pat) { return false; } }
    });
    let rollback_decision = match rollback {
        Some(pat) => quote! { ::core::matches!(__tx_err, #pat) },
        None => quote! { true },
    };
    Ok(Some(quote! {
        |__tx_err: &_| -> bool {
            #no_rollback_guard
            #rollback_decision
        }
    }))
}

/// Parses a `rollback_only_for` / `no_rollback_for` string into a match pattern
/// (allowing `A | B` alternatives; no `if` guard — `matches!` rejects guards),
/// with an error message naming the argument.
fn parse_pattern(src: &str, arg: &str) -> syn::Result<syn::Pat> {
    syn::Pat::parse_multi.parse_str(src).map_err(|e| {
        syn::Error::new(
            Span::call_site(),
            format!(
                "#[transactional] `{arg}` must be a match pattern valid for the fn's error type \
                 (e.g. {arg} = \"Error::NotFound\" or \"Error::A | Error::B\"): {e}"
            ),
        )
    })
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
        } else if meta.path.is_ident("manager") {
            attr.manager = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("rollback_only_for") {
            attr.rollback_only_for = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("no_rollback_for") {
            attr.no_rollback_for = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("rollback_for") {
            // A Spring migrant's reflex — but Spring's `rollbackFor` is *additive*
            // and has no faithful Rust analog (every `Err` already rolls back).
            // Reject with a pointer to the two correct rules rather than silently
            // inverting their transaction's rollback behaviour.
            return Err(meta.error(
                "#[transactional] has no `rollback_for`: Spring's `rollbackFor` is additive and \
                 every Rust `Err` already rolls back. Use `no_rollback_for = \"<pat>\"` (Spring's \
                 noRollbackFor — commit a matching error) or `rollback_only_for = \"<pat>\"` \
                 (roll back only matching errors)",
            ));
        } else {
            return Err(meta.error(
                "unknown #[transactional] argument; use propagation, isolation, read_only, \
                 timeout_ms, manager, no_rollback_for, rollback_only_for, or crate",
            ));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, args)?;
    Ok(attr)
}
