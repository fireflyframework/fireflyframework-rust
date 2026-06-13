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
//! Applied to an `impl` block of a `#[derive(Configuration)]` type. Every
//! method inside the block carrying a `#[bean(...)]` marker becomes a bean
//! factory keyed by its return type — the Rust analog of pyfly's
//! `_process_configurations`, which resolves the configuration bean, calls each
//! `@bean` method, and registers the result by its concrete return type.
//!
//! The macro generates a `firefly_register_beans(&Container)` associated
//! function that resolves the configuration bean and registers each factory.
//! Each factory's body calls the original method; method *arguments* are
//! resolved from the container (constructor injection), so a `@bean` method can
//! depend on other beans.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, ImplItem, ItemImpl, ReturnType};

use crate::common::facade_from_override;

/// One discovered `#[bean]` factory method.
struct BeanMethod {
    ident: syn::Ident,
    return_ty: syn::Type,
    /// Resolve expressions for each method argument (constructor injection).
    arg_resolvers: Vec<TokenStream>,
    name: String,
    scope: Option<String>,
    primary: bool,
    profile: Option<String>,
}

/// Per-method `#[bean(name = "...", scope = "...", primary, profile = "...")]`
/// options.
#[derive(Default)]
struct BeanArgs {
    name: Option<String>,
    scope: Option<String>,
    primary: bool,
    profile: Option<String>,
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
                    arg_resolvers.push(quote! {
                        #container::Container::resolve::<#inner>(__c)?
                    });
                }
            }
        }

        let name = bean_args
            .name
            .unwrap_or_else(|| method.sig.ident.to_string());
        methods.push(BeanMethod {
            ident: method.sig.ident.clone(),
            return_ty,
            arg_resolvers,
            name,
            scope: bean_args.scope,
            primary: bean_args.primary,
            profile: bean_args.profile,
        });
    }

    if methods.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[bean] on an impl block found no `#[bean]`-marked methods inside",
        ));
    }

    // Each factory resolves the configuration holder, then calls the method.
    let registrations = methods.iter().map(|m| {
        let mident = &m.ident;
        let return_ty = &m.return_ty;
        let resolvers = &m.arg_resolvers;
        let name = &m.name;
        let primary = m.primary;
        let scope_tokens = match m.scope.as_deref() {
            None | Some("singleton") | Some("Singleton") => quote!(#container::Scope::Singleton),
            Some("transient") | Some("Transient") | Some("prototype") | Some("Prototype") => {
                quote!(#container::Scope::Transient)
            }
            Some("request") | Some("Request") => quote!(#container::Scope::Request),
            Some("session") | Some("Session") => quote!(#container::Scope::Session),
            Some(_) => quote!(#container::Scope::Singleton),
        };
        // `#[bean(profile = "expr")]`: register the bean only when the active
        // profiles match (Spring `@Bean @Profile`). Evaluated against the
        // container's installed condition context at register time.
        let registration = quote! {
            __container.register_factory_with::<#return_ty, _>(
                #scope_tokens,
                #name,
                #primary,
                0,
                |__c: &#container::Container| {
                    let __cfg = #container::Container::resolve::<#self_ty>(__c)?;
                    ::core::result::Result::Ok(<#self_ty>::#mident(&__cfg, #(#resolvers),*))
                },
            );
            __container.set_stereotype::<#return_ty>("bean");
        };
        match m.profile.as_deref().filter(|s| !s.is_empty()) {
            Some(expr) => quote! {
                if __container.condition_context().accepts_profiles(#expr) {
                    #registration
                }
            },
            None => registration,
        }
    });

    let register_doc = format!(
        "Registers every `#[bean]` factory method on `{}` with the container. \
         Call this from `Container::scan()`/`register_all!` after the \
         configuration holder is registered. Generated by `#[bean]`.",
        quote!(#self_ty)
    );

    Ok(quote! {
        #item

        impl #self_ty {
            #[doc = #register_doc]
            pub fn firefly_register_beans(__container: &#container::Container) {
                #(#registrations)*
            }
        }
    })
}

/// Parse `#[bean(name = "...", scope = "...", primary, profile = "...")]`.
fn parse_bean_args(attr: &syn::Attribute) -> syn::Result<BeanArgs> {
    let mut args = BeanArgs::default();
    // A bare `#[bean]` has no list.
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Ok(args);
    }
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("name") {
            let v: syn::LitStr = meta.value()?.parse()?;
            args.name = Some(v.value());
        } else if meta.path.is_ident("scope") {
            let v: syn::LitStr = meta.value()?.parse()?;
            args.scope = Some(v.value());
        } else if meta.path.is_ident("primary") {
            args.primary = true;
        } else if meta.path.is_ident("profile") {
            let v: syn::LitStr = meta.value()?.parse()?;
            args.profile = Some(v.value());
        } else {
            return Err(
                meta.error("unknown #[bean] argument; use name, scope, primary, or profile")
            );
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
