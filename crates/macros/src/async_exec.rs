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

//! The `#[async_method]` attribute — Spring's `@Async`.
//!
//! Rewrites an `async fn name(self: Arc<Self>, args…) -> R` into a **non-async**
//! `fn name(self: Arc<Self>, args…) -> firefly_scheduling::TaskHandle<R>` whose
//! body hands the original body off to a [`TaskExecutor`] via
//! `firefly_scheduling::task_executor().spawn(async move { <body> })`, so the
//! call returns immediately and the work runs on its own tokio task. The caller
//! `.await`s (or `.join()`s) the returned handle for the result.
//!
//! The receiver **must** be `self: Arc<Self>`: the spawned future has to be
//! `'static`, which a `&self`/`self`-by-value receiver cannot guarantee, so the
//! macro requires the `Arc<Self>` form and emits a clear compile error
//! otherwise. The runtime type is routed through the `firefly` facade's
//! `__rt` contract (`facade.scheduling()`), so a one-dependency service compiles
//! the expansion without naming `firefly-scheduling`.

use darling::{ast::NestedMeta, FromMeta};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, ReturnType};

use crate::common::facade_from_override;

/// Per-call `#[async_method(...)]` options.
#[derive(FromMeta, Default)]
#[darling(default)]
struct AsyncMethodArgs {
    /// Facade override for a renamed/shimmed `firefly`.
    #[darling(rename = "crate")]
    krate: Option<String>,
    /// A Rust expression yielding the [`TaskExecutor`] to spawn on, instead of
    /// the process-global `task_executor()`. Evaluated inside the rewritten
    /// (non-async) body before the spawn.
    executor: Option<String>,
}

/// Expands `#[async_method]` / `#[async_method(executor = "…")]` on an
/// `async fn(self: Arc<Self>, …) -> R`.
pub(crate) fn async_method_impl(args: TokenStream, mut func: ItemFn) -> syn::Result<TokenStream> {
    let attr_args = NestedMeta::parse_meta_list(args)?;
    let args = AsyncMethodArgs::from_list(&attr_args).map_err(syn::Error::from)?;
    let facade = facade_from_override(&args.krate)?;
    let scheduling = facade.scheduling();

    // The fn must be `async`: the hand-off wraps its body in an `async move`.
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[async_method] requires an `async fn` — its body becomes the spawned future",
        ));
    }

    // The receiver must be `self: Arc<Self>` so the spawned future is `'static`.
    require_arc_self_receiver(&func)?;

    // The optional `executor = "expr"` parses to a Rust expression evaluated in
    // the rewritten body; default is the process-global `task_executor()`.
    let executor_expr = match &args.executor {
        Some(raw) if !raw.trim().is_empty() => {
            let expr: syn::Expr = syn::parse_str(raw).map_err(|e| {
                syn::Error::new_spanned(
                    &func.sig,
                    format!("#[async_method(executor = \"…\")] is not a valid Rust expression: {e}"),
                )
            })?;
            quote!(#expr)
        }
        _ => quote!(#scheduling::task_executor()),
    };

    // The new (non-async) return type wraps the original output in a
    // `TaskHandle<R>`; a bare `-> ()` becomes `TaskHandle<()>`.
    let output_ty = match &func.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    };

    // Drop `async` and re-point the signature: `fn … -> TaskHandle<R>`.
    func.sig.asyncness = None;
    func.sig.output = syn::parse2(quote!(-> #scheduling::TaskHandle<#output_ty>))?;

    // The original body becomes the spawned future. `__executor` is `__`-prefixed
    // so it never collides with a user binding.
    let block = &func.block;
    let new_block = quote! {
        {
            let __executor = #executor_expr;
            #scheduling::TaskExecutor::spawn(&__executor, async move #block)
        }
    };
    func.block = syn::parse2(new_block)?;

    Ok(quote!(#func))
}

/// Validates that the function's receiver is exactly `self: Arc<Self>`, emitting
/// a guiding compile error for `&self` / `self` / a missing receiver.
fn require_arc_self_receiver(func: &ItemFn) -> syn::Result<()> {
    const HELP: &str = "#[async_method] requires a `self: Arc<Self>` receiver so the spawned \
                        future is `'static`; change the receiver to `self: std::sync::Arc<Self>` \
                        (a `&self`/`self`-by-value receiver cannot be moved into a `'static` task)";

    let Some(first) = func.sig.inputs.first() else {
        return Err(syn::Error::new_spanned(&func.sig, HELP));
    };
    let FnArg::Receiver(receiver) = first else {
        // First parameter is a typed argument, i.e. no receiver at all.
        return Err(syn::Error::new_spanned(&func.sig, HELP));
    };
    // An `Arc<Self>` receiver carries an explicit `ty` and no `&`/`&mut`; a plain
    // `self` / `&self` / `&mut self` does not (or has a reference).
    if receiver.reference.is_some() || !is_arc_self(&receiver.ty) {
        return Err(syn::Error::new_spanned(receiver, HELP));
    }
    Ok(())
}

/// Whether a receiver type is an `Arc<Self>` (matched by the last path segment
/// being `Arc` with a single `Self` type argument — tolerating `std::sync::Arc`,
/// `alloc::sync::Arc`, and a bare `Arc`).
fn is_arc_self(ty: &syn::Type) -> bool {
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    let Some(segment) = type_path.path.segments.last() else {
        return false;
    };
    if segment.ident != "Arc" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(generic) = &segment.arguments else {
        return false;
    };
    matches!(
        generic.args.first(),
        Some(syn::GenericArgument::Type(syn::Type::Path(inner)))
            if inner.path.is_ident("Self")
    )
}
