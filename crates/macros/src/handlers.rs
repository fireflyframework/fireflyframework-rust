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

//! `#[handlers]` — turns an `impl` block of a DI bean into a set of CQRS /
//! EDA handler registrations, the Rust analog of Spring scanning a
//! `@Component`'s `@CommandHandler` / `@QueryHandler` / `@EventListener` methods.
//!
//! The bean is a `#[derive(Service)]` (or any registered bean) whose collaborators
//! are `#[autowired]`. Inside `#[handlers] impl Bean { ... }`, each method marked
//! `#[command_handler]` / `#[query_handler]` (a CQRS message handler) or
//! `#[event_listener("topic")]` (an EDA listener) takes `&self` + one message /
//! event argument. The macro emits a link-time registration thunk that resolves
//! the bean from the container and installs a closure capturing it — so the
//! handler reaches its collaborators through `self.<field>` instead of a
//! process-global. `FireflyApplication` drains these registrations after the
//! container is scanned, exactly like the free-`fn` `#[command_handler]` /
//! `#[event_listener]` discovery.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ItemImpl, ReturnType};

use crate::common::facade_from_override;
use crate::scheduling::{scheduled_register_call, ScheduledArgs};

/// Arguments accepted by `#[handlers(...)]`.
#[derive(FromMeta, Default)]
#[darling(default)]
struct HandlersArgs {
    /// Facade override.
    #[darling(rename = "crate")]
    krate: Option<String>,
}

/// The per-method markers `#[handlers]` consumes.
const MARKERS: &[&str] = &[
    "command_handler",
    "query_handler",
    "event_listener",
    "scheduled",
];

/// Expands `#[handlers]` on a bean `impl` block into the original impl plus a
/// registration thunk for every marked handler / listener method.
pub(crate) fn handlers(args: TokenStream, item: ItemImpl) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = HandlersArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let cqrs = facade.cqrs();
    let eda = facade.eda();
    let scheduling = facade.scheduling();
    let container = facade.container();

    let mut item = item;
    let self_ty = (*item.self_ty).clone();
    let self_name = quote!(#self_ty).to_string();
    let not_registered = format!(
        "#[handlers] {self_name}: the handler bean is not a registered container bean — \
         add #[derive(Service)] (or register it) so it is scanned. Cause: {{}}"
    );

    let mut registrations: Vec<TokenStream> = Vec::new();

    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };

        // Find and strip exactly one handler marker from the method.
        let mut marker: Option<(&'static str, syn::Attribute)> = None;
        let mut kept = Vec::with_capacity(method.attrs.len());
        for attr in std::mem::take(&mut method.attrs) {
            match MARKERS.iter().find(|m| attr.path().is_ident(m)) {
                Some(_) if marker.is_some() => {
                    return Err(syn::Error::new_spanned(
                        &attr,
                        "a #[handlers] method may carry at most one of \
                         #[command_handler] / #[query_handler] / #[event_listener] / #[scheduled]",
                    ));
                }
                Some(m) => marker = Some((m, attr)),
                None => kept.push(attr),
            }
        }
        method.attrs = kept;
        let Some((kind, marker_attr)) = marker else {
            continue;
        };

        // Every handler / listener / task method is `async fn` taking `&self`,
        // returning a `Result<..>`. CQRS/EDA methods take one message / event
        // argument; a scheduled task takes none.
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a #[handlers] handler / listener / task method must be `async fn`",
            ));
        }
        let mut inputs = method.sig.inputs.iter();
        match inputs.next() {
            Some(FnArg::Receiver(recv)) if recv.reference.is_some() => {}
            _ => {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "a #[handlers] handler / listener / task method must take `&self`",
                ));
            }
        }
        if matches!(method.sig.output, ReturnType::Default) {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a #[handlers] handler / listener / task method must return a `Result<..>`",
            ));
        }
        let method_ident = method.sig.ident.clone();

        // The one message / event argument (absent for a scheduled task).
        let msg_ty = match inputs.next() {
            Some(FnArg::Typed(pat)) => Some((*pat.ty).clone()),
            Some(_) => {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "the argument after `&self` must be a typed message / event",
                ));
            }
            None => None,
        };
        if inputs.next().is_some() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a #[handlers] method takes at most one argument after `&self`",
            ));
        }
        let require_msg = || {
            msg_ty.clone().ok_or_else(|| {
                syn::Error::new_spanned(
                    &method.sig,
                    "a #[command_handler] / #[query_handler] / #[event_listener] method takes \
                     exactly one message / event argument after `&self`",
                )
            })
        };

        match kind {
            "scheduled" => {
                if msg_ty.is_some() {
                    return Err(syn::Error::new_spanned(
                        &method.sig,
                        "a #[scheduled] task method takes no argument after `&self`",
                    ));
                }
                // Parse the trigger args from the marker and build the
                // scheduler call over a closure that captures the resolved bean.
                let sched_args = parse_scheduled_args(&marker_attr)?;
                let task_name = method_ident.to_string();
                let body = quote! {
                    move || {
                        let __bean = ::std::sync::Arc::clone(&__bean);
                        async move {
                            __bean.#method_ident().await.map_err(|__e| -> #scheduling::TaskError {
                                ::std::boxed::Box::<dyn ::std::error::Error + ::std::marker::Send + ::std::marker::Sync>::from(
                                    ::std::format!("{}", __e)
                                )
                            })
                        }
                    }
                };
                let register_call =
                    scheduled_register_call(&sched_args, &task_name, &body, method_ident.span())?;
                registrations.push(quote! {
                    #scheduling::inventory::submit! {
                        #scheduling::BeanScheduledRegistration {
                            schedule: |__scheduler: &#scheduling::Scheduler, __c: &#container::Container| {
                                let __bean = #container::Container::resolve::<#self_ty>(__c)
                                    .unwrap_or_else(|__e| ::core::panic!(#not_registered, __e));
                                #register_call
                            }
                        }
                    }
                });
            }
            "command_handler" | "query_handler" => {
                let msg_ty = require_msg()?;
                // Resolve the bean, then register a bus handler capturing it.
                registrations.push(quote! {
                    #cqrs::inventory::submit! {
                        #cqrs::BeanHandlerRegistration {
                            register: |__bus: &#cqrs::Bus, __c: &#container::Container| {
                                let __bean = #container::Container::resolve::<#self_ty>(__c)
                                    .unwrap_or_else(|__e| ::core::panic!(#not_registered, __e));
                                __bus.register(move |__msg: #msg_ty| {
                                    let __bean = ::std::sync::Arc::clone(&__bean);
                                    async move { __bean.#method_ident(__msg).await }
                                });
                            }
                        }
                    }
                });
            }
            "event_listener" => {
                // The method must take the event argument (`#eda::Event`).
                let _ = require_msg()?;
                let (topic, group) = parse_listener(&marker_attr)?;
                let wrapper = format_ident!("__firefly_bean_listener_{}", method_ident);
                let subscribe_call = match &group {
                    Some(group) if !group.is_empty() => quote! {
                        __broker.subscribe_group(#topic, #group, __handler).await
                    },
                    _ => quote! { __broker.subscribe(#topic, __handler).await },
                };
                registrations.push(quote! {
                    #[doc(hidden)]
                    fn #wrapper<'__fa>(
                        __broker: &'__fa dyn #eda::Broker,
                        __c: &'__fa #container::Container,
                    ) -> #eda::BoxSubscribeFuture<'__fa> {
                        let __bean = #container::Container::resolve::<#self_ty>(__c)
                            .unwrap_or_else(|__e| ::core::panic!(#not_registered, __e));
                        ::std::boxed::Box::pin(async move {
                            let __handler: #eda::Handler =
                                #eda::handler(move |__ev: #eda::Event| {
                                    let __bean = ::std::sync::Arc::clone(&__bean);
                                    async move { __bean.#method_ident(__ev).await }
                                });
                            #subscribe_call
                        })
                    }
                    #eda::inventory::submit! {
                        #eda::BeanListenerRegistration { subscribe: #wrapper }
                    }
                });
            }
            _ => unreachable!("MARKERS is the exhaustive set"),
        }
    }

    if registrations.is_empty() {
        return Err(syn::Error::new_spanned(
            &self_ty,
            "#[handlers] found no #[command_handler] / #[query_handler] / \
             #[event_listener] / #[scheduled] methods",
        ));
    }

    Ok(quote! {
        #item

        #(#registrations)*
    })
}

/// Parses the `#[scheduled(...)]` trigger arguments off a marker attribute.
/// (`scheduled_register_call` validates them; a bare `#[scheduled]` with no
/// trigger fails there with a clear message.)
fn parse_scheduled_args(attr: &syn::Attribute) -> syn::Result<ScheduledArgs> {
    match &attr.meta {
        syn::Meta::Path(_) => Ok(ScheduledArgs::default()),
        syn::Meta::List(list) => {
            let nested = NestedMeta::parse_meta_list(list.tokens.clone())?;
            ScheduledArgs::from_list(&nested).map_err(syn::Error::from)
        }
        syn::Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attr,
            "expected #[scheduled(fixed_rate = \"..\")] (or cron / fixed_delay)",
        )),
    }
}

/// Parses the topic (positional or `topic = "..."`) and the optional consumer
/// `group` out of an `#[event_listener(...)]` marker.
fn parse_listener(attr: &syn::Attribute) -> syn::Result<(String, Option<String>)> {
    let err = || {
        syn::Error::new_spanned(
            attr,
            "#[event_listener] needs a topic: #[event_listener(\"topic\")] or \
             #[event_listener(topic = \"topic\", group = \"g\")]",
        )
    };
    let syn::Meta::List(list) = &attr.meta else {
        return Err(err());
    };
    let nested = NestedMeta::parse_meta_list(list.tokens.clone())?;
    let mut topic: Option<String> = None;
    let mut group: Option<String> = None;
    for meta in &nested {
        match meta {
            // Positional `"topic"`.
            NestedMeta::Lit(syn::Lit::Str(s)) if topic.is_none() => topic = Some(s.value()),
            NestedMeta::Meta(syn::Meta::NameValue(nv)) => {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    if nv.path.is_ident("topic") {
                        topic = Some(s.value());
                    } else if nv.path.is_ident("group") {
                        group = Some(s.value());
                    }
                }
            }
            _ => {}
        }
    }
    match topic {
        Some(topic) => Ok((topic, group)),
        None => Err(err()),
    }
}
