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

//! `#[derive(Command)]` / `#[derive(Query)]` and the
//! `#[command_handler]` / `#[query_handler]` attribute macros.

use darling::{ast::NestedMeta, FromDeriveInput, FromField, FromMeta};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, ItemFn, ReturnType};

use crate::common::{facade_from_override, parse_duration};

// ---------------------------------------------------------------------------
// #[derive(Command)] / #[derive(Query)]
// ---------------------------------------------------------------------------

/// Container-level `#[firefly(...)]` options on a `#[derive(Command/Query)]`.
#[derive(FromDeriveInput)]
#[darling(attributes(firefly), supports(struct_any))]
struct MessageDeriveOpts {
    ident: syn::Ident,
    generics: syn::Generics,
    data: darling::ast::Data<darling::util::Ignored, MessageField>,
    /// Facade-crate override: `#[firefly(crate = "...")]`.
    #[darling(rename = "crate", default)]
    krate: Option<String>,
    /// Type-level cache TTL for a query, e.g. `#[firefly(cache_ttl = "60s")]`.
    #[darling(default)]
    cache_ttl: Option<String>,
}

/// Per-field `#[firefly(...)]` options.
#[derive(FromField)]
#[darling(attributes(firefly))]
struct MessageField {
    ident: Option<syn::Ident>,
    ty: syn::Type,
    /// `#[firefly(validate)]` — emit a "field required / non-empty" check for
    /// this field inside the generated `Message::validate`.
    #[darling(default)]
    validate: bool,
    /// `#[firefly(cache_ttl = "30s")]` on a field is rejected (TTL is a
    /// type-level concern); kept so darling does not choke on the attribute.
    #[darling(default)]
    cache_ttl: Option<String>,
}

/// Drives both `#[derive(Command)]` and `#[derive(Query)]`. `is_query` only
/// affects which default cache behaviour is documented; the generated impl is
/// identical because both are dispatched through the same `Bus`.
pub(crate) fn derive_message(input: DeriveInput, is_query: bool) -> syn::Result<TokenStream> {
    let opts = MessageDeriveOpts::from_derive_input(&input).map_err(syn::Error::from)?;
    let facade = facade_from_override(&opts.krate)?;
    let cqrs = facade.cqrs();
    let ident = &opts.ident;
    let (impl_g, ty_g, where_g) = opts.generics.split_for_impl();

    let fields = match &opts.data {
        darling::ast::Data::Struct(fields) => &fields.fields,
        darling::ast::Data::Enum(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Command)]/#[derive(Query)] only supports structs",
            ));
        }
    };

    // Build per-field `validate` checks. A `String` field gets a non-empty
    // check; any other field is checked against `Default` (best-effort, but
    // the common case is string ids / names).
    let mut checks = Vec::new();
    for field in fields {
        if let Some(ttl) = &field.cache_ttl {
            if !ttl.is_empty() {
                return Err(syn::Error::new_spanned(
                    field.ident.as_ref().unwrap_or(ident),
                    "cache_ttl is a type-level option; place it on the struct: \
                     #[firefly(cache_ttl = \"...\")]",
                ));
            }
        }
        if field.validate {
            let fident = field.ident.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(ident, "#[firefly(validate)] needs a named field")
            })?;
            let name = fident.to_string();
            let fty = &field.ty;
            // "required / non-empty" is expressed as "not equal to the type's
            // `Default`": an empty `String`, a zero number, `None`, etc. This
            // needs no runtime support — the field type only has to be
            // `Default + PartialEq`, which the compiler enforces at the call
            // site and which yields a clear diagnostic if it is not. The
            // default is fully qualified (`<Ty as Default>::default()`) so type
            // inference never stalls.
            checks.push(quote! {
                if self.#fident == <#fty as ::core::default::Default>::default() {
                    return ::core::result::Result::Err(
                        #cqrs::CqrsError::validation(
                            ::core::concat!("field `", #name, "` is required")
                        )
                    );
                }
            });
        }
    }

    let validate_impl = if checks.is_empty() {
        quote!()
    } else {
        quote! {
            fn validate(&self) -> ::core::result::Result<(), #cqrs::CqrsError> {
                #(#checks)*
                ::core::result::Result::Ok(())
            }
        }
    };

    let cache_impl = match &opts.cache_ttl {
        Some(ttl) if !ttl.is_empty() => {
            let dur = parse_duration(ttl, ident.span())?;
            quote! {
                fn cache_ttl(&self) -> ::core::option::Option<::core::time::Duration> {
                    ::core::option::Option::Some(#dur)
                }
            }
        }
        _ => quote!(),
    };

    // `#[derive(Query)]` overrides the CQRS kind to Query so the bus can report
    // it under the query registry; `#[derive(Command)]` keeps the default.
    let kind_impl = if is_query {
        quote! {
            fn kind() -> #cqrs::MessageKind {
                #cqrs::MessageKind::Query
            }
        }
    } else {
        quote!()
    };

    Ok(quote! {
        impl #impl_g #cqrs::Message for #ident #ty_g #where_g {
            #kind_impl
            #validate_impl
            #cache_impl
        }
    })
}

// ---------------------------------------------------------------------------
// #[command_handler] / #[query_handler]
// ---------------------------------------------------------------------------

/// Arguments accepted by `#[command_handler(...)]` / `#[query_handler(...)]`.
#[derive(FromMeta, Default)]
#[darling(default)]
struct HandlerArgs {
    /// Facade override.
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// Override the generated registration-fn name (default `register_<fn>`).
    register: Option<String>,
}

/// Expands `#[command_handler]` / `#[query_handler]` on a free `async fn`.
///
/// The annotated fn must take exactly one argument — the message — and return
/// `Result<R, CqrsError>`. A `register_<name>(bus)` helper is generated that
/// installs the fn on a [`Bus`] via `register_with_context`, so the handler
/// type and its registration live together (pyfly's auto-discovery, made an
/// explicit, testable helper in Rust).
pub(crate) fn handler_attr(
    args: TokenStream,
    item: ItemFn,
    _is_query: bool,
) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = HandlerArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let cqrs = facade.cqrs();

    let fn_ident = &item.sig.ident;
    if item.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "#[command_handler]/#[query_handler] requires an `async fn`",
        ));
    }

    // Exactly one (message) argument, by value.
    let inputs: Vec<&syn::FnArg> = item.sig.inputs.iter().collect();
    if inputs.len() != 1 {
        return Err(syn::Error::new_spanned(
            &item.sig.inputs,
            "a command/query handler must take exactly one argument: the message",
        ));
    }
    let msg_ty = match inputs[0] {
        syn::FnArg::Typed(pat) => pat.ty.clone(),
        syn::FnArg::Receiver(_) => {
            return Err(syn::Error::new_spanned(
                inputs[0],
                "free-fn handlers cannot take `self`; put the fn in an `impl` and call \
                 `Self::<fn>` from your own registration, or make it free-standing",
            ));
        }
    };

    // Return type must be a `Result<...>`; we forward it verbatim.
    if matches!(item.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &item.sig,
            "a handler must return `Result<R, CqrsError>`",
        ));
    }

    let register_ident = match &args.register {
        Some(name) => format_ident!("{}", name),
        None => format_ident!("register_{}", fn_ident),
    };
    let vis = &item.vis;
    let doc = format!(
        "Registers [`{fn}`] on the given bus. Generated by `#[command_handler]`/\
         `#[query_handler]`; call it once at startup so dispatching the \
         message type routes to `{fn}`.",
        fn = fn_ident
    );

    Ok(quote! {
        #item

        #[doc = #doc]
        #vis fn #register_ident(bus: &#cqrs::Bus) {
            bus.register(move |__msg: #msg_ty| async move { #fn_ident(__msg).await });
        }
    })
}
