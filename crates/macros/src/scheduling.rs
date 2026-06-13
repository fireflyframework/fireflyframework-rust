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

//! The `#[scheduled(...)]` attribute macro.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::ItemFn;

use crate::common::{facade_from_override, parse_duration};

/// Arguments accepted by `#[scheduled(...)]`. Mirrors pyfly's
/// `@scheduled(cron=/fixed_rate=/fixed_delay=/initial_delay=/zone=)`.
#[derive(FromMeta, Default)]
#[darling(default)]
struct ScheduledArgs {
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// A 5- or 6-field cron expression.
    cron: Option<String>,
    /// A fixed-rate interval (anchored at startup), e.g. `"30s"`.
    fixed_rate: Option<String>,
    /// A fixed delay measured from the previous run's completion, e.g. `"5s"`.
    fixed_delay: Option<String>,
    /// An optional delay before the first firing, e.g. `"10s"`.
    initial_delay: Option<String>,
    /// An IANA time zone for `cron` evaluation, e.g. `"America/New_York"`.
    zone: Option<String>,
    /// Override the generated helper name (default `schedule_<fn>`).
    register: Option<String>,
}

/// Expands `#[scheduled(...)]` on an `async fn() -> Result<(), E>`.
///
/// A `schedule_<name>(scheduler)` helper is generated that registers the fn
/// against the matching [`Scheduler`] trigger. Exactly one of `cron`,
/// `fixed_rate`, or `fixed_delay` must be present — a violation is a compile
/// error (strictly better than pyfly's runtime `ValueError`).
pub(crate) fn scheduled_attr(args: TokenStream, item: ItemFn) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = ScheduledArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let scheduling = facade.scheduling();

    let fn_ident = &item.sig.ident;
    if item.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "#[scheduled] requires an `async fn`",
        ));
    }
    if !item.sig.inputs.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.sig.inputs,
            "#[scheduled] requires a zero-argument `async fn` (the framework calls it on a tick)",
        ));
    }

    // Enforce exactly-one-trigger at expansion time.
    let triggers = [
        args.cron.is_some(),
        args.fixed_rate.is_some(),
        args.fixed_delay.is_some(),
    ]
    .iter()
    .filter(|p| **p)
    .count();
    if triggers != 1 {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "#[scheduled] needs exactly one trigger: cron, fixed_rate, or fixed_delay",
        ));
    }
    if args.zone.is_some() && args.cron.is_none() {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "#[scheduled(zone = ...)] is only valid with `cron`",
        ));
    }

    let task_name = fn_ident.to_string();
    let register_ident = match &args.register {
        Some(name) => format_ident!("{}", name),
        None => format_ident!("schedule_{}", fn_ident),
    };
    let vis = &item.vis;

    // The closure the scheduler runs maps the user's `Result<(), E>` onto the
    // scheduler's `TaskError` (a boxed `std::error::Error`). The user's error
    // only needs to be `Display`, which we box behind a string so any error
    // type works without extra bounds.
    let body = quote! {
        move || async move {
            #fn_ident()
                .await
                .map_err(|__e| -> #scheduling::TaskError {
                    ::std::boxed::Box::<dyn ::std::error::Error + ::std::marker::Send + ::std::marker::Sync>::from(
                        ::std::format!("{}", __e)
                    )
                })
        }
    };

    let initial_delay = match &args.initial_delay {
        Some(spec) if !spec.is_empty() => Some(parse_duration(spec, fn_ident.span())?),
        _ => None,
    };

    let register_body = if let Some(cron) = &args.cron {
        match &args.zone {
            Some(zone) => quote! {
                __scheduler.cron_in_zone(#task_name, #cron, #zone, #body)
                    .expect("firefly-macros: invalid #[scheduled(cron=..., zone=...)]");
            },
            None => quote! {
                __scheduler.cron(#task_name, #cron, #body)
                    .expect("firefly-macros: invalid #[scheduled(cron=...)] expression");
            },
        }
    } else if let Some(rate) = &args.fixed_rate {
        let dur = parse_duration(rate, fn_ident.span())?;
        match &initial_delay {
            Some(id) => quote! {
                __scheduler.fixed_rate_with_initial_delay(#task_name, #dur, #id, #body);
            },
            None => quote! {
                __scheduler.fixed_rate(#task_name, #dur, #body);
            },
        }
    } else {
        // fixed_delay
        let spec = args.fixed_delay.as_ref().expect("trigger count checked");
        let dur = parse_duration(spec, fn_ident.span())?;
        match &initial_delay {
            Some(id) => quote! {
                __scheduler.fixed_delay_with_initial_delay(#task_name, #dur, #id, #body);
            },
            None => quote! {
                __scheduler.fixed_delay(#task_name, #dur, #body);
            },
        }
    };

    let doc = format!(
        "Registers [`{fn}`] on the given scheduler under the task name \
         `\"{fn}\"`. Generated by `#[scheduled]`.",
        fn = fn_ident
    );

    Ok(quote! {
        #item

        #[doc = #doc]
        #vis fn #register_ident(__scheduler: &#scheduling::Scheduler) {
            #register_body
        }
    })
}
