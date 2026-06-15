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

//! The `#[bean]` factory-method attribute (Spring/pyfly `@Bean`).
//!
//! Applied to an `impl` block of a `#[derive(Configuration)]` /
//! `#[derive(AutoConfiguration)]` type. Every method inside the block carrying a
//! `#[bean(...)]` marker becomes a bean factory keyed by its return type — the
//! Rust analog of pyfly's `_process_configurations`, which resolves the
//! configuration bean, calls each `@bean` method, and registers the result by
//! its concrete return type.
//!
//! Each `#[bean]` method generates two things:
//!
//! 1. a per-method `firefly_register_bean__<name>(&Container)` associated
//!    function that resolves the configuration holder and registers the factory;
//! 2. an [`inventory::submit!`] of a `ComponentRegistration` (stereotype
//!    `Bean`) carrying the method's conditions, so [`Container::scan`] discovers
//!    and registers the bean **automatically** — no manual call needed — and
//!    honours `#[bean(profile / condition_on_* )]` through the same two-pass
//!    evaluation used for stereotypes. This is what makes `@Bean` +
//!    `@ConditionalOnMissingBean` auto-configuration work.
//!
//! A `firefly_register_beans(&Container)` aggregate function is also generated
//! (it calls every per-method registrar, honouring `profile`) for the explicit
//! `register_all!`-style path and for generic holders that cannot be
//! inventoried.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ItemImpl, ReturnType};

use crate::common::facade_from_override;

/// One discovered `#[bean]` factory method.
struct BeanMethod {
    ident: syn::Ident,
    return_ty: syn::Type,
    /// `true` when the factory method is `async fn` — registered as an async
    /// bean (awaited during `Container::init_async_beans`) rather than a
    /// synchronous factory.
    is_async: bool,
    /// Resolve expressions for each method argument (constructor injection).
    arg_resolvers: Vec<TokenStream>,
    /// Short type names of each `Arc<Dep>` argument, for the admin dependency
    /// graph's edges.
    dep_names: Vec<String>,
    args: BeanArgs,
}

/// Per-method `#[bean(...)]` options.
#[derive(Default)]
struct BeanArgs {
    name: Option<String>,
    scope: Option<String>,
    primary: bool,
    order: Option<i32>,
    profile: Option<String>,
    condition_on_property: Option<String>,
    condition_on_class: Option<String>,
    condition_on_bean: Option<String>,
    condition_on_missing_bean: Option<String>,
    condition_on_single_candidate: Option<String>,
}

/// Expands `#[bean]` on an `impl` block. The optional `crate = "..."` argument
/// overrides the facade path.
pub(crate) fn bean_impl(args: TokenStream, item: ItemImpl) -> syn::Result<TokenStream> {
    // Parse an optional `crate = "..."` facade override from the attribute args.
    let krate = parse_crate_override(args)?;
    let facade = facade_from_override(&krate)?;
    let container = facade.container();

    let mut item = item;
    let self_ty = (*item.self_ty).clone();
    // Generic holders cannot be inventoried (the monomorphization is chosen at
    // the use site), so they fall back to the explicit `firefly_register_beans`
    // path only — no `inventory::submit!`.
    let is_generic = !item.generics.params.is_empty();

    let mut methods: Vec<BeanMethod> = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        // Find and strip the `#[bean(...)]` marker.
        let mut bean_attr: Option<syn::Attribute> = None;
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            if attr.path().is_ident("bean") {
                if bean_attr.is_some() {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "a method may carry at most one #[bean] marker",
                    ));
                }
                bean_attr = Some(attr);
            } else {
                kept.push(attr);
            }
        }
        method.attrs = kept;
        let Some(attr) = bean_attr else { continue };

        let bean_args = parse_bean_args(&attr)?;

        // Determine the return type (the bean's interface/concrete type).
        let return_ty = match &method.sig.output {
            ReturnType::Type(_, ty) => (**ty).clone(),
            ReturnType::Default => {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "a #[bean] method must declare a return type — it is the bean's key",
                ));
            }
        };

        // Build resolve expressions for each argument (skip the receiver).
        let mut arg_resolvers = Vec::new();
        let mut dep_names = Vec::new();
        for input in &method.sig.inputs {
            match input {
                FnArg::Receiver(_) => {}
                FnArg::Typed(pat) => {
                    let inner = arc_inner(&pat.ty).ok_or_else(|| {
                        syn::Error::new_spanned(
                            &pat.ty,
                            "a #[bean] method parameter must be `Arc<Dep>` — \
                             the container resolves dependencies as `Arc<T>`",
                        )
                    })?;
                    if let Some(name) = type_short_name(inner) {
                        dep_names.push(name);
                    }
                    arg_resolvers.push(quote! {
                        #container::Container::resolve::<#inner>(__c)?
                    });
                }
            }
        }

        methods.push(BeanMethod {
            ident: method.sig.ident.clone(),
            return_ty,
            is_async: method.sig.asyncness.is_some(),
            arg_resolvers,
            dep_names,
            args: bean_args,
        });
    }

    if methods.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[bean] on an impl block found no `#[bean]`-marked methods inside",
        ));
    }

    // Per-method registrar functions, inventory submissions, and the aggregate
    // `firefly_register_beans` call list.
    let mut registrar_fns: Vec<TokenStream> = Vec::new();
    let mut submissions: Vec<TokenStream> = Vec::new();
    let mut aggregate_calls: Vec<TokenStream> = Vec::new();

    for m in &methods {
        let mident = &m.ident;
        let return_ty = &m.return_ty;
        let resolvers = &m.arg_resolvers;
        let dep_names = &m.dep_names;
        let name = m.args.name.clone().unwrap_or_else(|| mident.to_string());
        let primary = m.args.primary;
        let order: i32 = m.args.order.unwrap_or(0);
        let scope_tokens = bean_scope_tokens(&container, m.args.scope.as_deref());
        let registrar_ident = format_ident!("__firefly_register_bean_{}", mident);

        // The raw registration body (no condition checks — those are evaluated
        // by `Container::scan()` via the inventory thunk's `conditions`). An
        // `async fn` factory registers an async bean (awaited as a batch by
        // `Container::init_async_beans` after the scan); a plain `fn` registers a
        // synchronous factory resolved on demand.
        let registrar_fn = if m.is_async {
            quote! {
                #[doc(hidden)]
                #[allow(non_snake_case)]
                pub fn #registrar_ident(__container: &#container::Container) {
                    // The stereotype + dependency labels are recorded by
                    // `register_async_factory` once the bean is built, so they
                    // are not set here (the bean does not exist yet).
                    __container.register_async_factory::<#return_ty, _, _>(
                        #scope_tokens,
                        #name,
                        #primary,
                        #order,
                        &[#(#dep_names),*],
                        move |__carc: ::std::sync::Arc<#container::Container>| async move {
                            let __c: &#container::Container = &*__carc;
                            let __cfg = #container::Container::resolve::<#self_ty>(__c)?;
                            ::core::result::Result::Ok(
                                <#self_ty>::#mident(&__cfg, #(#resolvers),*).await
                            )
                        },
                    );
                }
            }
        } else {
            quote! {
                #[doc(hidden)]
                #[allow(non_snake_case)]
                pub fn #registrar_ident(__container: &#container::Container) {
                    __container.register_factory_with::<#return_ty, _>(
                        #scope_tokens,
                        #name,
                        #primary,
                        #order,
                        |__c: &#container::Container| {
                            let __cfg = #container::Container::resolve::<#self_ty>(__c)?;
                            ::core::result::Result::Ok(<#self_ty>::#mident(&__cfg, #(#resolvers),*))
                        },
                    );
                    __container.set_stereotype::<#return_ty>("bean");
                    __container.set_dependencies::<#return_ty>(&[#(#dep_names),*]);
                }
            }
        };
        registrar_fns.push(registrar_fn);

        // The aggregate `firefly_register_beans` path keeps the legacy profile
        // guard so a manual call (without `scan`) still honours `#[bean(profile)]`.
        let aggregate_call = match m.args.profile.as_deref().filter(|s| !s.is_empty()) {
            Some(expr) => quote! {
                if __container.condition_context().accepts_profiles(#expr) {
                    <#self_ty>::#registrar_ident(__container);
                }
            },
            None => quote! { <#self_ty>::#registrar_ident(__container); },
        };
        aggregate_calls.push(aggregate_call);

        // The inventory thunk: `scan()` discovers it and evaluates every
        // condition (profile + conditional_on_*) in its two-pass system.
        if !is_generic {
            let conditions = build_bean_conditions(&container, &m.args);
            let type_name_lit = type_name_string(return_ty);
            submissions.push(quote! {
                #container::inventory::submit! {
                    #container::ComponentRegistration {
                        type_name: #type_name_lit,
                        module_path: ::core::module_path!(),
                        bean_name: #name,
                        stereotype: #container::BeanStereotype::Bean,
                        scope: #scope_tokens,
                        primary: #primary,
                        order: #order,
                        // A `#[bean]` factory bean is eagerly warmed like any
                        // other singleton (it is not `@Lazy`).
                        lazy: false,
                        register: <#self_ty>::#registrar_ident,
                        conditions: || #conditions,
                    }
                }
            });
        }
    }

    let register_doc = format!(
        "Registers every `#[bean]` factory method on `{}` with the container, \
         honouring `#[bean(profile = ...)]`. `Container::scan()` registers these \
         automatically; call this only for the explicit `register_all!` path or \
         for a generic holder that cannot be inventoried. Generated by `#[bean]`.",
        quote!(#self_ty)
    );

    Ok(quote! {
        #item

        impl #self_ty {
            #(#registrar_fns)*

            #[doc = #register_doc]
            pub fn firefly_register_beans(__container: &#container::Container) {
                #(#aggregate_calls)*
            }
        }

        #(#submissions)*
    })
}

/// Resolve the `Scope` token from the `#[bean(scope = "...")]` option.
fn bean_scope_tokens(container: &TokenStream, scope: Option<&str>) -> TokenStream {
    match scope {
        None | Some("singleton") | Some("Singleton") => quote!(#container::Scope::Singleton),
        Some("transient") | Some("Transient") | Some("prototype") | Some("Prototype") => {
            quote!(#container::Scope::Transient)
        }
        Some("request") | Some("Request") => quote!(#container::Scope::Request),
        Some("session") | Some("Session") => quote!(#container::Scope::Session),
        Some(_) => quote!(#container::Scope::Singleton),
    }
}

/// Build the `Vec<Condition>` literal for a bean method's inventory thunk.
fn build_bean_conditions(container: &TokenStream, args: &BeanArgs) -> TokenStream {
    let mut entries: Vec<TokenStream> = Vec::new();
    if let Some(p) = args.profile.as_deref().filter(|s| !s.is_empty()) {
        entries.push(quote!(#container::Condition::Profile(#p.to_string())));
    }
    if let Some(p) = args
        .condition_on_property
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        entries.push(quote!(#container::Condition::on_property(#p)));
    }
    if let Some(c) = args.condition_on_class.as_deref().filter(|s| !s.is_empty()) {
        entries.push(quote!(#container::Condition::OnClass(#c.to_string())));
    }
    if let Some(b) = args.condition_on_bean.as_deref().filter(|s| !s.is_empty()) {
        entries.push(quote!(#container::Condition::OnBean(#b.to_string())));
    }
    if let Some(b) = args
        .condition_on_missing_bean
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        entries.push(quote!(#container::Condition::OnMissingBean(#b.to_string())));
    }
    if let Some(b) = args
        .condition_on_single_candidate
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        entries.push(quote!(#container::Condition::OnSingleCandidate(#b.to_string())));
    }
    quote!(::std::vec![#(#entries),*])
}

/// A readable type-name string for a bean's return type (diagnostics + `/beans`).
fn type_name_string(ty: &syn::Type) -> String {
    quote!(#ty).to_string().replace(' ', "")
}

/// Parse `#[bean(name, scope, primary, order, profile, condition_on_*)]`.
fn parse_bean_args(attr: &syn::Attribute) -> syn::Result<BeanArgs> {
    let mut args = BeanArgs::default();
    // A bare `#[bean]` has no list.
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Ok(args);
    }
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            args.name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("scope") {
            args.scope = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("primary") {
            args.primary = true;
        } else if meta.path.is_ident("order") {
            args.order = Some(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?);
        } else if meta.path.is_ident("profile") {
            args.profile = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("condition_on_property") {
            args.condition_on_property = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("condition_on_class") {
            args.condition_on_class = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("condition_on_bean") {
            args.condition_on_bean = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("condition_on_missing_bean") {
            args.condition_on_missing_bean = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if meta.path.is_ident("condition_on_single_candidate") {
            args.condition_on_single_candidate =
                Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else {
            return Err(meta.error(
                "unknown #[bean] argument; use name, scope, primary, order, profile, \
                 condition_on_property, condition_on_class, condition_on_bean, \
                 condition_on_missing_bean, or condition_on_single_candidate",
            ));
        }
        Ok(())
    })?;
    Ok(args)
}

/// Parse a `crate = "..."` override out of the attribute args of `#[bean]`.
fn parse_crate_override(args: TokenStream) -> syn::Result<Option<String>> {
    if args.is_empty() {
        return Ok(None);
    }
    let mut found = None;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("crate") {
            let v: syn::LitStr = meta.value()?.parse()?;
            found = Some(v.value());
            Ok(())
        } else {
            Err(meta.error("the #[bean] impl-block attribute only accepts `crate = \"...\"`"))
        }
    });
    syn::parse::Parser::parse2(parser, args)?;
    Ok(found)
}

/// The short type name of a dependency for the admin graph — the last path
/// segment of a concrete type (`a::b::ReadModel` → `ReadModel`) or the trait
/// name of a `dyn Trait` port (`dyn Broker` → `Broker`).
fn type_short_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
        syn::Type::TraitObject(to) => to.bounds.iter().find_map(|b| match b {
            syn::TypeParamBound::Trait(t) => t.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        }),
        _ => None,
    }
}

/// Returns the inner `T` of an `Arc<T>` type, or `None`.
fn arc_inner(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Arc" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(a) = &seg.arguments else {
        return None;
    };
    a.args.iter().find_map(|g| match g {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}
