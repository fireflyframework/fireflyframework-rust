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

//! `#[repository]` — Spring Data's declarative query repository.
//!
//! Applied to an `impl` block, it generates each method body and delegates to
//! the (tested) runtime engine on `SqlxReactiveRepository`. A method is one of
//! three shapes:
//!
//! 1. **Derived query** — named the Spring-Data way (`find_by_…`, `count_by_…`,
//!    `exists_by_…`, `delete_by_…`); the name is parsed into SQL.
//! 2. **Paged derived query** — a `find_by_…` method whose **last argument is a
//!    `Pageable`** and which returns `Result<Vec<T>, _>`; the pageable's sort +
//!    window are appended (Spring's `findByStatus(status, Pageable)`).
//! 3. **`@query` custom query** — a method carrying `#[query("SELECT …")]`
//!    (native SQL) or `#[query(jpql = "…", entity = "…")]`; the method's
//!    arguments bind to the query's `:name` placeholders by identifier.
//!
//! ```ignore
//! #[firefly::repository]
//! impl AccountRepo {
//!     async fn find_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
//!     async fn find_by_owner(&self, owner: &str, page: Pageable) -> Result<Vec<Account>, DataError> { unimplemented!() }
//!     #[query("SELECT * FROM accounts WHERE balance > :min ORDER BY balance DESC")]
//!     async fn richer_than(&self, min: i64) -> Result<Vec<Account>, DataError> { unimplemented!() }
//!     #[query("UPDATE accounts SET status = :to WHERE status = :from")]
//!     async fn migrate(&self, from: &str, to: &str) -> Result<u64, DataError> { unimplemented!() }
//! }
//! ```
//!
//! Each impl-block type exposes the backing repository via an accessor (default
//! `self.repository()`, overridable with `#[repository(repo = "…")]`) returning
//! a `SqlxReactiveRepository<Entity, Id>`. Supported return shapes are
//! `Result<Vec<T>, DataError>`, `Result<Option<T>, DataError>`,
//! `Result<i64, DataError>`, `Result<bool, DataError>`, `Result<u64, DataError>`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, FnArg, ImplItem, ItemImpl, ReturnType, Type};

use crate::common::facade_from_override;

/// The query kind, inferred from the method's return type.
#[derive(Clone, Copy, PartialEq)]
enum QueryKind {
    /// `Vec<T>` — list of rows.
    FindList,
    /// `Option<T>` — first row.
    FindOptional,
    /// `i64` — count.
    Count,
    /// `bool` — exists.
    Exists,
    /// `u64` — delete / modifying execute.
    Delete,
}

/// A parsed `#[query(...)]` attribute: the `CustomQuery` constructor tokens and
/// the JPQL entity name (empty for native SQL).
struct QuerySpec {
    ctor: TokenStream,
    entity: String,
}

#[derive(Default)]
struct RepoArgs {
    krate: Option<String>,
    repo: Option<String>,
}

pub(crate) fn repository_impl(args: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let parsed = parse_args(args)?;
    let facade = facade_from_override(&parsed.krate)?;
    let root = &facade.0;
    let rt = facade.rt();
    let accessor = syn::Ident::new(
        parsed.repo.as_deref().unwrap_or("repository"),
        proc_macro2::Span::call_site(),
    );

    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let name = method.sig.ident.to_string();

        // A `#[query(...)]` attribute (if present) is consumed here, not emitted.
        let query_spec = extract_query_attr(root, &mut method.attrs)?;

        let inner = result_inner_type(&method.sig.output).ok_or_else(|| {
            syn::Error::new_spanned(
                &method.sig,
                "#[repository] methods must return `Result<X, firefly::data::DataError>` where X \
                 is Vec<T> / Option<T> / i64 / bool / u64",
            )
        })?;
        let kind = classify(inner).ok_or_else(|| {
            syn::Error::new_spanned(
                inner,
                "unsupported #[repository] return type; use Result<Vec<T> | Option<T> | i64 | \
                 bool | u64, DataError>",
            )
        })?;

        // Collect argument idents; a trailing `Pageable` is split off as the
        // page request rather than a query parameter.
        let mut arg_idents = Vec::new();
        let mut pageable: Option<syn::Ident> = None;
        let typed: Vec<&syn::PatType> = method
            .sig
            .inputs
            .iter()
            .filter_map(|a| match a {
                FnArg::Typed(p) => Some(p),
                FnArg::Receiver(_) => None,
            })
            .collect();
        for (idx, pat) in typed.iter().enumerate() {
            let syn::Pat::Ident(p) = &*pat.pat else {
                return Err(syn::Error::new_spanned(
                    &pat.pat,
                    "#[repository] method arguments must be simple identifiers",
                ));
            };
            if idx + 1 == typed.len() && is_pageable(&pat.ty) {
                pageable = Some(p.ident.clone());
            } else {
                arg_idents.push(p.ident.clone());
            }
        }

        if pageable.is_some() && query_spec.is_some() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a `#[query]` already pins the query; drop the trailing `Pageable` and write the \
                 ORDER BY / LIMIT yourself",
            ));
        }
        if pageable.is_some() && kind != QueryKind::FindList {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a trailing `Pageable` is only supported on a `find_by_…` returning `Vec<T>`",
            ));
        }

        // Marshal step: a `:name`→value map for `@query`, else a positional Vec.
        let backend_err = quote! {
            |__me: #rt::serde_json::Error| #root::data::DataError::Backend(__me.to_string())
        };
        let marshal = if let Some(spec) = &query_spec {
            let ctor = &spec.ctor;
            quote! {
                let mut __params: ::std::collections::BTreeMap<
                    ::std::string::String, #rt::serde_json::Value> =
                    ::std::collections::BTreeMap::new();
                #(
                    __params.insert(
                        ::core::stringify!(#arg_idents).to_string(),
                        #rt::serde_json::to_value(&#arg_idents).map_err(#backend_err)?,
                    );
                )*
                let __q = #ctor;
            }
        } else {
            quote! {
                let __args: ::std::vec::Vec<#rt::serde_json::Value> = ::std::vec![
                    #( #rt::serde_json::to_value(&#arg_idents).map_err(#backend_err)? ),*
                ];
            }
        };

        // The Flux/Mono producer for this method.
        let call = if let Some(spec) = &query_spec {
            let entity = &spec.entity;
            match kind {
                QueryKind::FindList | QueryKind::FindOptional => {
                    quote!(self.#accessor().query_list(&__q, #entity, &__params))
                }
                QueryKind::Count => quote!(self.#accessor().query_count(&__q, #entity, &__params)),
                QueryKind::Exists => quote!(self.#accessor().query_exists(&__q, #entity, &__params)),
                QueryKind::Delete => {
                    quote!(self.#accessor().query_execute(&__q, #entity, &__params))
                }
            }
        } else if let Some(page) = &pageable {
            quote!(self.#accessor().find_by_derived_paged(#name, &__args, &#page))
        } else {
            match kind {
                QueryKind::FindList | QueryKind::FindOptional => {
                    quote!(self.#accessor().find_by_derived(#name, &__args))
                }
                QueryKind::Count => quote!(self.#accessor().count_by_derived(#name, &__args)),
                QueryKind::Exists => quote!(self.#accessor().exists_by_derived(#name, &__args)),
                QueryKind::Delete => quote!(self.#accessor().delete_by_derived(#name, &__args)),
            }
        };

        // The Err binding `__e` is the runtime FireflyError; map it inline.
        let err_arm = quote! {
            ::core::result::Result::Err(__e) =>
                ::core::result::Result::Err(#root::data::DataError::Backend(__e.to_string()))
        };
        let body = match kind {
            QueryKind::FindList => quote! {{
                #marshal
                match #call.collect_list().into_future().await {
                    ::core::result::Result::Ok(__o) => ::core::result::Result::Ok(__o.unwrap_or_default()),
                    #err_arm,
                }
            }},
            QueryKind::FindOptional => quote! {{
                #marshal
                match #call.collect_list().into_future().await {
                    ::core::result::Result::Ok(__o) =>
                        ::core::result::Result::Ok(__o.unwrap_or_default().into_iter().next()),
                    #err_arm,
                }
            }},
            QueryKind::Count => quote! {{
                #marshal
                match #call.into_future().await {
                    ::core::result::Result::Ok(__o) => ::core::result::Result::Ok(__o.unwrap_or_default()),
                    #err_arm,
                }
            }},
            QueryKind::Exists => quote! {{
                #marshal
                match #call.into_future().await {
                    ::core::result::Result::Ok(__o) => ::core::result::Result::Ok(__o.unwrap_or(false)),
                    #err_arm,
                }
            }},
            QueryKind::Delete => quote! {{
                #marshal
                match #call.into_future().await {
                    ::core::result::Result::Ok(__o) => ::core::result::Result::Ok(__o.unwrap_or_default()),
                    #err_arm,
                }
            }},
        };
        method.block = syn::parse2(body)?;
    }

    Ok(quote!(#item))
}

/// Finds and removes a `#[query(...)]` attribute, parsing it into a
/// [`QuerySpec`]. Returns `Ok(None)` when the method has no `#[query]`.
fn extract_query_attr(
    root: &syn::Path,
    attrs: &mut Vec<Attribute>,
) -> syn::Result<Option<QuerySpec>> {
    let Some(pos) = attrs.iter().position(|a| a.path().is_ident("query")) else {
        return Ok(None);
    };
    let attr = attrs.remove(pos);
    // `#[query("SELECT …")]` — a single string literal means native SQL.
    if let Ok(lit) = attr.parse_args::<syn::LitStr>() {
        let sql = lit.value();
        return Ok(Some(QuerySpec {
            ctor: quote!(#root::data::CustomQuery::native(#sql)),
            entity: String::new(),
        }));
    }
    // Keyed forms: `sql = "…"` / `native = "…"` / `jpql = "…"` / `entity = "…"`.
    let mut sql: Option<String> = None;
    let mut jpql: Option<String> = None;
    let mut entity = String::new();
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("sql") || meta.path.is_ident("native") {
            sql = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("jpql") {
            jpql = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("entity") {
            entity = meta.value()?.parse::<syn::LitStr>()?.value();
        } else {
            return Err(meta.error("unknown #[query] argument; use `sql`, `jpql`, or `entity`"));
        }
        Ok(())
    });
    attr.parse_args_with(parser)?;
    let ctor = match (sql, jpql) {
        (Some(s), None) => quote!(#root::data::CustomQuery::native(#s)),
        (None, Some(j)) => quote!(#root::data::CustomQuery::jpql(#j)),
        (None, None) => {
            return Err(syn::Error::new_spanned(
                &attr,
                "#[query] needs the query text: #[query(\"…\")], #[query(sql = \"…\")], or \
                 #[query(jpql = \"…\")]",
            ))
        }
        (Some(_), Some(_)) => {
            return Err(syn::Error::new_spanned(
                &attr,
                "#[query] takes either `sql`/native or `jpql`, not both",
            ))
        }
    };
    Ok(Some(QuerySpec { ctor, entity }))
}

/// Whether `ty` is a `Pageable` (matched by the last path segment, so both
/// `Pageable` and `firefly::data::Pageable` are recognised).
fn is_pageable(ty: &Type) -> bool {
    matches!(ty, Type::Path(tp) if tp.path.segments.last().map(|s| s.ident == "Pageable").unwrap_or(false))
}

/// Extracts `X` from a `-> Result<X, _>` return type.
fn result_inner_type(output: &ReturnType) -> Option<&Type> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    let Type::Path(tp) = ty.as_ref() else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Result" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|a| match a {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// Classifies the inner return type into a [`QueryKind`].
fn classify(ty: &Type) -> Option<QueryKind> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    match seg.ident.to_string().as_str() {
        "Vec" => Some(QueryKind::FindList),
        "Option" => Some(QueryKind::FindOptional),
        "i64" => Some(QueryKind::Count),
        "bool" => Some(QueryKind::Exists),
        "u64" => Some(QueryKind::Delete),
        _ => None,
    }
}

fn parse_args(args: TokenStream) -> syn::Result<RepoArgs> {
    let mut out = RepoArgs::default();
    if args.is_empty() {
        return Ok(out);
    }
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("crate") {
            out.krate = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("repo") {
            out.repo = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else {
            return Err(meta.error("unknown #[repository] argument; use `repo` or `crate`"));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, args)?;
    Ok(out)
}
