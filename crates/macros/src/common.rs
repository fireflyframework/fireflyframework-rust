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

//! Shared helpers for every macro: the `::firefly::__rt::*` contract path,
//! the `#[firefly(crate = "...")]` override, and small parsing utilities.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{LitStr, Path, Token};

/// The facade crate the generated code resolves runtime types through.
///
/// Every macro emits paths of the form `#facade::__rt::firefly_cqrs::Bus` so a
/// user who depends only on the `firefly` facade never needs to add the
/// underlying `firefly-*` crates. A `#[firefly(crate = "my_alias")]` argument
/// overrides the leading `::firefly` segment for users who rename or re-export
/// the facade (or who depend on the crates directly through a shim).
#[derive(Clone)]
pub(crate) struct Facade(pub(crate) Path);

impl Default for Facade {
    fn default() -> Self {
        // `::firefly` — the canonical, crate-name-rooted facade path.
        Facade(syn::parse_quote!(::firefly))
    }
}

impl Facade {
    /// The `__rt` contract module path: `#facade::__rt`.
    pub(crate) fn rt(&self) -> TokenStream {
        let p = &self.0;
        quote!(#p::__rt)
    }

    /// `#facade::__rt::firefly_cqrs`.
    pub(crate) fn cqrs(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_cqrs)
    }

    /// `#facade::__rt::firefly_container`.
    pub(crate) fn container(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_container)
    }

    /// `#facade::__rt::firefly_config` — the config crate, used by
    /// `#[derive(ConfigProperties)]` to bind a prefix-scoped struct.
    pub(crate) fn config(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_config)
    }

    /// `#facade::__rt::firefly_scheduling`.
    pub(crate) fn scheduling(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_scheduling)
    }

    /// `#facade::__rt::firefly_eda`.
    pub(crate) fn eda(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_eda)
    }

    /// `#facade::__rt::firefly_eventsourcing`.
    pub(crate) fn eventsourcing(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_eventsourcing)
    }

    /// `#facade::__rt::firefly_transactional` — the declarative transaction
    /// runtime the `#[transactional]` macro expands against.
    pub(crate) fn transactional(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_transactional)
    }

    /// `#facade::__rt::firefly_security` — the security runtime the
    /// `#[pre_authorize]` / `#[post_authorize]` method-security macros expand
    /// against (the ambient `Authentication` context and `AccessRule`).
    pub(crate) fn security(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::firefly_security)
    }

    /// `#facade::__rt::serde_json` — the facade-re-exported `serde_json`, so
    /// generated code (e.g. `#[derive(DomainEvent)]`'s payload encoder) never
    /// forces the user crate to depend on `serde_json` directly.
    pub(crate) fn serde_json(&self) -> TokenStream {
        let rt = self.rt();
        quote!(#rt::serde_json)
    }
}

/// Parses a `crate = "..."` argument out of a darling-collected attribute or a
/// raw `Option<String>`, falling back to the default `::firefly` facade.
///
/// Accepts both a bare crate identifier (`firefly`) and a leading-`::` path
/// (`::my_firefly`); the result is normalised to an absolute path so generated
/// code is hygienic regardless of where it is expanded.
pub(crate) fn facade_from_override(value: &Option<String>) -> syn::Result<Facade> {
    match value {
        None => Ok(Facade::default()),
        Some(raw) => {
            let raw = raw.trim();
            if raw.is_empty() {
                return Ok(Facade::default());
            }
            let path: Path = if raw.starts_with("::") || raw.starts_with("crate") {
                syn::parse_str(raw)?
            } else {
                syn::parse_str(&format!("::{raw}"))?
            };
            Ok(Facade(path))
        }
    }
}

/// A duration literal such as `"30s"`, `"500ms"`, `"2m"`, `"1h"`, or a bare
/// integer (interpreted as seconds) — the Rust spelling of pyfly's
/// `timedelta`/`cron_ttl` arguments. Parsed at macro-expansion time so a
/// malformed value is a compile error rather than a runtime panic.
pub(crate) fn parse_duration(spec: &str, span: proc_macro2::Span) -> syn::Result<TokenStream> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(syn::Error::new(span, "empty duration string"));
    }

    // Split the numeric head from the unit suffix.
    let split = trimmed
        .find(|c: char| c.is_alphabetic())
        .unwrap_or(trimmed.len());
    let (num, unit) = trimmed.split_at(split);
    let unit = unit.trim();

    let parse_int = |s: &str| -> syn::Result<u64> {
        s.trim()
            .parse::<u64>()
            .map_err(|_| syn::Error::new(span, format!("invalid duration value: {trimmed:?}")))
    };
    let parse_f = |s: &str| -> syn::Result<f64> {
        s.trim()
            .parse::<f64>()
            .map_err(|_| syn::Error::new(span, format!("invalid duration value: {trimmed:?}")))
    };

    let tokens = match unit {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => {
            // Seconds tolerate a fractional value (`"1.5s"`): try an integer
            // first for an exact `from_secs`, else fall back to `from_secs_f64`.
            if let Ok(v) = num.trim().parse::<u64>() {
                quote!(::core::time::Duration::from_secs(#v))
            } else {
                let v = parse_f(num)?;
                quote!(::core::time::Duration::from_secs_f64(#v))
            }
        }
        "ms" | "milli" | "millis" | "millisecond" | "milliseconds" => {
            let v = parse_int(num)?;
            quote!(::core::time::Duration::from_millis(#v))
        }
        "us" | "micro" | "micros" | "microsecond" | "microseconds" => {
            let v = parse_int(num)?;
            quote!(::core::time::Duration::from_micros(#v))
        }
        "m" | "min" | "mins" | "minute" | "minutes" => {
            let v = parse_int(num)?;
            quote!(::core::time::Duration::from_secs(#v * 60))
        }
        "h" | "hr" | "hrs" | "hour" | "hours" => {
            let v = parse_int(num)?;
            quote!(::core::time::Duration::from_secs(#v * 3600))
        }
        "d" | "day" | "days" => {
            let v = parse_int(num)?;
            quote!(::core::time::Duration::from_secs(#v * 86400))
        }
        other => {
            return Err(syn::Error::new(
                span,
                format!("unknown duration unit {other:?} in {trimmed:?}; use s/ms/us/m/h/d"),
            ));
        }
    };
    Ok(tokens)
}

/// A single `path = literal` pair inside an attribute argument list, used by
/// the hand-rolled method-mapping parser (`#[get("/:id")]`).
pub(crate) struct LitStrArg(pub(crate) LitStr);

impl Parse for LitStrArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Accept either `#[get("/path")]` or an empty `#[get]`/`#[post()]`.
        if input.is_empty() {
            return Ok(LitStrArg(LitStr::new("", proc_macro2::Span::call_site())));
        }
        let lit: LitStr = input.parse()?;
        // Tolerate a trailing comma.
        if input.peek(Token![,]) {
            let _: Token![,] = input.parse()?;
        }
        Ok(LitStrArg(lit))
    }
}
