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

//! The resilience **decorator** attributes — Resilience4j / Spring-Retry on a
//! method, instead of hand-building a [`firefly_resilience`] primitive at every
//! call site:
//!
//! | macro | Spring / Resilience4j | wraps with |
//! |---|---|---|
//! | `#[retry(...)]`           | `@Retry`          | [`Retry`](firefly_resilience::Retry) |
//! | `#[circuit_breaker(...)]` | `@CircuitBreaker` | [`CircuitBreaker`](firefly_resilience::CircuitBreaker) |
//! | `#[rate_limit(...)]`      | `@RateLimiter`    | [`RateLimiter`](firefly_resilience::RateLimiter) |
//! | `#[bulkhead(...)]`        | `@Bulkhead`       | [`Bulkhead`](firefly_resilience::Bulkhead) |
//! | `#[timeout(...)]`         | `@TimeLimiter`    | [`Timeout`](firefly_resilience::Timeout) |
//!
//! # The contract
//!
//! Each macro decorates an `async fn` returning `Result<T, E>` where the error
//! `E: std::error::Error + Send + Sync + 'static + From<firefly_resilience::ResilienceError>`.
//! The decorator threads the body's own failure through the primitive as
//! [`ResilienceError::Operation`](firefly_resilience::ResilienceError::Operation)
//! and recovers the **original `E`** on the way out (so the caller still sees
//! the domain error), while a short-circuit the primitive itself raises —
//! a timeout, an open circuit, a rate-limit or bulkhead rejection — surfaces
//! through `E::from(ResilienceError)`. The one annotation Spring users expect.
//!
//! # Composition
//!
//! The attributes **stack** (outermost first), exactly like layering
//! `@Retry` over `@CircuitBreaker`:
//!
//! ```ignore
//! #[retry(max_attempts = 3, delay = "50ms")]   // outer: re-runs the guarded call
//! #[circuit_breaker(failure_threshold = 5)]    // inner: trips on a failing dependency
//! async fn call_upstream(&self) -> Result<Quote, IntegrationError> { /* … */ }
//! ```
//!
//! The stateful primitives ([`CircuitBreaker`](firefly_resilience::CircuitBreaker),
//! [`RateLimiter`](firefly_resilience::RateLimiter),
//! [`Bulkhead`](firefly_resilience::Bulkhead)) are held in a function-local
//! `static` so their state (the breaker's failure count, the bucket's tokens,
//! the in-flight permits) is **shared across every call** to the method — the
//! Resilience4j registry-bean semantics. [`Retry`](firefly_resilience::Retry)
//! and [`Timeout`](firefly_resilience::Timeout) are stateless and built fresh.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{GenericArgument, ItemFn, PathArguments, ReturnType, Type};

use crate::common::facade_from_override;

/// Which primitive a decorator builds — drives both the builder tokens and
/// whether the guard needs a shared `static`.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Retry,
    CircuitBreaker,
    RateLimit,
    Bulkhead,
    Timeout,
}

impl Kind {
    fn attr_name(self) -> &'static str {
        match self {
            Kind::Retry => "retry",
            Kind::CircuitBreaker => "circuit_breaker",
            Kind::RateLimit => "rate_limit",
            Kind::Bulkhead => "bulkhead",
            Kind::Timeout => "timeout",
        }
    }

    /// Stateful primitives keep per-method state across calls and must live in a
    /// shared `static`; stateless ones are rebuilt on each call.
    fn is_stateful(self) -> bool {
        matches!(
            self,
            Kind::CircuitBreaker | Kind::RateLimit | Kind::Bulkhead
        )
    }
}

/// Expands one resilience decorator onto an `async fn`.
pub(crate) fn expand(kind: Kind, args: TokenStream, mut func: ItemFn) -> syn::Result<TokenStream> {
    let opts = ResilienceArgs::parse(kind, args)?;
    let facade = facade_from_override(&opts.krate)?;
    let res = facade.resilience();

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            format!(
                "#[{}] requires an `async fn` — the guarded call is awaited",
                kind.attr_name()
            ),
        ));
    }
    let err_ty = result_err_type(kind, &func.sig.output)?;
    // The full `Result<T, E>` return type — used to pin the body's output so its
    // (otherwise free) error type is inferred before `operation()` erases it.
    let ret_ty = match &func.sig.output {
        ReturnType::Type(_, ty) => ty.clone(),
        ReturnType::Default => unreachable!("result_err_type rejects a missing return type"),
    };

    let build = kind.build(&opts, &res)?;
    let block = &func.block;

    // The operation: run the original body, mapping its own `E` failure into the
    // primitive's `ResilienceError::Operation` so a single error type flows
    // through the guard. A non-`move` closure is `Fn` (re-runnable for
    // `#[retry]`) and also satisfies the one-shot `FnOnce` the others want.
    let op = quote! {
        || async {
            let __ff_out: #ret_ty = (async #block).await;
            match __ff_out {
                ::core::result::Result::Ok(__ff_v) => ::core::result::Result::Ok(__ff_v),
                ::core::result::Result::Err(__ff_e) =>
                    ::core::result::Result::Err(#res::ResilienceError::operation(__ff_e)),
            }
        }
    };

    // Recover the original `E` (the body's failure round-trips through
    // `Operation`), or map a primitive short-circuit through `E::from`.
    let reconcile = quote! {
        |__ff_re| match __ff_re {
            #res::ResilienceError::Operation(__ff_b) => match __ff_b.downcast::<#err_ty>() {
                ::core::result::Result::Ok(__ff_inner) => *__ff_inner,
                ::core::result::Result::Err(__ff_b) =>
                    <#err_ty as ::core::convert::From<#res::ResilienceError>>::from(
                        #res::ResilienceError::Operation(__ff_b),
                    ),
            },
            __ff_other =>
                <#err_ty as ::core::convert::From<#res::ResilienceError>>::from(__ff_other),
        }
    };

    // The guard: a fresh primitive (stateless) or a shared, lazily-built
    // function-local `static` (stateful), then `execute(op)`.
    let guarded = if kind.is_stateful() {
        let ty = kind.primitive_type(&res);
        quote! {
            static __FF_GUARD: ::std::sync::OnceLock<#ty> = ::std::sync::OnceLock::new();
            let __ff_guard = __FF_GUARD.get_or_init(|| #build);
            __ff_guard.execute(#op).await.map_err(#reconcile)
        }
    } else {
        quote! {
            let __ff_guard = #build;
            __ff_guard.execute(#op).await.map_err(#reconcile)
        }
    };

    func.block = syn::parse2(quote!({ #guarded }))?;
    Ok(quote!(#func))
}

/// Returns the `E` of an `async fn … -> Result<T, E>` return type.
fn result_err_type(kind: Kind, output: &ReturnType) -> syn::Result<Type> {
    let ty = match output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                output,
                format!(
                    "#[{}] async fn must return `Result<T, E>` where \
                     `E: std::error::Error + Send + Sync + 'static + \
                     From<firefly_resilience::ResilienceError>`",
                    kind.attr_name()
                ),
            ))
        }
    };
    let err = result_args(ty)
        .and_then(|args| args.get(1).cloned())
        .ok_or_else(|| {
            syn::Error::new_spanned(
                ty,
                format!(
                    "#[{}] requires a `Result<T, E>` return type — found `{}`",
                    kind.attr_name(),
                    quote!(#ty)
                ),
            )
        })?;
    Ok(err)
}

/// Extracts the two type arguments of a `Result<T, E>` type path.
fn result_args(ty: &Type) -> Option<Vec<Type>> {
    let Type::Path(tp) = ty else { return None };
    let seg = tp.path.segments.last()?;
    if seg.ident != "Result" {
        return None;
    }
    let PathArguments::AngleBracketed(ab) = &seg.arguments else {
        return None;
    };
    let types: Vec<Type> = ab
        .args
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    (types.len() >= 2).then_some(types)
}

/// The parsed arguments common to every decorator (the union of all keys; each
/// macro accepts only its own subset, validated in [`ResilienceArgs::parse`]).
#[derive(Default)]
struct ResilienceArgs {
    krate: Option<String>,
    // retry
    max_attempts: Option<usize>,
    delay: Option<TokenStream>,
    backoff: Option<f64>,
    max_delay: Option<TokenStream>,
    jitter: Option<f64>,
    // circuit_breaker
    failure_threshold: Option<usize>,
    open_duration: Option<TokenStream>,
    window: Option<TokenStream>,
    failure_rate: Option<f64>,
    window_size: Option<usize>,
    // rate_limit
    rate: Option<f64>,
    burst: Option<usize>,
    // bulkhead
    max_concurrent: Option<usize>,
    // timeout (positional or `duration = "…"`)
    duration: Option<TokenStream>,
}

impl ResilienceArgs {
    fn parse(kind: Kind, args: TokenStream) -> syn::Result<Self> {
        let mut out = ResilienceArgs::default();
        if args.is_empty() {
            return Ok(out);
        }
        // `#[timeout("2s")]` / `#[bulkhead(20)]` accept a single positional
        // value; everything else is `key = value`.
        if let Some(pos) = parse_positional(kind, args.clone(), &mut out)? {
            // A positional value was consumed and there is nothing else.
            if pos {
                return Ok(out);
            }
        }
        let parser = syn::meta::parser(|meta| out.consume(kind, meta));
        syn::parse::Parser::parse2(parser, args)?;
        Ok(out)
    }

    fn consume(&mut self, kind: Kind, meta: syn::meta::ParseNestedMeta) -> syn::Result<()> {
        let p = &meta.path;
        if p.is_ident("crate") {
            self.krate = Some(meta.value()?.parse::<syn::LitStr>()?.value());
        } else if p.is_ident("max_attempts") {
            self.max_attempts = Some(int(&meta)?);
        } else if p.is_ident("delay") {
            self.delay = Some(duration_value(&meta)?);
        } else if p.is_ident("backoff") {
            self.backoff = Some(float(&meta)?);
        } else if p.is_ident("max_delay") {
            self.max_delay = Some(duration_value(&meta)?);
        } else if p.is_ident("jitter") {
            self.jitter = Some(float(&meta)?);
        } else if p.is_ident("failure_threshold") {
            self.failure_threshold = Some(int(&meta)?);
        } else if p.is_ident("open_duration") {
            self.open_duration = Some(duration_value(&meta)?);
        } else if p.is_ident("window") {
            self.window = Some(duration_value(&meta)?);
        } else if p.is_ident("failure_rate") {
            self.failure_rate = Some(float(&meta)?);
        } else if p.is_ident("window_size") {
            self.window_size = Some(int(&meta)?);
        } else if p.is_ident("rate") {
            self.rate = Some(float(&meta)?);
        } else if p.is_ident("burst") {
            self.burst = Some(int(&meta)?);
        } else if p.is_ident("max_concurrent") {
            self.max_concurrent = Some(int(&meta)?);
        } else if p.is_ident("duration") || p.is_ident("budget") {
            self.duration = Some(duration_value(&meta)?);
        } else {
            return Err(meta.error(format!(
                "unknown #[{}] argument `{}`",
                kind.attr_name(),
                quote!(#p)
            )));
        }
        Ok(())
    }
}

/// Pull a single positional literal (`#[timeout("2s")]`, `#[bulkhead(20)]`,
/// `#[rate_limit(100.0)]`) when the attribute body is exactly one literal.
/// Returns `Ok(Some(true))` when one was consumed and the body is exhausted.
fn parse_positional(
    kind: Kind,
    args: TokenStream,
    out: &mut ResilienceArgs,
) -> syn::Result<Option<bool>> {
    use syn::Lit;
    // Try to parse the entire body as a single literal; if it has commas or
    // `=`, this fails and we fall back to the keyed parser.
    let lit: Lit = match syn::parse2(args) {
        Ok(l) => l,
        Err(_) => return Ok(None),
    };
    match kind {
        Kind::Timeout => {
            out.duration = Some(lit_to_duration(&lit)?);
            Ok(Some(true))
        }
        Kind::Bulkhead => {
            out.max_concurrent = Some(lit_to_usize(&lit)?);
            Ok(Some(true))
        }
        _ => Err(syn::Error::new_spanned(
            lit,
            format!(
                "#[{}] does not take a positional argument; use `key = value`",
                kind.attr_name()
            ),
        )),
    }
}

// ---- builder dispatch ------------------------------------------------------

impl Kind {
    /// The concrete primitive type — the `static` slot of a stateful guard.
    fn primitive_type(self, res: &TokenStream) -> TokenStream {
        match self {
            Kind::CircuitBreaker => quote!(#res::CircuitBreaker),
            Kind::RateLimit => quote!(#res::RateLimiter),
            Kind::Bulkhead => quote!(#res::Bulkhead),
            // Stateless primitives are built fresh and never reach this slot.
            Kind::Retry => quote!(#res::Retry),
            Kind::Timeout => quote!(#res::Timeout),
        }
    }

    /// The `Primitive::new(...)` / fluent-builder tokens for this decorator.
    fn build(self, a: &ResilienceArgs, res: &TokenStream) -> syn::Result<TokenStream> {
        Ok(match self {
            Kind::Retry => {
                let mut b = quote!(#res::Retry::new());
                if let Some(n) = a.max_attempts {
                    b = quote!(#b.max_attempts(#n));
                }
                if let Some(d) = &a.delay {
                    b = quote!(#b.delay(#d));
                }
                if let Some(f) = a.backoff {
                    b = quote!(#b.backoff(#f));
                }
                if let Some(d) = &a.max_delay {
                    b = quote!(#b.max_delay(#d));
                }
                if let Some(j) = a.jitter {
                    b = quote!(#b.jitter(#j));
                }
                b
            }
            Kind::Timeout => {
                let d = a.duration.clone().ok_or_else(|| {
                    syn::Error::new(
                        proc_macro2::Span::call_site(),
                        "#[timeout] needs a budget, e.g. #[timeout(\"2s\")]",
                    )
                })?;
                quote!(#res::Timeout::new(#d))
            }
            Kind::CircuitBreaker => {
                // Override only the fields the attribute set, keeping every other
                // `CircuitConfig` knob (clock, half-open budget, …) at its
                // Resilience4j-matching default.
                let mut overrides: Vec<TokenStream> = Vec::new();
                if let Some(n) = a.failure_threshold {
                    overrides.push(quote!(__ff_cfg.failure_threshold = #n;));
                }
                if let Some(d) = &a.open_duration {
                    overrides.push(quote!(__ff_cfg.open_duration = #d;));
                }
                if let Some(d) = &a.window {
                    overrides.push(quote!(__ff_cfg.window = #d;));
                }
                if let Some(r) = a.failure_rate {
                    overrides.push(
                        quote!(__ff_cfg.failure_rate_threshold = ::core::option::Option::Some(#r);),
                    );
                }
                if let Some(n) = a.window_size {
                    overrides.push(quote!(__ff_cfg.window_size = #n;));
                }
                quote! {
                    {
                        let mut __ff_cfg = <#res::CircuitConfig as ::core::default::Default>::default();
                        #(#overrides)*
                        #res::CircuitBreaker::new(__ff_cfg)
                    }
                }
            }
            Kind::RateLimit => {
                let rate = a.rate.ok_or_else(|| {
                    syn::Error::new(
                        proc_macro2::Span::call_site(),
                        "#[rate_limit] needs `rate = <per-second>`, e.g. \
                         #[rate_limit(rate = 100.0, burst = 20)]",
                    )
                })?;
                let burst = a.burst.unwrap_or(1);
                quote!(#res::RateLimiter::new(#rate, #burst))
            }
            Kind::Bulkhead => {
                let max = a.max_concurrent.ok_or_else(|| {
                    syn::Error::new(
                        proc_macro2::Span::call_site(),
                        "#[bulkhead] needs a max concurrency, e.g. #[bulkhead(20)]",
                    )
                })?;
                quote!(#res::Bulkhead::new(#max))
            }
        })
    }
}

// ---- small literal helpers -------------------------------------------------

fn int(meta: &syn::meta::ParseNestedMeta) -> syn::Result<usize> {
    meta.value()?.parse::<syn::LitInt>()?.base10_parse()
}

fn float(meta: &syn::meta::ParseNestedMeta) -> syn::Result<f64> {
    let v = meta.value()?;
    if let Ok(f) = v.parse::<syn::LitFloat>() {
        return f.base10_parse();
    }
    // Accept an integer literal where a float is expected (`backoff = 2`).
    let i: syn::LitInt = v.parse()?;
    Ok(i.base10_parse::<i64>()? as f64)
}

/// Parses a duration value: a string with a unit suffix (`"100ms"`, `"2s"`,
/// `"1m"`) or a bare integer (milliseconds).
fn duration_value(meta: &syn::meta::ParseNestedMeta) -> syn::Result<TokenStream> {
    let v = meta.value()?;
    let lit: syn::Lit = v.parse()?;
    lit_to_duration(&lit)
}

fn lit_to_duration(lit: &syn::Lit) -> syn::Result<TokenStream> {
    match lit {
        syn::Lit::Str(s) => parse_duration_str(&s.value(), s.span()),
        syn::Lit::Int(i) => {
            let ms: u64 = i.base10_parse()?;
            Ok(quote!(::core::time::Duration::from_millis(#ms)))
        }
        other => Err(syn::Error::new_spanned(
            other,
            "expected a duration like \"100ms\", \"2s\", or an integer of milliseconds",
        )),
    }
}

/// `"100ms"` → `Duration::from_millis(100)`, etc. Supports `ns`/`us`/`ms`/`s`/`m`/`h`.
fn parse_duration_str(s: &str, span: proc_macro2::Span) -> syn::Result<TokenStream> {
    let s = s.trim();
    let err = || {
        syn::Error::new(
            span,
            format!("invalid duration {s:?}; e.g. \"100ms\", \"2s\""),
        )
    };
    let (num, ctor): (&str, TokenStream) = if let Some(n) = s.strip_suffix("ms") {
        (n, quote!(from_millis))
    } else if let Some(n) = s.strip_suffix("us") {
        (n, quote!(from_micros))
    } else if let Some(n) = s.strip_suffix("ns") {
        (n, quote!(from_nanos))
    } else if let Some(n) = s.strip_suffix('s') {
        (n, quote!(from_secs))
    } else if let Some(n) = s.strip_suffix('m') {
        let secs: u64 = n.trim().parse().map_err(|_| err())?;
        let total = secs.checked_mul(60).ok_or_else(err)?;
        return Ok(quote!(::core::time::Duration::from_secs(#total)));
    } else if let Some(n) = s.strip_suffix('h') {
        let hours: u64 = n.trim().parse().map_err(|_| err())?;
        let total = hours.checked_mul(3600).ok_or_else(err)?;
        return Ok(quote!(::core::time::Duration::from_secs(#total)));
    } else {
        return Err(err());
    };
    let value: u64 = num.trim().parse().map_err(|_| err())?;
    Ok(quote!(::core::time::Duration::#ctor(#value)))
}

fn lit_to_usize(lit: &syn::Lit) -> syn::Result<usize> {
    match lit {
        syn::Lit::Int(i) => i.base10_parse(),
        other => Err(syn::Error::new_spanned(other, "expected an integer")),
    }
}
