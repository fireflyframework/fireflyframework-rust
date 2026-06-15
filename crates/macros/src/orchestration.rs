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

//! Declarative orchestration macros — `#[saga]` (and, sharing this machinery,
//! `#[workflow]` / `#[tcc]`). The Rust spelling of Java/pyfly `@Saga` +
//! `@SagaStep`: an `impl` block of `async fn(&self, …) -> Result<T, E>` methods
//! marked `#[saga_step(...)]`, whose parameters are injected from the saga
//! context (`#[input]` / `#[from_step]` / `#[variable]` / `#[ctx]`), lowered
//! onto the `firefly-orchestration` `Saga` engine (`depends_on` DAG +
//! compensation + retry). The generated code routes every runtime type through
//! the `firefly` facade's `__rt` contract, so a one-dependency service compiles
//! it without naming `firefly-orchestration`.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ItemImpl, LitStr};

use crate::common::facade_from_override;

// ===========================================================================
// #[saga]
// ===========================================================================

#[derive(FromMeta, Default)]
#[darling(default)]
struct SagaArgs {
    #[darling(rename = "crate")]
    krate: Option<String>,
    name: Option<String>,
    policy: Option<String>,
}

/// How one step/compensation method parameter is supplied from the context.
enum Inject {
    /// `#[input]` (whole input) / `#[input("field")]` (a field of the input).
    Input(Option<String>),
    /// `#[from_step("id")]` — the `Ok` value a prior step published.
    FromStep(String),
    /// `#[variable("key")]` — a saga-scoped context variable.
    Variable(String),
    /// `#[ctx]` — the `StepContext` itself (cloned).
    Ctx,
}

struct ParamPlan {
    inject: Inject,
    ty: syn::Type,
}

/// The positional injection plan for one method (receiver excluded).
struct MethodPlan {
    params: Vec<ParamPlan>,
}

#[derive(Default)]
struct StepMeta {
    id: String,
    depends_on: Vec<String>,
    compensate: Option<String>,
    retry: u32,
    backoff_ms: u64,
    timeout_ms: u64,
    jitter: bool,
}

const INJECT_ATTRS: &[&str] = &["input", "from_step", "variable", "ctx"];

fn is_inject_attr(attr: &syn::Attribute) -> bool {
    INJECT_ATTRS.iter().any(|n| attr.path().is_ident(n))
}

/// Reads the injection marker on a typed parameter (and validates its shape).
fn param_inject(pt: &syn::PatType) -> syn::Result<Inject> {
    let mut found: Option<Inject> = None;
    for attr in &pt.attrs {
        let p = attr.path();
        let inject = if p.is_ident("ctx") {
            if !matches!(attr.meta, syn::Meta::Path(_)) {
                return Err(syn::Error::new_spanned(attr, "#[ctx] takes no arguments"));
            }
            Inject::Ctx
        } else if p.is_ident("input") {
            match &attr.meta {
                syn::Meta::Path(_) => Inject::Input(None),
                _ => Inject::Input(Some(attr.parse_args::<LitStr>()?.value())),
            }
        } else if p.is_ident("from_step") {
            Inject::FromStep(
                attr.parse_args::<LitStr>()
                    .map_err(|_| {
                        syn::Error::new_spanned(
                            attr,
                            "#[from_step(\"step-id\")] requires a step id",
                        )
                    })?
                    .value(),
            )
        } else if p.is_ident("variable") {
            Inject::Variable(
                attr.parse_args::<LitStr>()
                    .map_err(|_| {
                        syn::Error::new_spanned(
                            attr,
                            "#[variable(\"key\")] requires a variable key",
                        )
                    })?
                    .value(),
            )
        } else {
            continue;
        };
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                pt,
                "a saga parameter may carry at most one injection marker",
            ));
        }
        found = Some(inject);
    }
    found.ok_or_else(|| {
        syn::Error::new_spanned(
            pt,
            "every saga-method parameter must be injected: use #[input], #[from_step(\"id\")], \
             #[variable(\"key\")], or #[ctx]",
        )
    })
}

/// Records a method's positional injection plan and strips the injection markers
/// from its parameters so the re-emitted method is plain Rust.
fn plan_method(method: &mut syn::ImplItemFn) -> syn::Result<MethodPlan> {
    let mut params = Vec::new();
    let mut has_receiver = false;
    for arg in method.sig.inputs.iter_mut() {
        match arg {
            FnArg::Receiver(_) => has_receiver = true,
            FnArg::Typed(pt) => {
                let inject = param_inject(pt)?;
                params.push(ParamPlan {
                    inject,
                    ty: (*pt.ty).clone(),
                });
                pt.attrs.retain(|a| !is_inject_attr(a));
            }
        }
    }
    if !has_receiver {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "a saga step/compensation method must take `&self`",
        ));
    }
    if method.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "a saga step/compensation method must be `async fn`",
        ));
    }
    Ok(MethodPlan { params })
}

/// Plans (and strips the injection markers from) every method whose name is in
/// `names` — the steps/participants plus the compensation/confirm/cancel methods
/// they reference. A method with only `&self` (no injected parameters) plans to
/// an empty parameter list, so a no-argument compensation is valid.
fn plan_participating(
    items: &mut [ImplItem],
    names: &std::collections::BTreeSet<String>,
) -> syn::Result<std::collections::BTreeMap<String, MethodPlan>> {
    let mut plans = std::collections::BTreeMap::new();
    for it in items.iter_mut() {
        if let ImplItem::Fn(method) = it {
            let name = method.sig.ident.to_string();
            if names.contains(&name) {
                plans.insert(name, plan_method(method)?);
            }
        }
    }
    Ok(plans)
}

fn parse_step_meta(attr: &syn::Attribute) -> syn::Result<StepMeta> {
    let mut meta = StepMeta::default();
    attr.parse_nested_meta(|m| {
        if m.path.is_ident("id") {
            meta.id = m.value()?.parse::<LitStr>()?.value();
        } else if m.path.is_ident("compensate") {
            meta.compensate = Some(m.value()?.parse::<LitStr>()?.value());
        } else if m.path.is_ident("depends_on") {
            let arr: syn::ExprArray = m.value()?.parse()?;
            for el in arr.elems {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = el
                {
                    meta.depends_on.push(s.value());
                } else {
                    return Err(m.error(
                        "depends_on takes string literals, e.g. depends_on = [\"a\", \"b\"]",
                    ));
                }
            }
        } else if m.path.is_ident("retry") {
            meta.retry = m.value()?.parse::<syn::LitInt>()?.base10_parse()?;
        } else if m.path.is_ident("backoff_ms") {
            meta.backoff_ms = m.value()?.parse::<syn::LitInt>()?.base10_parse()?;
        } else if m.path.is_ident("timeout_ms") {
            meta.timeout_ms = m.value()?.parse::<syn::LitInt>()?.base10_parse()?;
        } else if m.path.is_ident("jitter") {
            meta.jitter = if m.input.peek(syn::Token![=]) {
                m.value()?.parse::<syn::LitBool>()?.value()
            } else {
                true
            };
        } else {
            return Err(m.error(
                "unknown #[saga_step] argument; use id, depends_on, compensate, retry, \
                 backoff_ms, timeout_ms, or jitter",
            ));
        }
        Ok(())
    })?;
    if meta.id.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[saga_step] requires an id, e.g. #[saga_step(id = \"reserve\")]",
        ));
    }
    Ok(meta)
}

/// Builds the `async move { … }` body of a step/compensation closure: resolve
/// every injected parameter from the `__ctx`, call `__self.method(...)`, and map
/// the `Result`. A step (`set_id = Some`) publishes its `Ok` value; a
/// compensation (`set_id = None`) maps `Ok` to `()`.
fn invocation_body(
    method: &syn::Ident,
    plan: &MethodPlan,
    orch: &TokenStream,
    sj: &TokenStream,
    set_id: Option<&str>,
) -> TokenStream {
    let box_err = |msg: TokenStream| -> TokenStream {
        quote! {
            ::std::boxed::Box::<dyn ::std::error::Error + ::core::marker::Send + ::core::marker::Sync>::from(#msg)
        }
    };
    let mname = method.to_string();
    let mut stmts = Vec::new();
    let mut args = Vec::new();
    for (i, p) in plan.params.iter().enumerate() {
        let a = format_ident!("__a{}", i);
        let ty = &p.ty;
        let decode_err = box_err(quote! {
            ::std::format!("saga: could not decode a parameter of step {:?}: {}", #mname, __e)
        });
        let stmt = match &p.inject {
            Inject::Ctx => quote! { let #a = __ctx.clone(); },
            Inject::Input(None) => quote! {
                let #a: #ty = #sj::from_value(__ctx.input()).map_err(|__e| #decode_err)?;
            },
            Inject::Input(Some(field)) => {
                let miss = box_err(quote! {
                    ::std::format!("saga: step {:?} requires input field {:?}", #mname, #field)
                });
                quote! {
                    let #a: #ty = #sj::from_value(
                        __ctx.input_field(#field).ok_or_else(|| #miss)?
                    ).map_err(|__e| #decode_err)?;
                }
            }
            Inject::FromStep(sid) => {
                let miss = box_err(quote! {
                    ::std::format!("saga: a parameter needs the result of step {:?}, which has not run", #sid)
                });
                quote! {
                    let #a: #ty = #sj::from_value(
                        __ctx.result(#sid).ok_or_else(|| #miss)?
                    ).map_err(|__e| #decode_err)?;
                }
            }
            Inject::Variable(key) => {
                let miss = box_err(quote! {
                    ::std::format!("saga: a parameter needs context variable {:?}, which is unset", #key)
                });
                quote! {
                    let #a: #ty = #sj::from_value(
                        __ctx.variable(#key).ok_or_else(|| #miss)?
                    ).map_err(|__e| #decode_err)?;
                }
            }
        };
        stmts.push(stmt);
        args.push(a);
    }
    let call = quote! { __self.#method(#(#args),*).await };
    match set_id {
        Some(id) => {
            // A successful step's Ok value is published for `#[from_step]`
            // consumers. A serialization failure there is the *producing*
            // step's error (mirroring the input-decode path) rather than a
            // silently-stored Null that would mislead a downstream consumer.
            let encode_err = box_err(quote! {
                ::std::format!("saga: could not encode the result of step {:?}: {}", #id, __e)
            });
            quote! {
                #(#stmts)*
                match #call {
                    ::core::result::Result::Ok(__v) => {
                        let __encoded = #sj::to_value(&__v).map_err(|__e| #encode_err)?;
                        __ctx.set_result(#id, __encoded);
                        ::core::result::Result::Ok(())
                    }
                    ::core::result::Result::Err(__e) =>
                        ::core::result::Result::Err(::std::boxed::Box::new(__e) as #orch::BoxError),
                }
            }
        }
        None => quote! {
            #(#stmts)*
            match #call {
                ::core::result::Result::Ok(_) => ::core::result::Result::Ok(()),
                ::core::result::Result::Err(__e) =>
                    ::core::result::Result::Err(::std::boxed::Box::new(__e) as #orch::BoxError),
            }
        },
    }
}

fn policy_variant(raw: &str) -> Option<&'static str> {
    match raw.trim().to_lowercase().replace('-', "_").as_str() {
        "best_effort" => Some("BestEffort"),
        "stop_on_error" => Some("StopOnError"),
        "retry_with_backoff" => Some("RetryWithBackoff"),
        "circuit_breaker" => Some("CircuitBreaker"),
        "best_effort_parallel" => Some("BestEffortParallel"),
        "grouped_parallel" => Some("GroupedParallel"),
        _ => None,
    }
}

/// The type's bare ident, for the default saga name.
fn type_ident_name(ty: &syn::Type) -> String {
    if let syn::Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            return seg.ident.to_string();
        }
    }
    quote!(#ty).to_string()
}

pub(crate) fn saga_impl(args: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = SagaArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let orch = facade.orchestration();
    let sj = facade.serde_json();

    let self_ty = (*item.self_ty).clone();
    let saga_name = args
        .name
        .clone()
        .unwrap_or_else(|| type_ident_name(&self_ty));

    let policy_tokens = match &args.policy {
        None => None,
        Some(p) => {
            let variant = policy_variant(p).ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    format!(
                        "unknown saga policy {p:?}; use best_effort, stop_on_error, \
                         retry_with_backoff, circuit_breaker, best_effort_parallel, or grouped_parallel"
                    ),
                )
            })?;
            let v = format_ident!("{}", variant);
            Some(quote!(.policy(#orch::CompensationPolicy::#v)))
        }
    };

    // Pass 1: pull the #[saga_step] markers off the methods.
    let mut steps: Vec<(StepMeta, syn::Ident)> = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let mut step_meta: Option<StepMeta> = None;
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            if attr.path().is_ident("saga_step") {
                if step_meta.is_some() {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "a method may carry at most one #[saga_step]",
                    ));
                }
                step_meta = Some(parse_step_meta(&attr)?);
            } else {
                kept.push(attr);
            }
        }
        method.attrs = kept;
        if let Some(meta) = step_meta {
            steps.push((meta, method.sig.ident.clone()));
        }
    }

    if steps.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[saga] found no #[saga_step] methods",
        ));
    }

    // Pass 2: plan the participating methods (each step plus the compensation
    // it names), stripping their parameter injection markers.
    let mut participating: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (meta, ident) in &steps {
        participating.insert(ident.to_string());
        if let Some(comp) = &meta.compensate {
            participating.insert(comp.clone());
        }
    }
    let plans = plan_participating(&mut item.items, &participating)?;

    // Build the step expressions.
    let mut step_exprs = Vec::new();
    for (meta, method) in &steps {
        let id = &meta.id;
        let plan = plans
            .get(&method.to_string())
            .expect("step method was planned");
        let body = invocation_body(method, plan, &orch, &sj, Some(id));

        let depends = if meta.depends_on.is_empty() {
            quote!()
        } else {
            let deps = &meta.depends_on;
            quote!(.depends_on([#(#deps),*]))
        };

        let retry = if meta.retry > 0 || meta.timeout_ms > 0 || meta.backoff_ms > 0 {
            let attempts = meta.retry + 1;
            let backoff = meta.backoff_ms;
            let timeout = meta.timeout_ms;
            let jitter = meta.jitter;
            quote!(.with_retry(#orch::RetryPolicy {
                max_attempts: #attempts,
                backoff_ms: #backoff,
                timeout_ms: #timeout,
                jitter: #jitter,
                jitter_factor: 0.5f64,
            }))
        } else {
            quote!()
        };

        let compensation = match &meta.compensate {
            None => quote!(),
            Some(comp_name) => {
                let comp_ident = format_ident!("{}", comp_name);
                let comp_plan = plans.get(comp_name).ok_or_else(|| {
                    syn::Error::new_spanned(
                        &self_ty,
                        format!(
                            "saga step {:?} names compensation {comp_name:?}, but no such method \
                             was found in this impl (a compensation method must take `&self` and \
                             return `Result<_, E>`)",
                            meta.id
                        ),
                    )
                })?;
                let comp_body = invocation_body(&comp_ident, comp_plan, &orch, &sj, None);
                quote!(.with_context_compensation({
                    let __self = ::std::sync::Arc::clone(&self);
                    move |__ctx: #orch::StepContext| {
                        let __self = ::std::sync::Arc::clone(&__self);
                        async move { #comp_body }
                    }
                }))
            }
        };

        step_exprs.push(quote! {
            __saga = __saga.step(
                #orch::Step::with_context(#id, {
                    let __self = ::std::sync::Arc::clone(&self);
                    move |__ctx: #orch::StepContext| {
                        let __self = ::std::sync::Arc::clone(&__self);
                        async move { #body }
                    }
                })
                #depends
                #retry
                #compensation
            );
        });
    }

    let saga_doc = format!(
        "Builds the `{saga_name}` saga from this `#[saga]` impl — the steps, their \
         `depends_on` order, compensations, and retry policies, ready to `.run()` / \
         `.run_with_context(&ctx)`. Generated by `#[saga]`."
    );
    let run_doc = format!(
        "Runs the `{saga_name}` saga: serialises `input` into the step context and \
         executes the DAG, compensating on failure. Generated by `#[saga]`."
    );

    Ok(quote! {
        #item

        impl #self_ty {
            #[doc = #saga_doc]
            pub fn saga(self: ::std::sync::Arc<Self>) -> #orch::Saga {
                let mut __saga = #orch::Saga::new(#saga_name) #policy_tokens;
                #(#step_exprs)*
                __saga
            }

            #[doc = #run_doc]
            pub async fn run<__I>(
                self: ::std::sync::Arc<Self>,
                input: __I,
            ) -> ::core::result::Result<#orch::Outcome, #orch::SagaFailure>
            where
                __I: ::serde::Serialize,
            {
                let __ctx = #orch::StepContext::with_input(
                    #sj::to_value(&input).unwrap_or(#sj::Value::Null),
                );
                <Self>::saga(self).run_with_context(&__ctx).await
            }
        }
    })
}

// ===========================================================================
// #[workflow]
// ===========================================================================

#[derive(FromMeta, Default)]
#[darling(default)]
struct WorkflowArgs {
    #[darling(rename = "crate")]
    krate: Option<String>,
    name: Option<String>,
}

#[derive(Default)]
struct NodeMeta {
    id: String,
    depends_on: Vec<String>,
    compensate: Option<String>,
    when: Option<String>,
    fire_and_forget: bool,
}

fn parse_node_meta(attr: &syn::Attribute) -> syn::Result<NodeMeta> {
    let mut meta = NodeMeta::default();
    attr.parse_nested_meta(|m| {
        if m.path.is_ident("id") {
            meta.id = m.value()?.parse::<LitStr>()?.value();
        } else if m.path.is_ident("compensate") {
            meta.compensate = Some(m.value()?.parse::<LitStr>()?.value());
        } else if m.path.is_ident("when") {
            meta.when = Some(m.value()?.parse::<LitStr>()?.value());
        } else if m.path.is_ident("depends_on") {
            let arr: syn::ExprArray = m.value()?.parse()?;
            for el in arr.elems {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = el
                {
                    meta.depends_on.push(s.value());
                } else {
                    return Err(m.error("depends_on takes string literals, e.g. depends_on = [\"a\", \"b\"]"));
                }
            }
        } else if m.path.is_ident("fire_and_forget") {
            meta.fire_and_forget = if m.input.peek(syn::Token![=]) {
                m.value()?.parse::<syn::LitBool>()?.value()
            } else {
                true
            };
        } else {
            return Err(m.error(
                "unknown #[workflow_step] argument; use id, depends_on, compensate, when, or fire_and_forget",
            ));
        }
        Ok(())
    })?;
    if meta.id.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[workflow_step] requires an id, e.g. #[workflow_step(id = \"fraud-scan\")]",
        ));
    }
    Ok(meta)
}

pub(crate) fn workflow_impl(args: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = WorkflowArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let orch = facade.orchestration();
    let sj = facade.serde_json();
    let self_ty = (*item.self_ty).clone();
    let wf_name = args
        .name
        .clone()
        .unwrap_or_else(|| type_ident_name(&self_ty));

    let mut nodes: Vec<(NodeMeta, syn::Ident)> = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let mut node_meta: Option<NodeMeta> = None;
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            if attr.path().is_ident("workflow_step") {
                if node_meta.is_some() {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "a method may carry at most one #[workflow_step]",
                    ));
                }
                node_meta = Some(parse_node_meta(&attr)?);
            } else {
                kept.push(attr);
            }
        }
        method.attrs = kept;
        if let Some(meta) = node_meta {
            nodes.push((meta, method.sig.ident.clone()));
        }
    }
    if nodes.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[workflow] found no #[workflow_step] methods",
        ));
    }

    let mut participating: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (meta, ident) in &nodes {
        participating.insert(ident.to_string());
        if let Some(comp) = &meta.compensate {
            participating.insert(comp.clone());
        }
    }
    let plans = plan_participating(&mut item.items, &participating)?;

    let mut node_exprs = Vec::new();
    for (meta, method) in &nodes {
        let id = &meta.id;
        let plan = plans.get(&method.to_string()).expect("node method planned");
        let body = invocation_body(method, plan, &orch, &sj, Some(id));
        let depends = if meta.depends_on.is_empty() {
            quote!()
        } else {
            let deps = &meta.depends_on;
            quote!(.depends_on([#(#deps),*]))
        };
        let when = match &meta.when {
            Some(expr) => quote!(.when(#expr)),
            None => quote!(),
        };
        let faf = if meta.fire_and_forget {
            quote!(.fire_and_forget())
        } else {
            quote!()
        };
        let compensation = match &meta.compensate {
            None => quote!(),
            Some(comp_name) => {
                let comp_ident = format_ident!("{}", comp_name);
                let comp_plan = plans.get(comp_name).ok_or_else(|| {
                    syn::Error::new_spanned(
                        &self_ty,
                        format!(
                            "workflow node {:?} names compensation {comp_name:?}, but no such \
                             method was found in this impl",
                            meta.id
                        ),
                    )
                })?;
                let comp_body = invocation_body(&comp_ident, comp_plan, &orch, &sj, None);
                quote!(.with_compensation({
                    let __self = ::std::sync::Arc::clone(&self);
                    move |__ctx: #orch::StepContext| {
                        let __self = ::std::sync::Arc::clone(&__self);
                        async move { #comp_body }
                    }
                }))
            }
        };
        node_exprs.push(quote! {
            __wf = __wf.node(
                #orch::Node::with_context(#id, {
                    let __self = ::std::sync::Arc::clone(&self);
                    move |__ctx: #orch::StepContext| {
                        let __self = ::std::sync::Arc::clone(&__self);
                        async move { #body }
                    }
                })
                #depends
                #when
                #faf
                #compensation
            );
        });
    }

    let wf_doc = format!(
        "Builds the `{wf_name}` workflow (a DAG of nodes with parallel layers, \
         conditions, and compensation) from this `#[workflow]` impl. Generated by `#[workflow]`."
    );
    let run_doc = format!(
        "Runs the `{wf_name}` workflow: serialises `input` into the step context, executes the \
         DAG in topological waves, and compensates completed nodes on failure. Generated by `#[workflow]`."
    );

    Ok(quote! {
        #item

        impl #self_ty {
            #[doc = #wf_doc]
            pub fn workflow(self: ::std::sync::Arc<Self>) -> #orch::Workflow {
                let mut __wf = #orch::Workflow::new(#wf_name);
                #(#node_exprs)*
                __wf
            }

            #[doc = #run_doc]
            pub async fn run<__I>(
                self: ::std::sync::Arc<Self>,
                input: __I,
            ) -> ::core::result::Result<(), #orch::WorkflowError>
            where
                __I: ::serde::Serialize,
            {
                let __ctx = #orch::StepContext::with_input(
                    #sj::to_value(&input).unwrap_or(#sj::Value::Null),
                );
                <Self>::workflow(self).run_with_context(&__ctx).await
            }
        }
    })
}

// ===========================================================================
// #[tcc]
// ===========================================================================

#[derive(FromMeta, Default)]
#[darling(default)]
struct TccArgs {
    #[darling(rename = "crate")]
    krate: Option<String>,
    name: Option<String>,
}

#[derive(Default)]
struct ParticipantMeta {
    name: String,
    confirm: String,
    cancel: Option<String>,
    retry: u32,
    backoff_ms: u64,
    timeout_ms: u64,
}

fn parse_participant_meta(attr: &syn::Attribute) -> syn::Result<ParticipantMeta> {
    let mut meta = ParticipantMeta::default();
    attr.parse_nested_meta(|m| {
        if m.path.is_ident("name") {
            meta.name = m.value()?.parse::<LitStr>()?.value();
        } else if m.path.is_ident("confirm") {
            meta.confirm = m.value()?.parse::<LitStr>()?.value();
        } else if m.path.is_ident("cancel") {
            meta.cancel = Some(m.value()?.parse::<LitStr>()?.value());
        } else if m.path.is_ident("retry") {
            meta.retry = m.value()?.parse::<syn::LitInt>()?.base10_parse()?;
        } else if m.path.is_ident("backoff_ms") {
            meta.backoff_ms = m.value()?.parse::<syn::LitInt>()?.base10_parse()?;
        } else if m.path.is_ident("timeout_ms") {
            meta.timeout_ms = m.value()?.parse::<syn::LitInt>()?.base10_parse()?;
        } else {
            return Err(m.error(
                "unknown #[participant] argument; use name, confirm, cancel, retry, backoff_ms, or timeout_ms",
            ));
        }
        Ok(())
    })?;
    if meta.name.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[participant] requires name = \"...\" and confirm = \"method\"",
        ));
    }
    if meta.confirm.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[participant] requires confirm = \"method\" (the confirm phase)",
        ));
    }
    Ok(meta)
}

pub(crate) fn tcc_impl(args: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = TccArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let orch = facade.orchestration();
    let sj = facade.serde_json();
    let self_ty = (*item.self_ty).clone();
    let tcc_name = args
        .name
        .clone()
        .unwrap_or_else(|| type_ident_name(&self_ty));

    // The #[participant(...)] marker sits on the *try* method.
    let mut participants: Vec<(ParticipantMeta, syn::Ident)> = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let mut pmeta: Option<ParticipantMeta> = None;
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            if attr.path().is_ident("participant") {
                if pmeta.is_some() {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "a method may carry at most one #[participant]",
                    ));
                }
                pmeta = Some(parse_participant_meta(&attr)?);
            } else {
                kept.push(attr);
            }
        }
        method.attrs = kept;
        if let Some(meta) = pmeta {
            participants.push((meta, method.sig.ident.clone()));
        }
    }
    if participants.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[tcc] found no #[participant] methods (mark each try method with \
             #[participant(name = \"...\", confirm = \"...\")])",
        ));
    }

    let mut participating: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (meta, ident) in &participants {
        participating.insert(ident.to_string());
        participating.insert(meta.confirm.clone());
        if let Some(c) = &meta.cancel {
            participating.insert(c.clone());
        }
    }
    let plans = plan_participating(&mut item.items, &participating)?;

    let closure = |body: TokenStream, orch: &TokenStream| -> TokenStream {
        quote!({
            let __self = ::std::sync::Arc::clone(&self);
            move |__ctx: #orch::StepContext| {
                let __self = ::std::sync::Arc::clone(&__self);
                async move { #body }
            }
        })
    };

    let mut participant_exprs = Vec::new();
    for (meta, try_ident) in &participants {
        let pname = &meta.name;
        let try_plan = plans
            .get(&try_ident.to_string())
            .expect("try method planned");
        // Try publishes its result under the participant name; confirm/cancel
        // read it back via #[from_step("<name>")].
        let try_body = invocation_body(try_ident, try_plan, &orch, &sj, Some(pname));

        let confirm_ident = format_ident!("{}", meta.confirm);
        let confirm_plan = plans.get(&meta.confirm).ok_or_else(|| {
            syn::Error::new_spanned(
                &self_ty,
                format!(
                    "TCC participant {:?} names confirm method {:?}, but no such method was found",
                    meta.name, meta.confirm
                ),
            )
        })?;
        let confirm_body = invocation_body(&confirm_ident, confirm_plan, &orch, &sj, None);

        let cancel = match &meta.cancel {
            None => quote!(),
            Some(cancel_name) => {
                let cancel_ident = format_ident!("{}", cancel_name);
                let cancel_plan = plans.get(cancel_name).ok_or_else(|| {
                    syn::Error::new_spanned(
                        &self_ty,
                        format!(
                            "TCC participant {:?} names cancel method {cancel_name:?}, but no such \
                             method was found",
                            meta.name
                        ),
                    )
                })?;
                let cancel_body = invocation_body(&cancel_ident, cancel_plan, &orch, &sj, None);
                let c = closure(cancel_body, &orch);
                quote!(.with_context_cancel(#c))
            }
        };

        let retry = if meta.retry > 0 || meta.timeout_ms > 0 || meta.backoff_ms > 0 {
            let attempts = meta.retry + 1;
            let backoff = meta.backoff_ms;
            let timeout = meta.timeout_ms;
            quote!(.with_retry(#orch::RetryPolicy {
                max_attempts: #attempts,
                backoff_ms: #backoff,
                timeout_ms: #timeout,
                jitter: false,
                jitter_factor: 0.5f64,
            }))
        } else {
            quote!()
        };

        let try_c = closure(try_body, &orch);
        let confirm_c = closure(confirm_body, &orch);
        participant_exprs.push(quote! {
            __tcc = __tcc.participant(
                #orch::TccParticipant::with_context(#pname, #try_c, #confirm_c)
                #cancel
                #retry
            );
        });
    }

    let tcc_doc = format!(
        "Builds the `{tcc_name}` TCC coordinator (Try / Confirm / Cancel) from this `#[tcc]` impl. \
         Generated by `#[tcc]`."
    );
    let run_doc = format!(
        "Runs the `{tcc_name}` TCC: tries every participant, then confirms all on success or \
         cancels the tried ones on any try failure. Generated by `#[tcc]`."
    );

    Ok(quote! {
        #item

        impl #self_ty {
            #[doc = #tcc_doc]
            pub fn tcc(self: ::std::sync::Arc<Self>) -> #orch::Tcc {
                let mut __tcc = #orch::Tcc::new(#tcc_name);
                #(#participant_exprs)*
                __tcc
            }

            #[doc = #run_doc]
            pub async fn run<__I>(
                self: ::std::sync::Arc<Self>,
                input: __I,
            ) -> ::core::result::Result<(), #orch::TccError>
            where
                __I: ::serde::Serialize,
            {
                let __ctx = #orch::StepContext::with_input(
                    #sj::to_value(&input).unwrap_or(#sj::Value::Null),
                );
                <Self>::tcc(self).run_with_context(&__ctx).await
            }
        }
    })
}
