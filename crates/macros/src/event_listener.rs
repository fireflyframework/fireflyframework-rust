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

//! `#[application_event_listener]` / `#[transactional_event_listener]` —
//! declarative in-process event listeners (Spring's `@EventListener` /
//! `@TransactionalEventListener`).
//!
//! These are distinct from the EDA broker listener `#[event_listener("topic")]`
//! (Spring's `@KafkaListener`-style broker subscription): these handle
//! *in-process* domain events published with
//! [`publish_event`](firefly_transactional::publish_event), while the EDA macro
//! subscribes a handler to a message-broker topic.
//!
//! Each macro decorates a free `async fn` taking a single shared-reference
//! parameter — `async fn on_order(event: &OrderPlaced)` — leaves the function
//! itself untouched, and emits an [`inventory`] thunk that registers an erased
//! dispatcher with [`firefly_transactional::register_event_listener`]. The
//! dispatcher downcasts the published event to the parameter type and awaits the
//! handler.
//!
//! `#[event_listener]` registers an **immediate** listener (runs at publish
//! time). `#[event_listener(phase = "after_commit")]` or
//! `#[transactional_event_listener]` (which defaults to `after_commit`) register
//! a **transaction-bound** listener that runs at the named
//! [`TransactionPhase`](firefly_transactional::TransactionPhase) of the
//! surrounding transaction.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, Type};

use crate::common::facade_from_override;

/// Arguments for both listener macros.
#[derive(FromMeta, Default)]
#[darling(default)]
struct ListenerArgs {
    /// Facade override: `#[event_listener(crate = "...")]`.
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// Transaction phase: one of `before_commit`, `after_commit`,
    /// `after_rollback`, `after_completion`. Absent on `#[event_listener]`
    /// means an immediate listener.
    phase: Option<String>,
}

/// Entry point for `#[application_event_listener]` — immediate unless a `phase`
/// is given.
pub(crate) fn application_event_listener_attr(
    args: TokenStream,
    func: ItemFn,
) -> syn::Result<TokenStream> {
    expand(args, func, None)
}

/// Entry point for `#[transactional_event_listener]` — transaction-bound,
/// defaulting to the `after_commit` phase.
pub(crate) fn transactional_event_listener_attr(
    args: TokenStream,
    func: ItemFn,
) -> syn::Result<TokenStream> {
    expand(args, func, Some("after_commit"))
}

fn expand(args: TokenStream, func: ItemFn, default_phase: Option<&str>) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let parsed = ListenerArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&parsed.krate)?;
    let transactional = facade.transactional();

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "event listeners must be an `async fn` — the handler is awaited",
        ));
    }

    let event_ty = event_param_type(&func)?;
    let phase_tokens = phase_tokens(&parsed, default_phase, &transactional, &func)?;

    let fn_ident = &func.sig.ident;

    let registration = quote! {
        #transactional::inventory::submit! {
            #transactional::EventListenerRegistration {
                register: || {
                    #transactional::register_event_listener::<#event_ty>(
                        #phase_tokens,
                        ::std::sync::Arc::new(
                            |__firefly_ev: ::std::sync::Arc<
                                dyn ::core::any::Any + ::core::marker::Send + ::core::marker::Sync,
                            >| {
                                ::std::boxed::Box::pin(async move {
                                    if let ::core::result::Result::Ok(__firefly_typed) =
                                        __firefly_ev.downcast::<#event_ty>()
                                    {
                                        #fn_ident(&*__firefly_typed).await;
                                    }
                                })
                            },
                        ),
                    );
                },
            }
        }
    };

    Ok(quote! {
        #func
        #registration
    })
}

/// Validates the listener signature (a free `async fn` with exactly one
/// `&Event` parameter) and returns the referenced event type `Event`.
fn event_param_type(func: &ItemFn) -> syn::Result<Type> {
    let inputs = &func.sig.inputs;
    if inputs.len() != 1 {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "an event listener takes exactly one parameter: the event by shared \
             reference, e.g. `async fn on_event(event: &OrderPlaced)`",
        ));
    }
    match inputs.first().expect("checked len == 1") {
        FnArg::Receiver(recv) => Err(syn::Error::new_spanned(
            recv,
            "event listeners must be free functions, not methods; take the event \
             by shared reference: `async fn on_event(event: &OrderPlaced)`",
        )),
        FnArg::Typed(pat) => match pat.ty.as_ref() {
            Type::Reference(reference) => Ok((*reference.elem).clone()),
            other => Err(syn::Error::new_spanned(
                other,
                "the event parameter must be a shared reference `&Event` \
                 (the event is shared with every listener)",
            )),
        },
    }
}

/// Resolves the `Option<TransactionPhase>` tokens from the `phase` argument and
/// the macro's default phase.
fn phase_tokens(
    args: &ListenerArgs,
    default_phase: Option<&str>,
    transactional: &TokenStream,
    func: &ItemFn,
) -> syn::Result<TokenStream> {
    let phase = args.phase.as_deref().or(default_phase);
    let Some(phase) = phase else {
        // `#[event_listener]` with no phase: an immediate listener.
        return Ok(quote!(::core::option::Option::None));
    };
    let variant = match phase {
        "before_commit" => quote!(BeforeCommit),
        "after_commit" => quote!(AfterCommit),
        "after_rollback" => quote!(AfterRollback),
        "after_completion" => quote!(AfterCompletion),
        other => {
            return Err(syn::Error::new_spanned(
                &func.sig,
                format!(
                    "unknown transaction phase `{other}`; expected one of \
                     before_commit, after_commit, after_rollback, after_completion"
                ),
            ));
        }
    };
    Ok(quote!(::core::option::Option::Some(#transactional::TransactionPhase::#variant)))
}
