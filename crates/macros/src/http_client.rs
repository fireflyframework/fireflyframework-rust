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

//! `#[http_client]` — the declarative HTTP-interface client (Spring's
//! `@HttpExchange`).
//!
//! Applied to a `trait` of methods, it emits the trait verbatim (with the verb
//! and per-arg marker attributes stripped) plus a generated concrete
//! `<Trait>Impl` struct that wraps a [`WebClient`](firefly_client::WebClient)
//! and implements the trait by translating each method's verb attribute, path
//! template, and bound arguments into a fluent `WebClient` request.
//!
//! See the macro entry point's rustdoc in `lib.rs` for the user-facing surface.
//!
//! # Error-fold fidelity caveat
//!
//! For the awaited `async fn -> Result<T, ClientError>` shape, every failure
//! (transport, encode, decode, invalid URL, and an upstream error status) folds
//! into [`ClientError::Problem`](firefly_client::ClientError::Problem) carrying a
//! `FireflyError`. The original HTTP status / problem code is preserved, so the
//! `ClientError` classifiers (`is_not_found()`, `is_server_error()`,
//! `is_retryable()`) still answer correctly. The structured
//! `ClientError::Transport` / `::Decode` / `::Encode` / `::InvalidUrl` variants
//! are **not** reconstructed in this form — they are preserved only on the
//! `Mono<T>` / `Flux<T>` (non-awaited) return forms, which surface the raw
//! `ClientError` from the underlying `WebClient` pipeline unchanged.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{FnArg, GenericArgument, ItemTrait, PathArguments, ReturnType, TraitItem, Type};

use crate::common::{facade_from_override, Facade};
use crate::web::{join_path, unwrap_result_ok, MappingAttr, VERBS};

/// Trait-level `#[http_client(...)]` options.
#[derive(Default)]
struct ClientArgs {
    krate: Option<String>,
    /// Base path joined onto every method route.
    path: Option<String>,
    /// DI bean name / qualifier.
    name: Option<String>,
    /// Trait-wide default `Accept` header.
    accept: Option<String>,
    /// Trait-wide default `Content-Type` header.
    content_type: Option<String>,
    /// Name of a `WebClient` bean to resolve (instead of building one).
    client: Option<String>,
    /// Opt-in DI bean registration.
    bean: bool,
}

/// Where a bound argument's value goes on the wire.
enum Binding {
    /// Substitutes the `:name` template hole (percent-encoded, via `Display`).
    Path { var: String, ident: syn::Ident },
    /// `.query(key, value)`; `Option<_>` omits `None`, `Vec`/`&[_]` repeats.
    Query {
        key: String,
        ident: syn::Ident,
        ty: QueryShape,
    },
    /// `.header(name, value)` (via `Display`); `Option<_>` omits `None`.
    Header {
        name: String,
        ident: syn::Ident,
        optional: bool,
    },
    /// `.body(&value)` (via `Serialize`).
    Body { ident: syn::Ident },
}

/// The query-scalar container shape, selecting how a query arg is emitted.
#[derive(Clone, Copy)]
enum QueryShape {
    /// A bare scalar — `.query(k, v.to_string())`.
    Scalar,
    /// `Option<T>` — conditional `.query` only when `Some`.
    Option,
    /// `Vec<T>` / `&[T]` — repeated `.query` per element.
    Repeated,
}

/// The detected return shape of a trait method.
enum ReturnShape {
    /// `async fn -> Result<T, ClientError>` (or a custom `E: From<ClientError>`).
    /// Boxed so the variant sizes stay close (it carries several `Type`s).
    ResultMono(Box<ResultPlan>),
    /// `fn -> Mono<T>` (non-async) — returns the `Mono` directly.
    Mono { item_ty: Type, is_exchange: bool },
    /// `fn -> Flux<T>` (non-async) — returns the `Flux` directly.
    Flux { item_ty: Type },
}

/// The folding plan for the `async fn -> Result<T, E>` return shape.
struct ResultPlan {
    /// The success type `T` (after unwrapping `Result`).
    ok_ty: Type,
    /// `Some` when the error type is not the bare `ClientError` and the fold
    /// must `.map_err(<E as From<ClientError>>::from)`.
    map_err: Option<Type>,
    /// The empty-body fold class for `T`.
    empty: EmptyClass,
    /// `WebClientResponse` escape hatch — use `.exchange()` not `body_to_mono`.
    is_exchange: bool,
}

/// How an empty (204 / null) body folds for a `Result<T, _>` success type.
#[derive(Clone, Copy)]
enum EmptyClass {
    /// `T = ()` — `Ok(())`.
    Unit,
    /// `T = Option<_>` — `Ok(None)`.
    OptionNone,
    /// `T = Vec<_>` — `Ok(vec![])`.
    VecEmpty,
    /// `T` is a concrete required value — synthesize a CLIENT_EMPTY_BODY problem.
    Required,
}

/// One processed method: its signature plus the computed plan.
struct Method {
    sig: syn::Signature,
    verb: VerbCall,
    /// The fully joined path template (base + method path), with `:name` holes.
    path: String,
    bindings: Vec<Binding>,
    ret: ReturnShape,
}

/// The verb a method maps to, as the `WebClient` call to make.
enum VerbCall {
    /// `.get()` / `.post()` / `.put()` / `.delete()` / `.patch()`.
    Named(&'static str),
    /// `request(method = "HEAD")` → `.method(Method::from_bytes(...))`.
    Method(String),
}

/// Expands `#[http_client]` on a trait.
pub(crate) fn http_client(args: TokenStream, item: ItemTrait) -> syn::Result<TokenStream> {
    let cfg = parse_args(args)?;
    let facade = facade_from_override(&cfg.krate)?;

    let trait_ident = item.ident.clone();
    let impl_ident = format_ident!("{}Impl", trait_ident);
    let base_path = cfg.path.clone().unwrap_or_default();
    let base_path = base_path.trim_end_matches('/').to_string();

    // Process every method against a *cleaned* copy of the trait — the emitted
    // trait must carry no verb / per-arg marker attributes.
    let mut clean = item.clone();
    let mut methods: Vec<Method> = Vec::new();
    let mut has_async = false;
    for trait_item in &mut clean.items {
        let TraitItem::Fn(method) = trait_item else {
            continue;
        };
        let processed = process_method(method, &base_path, &cfg)?;
        if processed.sig.asyncness.is_some() {
            has_async = true;
        }
        methods.push(processed);
    }

    if methods.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "#[http_client] found no methods carrying a #[get]/#[post]/#[put]/#[delete]/\
             #[patch]/#[request] verb attribute",
        ));
    }

    // When `bean` is requested the trait must be object-safe for the `dyn`
    // bind; surface the un-object-safe shapes up front rather than as a
    // downstream `dyn Trait` error. The `dyn Trait` autowire target must also be
    // `Send + Sync` (every `Arc<dyn _>` bean is), so add those supertraits to
    // the emitted trait when they are not already present.
    if cfg.bean {
        check_object_safe(&item)?;
        ensure_send_sync_supertraits(&mut clean);
    }

    let method_impls = methods
        .iter()
        .map(|m| emit_method(m, &facade))
        .collect::<syn::Result<Vec<_>>>()?;

    let rt = facade.rt();
    let client_rt = quote!(#rt::firefly_client);
    let aop = facade.aop();

    // `::new(base_url)` applies the trait-wide default headers.
    let mut header_chain = TokenStream::new();
    if let Some(accept) = cfg.accept.as_deref().filter(|s| !s.is_empty()) {
        header_chain.extend(quote! { .with_header("Accept", #accept) });
    }
    if let Some(ct) = cfg.content_type.as_deref().filter(|s| !s.is_empty()) {
        header_chain.extend(quote! { .with_header("Content-Type", #ct) });
    }

    let new_doc = format!(
        "Builds a `{impl_ident}` that issues every request through a freshly built \
         `WebClient` rooted at `base_url`. The trait's `accept` / `content_type` \
         defaults (if any) are applied as default headers. Generated by `#[http_client]`."
    );
    let with_client_doc = format!(
        "Builds a `{impl_ident}` over an already-configured `WebClient` (timeouts, \
         default headers, a shared connection pool) — the primary DI seam, the \
         analog of Spring's `HttpServiceProxyFactory`. Generated by `#[http_client]`."
    );

    // The `#[async_trait]` impl is emitted only when the trait has an `async fn`
    // method; async-trait passes non-async (`Mono`/`Flux`) methods through
    // unchanged, but emitting it on a fully-non-async trait would still rewrite
    // those signatures, so it is gated.
    let trait_impl = if has_async {
        quote! {
            #[#aop::async_trait]
            impl #trait_ident for #impl_ident {
                #(#method_impls)*
            }
        }
    } else {
        quote! {
            impl #trait_ident for #impl_ident {
                #(#method_impls)*
            }
        }
    };

    let di_tokens = if cfg.bean {
        emit_di(&trait_ident, &impl_ident, &cfg, &facade)?
    } else {
        TokenStream::new()
    };

    // When the trait carries `async fn` methods, both the trait declaration and
    // its impl must wear `#[async_trait]` so the desugared
    // `-> Pin<Box<dyn Future>>` signatures line up (and the trait stays
    // dyn-compatible for the `Arc<dyn Trait>` autowire). A fully-non-async
    // (`Mono`/`Flux`) trait is emitted untouched.
    let trait_decl = if has_async {
        quote! {
            #[#aop::async_trait]
            #clean
        }
    } else {
        quote! { #clean }
    };

    Ok(quote! {
        #trait_decl

        #[derive(::core::clone::Clone)]
        pub struct #impl_ident {
            __web: #client_rt::WebClient,
        }

        impl #impl_ident {
            #[doc = #new_doc]
            pub fn new(base_url: impl ::core::convert::AsRef<str>) -> Self {
                let __web = #client_rt::new_web_client(base_url)
                    #header_chain
                    .build();
                Self { __web }
            }

            #[doc = #with_client_doc]
            pub fn with_client(__web: #client_rt::WebClient) -> Self {
                Self { __web }
            }
        }

        #trait_impl

        #di_tokens
    })
}

/// Parses the trait-level `#[http_client(...)]` arguments.
fn parse_args(args: TokenStream) -> syn::Result<ClientArgs> {
    let mut out = ClientArgs::default();
    if args.is_empty() {
        return Ok(out);
    }
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("crate") {
            out.krate = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("path") {
            out.path = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("name") {
            out.name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("accept") {
            out.accept = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("content_type") {
            out.content_type = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("client") {
            out.client = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("bean") {
            // Bare flag (or `bean = true/false`).
            if meta.input.peek(syn::Token![=]) {
                out.bean = meta.value()?.parse::<syn::LitBool>()?.value();
            } else {
                out.bean = true;
            }
        } else {
            return Err(meta.error(
                "unknown #[http_client] argument; use path, name, accept, content_type, \
                 client, bean, or crate",
            ));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, args)?;
    Ok(out)
}

/// Strips the verb + per-arg markers from `method`, parses them, and computes
/// the binding plan and return shape.
fn process_method(
    method: &mut syn::TraitItemFn,
    base_path: &str,
    _cfg: &ClientArgs,
) -> syn::Result<Method> {
    // 1. Find and strip the single verb attribute.
    let (verb, mapping) = take_verb_attr(method)?;

    // 2. Validate the `&self` receiver.
    let mut inputs = method.sig.inputs.iter();
    match inputs.next() {
        Some(FnArg::Receiver(r)) if r.reference.is_some() && r.mutability.is_none() => {}
        _ => {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "an #[http_client] method must take `&self`",
            ));
        }
    }

    // 3. The joined path template + its `:name` holes.
    let sub_path = mapping.path.as_ref().map(|l| l.value()).unwrap_or_default();
    let path = join_path(base_path, &sub_path);
    let segments = parse_path_template(&path, &method.sig)?;
    let path_vars: Vec<String> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Var(v) => Some(v.clone()),
            Segment::Literal(_) => None,
        })
        .collect();

    // 4. Body-allowing verbs. Named `post`/`put`/`patch` carry a body; `get`
    //    (and the other named read verbs) do not. A custom `#[request(method)]`
    //    verb is body-eligible only when it is not one of the bodyless methods
    //    (GET/HEAD/OPTIONS/TRACE/CONNECT), so a struct arg on e.g. `HEAD` is not
    //    silently routed to the body.
    let body_allowed = match &verb {
        VerbCall::Named("post") | VerbCall::Named("put") | VerbCall::Named("patch") => true,
        VerbCall::Named(_) => false,
        VerbCall::Method(m) => !matches!(
            m.to_ascii_uppercase().as_str(),
            "GET" | "HEAD" | "OPTIONS" | "TRACE" | "CONNECT"
        ),
    };

    // 5. Parse + strip per-arg attrs and compute the binding plan.
    let bindings = bind_params(method, &path_vars, body_allowed)?;

    // 6. Return shape.
    let ret = return_shape(method)?;

    Ok(Method {
        sig: method.sig.clone(),
        verb,
        path,
        bindings,
        ret,
    })
}

/// Finds (and strips) the single verb attribute on a method, returning the verb
/// call + parsed [`MappingAttr`]. An unknown / duplicate / missing verb attr is
/// a precise compile error.
fn take_verb_attr(method: &mut syn::TraitItemFn) -> syn::Result<(VerbCall, MappingAttr)> {
    let mut found: Option<(VerbCall, MappingAttr)> = None;
    let mut kept = Vec::with_capacity(method.attrs.len());
    for attr in std::mem::take(&mut method.attrs) {
        let named_verb = VERBS.iter().find(|v| attr.path().is_ident(v));
        let is_request = attr.path().is_ident("request");
        if named_verb.is_none() && !is_request {
            kept.push(attr);
            continue;
        }
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                &attr,
                "an #[http_client] method may carry at most one HTTP-verb mapping",
            ));
        }
        let (verb, mapping) = if let Some(v) = named_verb {
            // A named verb reuses the server's `MappingAttr` grammar verbatim.
            (VerbCall::Named(v), parse_mapping_attr(&attr)?)
        } else {
            // `#[request(method = "HEAD", path = "/x", status = N)]` — the
            // generic form has its own small grammar (`method` is required and
            // is not part of the server's `MappingAttr`).
            parse_request_attr(&attr)?
        };
        found = Some((verb, mapping));
    }
    method.attrs = kept;
    found.ok_or_else(|| {
        syn::Error::new_spanned(
            &method.sig,
            "an #[http_client] method must carry a verb attribute: #[get]/#[post]/#[put]/\
             #[delete]/#[patch] or #[request(method = \"...\")]",
        )
    })
}

/// Parses a verb attribute into a [`MappingAttr`] (handling the bare-path
/// `#[get]` form), reusing the server's grammar.
fn parse_mapping_attr(attr: &syn::Attribute) -> syn::Result<MappingAttr> {
    match &attr.meta {
        syn::Meta::Path(_) => Ok(MappingAttr::default()),
        syn::Meta::List(_) => attr.parse_args::<MappingAttr>(),
        syn::Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected `#[get(\"/path\", ...)]` or a bare `#[get]`",
        )),
    }
}

/// Parses a `#[request(method = "...", path = "...", status = N)]` attribute
/// into the verb call + a [`MappingAttr`] carrying its `path` / `status`. The
/// generic verb's grammar is small and `method`-required, distinct from the
/// named-verb `MappingAttr` grammar (which has no `method` key).
fn parse_request_attr(attr: &syn::Attribute) -> syn::Result<(VerbCall, MappingAttr)> {
    let mut method_name: Option<String> = None;
    let mut mapping = MappingAttr::default();
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("method") {
            method_name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("path") {
            mapping.path = Some(meta.value()?.parse::<syn::LitStr>()?);
        } else if meta.path.is_ident("status") {
            mapping.status = Some(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?);
        } else if meta.input.peek(syn::Token![=]) {
            // Tolerate (and ignore) the other server-only OpenAPI keys so a
            // signature copies cleanly between server and client.
            let _: syn::Lit = meta.value()?.parse()?;
        } else {
            return Err(meta.error("unknown #[request(...)] argument; use method, path, or status"));
        }
        Ok(())
    })?;
    let method_name = method_name.ok_or_else(|| {
        syn::Error::new_spanned(
            attr,
            "#[request(...)] requires `method = \"<HTTP-METHOD>\"` (e.g. \
             #[request(method = \"HEAD\", path = \"/x\")])",
        )
    })?;
    // Validate the method string at expansion time so a malformed token is a
    // clean compile error rather than a runtime `from_bytes(..).expect(..)` panic.
    validate_http_method(attr, &method_name)?;
    Ok((VerbCall::Method(method_name), mapping))
}

/// Validates that a `#[request(method = "...")]` string is a non-empty ASCII
/// HTTP method token. The grammar is restricted to uppercase ASCII letters
/// (`A`–`Z`) — the safe superset covering every standard verb (and any sensible
/// custom one) — so a malformed method becomes a clean compile error instead of
/// a runtime panic from `http::Method::from_bytes(..).expect(..)`.
fn validate_http_method(attr: &syn::Attribute, method: &str) -> syn::Result<()> {
    let ok = !method.is_empty() && method.bytes().all(|b| b.is_ascii_uppercase());
    if !ok {
        return Err(syn::Error::new_spanned(
            attr,
            format!(
                "#[request(method = \"{method}\")] is not a valid HTTP method; expected a \
                 non-empty uppercase ASCII token (e.g. \"GET\", \"HEAD\", \"PATCH\")"
            ),
        ));
    }
    Ok(())
}

/// One piece of a parsed path template.
enum Segment {
    Literal(String),
    Var(String),
}

/// Splits a path template into literal + `:name` variable segments. Rejects the
/// Spring `{id}` spelling with a pointer at the axum `:id` convention.
fn parse_path_template(path: &str, sig: &syn::Signature) -> syn::Result<Vec<Segment>> {
    if path.contains('{') || path.contains('}') {
        return Err(syn::Error::new_spanned(
            sig,
            format!(
                "#[http_client] path {path:?} uses Spring `{{name}}` syntax; firefly uses the \
                 axum-style `:name` path-variable spelling (e.g. `/orders/:id`)"
            ),
        ));
    }
    // Split on '/', keeping the separators so the URI is reconstructed exactly.
    let mut out: Vec<Segment> = Vec::new();
    let mut literal = String::new();
    for (i, raw) in path.split('/').enumerate() {
        if i > 0 {
            literal.push('/');
        }
        if let Some(name) = raw.strip_prefix(':') {
            if name.is_empty() {
                return Err(syn::Error::new_spanned(
                    sig,
                    format!("#[http_client] path {path:?} has an empty `:` path variable"),
                ));
            }
            // Flush the accumulated literal (including the trailing '/'), then
            // record the variable.
            out.push(Segment::Literal(std::mem::take(&mut literal)));
            out.push(Segment::Var(name.to_string()));
        } else {
            literal.push_str(raw);
        }
    }
    if !literal.is_empty() {
        out.push(Segment::Literal(literal));
    }
    Ok(out)
}

/// Computes the per-argument binding plan, parsing + stripping each arg's
/// marker attribute. Applies the first-match-wins precedence: explicit attr >
/// path-name-match > body-default > query-default, then verifies every `:name`
/// is bound exactly once.
fn bind_params(
    method: &mut syn::TraitItemFn,
    path_vars: &[String],
    body_allowed: bool,
) -> syn::Result<Vec<Binding>> {
    // First pass: classify each typed arg, stripping its marker attribute.
    enum Slot {
        ExplicitPath {
            var: String,
            ident: syn::Ident,
            ty: Type,
        },
        ExplicitQuery {
            key: String,
            ident: syn::Ident,
            ty: Type,
        },
        ExplicitHeader {
            name: String,
            ident: syn::Ident,
            ty: Type,
        },
        ExplicitBody {
            ident: syn::Ident,
        },
        Unannotated {
            ident: syn::Ident,
            ty: Type,
        },
    }

    let mut slots: Vec<Slot> = Vec::new();
    let mut body_count = 0usize;
    for arg in method.sig.inputs.iter_mut() {
        let FnArg::Typed(pat) = arg else {
            // The receiver (`&self`), already validated.
            continue;
        };
        let ident = pat_ident(&pat.pat)?;
        let arg_attr = take_arg_attr(&mut pat.attrs, &ident)?;
        let ty = (*pat.ty).clone();
        match arg_attr {
            Some(ArgAttr::Path { name }) => {
                let var = name.unwrap_or_else(|| ident.to_string());
                slots.push(Slot::ExplicitPath { var, ident, ty });
            }
            Some(ArgAttr::Query { name }) => {
                let key = name.unwrap_or_else(|| ident.to_string());
                slots.push(Slot::ExplicitQuery { key, ident, ty });
            }
            Some(ArgAttr::Header { name }) => {
                slots.push(Slot::ExplicitHeader { name, ident, ty });
            }
            Some(ArgAttr::Body) => {
                body_count += 1;
                slots.push(Slot::ExplicitBody { ident });
            }
            None => slots.push(Slot::Unannotated { ident, ty }),
        }
    }
    if body_count > 1 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "an #[http_client] method may carry at most one #[body] argument",
        ));
    }

    // Second pass: resolve the unannotated args by precedence.
    let mut bindings: Vec<Binding> = Vec::new();
    let mut bound_vars: Vec<String> = Vec::new();
    // Has any arg (explicit or matched) claimed the body slot yet?
    let mut has_body = body_count > 0;
    // Collect the body-eligible unannotated candidates to detect ambiguity.
    let mut body_candidates: Vec<syn::Ident> = Vec::new();

    for slot in &slots {
        match slot {
            Slot::ExplicitPath { var, ident, ty } => {
                if !path_vars.iter().any(|v| v == var) {
                    return Err(syn::Error::new_spanned(
                        &method.sig,
                        format!(
                            "#[path(\"{var}\")] on `{ident}` does not match any `:{var}` segment \
                             in the method path"
                        ),
                    ));
                }
                check_path_var_ty(var, ident, ty)?;
                bound_vars.push(var.clone());
                bindings.push(Binding::Path {
                    var: var.clone(),
                    ident: ident.clone(),
                });
            }
            Slot::ExplicitQuery { key, ident, ty } => {
                bindings.push(Binding::Query {
                    key: key.clone(),
                    ident: ident.clone(),
                    ty: query_shape(ty),
                });
            }
            Slot::ExplicitHeader { name, ident, ty } => {
                bindings.push(Binding::Header {
                    name: name.clone(),
                    ident: ident.clone(),
                    optional: is_option(ty),
                });
            }
            Slot::ExplicitBody { ident } => {
                bindings.push(Binding::Body {
                    ident: ident.clone(),
                });
            }
            Slot::Unannotated { ident, ty } => {
                // Precedence: path-name-match first.
                let ident_str = ident.to_string();
                if path_vars.iter().any(|v| v == &ident_str) {
                    check_path_var_ty(&ident_str, ident, ty)?;
                    bound_vars.push(ident_str.clone());
                    bindings.push(Binding::Path {
                        var: ident_str,
                        ident: ident.clone(),
                    });
                    continue;
                }
                // Body-default: a lone non-query-scalar arg on a body verb.
                if body_allowed && !is_query_scalar(ty) {
                    body_candidates.push(ident.clone());
                    // Defer the actual body binding until we know it is unique;
                    // record a placeholder via the candidate list.
                    continue;
                }
                // Query-default.
                bindings.push(Binding::Query {
                    key: ident_str,
                    ident: ident.clone(),
                    ty: query_shape(ty),
                });
            }
        }
    }

    // Resolve the body candidates.
    match (has_body, body_candidates.len()) {
        (false, 0) => {}
        (false, 1) => {
            bindings.push(Binding::Body {
                ident: body_candidates.remove(0),
            });
            has_body = true;
        }
        (false, _) => {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "ambiguous body; more than one argument could be the request body — \
                 annotate exactly one with #[body]",
            ));
        }
        (true, n) if n >= 1 => {
            // An explicit #[body] is present, but other non-scalar unannotated
            // args remain with no binding — also ambiguous.
            return Err(syn::Error::new_spanned(
                &method.sig,
                "ambiguous body; an #[body] argument is already declared but another \
                 non-scalar argument is unannotated — annotate it with #[query]/#[header] \
                 or remove it",
            ));
        }
        (true, _) => {}
    }
    let _ = has_body;

    // Every `:name` must be bound exactly once.
    for var in path_vars {
        let count = bound_vars.iter().filter(|v| *v == var).count();
        if count == 0 {
            return Err(syn::Error::new_spanned(
                &method.sig,
                format!(
                    "the path variable `:{var}` is not bound to any argument; add an argument \
                     named `{var}` or annotate one with #[path(\"{var}\")]"
                ),
            ));
        }
        if count > 1 {
            return Err(syn::Error::new_spanned(
                &method.sig,
                format!("the path variable `:{var}` is bound by more than one argument"),
            ));
        }
    }

    Ok(bindings)
}

/// A parsed per-argument marker attribute.
enum ArgAttr {
    Path { name: Option<String> },
    Query { name: Option<String> },
    Header { name: String },
    Body,
}

/// Finds and strips the single per-arg marker (`#[path]`/`#[query]`/`#[header]`/
/// `#[body]`) on an argument, rejecting conflicts.
fn take_arg_attr(
    attrs: &mut Vec<syn::Attribute>,
    ident: &syn::Ident,
) -> syn::Result<Option<ArgAttr>> {
    let mut found: Option<ArgAttr> = None;
    let mut kept = Vec::with_capacity(attrs.len());
    for attr in std::mem::take(attrs) {
        let which = ["path", "query", "header", "body"]
            .iter()
            .find(|k| attr.path().is_ident(k))
            .copied();
        let Some(which) = which else {
            kept.push(attr);
            continue;
        };
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                &attr,
                format!(
                    "argument `{ident}` carries conflicting binding attributes; use exactly one \
                     of #[path], #[query], #[header], #[body]"
                ),
            ));
        }
        let parsed = match which {
            "path" => ArgAttr::Path {
                name: optional_lit(&attr)?,
            },
            "query" => ArgAttr::Query {
                name: optional_lit(&attr)?,
            },
            "header" => {
                let name = required_lit(&attr).map_err(|_| {
                    syn::Error::new_spanned(
                        &attr,
                        format!(
                            "#[header(\"X-Name\")] on `{ident}` requires the header name as a \
                             string literal (header names are not identifiers)"
                        ),
                    )
                })?;
                ArgAttr::Header { name }
            }
            "body" => {
                if !matches!(attr.meta, syn::Meta::Path(_)) {
                    return Err(syn::Error::new_spanned(&attr, "#[body] takes no arguments"));
                }
                ArgAttr::Body
            }
            _ => unreachable!(),
        };
        found = Some(parsed);
    }
    *attrs = kept;
    Ok(found)
}

/// Reads an optional positional string literal from `#[path]` / `#[path("id")]`.
fn optional_lit(attr: &syn::Attribute) -> syn::Result<Option<String>> {
    match &attr.meta {
        syn::Meta::Path(_) => Ok(None),
        syn::Meta::List(_) => Ok(Some(attr.parse_args::<syn::LitStr>()?.value())),
        syn::Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected `#[path]` or `#[path(\"name\")]`",
        )),
    }
}

/// Reads a required positional string literal from `#[header("X")]`.
fn required_lit(attr: &syn::Attribute) -> syn::Result<String> {
    Ok(attr.parse_args::<syn::LitStr>()?.value())
}

/// The `Ident` of a simple argument pattern, or an error for a destructured /
/// non-ident pattern.
fn pat_ident(pat: &syn::Pat) -> syn::Result<syn::Ident> {
    match pat {
        syn::Pat::Ident(p) => Ok(p.ident.clone()),
        _ => Err(syn::Error::new_spanned(
            pat,
            "#[http_client] method arguments must be simple `name: Type` bindings",
        )),
    }
}

/// The macro-introduced locals of a generated method body, minted at
/// [`Span::mixed_site`] so user method params (or trait-wide names) can never
/// capture or shadow them. (`__web` is a struct field reached via `self`, so it
/// is intentionally *not* here — it must stay `call_site` to resolve.)
struct Hygiene {
    /// The built request URI (`let (mut) __uri = ...`).
    uri: syn::Ident,
    /// The mutable `RequestSpec` accumulator.
    spec: syn::Ident,
    /// The per-iteration / per-arm scratch value.
    v: syn::Ident,
    /// The folded `Result<T, ClientError>` before any `map_err`.
    out: syn::Ident,
    /// The bound `FireflyError` in the error arm.
    fe: syn::Ident,
}

impl Hygiene {
    fn new() -> Self {
        let id = |n: &str| syn::Ident::new(n, Span::mixed_site());
        Self {
            uri: id("__uri"),
            spec: id("__spec"),
            v: id("__v"),
            out: id("__out"),
            fe: id("__fe"),
        }
    }
}

/// Emits the body of one method in the generated trait impl.
fn emit_method(m: &Method, facade: &Facade) -> syn::Result<TokenStream> {
    let rt = facade.rt();
    let client_rt = quote!(#rt::firefly_client);
    let sig = &m.sig;
    let hy = Hygiene::new();
    let Hygiene { uri, spec, .. } = &hy;

    // 1. Build the `__uri` expression from the path template.
    let uri_build = emit_uri(m, &client_rt, &hy);

    // 2. The verb call producing the initial `RequestSpec`.
    let verb_call = match &m.verb {
        VerbCall::Named(v) => {
            let vident = syn::Ident::new(v, Span::call_site());
            quote!(self.__web.#vident())
        }
        VerbCall::Method(name) => {
            let bytes = syn::LitByteStr::new(name.as_bytes(), Span::call_site());
            quote!(self.__web.method(
                #client_rt::http::Method::from_bytes(#bytes)
                    .expect("#[http_client] #[request(method)] is a valid HTTP method")
            ))
        }
    };

    // 3. Per-arg request mutations appended after `.uri(__uri)`.
    let mutations = m.bindings.iter().map(|b| emit_binding(b, &hy));

    // The `Flux` accept default is set before the user-overridable headers so an
    // explicit per-trait `accept` still wins (it is a default header on the
    // WebClient, which `.header(..)` only overrides when re-set here).
    let flux_accept = if matches!(m.ret, ReturnShape::Flux { .. }) {
        quote! { #spec = #spec.header("Accept", #client_rt::NDJSON_CONTENT_TYPE); }
    } else {
        TokenStream::new()
    };

    let terminal = emit_terminal(m, facade, &hy)?;

    Ok(quote! {
        #sig {
            #uri_build
            let mut #spec = #verb_call.uri(#uri);
            #flux_accept
            #(#mutations)*
            #terminal
        }
    })
}

/// Emits the `let __uri = ...;` statement reconstructing the path with each
/// `:name` hole replaced by `encode_path_segment(arg.to_string())`.
fn emit_uri(m: &Method, client_rt: &TokenStream, hy: &Hygiene) -> TokenStream {
    let uri = &hy.uri;
    // The template was already validated in `process_method`; an error here is
    // unreachable, but fall back to the raw path literal rather than panicking.
    let segments = match parse_path_template(&m.path, &m.sig) {
        Ok(s) => s,
        Err(_) => {
            let path = &m.path;
            return quote! { let #uri = #path; };
        }
    };
    // Map each path var name to its bound arg ident.
    let mut pushes = TokenStream::new();
    let mut has_var = false;
    for seg in &segments {
        match seg {
            Segment::Literal(lit) => {
                if !lit.is_empty() {
                    pushes.extend(quote! { #uri.push_str(#lit); });
                }
            }
            Segment::Var(name) => {
                has_var = true;
                let ident = m.bindings.iter().find_map(|b| match b {
                    Binding::Path { var, ident } if var == name => Some(ident),
                    _ => None,
                });
                let ident = ident.expect("path var bound (validated in bind_params)");
                pushes.extend(quote! {
                    #uri.push_str(&#client_rt::encode_path_segment(
                        &::std::string::ToString::to_string(&#ident)
                    ));
                });
            }
        }
    }
    if has_var {
        quote! {
            let mut #uri = ::std::string::String::new();
            #pushes
        }
    } else {
        // No variables — a single static literal.
        let path = &m.path;
        quote! { let #uri = #path; }
    }
}

/// Emits the request mutation for one binding (appended after `.uri(..)`).
fn emit_binding(b: &Binding, hy: &Hygiene) -> TokenStream {
    let Hygiene { spec, v, .. } = hy;
    match b {
        Binding::Path { .. } => TokenStream::new(), // handled in the URI build
        Binding::Query { key, ident, ty } => match ty {
            QueryShape::Scalar => quote! {
                #spec = #spec.query(#key, ::std::string::ToString::to_string(&#ident));
            },
            QueryShape::Option => quote! {
                if let ::core::option::Option::Some(#v) = &#ident {
                    #spec = #spec.query(#key, ::std::string::ToString::to_string(#v));
                }
            },
            QueryShape::Repeated => quote! {
                for #v in (&#ident).into_iter() {
                    #spec = #spec.query(#key, ::std::string::ToString::to_string(#v));
                }
            },
        },
        Binding::Header {
            name,
            ident,
            optional,
        } => {
            if *optional {
                quote! {
                    if let ::core::option::Option::Some(#v) = &#ident {
                        #spec = #spec.header(#name, ::std::string::ToString::to_string(#v));
                    }
                }
            } else {
                quote! {
                    #spec = #spec.header(#name, ::std::string::ToString::to_string(&#ident));
                }
            }
        }
        Binding::Body { ident } => quote! {
            #spec = #spec.body(&#ident);
        },
    }
}

/// Emits the terminal operator + (for the `Result` shape) the fold.
fn emit_terminal(m: &Method, facade: &Facade, hy: &Hygiene) -> syn::Result<TokenStream> {
    let rt = facade.rt();
    let client_rt = &quote!(#rt::firefly_client);
    let kernel = quote!(#rt::firefly_kernel);
    let Hygiene {
        spec, v, out, fe, ..
    } = hy;
    match &m.ret {
        ReturnShape::Mono {
            item_ty,
            is_exchange,
        } => {
            if *is_exchange {
                Ok(quote! { #spec.retrieve().exchange() })
            } else {
                Ok(quote! { #spec.retrieve().body_to_mono::<#item_ty>() })
            }
        }
        ReturnShape::Flux { item_ty } => Ok(quote! { #spec.retrieve().body_to_flux::<#item_ty>() }),
        ReturnShape::ResultMono(plan) => {
            let ResultPlan {
                ok_ty,
                map_err,
                empty,
                is_exchange,
            } = &**plan;
            let empty_problem = emit_empty_body_problem(&kernel);
            let fold = if *is_exchange {
                // `Result<WebClientResponse, _>` — the raw exchange escape hatch.
                quote! {
                    match #spec.retrieve().exchange().await {
                        ::core::result::Result::Ok(::core::option::Option::Some(#v)) =>
                            ::core::result::Result::Ok(#v),
                        ::core::result::Result::Ok(::core::option::Option::None) =>
                            ::core::result::Result::Err(
                                #client_rt::ClientError::Problem(#empty_problem)
                            ),
                        ::core::result::Result::Err(#fe) =>
                            ::core::result::Result::Err(
                                #client_rt::ClientError::Problem(#fe)
                            ),
                    }
                }
            } else {
                let none_arm = emit_empty_arm(*empty, client_rt, &empty_problem);
                quote! {
                    match #spec.retrieve().body_to_mono::<#ok_ty>().await {
                        ::core::result::Result::Ok(::core::option::Option::Some(#v)) =>
                            ::core::result::Result::Ok(#v),
                        ::core::result::Result::Ok(::core::option::Option::None) =>
                            #none_arm,
                        ::core::result::Result::Err(#fe) =>
                            ::core::result::Result::Err(
                                #client_rt::ClientError::Problem(#fe)
                            ),
                    }
                }
            };
            // Route through the facade `ClientError` so `Problem(..)` and the
            // synthesized empty-body problem name the same type the user wrote.
            let body = quote! {
                let #out: ::core::result::Result<#ok_ty, #client_rt::ClientError> = { #fold };
            };
            match map_err {
                None => Ok(quote! {
                    #body
                    #out
                }),
                Some(err_ty) => Ok(quote! {
                    #body
                    #out.map_err(<#err_ty as ::core::convert::From<#client_rt::ClientError>>::from)
                }),
            }
        }
    }
}

/// The synthesized `CLIENT_EMPTY_BODY` 502 problem for a required-`T` empty
/// body — a typed error instead of a panic (defensive, ~unreachable).
fn emit_empty_body_problem(kernel: &TokenStream) -> TokenStream {
    quote! {
        #kernel::FireflyError::new(
            "CLIENT_EMPTY_BODY",
            "Bad Gateway",
            502u16,
            "the upstream returned an empty body for a required response value",
        )
    }
}

/// The `Ok(None)` fold arm for an empty body, by success-type class.
fn emit_empty_arm(
    empty: EmptyClass,
    client_rt: &TokenStream,
    empty_problem: &TokenStream,
) -> TokenStream {
    match empty {
        EmptyClass::Unit => quote!(::core::result::Result::Ok(())),
        EmptyClass::OptionNone => {
            quote!(::core::result::Result::Ok(::core::option::Option::None))
        }
        EmptyClass::VecEmpty => quote!(::core::result::Result::Ok(::std::vec::Vec::new())),
        EmptyClass::Required => quote! {
            ::core::result::Result::Err(#client_rt::ClientError::Problem(#empty_problem))
        },
    }
}

/// Detects the structural return shape and validates `asyncness` against it.
fn return_shape(method: &syn::TraitItemFn) -> syn::Result<ReturnShape> {
    let is_async = method.sig.asyncness.is_some();
    let ReturnType::Type(_, ty) = &method.sig.output else {
        return Err(non_supported_return_err(&method.sig));
    };

    // `Mono<T>` / `Flux<T>` (non-async, reactive-first).
    if let Some(inner) = generic_arg(ty, "Mono") {
        if is_async {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a Mono-returning method must not be `async fn`; the Mono is already deferred — \
                 drop `async` (or return `Result<_, ClientError>` for the awaited form)",
            ));
        }
        let is_exchange = last_ident_is(&inner, "WebClientResponse");
        return Ok(ReturnShape::Mono {
            item_ty: inner,
            is_exchange,
        });
    }
    if let Some(inner) = generic_arg(ty, "Flux") {
        if is_async {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a Flux-returning method must not be `async fn`; the Flux is already deferred — \
                 drop `async`",
            ));
        }
        return Ok(ReturnShape::Flux { item_ty: inner });
    }

    // `Result<T, E>` (the ergonomic awaited form).
    if last_ident_is(ty, "Result") {
        if !is_async {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a Result-returning #[http_client] method must be `async fn` (it is awaited); \
                 for the deferred form return `Mono<T>` / `Flux<T>` from a non-async fn",
            ));
        }
        let ok_ty = unwrap_result_ok(ty).clone();
        let err_ty = result_err(ty);
        let is_exchange = last_ident_is(&ok_ty, "WebClientResponse");
        let empty = classify_empty(&ok_ty);
        let map_err = match &err_ty {
            Some(e) if !last_ident_is(e, "ClientError") => Some(e.clone()),
            _ => None,
        };
        return Ok(ReturnShape::ResultMono(Box::new(ResultPlan {
            ok_ty,
            map_err,
            empty,
            is_exchange,
        })));
    }

    Err(non_supported_return_err(&method.sig))
}

/// The standard "unsupported return type" diagnostic.
fn non_supported_return_err(sig: &syn::Signature) -> syn::Error {
    syn::Error::new_spanned(
        sig,
        "unsupported #[http_client] return type; use `async fn -> Result<T, ClientError>` \
         (or a custom `E: From<ClientError>`), or a non-async `fn -> Mono<T>` / `Flux<T>`",
    )
}

/// The error type `E` of a `Result<T, E>`, or `None`.
fn result_err(ty: &Type) -> Option<Type> {
    let Type::Path(tp) = ty else { return None };
    let seg = tp.path.segments.last()?;
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let mut it = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    it.next(); // skip Ok type
    it.next().cloned()
}

/// Classifies how an empty body folds for a `Result` success type `T`.
fn classify_empty(ok_ty: &Type) -> EmptyClass {
    if is_unit(ok_ty) {
        EmptyClass::Unit
    } else if last_ident_is(ok_ty, "Option") {
        EmptyClass::OptionNone
    } else if last_ident_is(ok_ty, "Vec") {
        EmptyClass::VecEmpty
    } else {
        EmptyClass::Required
    }
}

/// The single angle-bracketed generic argument of a `Wrapper<T>` whose last
/// segment ident is `wrapper`, or `None`.
fn generic_arg(ty: &Type, wrapper: &str) -> Option<Type> {
    let Type::Path(tp) = ty else { return None };
    let seg = tp.path.segments.last()?;
    if seg.ident != wrapper {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|a| match a {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    })
}

/// Whether `ty`'s last path segment ident equals `name`.
fn last_ident_is(ty: &Type, name: &str) -> bool {
    matches!(ty, Type::Path(tp)
        if tp.path.segments.last().is_some_and(|s| s.ident == name))
}

/// Whether `ty` is the unit type `()`.
fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

/// Whether `ty` is `Option<_>`.
fn is_option(ty: &Type) -> bool {
    last_ident_is(ty, "Option")
}

/// The query container shape for an argument type.
fn query_shape(ty: &Type) -> QueryShape {
    if is_option(ty) {
        QueryShape::Option
    } else if last_ident_is(ty, "Vec") || is_slice_ref(ty) {
        QueryShape::Repeated
    } else {
        QueryShape::Scalar
    }
}

/// Whether `ty` is `&[T]` (a slice reference).
fn is_slice_ref(ty: &Type) -> bool {
    match ty {
        Type::Reference(r) => matches!(&*r.elem, Type::Slice(_)),
        _ => false,
    }
}

/// Whether `ty` is a `[T; N]` array.
fn is_array(ty: &Type) -> bool {
    matches!(ty, Type::Array(_))
        || matches!(ty, Type::Reference(r) if matches!(&*r.elem, Type::Array(_)))
}

/// Validates that an argument bound as a `:name` path variable is a `Display`
/// scalar, not a multi-value / optional container. An `Option<_>` / `Vec<_>` /
/// slice / array bound as a path var compiles but renders garbage URIs (e.g.
/// `…/Some(x)`, `…/None`, `…/[1, 2]`), so it is rejected at expansion time.
fn check_path_var_ty(var: &str, ident: &syn::Ident, ty: &Type) -> syn::Result<()> {
    let bad = is_option(ty) || last_ident_is(ty, "Vec") || is_slice_ref(ty) || is_array(ty);
    if bad {
        let rendered = quote!(#ty).to_string().replace(' ', "");
        return Err(syn::Error::new_spanned(
            ident,
            format!(
                "#[http_client] path variable `:{var}` must be a Display scalar, not \
                 Option/Vec/slice — got `{rendered}`"
            ),
        ));
    }
    Ok(())
}

/// Whether `ty` is on the query-scalar allowlist: a primitive integer / float /
/// bool, `String` / `&str`, `Uuid`, or an `Option` / `Vec` / `&[_]` of those.
fn is_query_scalar(ty: &Type) -> bool {
    match ty {
        Type::Reference(r) => match &*r.elem {
            // `&str`
            Type::Path(_) if last_ident_is(&r.elem, "str") => true,
            // `&[T]` of a scalar.
            Type::Slice(s) => is_query_scalar(&s.elem),
            other => is_query_scalar(other),
        },
        Type::Path(tp) => {
            let Some(seg) = tp.path.segments.last() else {
                return false;
            };
            let name = seg.ident.to_string();
            // `Option<T>` / `Vec<T>` of a scalar.
            if name == "Option" || name == "Vec" {
                if let Some(inner) = generic_arg(ty, &name) {
                    return is_query_scalar(&inner);
                }
                return false;
            }
            matches!(
                name.as_str(),
                "u8" | "u16"
                    | "u32"
                    | "u64"
                    | "u128"
                    | "usize"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "i128"
                    | "isize"
                    | "f32"
                    | "f64"
                    | "bool"
                    | "char"
                    | "str"
                    | "String"
                    | "Uuid"
            )
        }
        _ => false,
    }
}

/// Validates that the trait is object-safe enough for the `dyn Trait` bind that
/// `bean` registration requires: every method must take `&self` and declare no
/// generic type/const parameters.
fn check_object_safe(item: &ItemTrait) -> syn::Result<()> {
    for trait_item in &item.items {
        // An associated type or const makes the trait not `dyn`-compatible; the
        // `dyn Trait` bind `bean` registration needs would otherwise fail with a
        // cryptic downstream error, so reject it up front.
        match trait_item {
            TraitItem::Type(t) => {
                return Err(syn::Error::new_spanned(
                    t,
                    format!(
                        "#[http_client(bean)] requires an object-safe trait, but associated type \
                         `{}` makes it not `dyn`-compatible; remove it or drop `bean`",
                        t.ident
                    ),
                ));
            }
            TraitItem::Const(c) => {
                return Err(syn::Error::new_spanned(
                    c,
                    format!(
                        "#[http_client(bean)] requires an object-safe trait, but associated const \
                         `{}` makes it not `dyn`-compatible; remove it or drop `bean`",
                        c.ident
                    ),
                ));
            }
            _ => {}
        }
        let TraitItem::Fn(method) = trait_item else {
            continue;
        };
        let has_generics = method
            .sig
            .generics
            .params
            .iter()
            .any(|p| !matches!(p, syn::GenericParam::Lifetime(_)));
        if has_generics {
            return Err(syn::Error::new_spanned(
                &method.sig,
                format!(
                    "#[http_client(bean)] requires an object-safe trait, but method `{}` is \
                     generic; remove the generic parameters or drop `bean`",
                    method.sig.ident
                ),
            ));
        }
    }
    Ok(())
}

/// Adds `Send` + `Sync` supertrait bounds to the emitted trait when they are
/// not already present. The `#[http_client(bean)]` `dyn Trait` autowire target
/// (`Arc<dyn Trait>`) is bound through `Container::bind`, which requires the
/// interface type be `Send + Sync + 'static` — so the trait must carry those
/// bounds. Mirrors the hand-written `trait Port: Send + Sync {}` a
/// `#[firefly(provides = "dyn Port")]` component relies on.
fn ensure_send_sync_supertraits(item: &mut ItemTrait) {
    let has = |name: &str| {
        item.supertraits.iter().any(|b| {
            matches!(b, syn::TypeParamBound::Trait(t)
                if t.path.segments.last().is_some_and(|s| s.ident == name))
        })
    };
    let has_send = has("Send");
    let has_sync = has("Sync");
    if !has_send {
        item.supertraits
            .push(syn::parse_quote!(::core::marker::Send));
    }
    if !has_sync {
        item.supertraits
            .push(syn::parse_quote!(::core::marker::Sync));
    }
}

/// Emits the DI bean registration: the `firefly_register` thunk (resolving a
/// shared `WebClient`), the `inventory` `ComponentRegistration`, and the
/// `dyn Trait` auto-bind — the same shape `derive_component` emits.
fn emit_di(
    trait_ident: &syn::Ident,
    impl_ident: &syn::Ident,
    cfg: &ClientArgs,
    facade: &Facade,
) -> syn::Result<TokenStream> {
    let container = facade.container();
    let client_rt = {
        let rt = facade.rt();
        quote!(#rt::firefly_client)
    };
    let bean_name = cfg.name.clone().unwrap_or_default();
    let type_name_lit = impl_ident.to_string();

    // Resolve the shared WebClient: a named bean when `client = "..."` is set,
    // else the primary `WebClient` bean.
    let resolve_web = match cfg.client.as_deref().filter(|s| !s.is_empty()) {
        Some(name) => {
            let panic_msg = format!(
                "#[http_client(bean)] registration of `{impl_ident}`: no `WebClient` bean named \
                 `{name}` is registered. Cause: {{}}"
            );
            quote! {
                let __web = #container::Container::resolve_named::<#client_rt::WebClient>(__c, #name)
                    .unwrap_or_else(|__e| ::core::panic!(#panic_msg, __e));
            }
        }
        None => {
            let panic_msg = format!(
                "#[http_client(bean)] registration of `{impl_ident}`: no `WebClient` bean is \
                 registered. Register one (e.g. \
                 `container.register_instance(WebClientBuilder::new(url).build())`) before \
                 `container.scan()`. Cause: {{}}"
            );
            quote! {
                let __web = #container::Container::resolve::<#client_rt::WebClient>(__c)
                    .unwrap_or_else(|__e| ::core::panic!(#panic_msg, __e));
            }
        }
    };

    let register_doc = format!(
        "Registers `{impl_ident}` (and binds `dyn {trait_ident}`) on the container, resolving a \
         shared `WebClient` bean. Generated by `#[http_client(bean)]`."
    );

    Ok(quote! {
        impl #impl_ident {
            #[doc = #register_doc]
            pub fn firefly_register(__container: &#container::Container) {
                __container.register_factory_with::<#impl_ident, _>(
                    #container::Scope::Singleton,
                    #bean_name,
                    false,
                    0,
                    |__c: &#container::Container| {
                        #resolve_web
                        ::core::result::Result::Ok(#impl_ident::with_client((*__web).clone()))
                    },
                );
                __container.set_stereotype::<#impl_ident>("service");
                __container.bind::<dyn #trait_ident, #impl_ident>(|__impl_arc| __impl_arc);
            }
        }

        #container::inventory::submit! {
            #container::ComponentRegistration {
                type_name: #type_name_lit,
                module_path: ::core::module_path!(),
                bean_name: #bean_name,
                stereotype: #container::BeanStereotype::Service,
                scope: #container::Scope::Singleton,
                primary: false,
                order: 0,
                lazy: false,
                register: <#impl_ident>::firefly_register,
                conditions: || ::std::vec![],
            }
        }
    })
}
