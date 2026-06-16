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
use syn::parse::{Parse, ParseStream};
use syn::{
    FnArg, GenericArgument, ImplItem, ItemImpl, LitBool, LitStr, PathArguments, ReturnType,
    Signature, Token, Type,
};

use crate::common::facade_from_override;

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
    /// The OpenAPI tag grouping every operation on this controller (Spring's
    /// `@Tag(name = ...)`). Overrides the type-name-derived tag; a per-method
    /// `tags = [...]` still wins over it.
    tag: Option<String>,
}

/// One HTTP-verb mapping discovered on a method.
struct Mapping {
    method_ident: syn::Ident,
    /// `get`, `post`, `put`, `delete`, `patch`.
    verb: &'static str,
    /// The method-relative path (joined onto the controller base path).
    path: String,
    /// OpenAPI operation `summary`, or empty.
    summary: String,
    /// OpenAPI operation `description`, or empty.
    description: String,
    /// OpenAPI operation `tags` (per-method override of the controller tag).
    tags: Vec<String>,
    /// Whether the operation renders `deprecated: true`.
    deprecated: bool,
    /// Request-body schema name, or empty.
    request: String,
    /// Success-response schema name, or empty.
    response: String,
    /// Success status code (`0` = derive 201 for POST else 200).
    status: u16,
    /// `Query<T>` / `ValidQuery<T>` schema name to expand into query params.
    query_schema: String,
    /// Whether the handler takes a `PageRequest` (page/size/sort query params).
    pageable: bool,
    /// Explicitly-declared header/query parameters.
    params: Vec<ParamDecl>,
}

/// The parsed contents of a verb marker attribute — `#[get]`, `#[get("/x")]`,
/// or the rich `#[get("/x", summary = "...", description = "...",
/// tags = ["A"], deprecated)]` form that feeds the OpenAPI generator.
#[derive(Default)]
pub(crate) struct MappingAttr {
    pub(crate) path: Option<LitStr>,
    summary: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
    deprecated: bool,
    /// Request-body type name (`request = Foo` → `"Foo"`), the schema `$ref`ed
    /// in the OpenAPI operation; matches a `#[derive(Schema)]` type.
    request: Option<String>,
    /// Success-response type name (`response = Foo` → `"Foo"`).
    response: Option<String>,
    /// Success status code (`status = 202`).
    pub(crate) status: Option<u16>,
    /// Explicitly-declared `header("X-Foo", required, description = "…")` params.
    params: Vec<ParamDecl>,
}

/// One `header(...)` / `query(...)` declaration on a verb attribute.
#[derive(Default)]
struct ParamDecl {
    location: &'static str,
    name: String,
    required: bool,
    description: String,
}

impl Parse for MappingAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut out = MappingAttr::default();
        // Optional leading positional path literal: `#[get("/x", ...)]`.
        if input.peek(LitStr) {
            out.path = Some(input.parse()?);
            if input.is_empty() {
                return Ok(out);
            }
            input.parse::<Token![,]>()?;
        }
        // Remaining `name = value` arguments (and the bare `deprecated` flag).
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            match key.to_string().as_str() {
                "summary" => {
                    input.parse::<Token![=]>()?;
                    out.summary = Some(input.parse::<LitStr>()?.value());
                }
                "description" => {
                    input.parse::<Token![=]>()?;
                    out.description = Some(input.parse::<LitStr>()?.value());
                }
                "tags" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    syn::bracketed!(content in input);
                    let items = content.parse_terminated(<LitStr as Parse>::parse, Token![,])?;
                    out.tags = items.into_iter().map(|l| l.value()).collect();
                }
                "deprecated" => {
                    // Bare `deprecated` (true) or explicit `deprecated = false`.
                    if input.peek(Token![=]) {
                        input.parse::<Token![=]>()?;
                        out.deprecated = input.parse::<LitBool>()?.value;
                    } else {
                        out.deprecated = true;
                    }
                }
                "request" => {
                    input.parse::<Token![=]>()?;
                    out.request = Some(type_schema_name(&input.parse::<syn::Path>()?));
                }
                "response" => {
                    input.parse::<Token![=]>()?;
                    out.response = Some(type_schema_name(&input.parse::<syn::Path>()?));
                }
                "status" => {
                    input.parse::<Token![=]>()?;
                    out.status = Some(input.parse::<syn::LitInt>()?.base10_parse()?);
                }
                "header" | "query" => {
                    let location = if key == "header" { "header" } else { "query" };
                    out.params.push(parse_param_decl(location, input)?);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown route argument `{other}`; expected a path literal or one of: \
                             summary, description, tags, deprecated, request, response, status, \
                             header, query"
                        ),
                    ));
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(out)
    }
}

/// The OpenAPI component-schema name for a type path — its last segment
/// (`wallet::WalletView` → `"WalletView"`), matching the name `#[derive(Schema)]`
/// registers the type under.
fn type_schema_name(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

/// The last path-segment identifier of a type (`crate::WalletView` →
/// `"WalletView"`), or `None` for non-path types.
fn type_name_of(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// Parses a `header("X-Foo", required, description = "…")` / `query(...)`
/// declaration body (the parenthesised part after the keyword).
fn parse_param_decl(location: &'static str, input: ParseStream) -> syn::Result<ParamDecl> {
    let content;
    syn::parenthesized!(content in input);
    let name = content.parse::<LitStr>()?.value();
    let mut decl = ParamDecl {
        location,
        name,
        required: false,
        description: String::new(),
    };
    while !content.is_empty() {
        content.parse::<Token![,]>()?;
        if content.is_empty() {
            break;
        }
        let k: syn::Ident = content.parse()?;
        match k.to_string().as_str() {
            "required" => {
                if content.peek(Token![=]) {
                    content.parse::<Token![=]>()?;
                    decl.required = content.parse::<LitBool>()?.value;
                } else {
                    decl.required = true;
                }
            }
            "description" => {
                content.parse::<Token![=]>()?;
                decl.description = content.parse::<LitStr>()?.value();
            }
            other => {
                return Err(syn::Error::new(
                    k.span(),
                    format!("unknown {location} option `{other}`; use `required`, `description`"),
                ))
            }
        }
    }
    Ok(decl)
}

/// If `ty` is `Json<T>` (axum's body extractor/response), returns `T`'s schema
/// name; otherwise `None`. This is how request / response models are **inferred**
/// from a handler signature without the user naming them on the attribute.
fn json_inner_schema(ty: &Type) -> Option<String> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Json" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(inner) => type_name_of(inner),
        _ => None,
    }
}

/// The `Ok` type of a `Result<Ok, _>` / `WebResult<Ok>` return type, or the type
/// itself when it is not a result.
pub(crate) fn unwrap_result_ok(ty: &Type) -> &Type {
    let Type::Path(tp) = ty else {
        return ty;
    };
    let Some(seg) = tp.path.segments.last() else {
        return ty;
    };
    if seg.ident != "Result" && seg.ident != "WebResult" {
        return ty;
    }
    if let PathArguments::AngleBracketed(args) = &seg.arguments {
        if let Some(GenericArgument::Type(ok)) = args.args.first() {
            return ok;
        }
    }
    ty
}

/// Finds a `Json<T>` inside a (possibly tuple) success type and returns `T`'s
/// schema name — e.g. `(StatusCode, Json<WalletView>)` → `"WalletView"`.
fn find_json_schema(ty: &Type) -> Option<String> {
    if let Some(name) = json_inner_schema(ty) {
        return Some(name);
    }
    if let Type::Tuple(tuple) = ty {
        return tuple.elems.iter().find_map(json_inner_schema);
    }
    None
}

/// If `ty` is a body extractor — `Json<T>` **or** the validating `Valid<T>` —
/// returns `T`'s schema name. Both carry the request body; the response side only
/// uses `Json<T>`, so this is request-body-specific.
fn body_inner_schema(ty: &Type) -> Option<String> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Json" && seg.ident != "Valid" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(inner) => type_name_of(inner),
        _ => None,
    }
}

/// Infers the request-body schema name from a handler signature: the inner type
/// of the first `Json<T>` / `Valid<T>` body parameter, if any.
fn infer_request_schema(sig: &Signature) -> Option<String> {
    sig.inputs.iter().find_map(|arg| match arg {
        FnArg::Typed(pat) => body_inner_schema(&pat.ty),
        FnArg::Receiver(_) => None,
    })
}

/// If `ty` is `Query<T>` / `ValidQuery<T>`, returns `T`'s schema name — whose
/// fields the OpenAPI generator expands into `in: query` parameters.
fn query_inner_schema(ty: &Type) -> Option<String> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Query" && seg.ident != "ValidQuery" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(inner) => type_name_of(inner),
        _ => None,
    }
}

/// Infers the query-parameter schema name from a handler signature.
fn infer_query_schema(sig: &Signature) -> Option<String> {
    sig.inputs.iter().find_map(|arg| match arg {
        FnArg::Typed(pat) => query_inner_schema(&pat.ty),
        FnArg::Receiver(_) => None,
    })
}

/// Whether the handler takes a `PageRequest` extractor (Spring `Pageable`).
fn takes_pageable(sig: &Signature) -> bool {
    sig.inputs.iter().any(|arg| match arg {
        FnArg::Typed(pat) => matches!(&*pat.ty, Type::Path(tp)
            if tp.path.segments.last().is_some_and(|s| s.ident == "PageRequest")),
        FnArg::Receiver(_) => false,
    })
}

/// Infers the success-response schema name from a handler return type:
/// unwrap `WebResult` / `Result`, then find a `Json<T>` (directly or inside a
/// `(StatusCode, Json<T>)` tuple).
fn infer_response_schema(output: &ReturnType) -> Option<String> {
    let ReturnType::Type(_, ty) = output else {
        return None;
    };
    find_json_schema(unwrap_result_ok(ty))
}

pub(crate) const VERBS: &[&str] = &["get", "post", "put", "delete", "patch"];

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
        let mut found: Option<(&'static str, MappingAttr)> = None;
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
                    let parsed = parse_mapping_attr(&attr)?;
                    found = Some((verb, parsed));
                }
                None => kept.push(attr),
            }
        }
        method.attrs = kept;
        if let Some((verb, parsed)) = found {
            if method.sig.asyncness.is_none() {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "a #[get]/#[post]/... controller method must be `async fn`",
                ));
            }
            let sub_path = parsed.path.map(|l| l.value()).unwrap_or_default();
            // Request / response models are INFERRED from the handler signature
            // (the `Json<T>` parameter and the `Json<T>` in the return type); an
            // explicit `request = ` / `response = ` on the attribute overrides
            // the inference when a body type can't be read from the signature.
            let request = parsed
                .request
                .or_else(|| infer_request_schema(&method.sig))
                .unwrap_or_default();
            let response = parsed
                .response
                .or_else(|| infer_response_schema(&method.sig.output))
                .unwrap_or_default();
            let query_schema = infer_query_schema(&method.sig).unwrap_or_default();
            let pageable = takes_pageable(&method.sig);
            mappings.push(Mapping {
                method_ident: method.sig.ident.clone(),
                verb,
                path: join_path(&base_path, &sub_path),
                summary: parsed.summary.unwrap_or_default(),
                description: parsed.description.unwrap_or_default(),
                tags: parsed.tags,
                deprecated: parsed.deprecated,
                request,
                response,
                status: parsed.status.unwrap_or(0),
                query_schema,
                pageable,
                params: parsed.params,
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
    let web = _facade.web();
    let controller_name = quote!(#self_ty).to_string();
    let controller_tag = args.tag.clone().unwrap_or_default();
    // One `RouteDescriptor { .. }` literal per mapped method, carrying the
    // OpenAPI metadata the verb attributes declared. Tags resolve per method:
    // an explicit `tags = [..]` wins; otherwise the `#[rest_controller(tag)]`
    // override applies; otherwise the slice is empty and the OpenAPI generator
    // derives a tag from the controller type name.
    let descriptor_literals: Vec<TokenStream> = mappings
        .iter()
        .map(|m| {
            let verb = m.verb.to_uppercase();
            let path = &m.path;
            let handler = m.method_ident.to_string();
            let summary = &m.summary;
            let description = &m.description;
            let deprecated = m.deprecated;
            let request_schema = &m.request;
            let response_schema = &m.response;
            let status = m.status;
            let query_schema = &m.query_schema;
            let pageable = m.pageable;
            let param_literals = m.params.iter().map(|p| {
                let location = p.location;
                let name = &p.name;
                let required = p.required;
                let description = &p.description;
                quote! {
                    #container::ParamDescriptor {
                        location: #location,
                        name: #name,
                        required: #required,
                        schema_type: "string",
                        description: #description,
                    }
                }
            });
            let tags: Vec<String> = if !m.tags.is_empty() {
                m.tags.clone()
            } else if !controller_tag.is_empty() {
                vec![controller_tag.clone()]
            } else {
                Vec::new()
            };
            quote! {
                #container::RouteDescriptor {
                    controller: #controller_name,
                    method: #verb,
                    path: #path,
                    handler: #handler,
                    summary: #summary,
                    description: #description,
                    tags: &[ #(#tags),* ],
                    deprecated: #deprecated,
                    request_schema: #request_schema,
                    response_schema: #response_schema,
                    status: #status,
                    query_schema: #query_schema,
                    pageable: #pageable,
                    parameters: &[ #(#param_literals),* ],
                }
            }
        })
        .collect();
    let route_consts = descriptor_literals.iter();
    let route_meta_doc = format!(
        "Route metadata for `{controller_name}` — the (verb, path, handler) and \
         OpenAPI summary/description/tags of every mapped method. Consumed by the \
         OpenAPI generator. Generated by `#[rest_controller]`."
    );
    let route_inventory = descriptor_literals.iter().map(|literal| {
        quote! {
            #container::inventory::submit! { #literal }
        }
    });

    // Auto-mount thunk: resolve the controller's state bean from the DI
    // container and build its router. Submitted to `inventory` so
    // `firefly_web::mount_controllers` (and `FireflyApplication`) wire every
    // controller into the app with zero hand-mounting — the Rust analog of
    // Spring's `RequestMappingHandlerMapping`. The state type must be a
    // registered, `Clone` bean (axum already requires `Clone` for `routes`).
    let mount_panic = format!(
        "#[rest_controller] auto-mount of `{controller_name}`: its state type is not a registered \
         bean. Register it (e.g. `#[derive(Controller)]` + `container.scan()`, or \
         `container.register_instance(...)`) before calling `mount_controllers`. Cause: {{}}"
    );
    let controller_mount = quote! {
        #web::inventory::submit! {
            #web::ControllerMount {
                controller: #controller_name,
                mount: |__c: &#web::Container| -> ::axum::Router {
                    let __state = #web::Container::resolve::<#state_ty>(__c)
                        .unwrap_or_else(|__e| ::core::panic!(#mount_panic, __e));
                    <#self_ty>::routes((*__state).clone())
                },
            }
        }
    };

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

        #controller_mount
    })
}

/// Parses a verb marker attribute into its [`MappingAttr`]: a bare `#[get]`
/// (no path, defaults to the controller base path), `#[get("/foo")]`, or the
/// rich `#[get("/foo", summary = "...", tags = ["A"], deprecated)]` form.
fn parse_mapping_attr(attr: &syn::Attribute) -> syn::Result<MappingAttr> {
    match &attr.meta {
        // `#[get]` — no path, no metadata; defaults to the controller base path.
        syn::Meta::Path(_) => Ok(MappingAttr::default()),
        // `#[get("/foo", ...)]` / `#[post()]`.
        syn::Meta::List(list) => syn::parse2(list.tokens.clone()),
        syn::Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected `#[get(\"/path\", ...)]` or a bare `#[get]`",
        )),
    }
}

/// Joins a controller base path with a method-relative path, normalising
/// slashes so `("/api", "/x")`, `("/api", "x")` and `("/api/", "/x")` all
/// yield `"/api/x"`, and an empty method path yields the base path.
pub(crate) fn join_path(base: &str, sub: &str) -> String {
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
