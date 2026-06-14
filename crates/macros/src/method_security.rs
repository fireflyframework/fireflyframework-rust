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

//! `#[pre_authorize(...)]` and `#[post_authorize(...)]` — Spring Security
//! method security, lowered onto the
//! [`firefly_security`](firefly_security) ambient context.
//!
//! `#[pre_authorize]` evaluates an access rule *before* the body runs;
//! `#[post_authorize]` evaluates a boolean expression over the returned value
//! *after* the body completes successfully. Both read the caller from the
//! task-local security context that `BearerLayer` scopes around the request, so
//! they work on a plain service method that never sees the `Request`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Expr, ItemFn, Lit, Meta, ReturnType};

use crate::common::Facade;

/// Expands `#[pre_authorize(<rule>)]` on a function that returns
/// `Result<T, E>` where `E: From<firefly_security::SecurityError>`.
///
/// The rule is one of: `authenticated` (the default when the attribute is
/// empty), `role = "X"`, `any_role = ["A", "B"]`, `authority = "X"`, or
/// `any_authority = ["A", "B"]`.
pub(crate) fn pre_authorize_impl(args: TokenStream, mut func: ItemFn) -> syn::Result<TokenStream> {
    let sec = Facade::default().security();
    let rule = parse_rule(&sec, args)?;

    if matches!(func.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[pre_authorize] requires a function returning `Result<T, E>` where \
             `E: From<firefly_security::SecurityError>` — the denial surfaces as `Err`",
        ));
    }

    let block = &func.block;
    // The `?` propagates the denial through `From<SecurityError>`; the granted
    // `Authentication` it returns is intentionally discarded.
    let new_block = quote! {
        {
            #sec::check_access(&#rule)?;
            #block
        }
    };
    func.block = syn::parse2(new_block)?;
    Ok(quote!(#func))
}

/// Expands `#[post_authorize(<expr>)]` on an `async fn` returning
/// `Result<T, E>` where `E: From<firefly_security::SecurityError>`.
///
/// `<expr>` is a Rust boolean expression evaluated after a successful call with
/// two bindings in scope: `result` (a `&T` reference to the returned value, the
/// Spring `returnObject`) and `auth` (a `&Authentication` of the caller). When
/// it is `false` the call is denied with `Forbidden` and the value discarded;
/// when no security context is active the call is `Unauthenticated`.
pub(crate) fn post_authorize_impl(args: TokenStream, mut func: ItemFn) -> syn::Result<TokenStream> {
    let sec = Facade::default().security();

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[post_authorize] requires an `async fn` — the body is awaited before the \
             return value is authorized",
        ));
    }
    if matches!(func.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[post_authorize] requires an `async fn` returning `Result<T, E>` where \
             `E: From<firefly_security::SecurityError>`",
        ));
    }
    if args.is_empty() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[post_authorize(<expr>)] needs a boolean expression over `result` and `auth`, \
             e.g. #[post_authorize(result.owner == auth.principal)]",
        ));
    }
    let check: Expr = syn::parse2(args)?;

    let block = &func.block;
    // Run the body in an async block so even early `return` is captured as the
    // value to authorize, then evaluate `check` with `result`/`auth` bound.
    let new_block = quote! {
        {
            match (async move #block).await {
                ::core::result::Result::Ok(__value) => match #sec::current_authentication() {
                    ::core::option::Option::Some(__auth) => {
                        let __granted: bool = {
                            let result = &__value;
                            let auth = &__auth;
                            #check
                        };
                        if __granted {
                            ::core::result::Result::Ok(__value)
                        } else {
                            ::core::result::Result::Err(::core::convert::From::from(
                                #sec::SecurityError::Forbidden,
                            ))
                        }
                    }
                    ::core::option::Option::None => ::core::result::Result::Err(
                        ::core::convert::From::from(#sec::SecurityError::Unauthenticated),
                    ),
                },
                __err => __err,
            }
        }
    };
    func.block = syn::parse2(new_block)?;
    Ok(quote!(#func))
}

/// Parses the `#[pre_authorize(...)]` attribute into an `AccessRule`
/// constructor expression rooted at the security facade path `#sec`.
fn parse_rule(sec: &TokenStream, args: TokenStream) -> syn::Result<TokenStream> {
    if args.is_empty() {
        return Ok(quote!(#sec::AccessRule::Authenticated));
    }
    let meta: Meta = syn::parse2(args)?;
    match &meta {
        // `#[pre_authorize(authenticated)]`
        Meta::Path(path) if path.is_ident("authenticated") => {
            Ok(quote!(#sec::AccessRule::Authenticated))
        }
        Meta::Path(path) => Err(syn::Error::new_spanned(
            path,
            "unknown rule; use `authenticated`, `role = \"..\"`, `any_role = [..]`, \
             `authority = \"..\"`, or `any_authority = [..]`",
        )),
        // `key = value` forms.
        Meta::NameValue(nv) => {
            let key = nv
                .path
                .get_ident()
                .map(|i| i.to_string())
                .unwrap_or_default();
            match key.as_str() {
                "role" => {
                    let s = expect_str(&nv.value)?;
                    Ok(quote!(#sec::AccessRule::Role(#s)))
                }
                "authority" => {
                    let s = expect_str(&nv.value)?;
                    Ok(quote!(#sec::AccessRule::Authority(#s)))
                }
                "any_role" => {
                    let items = expect_str_array(&nv.value)?;
                    Ok(quote!(#sec::AccessRule::AnyRole(&[#(#items),*])))
                }
                "any_authority" => {
                    let items = expect_str_array(&nv.value)?;
                    Ok(quote!(#sec::AccessRule::AnyAuthority(&[#(#items),*])))
                }
                other => Err(syn::Error::new_spanned(
                    &nv.path,
                    format!(
                        "unknown rule key `{other}`; use `role`, `any_role`, `authority`, \
                         or `any_authority`"
                    ),
                )),
            }
        }
        Meta::List(list) => Err(syn::Error::new_spanned(
            list,
            "expected `authenticated` or `key = value`, not a nested list",
        )),
    }
}

/// Extracts a string-literal value from `role = ".."` / `authority = ".."`.
fn expect_str(value: &Expr) -> syn::Result<String> {
    if let Expr::Lit(lit) = value {
        if let Lit::Str(s) = &lit.lit {
            return Ok(s.value());
        }
    }
    Err(syn::Error::new_spanned(
        value,
        "expected a string literal, e.g. role = \"ADMIN\"",
    ))
}

/// Extracts the string literals from `any_role = ["A", "B"]`.
fn expect_str_array(value: &Expr) -> syn::Result<Vec<String>> {
    let Expr::Array(array) = value else {
        return Err(syn::Error::new_spanned(
            value,
            "expected an array of string literals, e.g. any_role = [\"ADMIN\", \"OPS\"]",
        ));
    };
    array.elems.iter().map(expect_str).collect()
}
