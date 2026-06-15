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

//! `#[aspect(pointcut = "...", order = N)]` — the declarative aspect macro
//! (Spring's `@Aspect` + advice annotations, pyfly's `@aspect`).
//!
//! Applied to an `impl` block, it reads the advice markers
//! `#[before]` / `#[after]` / `#[after_returning]` / `#[after_throwing]` /
//! `#[around]` on the block's methods, strips them (so the marked methods stay
//! ordinary, callable methods), and generates a
//! `#[async_trait] impl firefly_aop::Aspect for Self` whose hooks delegate to
//! the marked methods. Only the hooks that are present are emitted; the
//! [`Aspect`](firefly_aop::Aspect) trait's no-op/pass-through defaults cover the
//! rest. The macro also emits an [`inventory`](firefly_aop::inventory) thunk that
//! registers the aspect (constructed via `Default`) against the pointcut, so a
//! `#[aspect]`-declared aspect is discovered across the crate graph and woven by
//! [`firefly_aop::advised`] without manual registration.
//!
//! The marked method signatures must match the hook shapes — `before` / `after`
//! / `after_returning` / `after_throwing` take `&self, &JoinPoint` and are
//! `async`; `around` takes `&'a self, &'a JoinPoint, Proceed<'a>` and returns
//! `AdviceFuture<'a>` (non-`async`, like the trait hook). The generated
//! delegation calls these by name, so a mismatched signature surfaces as a clear
//! compiler error at the generated `impl`.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ItemImpl};

use crate::common::facade_from_override;

/// Arguments for `#[aspect(...)]`.
#[derive(FromMeta, Default)]
#[darling(default)]
struct AspectArgs {
    /// Facade override: `#[aspect(crate = "...")]`.
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// The pointcut glob this aspect binds to (required), e.g. `"service.*.*"`.
    pointcut: Option<String>,
    /// The advice order (lower runs first / is outermost); defaults to `0`.
    order: Option<i32>,
}

/// The five advice markers, in the order they are documented and validated.
const MARKERS: &[&str] = &[
    "before",
    "after",
    "after_returning",
    "after_throwing",
    "around",
];

/// The method idents found for each advice hook (each at most once).
#[derive(Default)]
struct Marked {
    before: Option<syn::Ident>,
    after: Option<syn::Ident>,
    after_returning: Option<syn::Ident>,
    after_throwing: Option<syn::Ident>,
    around: Option<syn::Ident>,
}

/// Entry point for `#[aspect]`.
pub(crate) fn aspect_impl(args: TokenStream, mut item: ItemImpl) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let parsed = AspectArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&parsed.krate)?;
    let aop = facade.aop();

    let pointcut = parsed
        .pointcut
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            syn::Error::new_spanned(
                &item.self_ty,
                "#[aspect] requires a `pointcut`, e.g. \
                 #[aspect(pointcut = \"service.*.*\")]",
            )
        })?
        .to_string();
    let order = parsed.order.unwrap_or(0);

    // Pull the advice markers off the methods (stripping them so the user's
    // methods stay callable).
    let marked = collect_markers(&mut item)?;
    if marked.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.self_ty,
            "#[aspect] found no advice markers; mark at least one method with \
             #[before], #[after], #[after_returning], #[after_throwing], or #[around]",
        ));
    }

    let self_ty = &item.self_ty;
    let (impl_generics, _, where_clause) = item.generics.split_for_impl();

    let hooks = generated_hooks(&marked, &aop);
    let registration = registration(self_ty, &pointcut, order, &aop);

    Ok(quote! {
        // (a) The original impl with the advice markers stripped.
        #item

        // (b) The generated Aspect impl delegating to the marked methods.
        #[#aop::async_trait]
        impl #impl_generics #aop::Aspect for #self_ty #where_clause {
            #hooks
        }

        // Registration thunk: discovered across the crate graph and registered
        // into the process-global aspect registry on first weave.
        #registration
    })
}

impl Marked {
    fn is_empty(&self) -> bool {
        self.before.is_none()
            && self.after.is_none()
            && self.after_returning.is_none()
            && self.after_throwing.is_none()
            && self.around.is_none()
    }

    /// Records the method ident for `marker`, erroring on a duplicate.
    fn set(&mut self, marker: &str, ident: syn::Ident, span: &syn::Attribute) -> syn::Result<()> {
        let slot = match marker {
            "before" => &mut self.before,
            "after" => &mut self.after,
            "after_returning" => &mut self.after_returning,
            "after_throwing" => &mut self.after_throwing,
            "around" => &mut self.around,
            _ => unreachable!("marker is checked against MARKERS before set"),
        };
        if slot.is_some() {
            return Err(syn::Error::new_spanned(
                span,
                format!("#[aspect] allows at most one #[{marker}] method per aspect"),
            ));
        }
        *slot = Some(ident);
        Ok(())
    }
}

/// Walks the impl methods, recording (and stripping) every advice marker. A
/// marker on a non-method item, an unknown marker spelled like one, or a marker
/// carrying arguments is a clear compile error.
fn collect_markers(item: &mut ItemImpl) -> syn::Result<Marked> {
    let mut marked = Marked::default();
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            // Reject a stray marker on a const/type/etc. so a misplaced advice
            // attribute is caught instead of silently ignored.
            if let Some(attr) = marker_attr_on_other(impl_item) {
                return Err(syn::Error::new_spanned(
                    attr,
                    "an #[aspect] advice marker must sit on a method",
                ));
            }
            continue;
        };
        let ident = method.sig.ident.clone();
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            match marker_name(&attr) {
                Some(name) => {
                    // A marker takes no arguments — `#[before]`, not
                    // `#[before(...)]`.
                    if !matches!(attr.meta, syn::Meta::Path(_)) {
                        return Err(syn::Error::new_spanned(
                            &attr,
                            format!("#[{name}] takes no arguments"),
                        ));
                    }
                    marked.set(&name, ident.clone(), &attr)?;
                }
                None => kept.push(attr),
            }
        }
        method.attrs = kept;
    }
    Ok(marked)
}

/// The marker name an attribute carries, if it is one of the five advice
/// markers.
fn marker_name(attr: &syn::Attribute) -> Option<String> {
    MARKERS
        .iter()
        .find(|m| attr.path().is_ident(m))
        .map(|m| (*m).to_string())
}

/// Finds an advice marker mistakenly placed on a non-method impl item, so it can
/// be reported rather than ignored.
fn marker_attr_on_other(item: &ImplItem) -> Option<&syn::Attribute> {
    let attrs = match item {
        ImplItem::Const(c) => &c.attrs,
        ImplItem::Type(t) => &t.attrs,
        ImplItem::Macro(m) => &m.attrs,
        _ => return None,
    };
    attrs.iter().find(|a| marker_name(a).is_some())
}

/// Builds the delegating hook bodies for the markers that are present. Only
/// these are emitted; the trait defaults cover the absent ones.
fn generated_hooks(marked: &Marked, aop: &TokenStream) -> TokenStream {
    let mut hooks = TokenStream::new();

    for (slot, name) in [
        (&marked.before, "before"),
        (&marked.after, "after"),
        (&marked.after_returning, "after_returning"),
        (&marked.after_throwing, "after_throwing"),
    ] {
        if let Some(method) = slot {
            // The hook ident is fixed by the `Aspect` trait; span it on the
            // delegating method so a signature mismatch points at the user's fn.
            let hook = syn::Ident::new(name, method.span());
            hooks.extend(quote! {
                async fn #hook(&self, __firefly_jp: &#aop::JoinPoint) {
                    self.#method(__firefly_jp).await
                }
            });
        }
    }

    if let Some(method) = &marked.around {
        hooks.extend(quote! {
            fn around<'__firefly_a>(
                &'__firefly_a self,
                __firefly_jp: &'__firefly_a #aop::JoinPoint,
                __firefly_proceed: #aop::Proceed<'__firefly_a>,
            ) -> #aop::AdviceFuture<'__firefly_a> {
                self.#method(__firefly_jp, __firefly_proceed)
            }
        });
    }

    hooks
}

/// The `inventory::submit!` thunk that registers the aspect (built via
/// `Default`) against the pointcut. The aspect type must be `Default` — Spring
/// aspects are singletons, and the auto-registered aspect is a single instance
/// constructed once via `Default`.
fn registration(
    self_ty: &syn::Type,
    pointcut: &str,
    order: i32,
    aop: &TokenStream,
) -> TokenStream {
    quote! {
        #aop::inventory::submit! {
            #aop::AspectRegistration {
                register: || {
                    #aop::register_aspect(
                        ::std::sync::Arc::new(<#self_ty as ::core::default::Default>::default()),
                        #pointcut,
                        #order,
                    );
                },
            }
        }
    }
}
