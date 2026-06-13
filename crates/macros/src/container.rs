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

//! `#[derive(Component)]` (+ `Service` / `Repository` aliases) and the
//! `register_all!` macro for dependency-injection wiring.

use darling::{FromDeriveInput, FromField};
use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

use crate::common::facade_from_override;

/// Which stereotype the derive was invoked as. Only changes the default scope
/// label and documentation; the generated registration is identical.
#[derive(Clone, Copy)]
pub(crate) enum Stereotype {
    Component,
    Service,
    Repository,
}

/// Container-level `#[firefly(...)]` options on a component struct.
#[derive(FromDeriveInput)]
#[darling(attributes(firefly), supports(struct_named, struct_unit))]
struct ComponentOpts {
    ident: syn::Ident,
    generics: syn::Generics,
    data: darling::ast::Data<darling::util::Ignored, ComponentField>,
    /// Facade override.
    #[darling(rename = "crate", default)]
    krate: Option<String>,
    /// `#[firefly(scope = "singleton" | "transient")]`. Defaults to singleton.
    #[darling(default)]
    scope: Option<String>,
    /// `#[firefly(name = "...")]` — explicit bean name.
    #[darling(default)]
    name: Option<String>,
    /// `#[firefly(primary)]` — disambiguates multiple bound implementations.
    #[darling(default)]
    primary: bool,
}

/// Per-field options. A field is constructor-injected (`c.resolve()`) when
/// marked `#[autowired]`; otherwise it is built from `Default`.
#[derive(FromField)]
#[darling(attributes(autowired, firefly))]
struct ComponentField {
    ident: Option<syn::Ident>,
    ty: syn::Type,
    /// Set when the field carried an `#[autowired]` attribute (with or without
    /// a qualifier).
    #[darling(default)]
    qualifier: Option<String>,
    /// Internal marker filled in below: did the field have `#[autowired]`?
    #[darling(skip)]
    autowired: bool,
}

/// Expands `#[derive(Component)]` / `Service` / `Repository`.
pub(crate) fn derive_component(
    input: DeriveInput,
    stereotype: Stereotype,
) -> syn::Result<TokenStream> {
    // darling's `FromField` does not see a bare marker attribute as "present",
    // so detect `#[autowired]` ourselves by scanning the raw fields, then zip.
    let autowired_flags = collect_autowired_flags(&input)?;

    let mut opts = ComponentOpts::from_derive_input(&input).map_err(syn::Error::from)?;
    let facade = facade_from_override(&opts.krate)?;
    let container = facade.container();
    let ident = &opts.ident;
    let (impl_g, ty_g, where_g) = opts.generics.split_for_impl();

    let fields = match &mut opts.data {
        darling::ast::Data::Struct(fields) => &mut fields.fields,
        darling::ast::Data::Enum(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Component)] only supports structs",
            ));
        }
    };
    for (field, present) in fields.iter_mut().zip(autowired_flags.iter()) {
        field.autowired = *present;
    }

    // Build the field initialisers for the factory closure.
    let mut inits = Vec::new();
    for field in fields.iter() {
        let fident = field.ident.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(
                ident,
                "#[derive(Component)] supports only structs with named fields (or a unit struct)",
            )
        })?;
        let fty = &field.ty;
        if field.autowired {
            // The container resolves to `Arc<T>`. An autowired field is
            // therefore expected to be typed `Arc<T>` (or `std::sync::Arc<T>`);
            // we resolve the inner `T` so the returned `Arc<T>` drops straight
            // into the field. A non-`Arc` field is a compile error with a
            // pointed message.
            let inner = arc_inner(fty).ok_or_else(|| {
                syn::Error::new_spanned(
                    fty,
                    "#[autowired] fields must be typed `Arc<T>` — the container resolves \
                     beans as `Arc<T>`",
                )
            })?;
            let resolve = match &field.qualifier {
                Some(q) if !q.is_empty() => quote! {
                    #container::Container::resolve_named::<#inner>(__c, #q)?
                },
                _ => quote! {
                    #container::Container::resolve::<#inner>(__c)?
                },
            };
            inits.push(quote! { #fident: #resolve });
        } else {
            inits.push(quote! { #fident: ::core::default::Default::default() });
        }
    }

    // Resolve the scope token.
    let scope_tokens = match opts.scope.as_deref() {
        None | Some("singleton") | Some("Singleton") => {
            quote!(#container::Scope::Singleton)
        }
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
    };

    let bean_name = opts.name.clone().unwrap_or_default();
    let primary = opts.primary;

    let stereo_label = match stereotype {
        Stereotype::Component => "component",
        Stereotype::Service => "service",
        Stereotype::Repository => "repository",
    };
    let register_doc = format!(
        "Registers this {stereo_label} on the container, constructor-injecting \
         every `#[autowired]` field via `Container::resolve`. Generated by \
         `#[derive(Component)]`."
    );

    Ok(quote! {
        impl #impl_g #ident #ty_g #where_g {
            #[doc = #register_doc]
            pub fn firefly_register(__container: &#container::Container) {
                __container.register_factory_with::<#ident #ty_g, _>(
                    #scope_tokens,
                    #bean_name,
                    #primary,
                    0,
                    |__c: &#container::Container| {
                        ::core::result::Result::Ok(#ident { #(#inits),* })
                    },
                );
            }
        }
    })
}

/// Returns the inner `T` of an `Arc<T>` field type (matching both `Arc<T>` and
/// a fully-qualified `std::sync::Arc<T>` / `::std::sync::Arc<T>`), or `None`
/// when the type is not an `Arc`.
fn arc_inner(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Arc" {
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

/// Scans the original `DeriveInput` to discover which fields carried an
/// `#[autowired]` attribute (darling's marker handling does not surface a bare
/// path attribute as a boolean on `FromField`).
fn collect_autowired_flags(input: &DeriveInput) -> syn::Result<Vec<bool>> {
    let data = match &input.data {
        syn::Data::Struct(s) => s,
        _ => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "#[derive(Component)] only supports structs",
            ))
        }
    };
    Ok(data
        .fields
        .iter()
        .map(|f| f.attrs.iter().any(|a| a.path().is_ident("autowired")))
        .collect())
}

/// Expands `register_all!(container, [TypeA, TypeB, ...])`.
///
/// Rust has no global bean scan, so the framework's explicit-list spelling
/// (recommended by the reanalysis) calls each type's generated
/// `firefly_register` in order.
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
