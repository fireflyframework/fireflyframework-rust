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

//! The dependency-injection macro layer: the stereotype derives
//! (`#[derive(Component)]` + `Service` / `Repository` / `Configuration` /
//! `Controller` / `ConfigProperties`), the `#[bean]` factory-method attribute,
//! field autowiring (`#[autowired]` + `#[firefly(value = ...)]`), bean
//! lifecycle (`#[post_construct]` / `#[pre_destroy]`), conditional/profile
//! gating, interface auto-binding, and component-scan `inventory` submission —
//! plus the `register_all!` explicit-list fallback.
//!
//! Every stereotype derive generates a `firefly_register(&Container)` method
//! that resolves and injects its fields, *and* submits an `inventory` thunk so
//! `Container::scan()` (the Rust analog of pyfly's `scan_package`) can discover
//! it across the whole crate graph.

use darling::FromDeriveInput;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Type};

use crate::common::facade_from_override;

/// Which stereotype the derive was invoked as. Sets the default scope label,
/// the documentation, and the `BeanStereotype` recorded for introspection.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stereotype {
    Component,
    Service,
    Repository,
    Configuration,
    Controller,
}

impl Stereotype {
    fn label(self) -> &'static str {
        match self {
            Stereotype::Component => "component",
            Stereotype::Service => "service",
            Stereotype::Repository => "repository",
            Stereotype::Configuration => "configuration",
            Stereotype::Controller => "controller",
        }
    }

    /// The `firefly_container::BeanStereotype` variant ident.
    fn variant(self) -> proc_macro2::Ident {
        let name = match self {
            Stereotype::Component => "Component",
            Stereotype::Service => "Service",
            Stereotype::Repository => "Repository",
            Stereotype::Configuration => "Configuration",
            Stereotype::Controller => "Controller",
        };
        proc_macro2::Ident::new(name, proc_macro2::Span::call_site())
    }
}

/// Container-level `#[firefly(...)]` options on a component struct.
#[derive(FromDeriveInput, Default)]
#[darling(attributes(firefly), supports(struct_named, struct_unit), default)]
struct ComponentOpts {
    /// Facade override.
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// `#[firefly(scope = "singleton" | "transient" | "request" | "session")]`.
    scope: Option<String>,
    /// `#[firefly(name = "...")]` — explicit bean name.
    name: Option<String>,
    /// `#[firefly(primary)]` — disambiguates multiple bound implementations.
    primary: bool,
    /// `#[firefly(order = N)]` — initialization / `resolve_all` ordering.
    order: Option<i32>,
    /// `#[firefly(lazy)]` — opt out of eager singleton warm-up.
    lazy: bool,
    /// `#[firefly(profile = "expr")]` — register only for matching profiles.
    profile: Option<String>,
    /// `#[firefly(condition_on_property = "key=value")]`.
    condition_on_property: Option<String>,
    /// `#[firefly(condition_on_class = "label")]`.
    condition_on_class: Option<String>,
    /// `#[firefly(condition_on_bean = "Type")]`.
    condition_on_bean: Option<String>,
    /// `#[firefly(condition_on_missing_bean = "Type")]`.
    condition_on_missing_bean: Option<String>,
    /// `#[firefly(condition_on_single_candidate = "Type")]`.
    condition_on_single_candidate: Option<String>,
    /// `#[firefly(provides = "dyn SomePort")]` — also bind the trait object.
    provides: Option<String>,
    /// `#[firefly(post_construct = "method")]` — call after construction.
    post_construct: Option<String>,
    /// `#[firefly(pre_destroy = "method")]` — call on container shutdown.
    pre_destroy: Option<String>,
}

/// A parsed component field: an `#[autowired]` dependency, a
/// `#[firefly(value = "${...}")]` config injection, or a `Default`-built field.
struct FieldPlan {
    ident: syn::Ident,
    ty: syn::Type,
    kind: FieldKind,
}

enum FieldKind {
    /// `Default::default()`.
    Default,
    /// `#[autowired]` — resolved from the container.
    Autowired { qualifier: Option<String> },
    /// `#[firefly(value = "expr")]` — resolved from config at construction.
    Value { expr: String },
}

/// The injection shape inferred from an `#[autowired]` field's type.
enum AutowiredShape<'a> {
    /// `Arc<T>` → `resolve::<T>()` (or `resolve_named` with a qualifier).
    Single(&'a Type),
    /// `Vec<Arc<T>>` → `resolve_all::<T>()`.
    All(&'a Type),
    /// `Option<Arc<T>>` → `resolve::<T>().ok()` (the `required=false` analog).
    Optional(&'a Type),
    /// `Provider<T>` → `container.provider::<T>()`.
    Provider(&'a Type),
}

/// Expands `#[derive(Component)]` / `Service` / `Repository` / `Configuration`
/// / `Controller`.
pub(crate) fn derive_component(
    input: DeriveInput,
    stereotype: Stereotype,
) -> syn::Result<TokenStream> {
    let opts = ComponentOpts::from_derive_input(&input).map_err(syn::Error::from)?;
    let facade = facade_from_override(&opts.krate)?;
    let container = facade.container();
    let ident = &input.ident;
    let generics = &input.generics;
    let (impl_g, ty_g, where_g) = generics.split_for_impl();
    let is_generic = !generics.params.is_empty();

    let fields = collect_fields(&input)?;

    // Build the field initialisers for the factory closure. A `Provider<T>`
    // field needs the container as an `Arc` so the closure receives `__c`
    // wrapped — but `register_factory_*` hands a `&Container`. We resolve
    // providers through a freshly-cloned `Arc` only when one is present; since
    // we cannot clone a borrowed `&Container` into an `Arc`, a `Provider<T>`
    // field uses `Container::provider_for` on the borrowed container, which we
    // expose below via a small bridge: `#container::Provider::new` requires an
    // `Arc<Container>`. To keep the borrowed-factory signature, we resolve the
    // provider lazily through the container reference captured at build time.
    let mut inits = Vec::new();
    for field in &fields {
        let fident = &field.ident;
        let fty = &field.ty;
        let init = match &field.kind {
            FieldKind::Default => quote! { #fident: ::core::default::Default::default() },
            FieldKind::Value { expr } => {
                let expr_lit = expr.as_str();
                quote! {
                    #fident: #container::resolve_value::<#fty>(__c, #expr_lit)?
                }
            }
            FieldKind::Autowired { qualifier } => {
                let shape = autowired_shape(fty).ok_or_else(|| {
                    syn::Error::new_spanned(
                        fty,
                        "#[autowired] fields must be `Arc<T>`, `Vec<Arc<T>>`, \
                         `Option<Arc<T>>`, or `Provider<T>` — the container resolves \
                         beans as `Arc<T>`",
                    )
                })?;
                autowired_init(&container, fident, &shape, qualifier.as_deref())
            }
        };
        inits.push(init);
    }

    let scope_tokens = scope_tokens(&container, opts.scope.as_deref(), ident)?;
    // `#[firefly(lazy)]` is accepted for Spring parity but is a no-op in Rust:
    // the container already creates singletons lazily on first resolve (there
    // is no eager-init pass to opt out of). Touch it so the field is "used".
    let _lazy = opts.lazy;
    let bean_name = opts.name.clone().unwrap_or_default();
    let primary = opts.primary;
    let order: i32 = opts.order.unwrap_or(0);
    let stereo_variant = stereotype.variant();
    let stereo_label = stereotype.label();

    // Optional interface auto-bind (`#[firefly(provides = "dyn Port")]`).
    let bind_tokens = match &opts.provides {
        Some(spec) if !spec.is_empty() => {
            let iface: syn::Type = syn::parse_str(spec).map_err(|e| {
                syn::Error::new_spanned(
                    ident,
                    format!("#[firefly(provides = {spec:?})] is not a valid type: {e}"),
                )
            })?;
            quote! {
                __container.bind::<#iface, #ident #ty_g>(|__impl_arc| __impl_arc);
            }
        }
        _ => quote! {},
    };

    // Conditions vector (profiles + conditional_on_*).
    let conditions = build_conditions(&container, &opts);

    // `#[post_construct]`: call the named method after building the struct,
    // before the bean is cached. Mirrors pyfly's `_call_post_construct`. The
    // method may take `&self` or `&mut self`, so the binding is `mut` only when
    // a post-construct hook is present (avoids an unused-`mut` lint otherwise).
    let (built_binding, post_construct) =
        match opts.post_construct.as_deref().filter(|s| !s.is_empty()) {
            Some(method) => {
                let m = syn::Ident::new(method, proc_macro2::Span::call_site());
                (quote! { let mut __built }, quote! { __built.#m(); })
            }
            None => (quote! { let __built }, quote! {}),
        };

    // `#[pre_destroy]`: register a teardown hook that calls the named method on
    // the shared instance at `Container::destroy()`. Mirrors `_call_pre_destroy`.
    let pre_destroy = match opts.pre_destroy.as_deref().filter(|s| !s.is_empty()) {
        Some(method) => {
            let m = syn::Ident::new(method, proc_macro2::Span::call_site());
            quote! {
                __container.set_destroy_hook::<#ident #ty_g, _>(|__bean| {
                    __bean.#m();
                });
            }
        }
        None => quote! {},
    };

    let register_doc = format!(
        "Registers this {stereo_label} on the container, resolving every \
         `#[autowired]` field via `Container::resolve` and every \
         `#[firefly(value = ...)]` field from config. Generated by the \
         stereotype derive."
    );

    // The `firefly_register` method always exists; it performs registration,
    // records the stereotype, and applies interface auto-binding.
    let register_method = quote! {
        impl #impl_g #ident #ty_g #where_g {
            #[doc = #register_doc]
            pub fn firefly_register(__container: &#container::Container) {
                __container.register_factory_with::<#ident #ty_g, _>(
                    #scope_tokens,
                    #bean_name,
                    #primary,
                    #order,
                    |__c: &#container::Container| {
                        #built_binding = #ident { #(#inits),* };
                        #post_construct
                        ::core::result::Result::Ok(__built)
                    },
                );
                __container.set_stereotype::<#ident #ty_g>(#stereo_label);
                #pre_destroy
                #bind_tokens
            }
        }
    };

    // Non-generic types also submit an inventory thunk so `Container::scan()`
    // discovers them. Generic types cannot be inventoried (the concrete
    // monomorphization is chosen at the use site), so they rely on the
    // `register_all!` / explicit `firefly_register` fallback — see the macro's
    // rustdoc.
    let inventory_tokens = if is_generic {
        quote! {}
    } else {
        let type_name_lit = ident.to_string();
        quote! {
            #container::inventory::submit! {
                #container::ComponentRegistration {
                    type_name: #type_name_lit,
                    bean_name: #bean_name,
                    stereotype: #container::BeanStereotype::#stereo_variant,
                    scope: #scope_tokens,
                    primary: #primary,
                    order: #order,
                    register: <#ident>::firefly_register,
                    conditions: || #conditions,
                }
            }
        }
    };

    Ok(quote! {
        #register_method
        #inventory_tokens
    })
}

/// Resolve the `Scope` token from the option string.
fn scope_tokens(
    container: &TokenStream,
    scope: Option<&str>,
    ident: &syn::Ident,
) -> syn::Result<TokenStream> {
    Ok(match scope {
        None | Some("singleton") | Some("Singleton") => quote!(#container::Scope::Singleton),
        Some("transient") | Some("Transient") | Some("prototype") | Some("Prototype") => {
            quote!(#container::Scope::Transient)
        }
        Some("request") | Some("Request") => quote!(#container::Scope::Request),
        Some("session") | Some("Session") => quote!(#container::Scope::Session),
        Some(other) => {
            return Err(syn::Error::new_spanned(
                ident,
                format!("unknown scope {other:?}; use singleton | transient | request | session"),
            ));
        }
    })
}

/// Build the `Vec<Condition>` literal for the inventory thunk.
fn build_conditions(container: &TokenStream, opts: &ComponentOpts) -> TokenStream {
    let mut entries: Vec<TokenStream> = Vec::new();
    if let Some(p) = opts.profile.as_deref().filter(|s| !s.is_empty()) {
        entries.push(quote!(#container::Condition::Profile(#p.to_string())));
    }
    if let Some(p) = opts
        .condition_on_property
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        entries.push(quote!(#container::Condition::on_property(#p)));
    }
    if let Some(c) = opts.condition_on_class.as_deref().filter(|s| !s.is_empty()) {
        entries.push(quote!(#container::Condition::OnClass(#c.to_string())));
    }
    if let Some(b) = opts.condition_on_bean.as_deref().filter(|s| !s.is_empty()) {
        entries.push(quote!(#container::Condition::OnBean(#b.to_string())));
    }
    if let Some(b) = opts
        .condition_on_missing_bean
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        entries.push(quote!(#container::Condition::OnMissingBean(#b.to_string())));
    }
    if let Some(b) = opts
        .condition_on_single_candidate
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        entries.push(quote!(#container::Condition::OnSingleCandidate(#b.to_string())));
    }
    quote!(::std::vec![#(#entries),*])
}

/// Generate the initializer expression for an `#[autowired]` field.
fn autowired_init(
    container: &TokenStream,
    fident: &syn::Ident,
    shape: &AutowiredShape,
    qualifier: Option<&str>,
) -> TokenStream {
    match shape {
        AutowiredShape::Single(inner) => match qualifier {
            Some(q) if !q.is_empty() => {
                quote! { #fident: #container::Container::resolve_named::<#inner>(__c, #q)? }
            }
            _ => quote! { #fident: #container::Container::resolve::<#inner>(__c)? },
        },
        AutowiredShape::All(inner) => {
            quote! { #fident: #container::Container::resolve_all::<#inner>(__c)? }
        }
        AutowiredShape::Optional(inner) => {
            quote! { #fident: #container::Container::resolve::<#inner>(__c).ok() }
        }
        AutowiredShape::Provider(inner) => {
            // `Provider<T>` needs an `Arc<Container>`. We obtain one without
            // changing the borrowed-factory signature by going through the
            // container's `provider_for` helper, which clones its own `Arc`.
            quote! { #fident: #container::Container::provider_for::<#inner>(__c) }
        }
    }
}

/// Collect a struct's fields into [`FieldPlan`]s, classifying each as a
/// `#[autowired]` dependency, a `#[firefly(value = ...)]` config injection, or
/// a `Default`-built field.
fn collect_fields(input: &DeriveInput) -> syn::Result<Vec<FieldPlan>> {
    let data = match &input.data {
        syn::Data::Struct(s) => s,
        _ => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "stereotype derives support only structs",
            ))
        }
    };
    let mut plans = Vec::new();
    for field in &data.fields {
        let Some(ident) = field.ident.clone() else {
            // A unit struct has no fields; tuple structs are unsupported.
            if matches!(data.fields, syn::Fields::Unit) {
                continue;
            }
            return Err(syn::Error::new_spanned(
                &input.ident,
                "stereotype derives support only structs with named fields (or a unit struct)",
            ));
        };
        let autowired = field.attrs.iter().any(|a| a.path().is_ident("autowired"));
        let qualifier = parse_field_qualifier(field)?;
        let value_expr = parse_field_value(field)?;

        let kind = if let Some(expr) = value_expr {
            FieldKind::Value { expr }
        } else if autowired || qualifier.is_some() {
            FieldKind::Autowired { qualifier }
        } else {
            FieldKind::Default
        };
        plans.push(FieldPlan {
            ident,
            ty: field.ty.clone(),
            kind,
        });
    }
    Ok(plans)
}

/// Extract `#[firefly(qualifier = "name")]` (also accepted as
/// `#[autowired(qualifier = "name")]`) from a field.
fn parse_field_qualifier(field: &syn::Field) -> syn::Result<Option<String>> {
    parse_field_str_arg(field, "qualifier")
}

/// Extract `#[firefly(value = "${...}")]` from a field.
fn parse_field_value(field: &syn::Field) -> syn::Result<Option<String>> {
    parse_field_str_arg(field, "value")
}

/// Parse a `#[firefly(<key> = "...")]` or `#[autowired(<key> = "...")]`
/// string-valued argument off a field.
fn parse_field_str_arg(field: &syn::Field, key: &str) -> syn::Result<Option<String>> {
    let mut found = None;
    for attr in &field.attrs {
        if !(attr.path().is_ident("firefly") || attr.path().is_ident("autowired")) {
            continue;
        }
        // `#[autowired]` (bare path) carries no args.
        if matches!(attr.meta, syn::Meta::Path(_)) {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                found = Some(lit.value());
            } else {
                // Ignore other nested keys (e.g. a `qualifier` while scanning
                // for `value`); consume any `= ...` so parsing succeeds.
                if meta.input.peek(syn::Token![=]) {
                    let value = meta.value()?;
                    let _: syn::Lit = value.parse()?;
                }
            }
            Ok(())
        })?;
    }
    Ok(found)
}

/// Infer the injection shape from an `#[autowired]` field's type.
fn autowired_shape(ty: &syn::Type) -> Option<AutowiredShape<'_>> {
    if let Some(inner) = arc_inner(ty) {
        return Some(AutowiredShape::Single(inner));
    }
    if let Some(inner) = generic_single(ty, "Vec") {
        // `Vec<Arc<T>>` — peel the Arc.
        return arc_inner(inner).map(AutowiredShape::All);
    }
    if let Some(inner) = generic_single(ty, "Option") {
        return arc_inner(inner).map(AutowiredShape::Optional);
    }
    if let Some(inner) = generic_single(ty, "Provider") {
        return Some(AutowiredShape::Provider(inner));
    }
    None
}

/// Returns the inner `T` of an `Arc<T>` field type, or `None`.
fn arc_inner(ty: &syn::Type) -> Option<&syn::Type> {
    generic_single(ty, "Arc")
}

/// Returns the single generic argument of a `Wrapper<T>` whose last path
/// segment ident is `wrapper`, or `None`.
fn generic_single<'a>(ty: &'a syn::Type, wrapper: &str) -> Option<&'a syn::Type> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != wrapper {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|a| match a {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Expands `register_all!(container, [TypeA, TypeB, ...])`.
///
/// Rust has no global bean scan for *generic* types (use `Container::scan()`
/// for non-generic stereotypes), so this explicit-list spelling calls each
/// type's generated `firefly_register` in order.
pub(crate) fn register_all(input: RegisterAllInput) -> TokenStream {
    let container = input.container;
    let types = input.types;
    let calls = types
        .iter()
        .map(|ty| quote!(<#ty>::firefly_register(&#container);));
    quote! {{
        #(#calls)*
    }}
}

/// Parsed form of `register_all!(container, [Ty, Ty, ...])`.
pub(crate) struct RegisterAllInput {
    container: syn::Expr,
    types: Vec<syn::Type>,
}

impl syn::parse::Parse for RegisterAllInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let container: syn::Expr = input.parse()?;
        let _: syn::Token![,] = input.parse()?;
        let content;
        syn::bracketed!(content in input);
        let punct =
            syn::punctuated::Punctuated::<syn::Type, syn::Token![,]>::parse_terminated(&content)?;
        Ok(RegisterAllInput {
            container,
            types: punct.into_iter().collect(),
        })
    }
}
