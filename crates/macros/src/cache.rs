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

//! Declarative caching attribute macros — `#[cacheable]` / `#[cache_put]` /
//! `#[cache_evict]` (Spring's `@Cacheable` / `@CachePut` / `@CacheEvict`,
//! pyfly's `@cacheable` / `@cache_put` / `@cache_evict`).
//!
//! Each macro body-rewrites an `async fn(...) -> Result<V, E>` so that, when a
//! process-global cache adapter has been registered through
//! [`firefly_cache::register_cache`], the method's result is read from / written
//! to it; when no adapter is registered the method runs its original body
//! unchanged. The cache crate is reached through the `firefly` facade's `__rt`
//! contract, so a service that depends only on `firefly` compiles the generated
//! `firefly_cache::Typed<V>` / `cache_adapter()` references without naming
//! `firefly-cache` directly.
//!
//! The `key` argument is a Rust expression yielding a `ToString` value
//! (commonly a `format!(...)`); it is evaluated *before* the loader closure is
//! built so a key that borrows a method parameter is valid. The Ok type `V` of
//! the `Result<V, E>` return is extracted at expansion time so the generated
//! `Typed::<V>` is fully inferred.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{GenericArgument, ItemFn, PathArguments, ReturnType, Type};

use crate::common::{facade_from_override, parse_duration};

/// Arguments accepted by every cache attribute.
///
/// `all_entries` is only meaningful for `#[cache_evict]`; the other macros
/// reject it. `key` is a string holding a Rust expression (a `format!(...)` or
/// a bare `id.to_string()`), parsed and re-emitted verbatim.
#[derive(FromMeta, Default)]
#[darling(default)]
struct CacheArgs {
    /// Facade override: `#[cacheable(crate = "...")]`.
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// The cache key — a Rust expression producing a `ToString` value, e.g.
    /// `key = "format!(\"order:{}\", id)"`.
    key: Option<String>,
    /// Time-to-live for the stored entry (`#[cacheable(ttl = "60s")]`); parsed
    /// via [`common::parse_duration`](crate::common::parse_duration). `None`
    /// means no expiry.
    ttl: Option<String>,
    /// `#[cache_evict(all_entries)]` — evict every entry whose key starts with
    /// `key` (prefix eviction) rather than the single exact key.
    all_entries: bool,
}

/// Which caching behaviour is being expanded — selects the validation rules and
/// the body-rewrite shape.
#[derive(Clone, Copy)]
enum Kind {
    /// `#[cacheable]` — read-through: on hit return the cached value, on miss
    /// run the body once and store its `Ok` value.
    Cacheable,
    /// `#[cache_put]` — always run the body and store its `Ok` value.
    Put,
    /// `#[cache_evict]` — run the body, then delete the key (or prefix) on `Ok`.
    Evict,
}

impl Kind {
    fn macro_name(self) -> &'static str {
        match self {
            Kind::Cacheable => "#[cacheable]",
            Kind::Put => "#[cache_put]",
            Kind::Evict => "#[cache_evict]",
        }
    }
}

/// Entry point for `#[cacheable]`.
pub(crate) fn cacheable_attr(args: TokenStream, item: ItemFn) -> syn::Result<TokenStream> {
    expand(args, item, Kind::Cacheable)
}

/// Entry point for `#[cache_put]`.
pub(crate) fn cache_put_attr(args: TokenStream, item: ItemFn) -> syn::Result<TokenStream> {
    expand(args, item, Kind::Put)
}

/// Entry point for `#[cache_evict]`.
pub(crate) fn cache_evict_attr(args: TokenStream, item: ItemFn) -> syn::Result<TokenStream> {
    expand(args, item, Kind::Evict)
}

/// Shared expansion: parse args, validate the signature, extract the Ok type,
/// and body-rewrite the function for the requested [`Kind`].
fn expand(args: TokenStream, mut func: ItemFn, kind: Kind) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let parsed = CacheArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&parsed.krate)?;
    let cache = facade.cache();

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            format!(
                "{} requires an `async fn` — the cache read/write is awaited",
                kind.macro_name()
            ),
        ));
    }

    // `all_entries` is a prefix-eviction switch, meaningful only for evict.
    if parsed.all_entries && !matches!(kind, Kind::Evict) {
        return Err(syn::Error::new_spanned(
            &func.sig,
            format!(
                "`all_entries` is only valid on #[cache_evict]; remove it from {}",
                kind.macro_name()
            ),
        ));
    }

    // Extract the Ok type `V` of the `Result<V, E>` return.
    let ok_ty = result_ok_type(&func.sig.output).ok_or_else(|| {
        syn::Error::new_spanned(
            &func.sig,
            format!(
                "{} requires an `async fn` returning `Result<V, E>` (V: Serialize + \
                 DeserializeOwned)",
                kind.macro_name()
            ),
        )
    })?;

    // The key expression, parsed once so a malformed expression is a compile
    // error here rather than at the (rewritten) call site. `#[cacheable]`/
    // `#[cache_put]`/`#[cache_evict]` all require a key.
    let key_expr = key_expr(&parsed, &func, kind)?;

    // The TTL, parsed at expansion time into `Some(Duration)` / `None`.
    let ttl = match &parsed.ttl {
        Some(spec) => {
            let dur = parse_duration(spec, proc_macro2::Span::call_site())?;
            quote!(::core::option::Option::Some(#dur))
        }
        None => quote!(::core::option::Option::None),
    };

    let new_block = match kind {
        Kind::Cacheable => cacheable_block(&func, &cache, &ok_ty, &key_expr, &ttl),
        Kind::Put => put_block(&func, &cache, &ok_ty, &key_expr, &ttl),
        Kind::Evict => evict_block(&func, &cache, &ok_ty, &key_expr, parsed.all_entries),
    };
    func.block = syn::parse2(new_block)?;
    Ok(quote!(#func))
}

/// Read-through `#[cacheable]` body. Computes the key (so a param-borrowing
/// `format!` is valid before the body is moved into the loader), checks the
/// cache on a registered adapter, and on miss runs the original body once and
/// stores its `Ok` value.
///
/// Any `Err` from the cache read — a genuine miss, a backend/transport failure,
/// or a stored-value deserialization error — is treated as a miss and falls
/// through to the loader, so a degraded cache transparently downgrades to a
/// recompute instead of failing the call. A subsequent write failure is
/// likewise swallowed (the freshly computed value is still returned).
fn cacheable_block(
    func: &ItemFn,
    cache: &TokenStream,
    ok_ty: &Type,
    key_expr: &TokenStream,
    ttl: &TokenStream,
) -> TokenStream {
    let orig = &func.block;
    quote! {
        {
            // The loader runs the original body exactly once on a miss. Built
            // before the adapter branch so the key can borrow parameters the
            // loader then moves.
            let __firefly_cache_key = ::std::string::ToString::to_string(&(#key_expr));
            let __firefly_cache_loader = move || async move #orig;
            match #cache::cache_adapter() {
                ::core::option::Option::Some(__firefly_adapter) => {
                    let __firefly_typed = #cache::Typed::<#ok_ty>::new(__firefly_adapter);
                    match __firefly_typed.get(&__firefly_cache_key).await {
                        ::core::result::Result::Ok(__firefly_hit) => {
                            ::core::result::Result::Ok(__firefly_hit)
                        }
                        ::core::result::Result::Err(_) => {
                            match __firefly_cache_loader().await {
                                ::core::result::Result::Ok(__firefly_value) => {
                                    // A cache-write failure must not mask the
                                    // freshly computed value.
                                    let _ = __firefly_typed
                                        .set(&__firefly_cache_key, &__firefly_value, #ttl)
                                        .await;
                                    ::core::result::Result::Ok(__firefly_value)
                                }
                                __firefly_err => __firefly_err,
                            }
                        }
                    }
                }
                ::core::option::Option::None => __firefly_cache_loader().await,
            }
        }
    }
}

/// `#[cache_put]` body: always run the original body; on `Ok`, write the value
/// through under the key (overwriting any existing entry).
fn put_block(
    func: &ItemFn,
    cache: &TokenStream,
    ok_ty: &Type,
    key_expr: &TokenStream,
    ttl: &TokenStream,
) -> TokenStream {
    let orig = &func.block;
    quote! {
        {
            let __firefly_cache_key = ::std::string::ToString::to_string(&(#key_expr));
            match (async move #orig).await {
                ::core::result::Result::Ok(__firefly_value) => {
                    if let ::core::option::Option::Some(__firefly_adapter) =
                        #cache::cache_adapter()
                    {
                        let __firefly_typed = #cache::Typed::<#ok_ty>::new(__firefly_adapter);
                        let _ = __firefly_typed
                            .set(&__firefly_cache_key, &__firefly_value, #ttl)
                            .await;
                    }
                    ::core::result::Result::Ok(__firefly_value)
                }
                __firefly_err => __firefly_err,
            }
        }
    }
}

/// `#[cache_evict]` body: run the original body; on `Ok`, delete the key (or, with
/// `all_entries`, every key with that prefix).
fn evict_block(
    func: &ItemFn,
    cache: &TokenStream,
    ok_ty: &Type,
    key_expr: &TokenStream,
    all_entries: bool,
) -> TokenStream {
    let orig = &func.block;
    let eviction = if all_entries {
        quote!(let _ = __firefly_typed.delete_prefix(&__firefly_cache_key).await;)
    } else {
        quote!(let _ = __firefly_typed.delete(&__firefly_cache_key).await;)
    };
    quote! {
        {
            let __firefly_cache_key = ::std::string::ToString::to_string(&(#key_expr));
            match (async move #orig).await {
                ::core::result::Result::Ok(__firefly_value) => {
                    if let ::core::option::Option::Some(__firefly_adapter) =
                        #cache::cache_adapter()
                    {
                        let __firefly_typed = #cache::Typed::<#ok_ty>::new(__firefly_adapter);
                        #eviction
                    }
                    ::core::result::Result::Ok(__firefly_value)
                }
                __firefly_err => __firefly_err,
            }
        }
    }
}

/// Parses the `key` argument into a Rust expression, turning a missing key into
/// a `syn::Error` compile error.
fn key_expr(args: &CacheArgs, func: &ItemFn, kind: Kind) -> syn::Result<TokenStream> {
    let raw = args.key.as_deref().filter(|s| !s.trim().is_empty());
    match raw {
        Some(expr) => {
            let parsed: syn::Expr = syn::parse_str(expr).map_err(|e| {
                syn::Error::new_spanned(
                    &func.sig,
                    format!(
                        "{} `key` must be a Rust expression producing a `ToString` value \
                         (e.g. key = \"format!(\\\"order:{{}}\\\", id)\"): {e}",
                        kind.macro_name()
                    ),
                )
            })?;
            Ok(quote!(#parsed))
        }
        None => Err(syn::Error::new_spanned(
            &func.sig,
            format!(
                "{} requires a `key`, e.g. key = \"format!(\\\"order:{{}}\\\", id)\"",
                kind.macro_name()
            ),
        )),
    }
}

/// Extracts the `V` of a `Result<V, E>` return type, or `None` when the return
/// is not a `Result<_, _>`. The path's last segment must be `Result` carrying at
/// least one angle-bracketed type argument.
fn result_ok_type(output: &ReturnType) -> Option<Type> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    let Type::Path(type_path) = ty.as_ref() else {
        return None;
    };
    let seg = type_path.path.segments.last()?;
    if seg.ident != "Result" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    })
}
