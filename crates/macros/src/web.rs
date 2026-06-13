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

//! `#[rest_controller(path = "...")]` on an `impl` block, plus the
//! `#[get]` / `#[post]` / `#[put]` / `#[delete]` / `#[patch]` method markers.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ItemImpl, LitStr};

use crate::common::{facade_from_override, LitStrArg};

/// Arguments accepted by `#[rest_controller(...)]`.
#[derive(FromMeta, Default)]
#[darling(default)]
struct ControllerArgs {
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// The base path prepended to every method route, e.g. `"/api/v1/orders"`.
    path: Option<String>,
    /// The axum `State` type the router is built with. Defaults to `Self` —
    /// the controller value itself becomes the shared state.
    state: Option<String>,
}

/// One HTTP-verb mapping discovered on a method.
struct Mapping {
    method_ident: syn::Ident,
    /// `get`, `post`, `put`, `delete`, `patch`.
    verb: &'static str,
    /// The method-relative path (joined onto the controller base path).
    path: String,
}

const VERBS: &[&str] = &["get", "post", "put", "delete", "patch"];

/// Expands `#[rest_controller(path = "...")]` on an `impl` block into the
/// original impl plus a generated `fn routes(state) -> axum::Router`.
pub(crate) fn rest_controller(args: TokenStream, item: ItemImpl) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = ControllerArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    // Validate the facade override even though the generated router needs no
    // `firefly_web` path itself: the controller's own handler signatures carry
    // `firefly_web::WebResult`, supplied by the user through the facade, and
    // axum performs the HTTP wiring.
    let _facade = facade_from_override(&args.krate)?;

    let mut item = item;
    let self_ty = (*item.self_ty).clone();
    let base_path = args.path.clone().unwrap_or_default();
    let base_path = base_path.trim_end_matches('/').to_string();

    // The state type the router carries. `Self` means the controller itself.
    let state_ty: syn::Type = match &args.state {
        Some(s) if !s.is_empty() => syn::parse_str(s)?,
        _ => syn::parse_quote!(#self_ty),
    };

    // Collect every verb-mapped method, stripping the marker attributes from
    // the emitted impl so they are not seen as unknown attributes downstream.
    let mut mappings: Vec<Mapping> = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let mut found: Option<(&'static str, LitStr)> = None;
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            let matched = VERBS.iter().find(|v| attr.path().is_ident(v));
            match matched {
                Some(verb) => {
                    if found.is_some() {
                        return Err(syn::Error::new_spanned(
                            &attr,
                            "a controller method may carry at most one HTTP-verb mapping",
                        ));
                    }
                    let lit = parse_mapping_path(&attr)?;
                    found = Some((verb, lit));
                }
                None => kept.push(attr),
            }
        }
        method.attrs = kept;
        if let Some((verb, lit)) = found {
            if method.sig.asyncness.is_none() {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "a #[get]/#[post]/... controller method must be `async fn`",
                ));
            }
            mappings.push(Mapping {
                method_ident: method.sig.ident.clone(),
                verb,
                path: join_path(&base_path, &lit.value()),
            });
        }
    }

    if mappings.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[rest_controller] found no #[get]/#[post]/#[put]/#[delete]/#[patch] methods",
        ));
    }

    // Build the route registrations. Each method is wired as an axum handler
    // by its associated-function path `<Self>::method`. axum's typed-handler
    // machinery type-checks the extractor signature; errors returned as
    // `WebResult` render as RFC 7807 through `firefly_web::WebError`.
    let routes = mappings.iter().map(|m| {
        let path = &m.path;
        let method_ident = &m.method_ident;
        let verb_ident = syn::Ident::new(m.verb, proc_macro2::Span::call_site());
        quote! {
            .route(
                #path,
                ::axum::routing::#verb_ident(<#self_ty>::#method_ident)
            )
        }
    });

    let routes_doc = format!(
        "Builds the axum router for this controller (base path `\"{base_path}\"`). \
         Generated by `#[rest_controller]`; mount it with \
         `.merge(routes(state))` or serve it directly. Handler errors render as \
         RFC 7807 problems via `firefly_web::WebError`."
    );

    // Route metadata: a const slice + an `inventory` submission so the openapi
    // crate (a separate phase-2 agent) can enumerate every controller method's
    // (verb, path, handler) without re-parsing source. The metadata mirrors the
    // routes the generated `routes()` mounts.
    let container = _facade.container();
    let controller_name = quote!(#self_ty).to_string();
    let route_consts = mappings.iter().map(|m| {
        let verb = m.verb.to_uppercase();
        let path = &m.path;
        let handler = m.method_ident.to_string();
        quote! {
            #container::RouteDescriptor {
                controller: #controller_name,
                method: #verb,
                path: #path,
                handler: #handler,
            }
        }
    });
    let route_meta_doc = format!(
        "Route metadata for `{controller_name}` — the (verb, path, handler) of \
         every mapped method. Consumed by the OpenAPI generator. Generated by \
         `#[rest_controller]`."
    );
    let route_inventory = mappings.iter().map(|m| {
        let verb = m.verb.to_uppercase();
        let path = &m.path;
        let handler = m.method_ident.to_string();
        quote! {
            #container::inventory::submit! {
                #container::RouteDescriptor {
                    controller: #controller_name,
                    method: #verb,
                    path: #path,
                    handler: #handler,
                }
            }
        }
    });

    Ok(quote! {
        #item

        impl #self_ty {
            #[doc = #routes_doc]
            pub fn routes(__state: #state_ty) -> ::axum::Router {
                ::axum::Router::new()
                    #(#routes)*
                    .with_state(__state)
            }

            #[doc = #route_meta_doc]
            pub const ROUTES: &'static [#container::RouteDescriptor] = &[
                #(#route_consts),*
            ];
        }

        #(#route_inventory)*
    })
}

/// Parses the optional path literal out of `#[get("/...")]` / `#[post]`.
fn parse_mapping_path(attr: &syn::Attribute) -> syn::Result<LitStr> {
    match &attr.meta {
        // `#[get]` — no path; defaults to the controller base path.
        syn::Meta::Path(_) => Ok(LitStr::new("", attr.span_or_default())),
        // `#[get("/foo")]` / `#[post()]`.
        syn::Meta::List(list) => {
            let parsed: LitStrArg = syn::parse2(list.tokens.clone())?;
            Ok(parsed.0)
        }
        syn::Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected `#[get(\"/path\")]` or a bare `#[get]`",
        )),
    }
}

/// Joins a controller base path with a method-relative path, normalising
/// slashes so `("/api", "/x")`, `("/api", "x")` and `("/api/", "/x")` all
/// yield `"/api/x"`, and an empty method path yields the base path.
fn join_path(base: &str, sub: &str) -> String {
    let base = base.trim_end_matches('/');
    let sub = sub.trim();
    if sub.is_empty() || sub == "/" {
        if base.is_empty() {
            return "/".to_string();
        }
        return base.to_string();
    }
    let sub = sub.trim_start_matches('/');
    if base.is_empty() {
        format!("/{sub}")
    } else {
        format!("{base}/{sub}")
    }
}

/// A tiny helper trait so `parse_mapping_path` can produce a span for a bare
/// `#[get]` (which has no tokens to span).
trait SpanOrDefault {
    fn span_or_default(&self) -> proc_macro2::Span;
}

impl SpanOrDefault for syn::Attribute {
    fn span_or_default(&self) -> proc_macro2::Span {
        use syn::spanned::Spanned;
        self.span()
    }
}
