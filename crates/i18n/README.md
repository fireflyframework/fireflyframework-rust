# `firefly-i18n`

> **Tier:** Foundational · **Status:** Stable

## Overview

`firefly-i18n` provides **locale-aware message lookup** with `{name}`-style
placeholder substitution. The default `Bundle` stores
`locale → key → template` behind an `RwLock`; a fallback locale is
consulted when the requested locale (or its language root, for region
tags like `es-MX`) has no entry.

`LocaleLayer` — a tower layer usable on any axum `Router` — resolves the
locale per request from the `Accept-Language` header (q-value-aware) and
stores it on the request extensions, where handlers retrieve it with the
`Locale` extractor or `locale_from`.

## Quick start

```rust
use axum::{routing::get, Router};
use firefly_i18n::{Bundle, Locale, LocaleLayer};
use std::sync::Arc;

let b = Arc::new(Bundle::new("en"));
b.load("en", [("hello", "Hello, {name}!")]);
b.load("es", [("hello", "¡Hola, {name}!")]);

let layer = LocaleLayer::new(&b);
let bundle = Arc::clone(&b);
let app: Router = Router::new()
    .route(
        "/greet",
        get(move |Locale(loc): Locale| {
            let bundle = Arc::clone(&bundle);
            async move { bundle.t(&loc, "hello", &[("name", "alice")]) }
        }),
    )
    .layer(layer);
```

`GET /greet` with `Accept-Language: es,en;q=0.5` → `¡Hola, alice!`
`GET /greet` with `Accept-Language: fr` → `Hello, alice!` (fallback)

## Region → language fallback

`b.t("es-MX", "hello", ...)` consults `es-mx` first, then `es`, then the
fallback locale. Region tags fall back to language tags automatically.

## Pluggable resolution surface

The crate exposes a pluggable shape so consumers depend on abstractions
rather than the concrete `Bundle`:

- **`MessageSource` port** — a pluggable resolution trait
  (`get_message` / `get_message_or_default`) so consumers depend on an
  abstraction, not the concrete `Bundle`. A miss is a typed
  `MessageNotFound`.
- **Positional `{0}`/`{1}` MessageFormat** — `format_message` and
  `Bundle::tn` substitute positional arguments with MessageFormat quote
  semantics (`''` → `'`, single-quoted text is literal), alongside the
  existing named `{name}` substitution.
- **File-convention loader** — `Bundle::load_dir(base, locale)` reads the
  first of `messages_{locale}.yaml` / `.yml` / `.json` under `base` and
  flattens nested keys with dots (`greeting.hello`).
- **`LocaleResolver` port** — `FixedLocaleResolver` (always one locale)
  and `AcceptHeaderLocaleResolver` (highest-quality `Accept-Language` tag,
  reduced to its language root).

```rust
use firefly_i18n::{Bundle, MessageSource};

let b = Bundle::new("en");
b.load_dir("i18n/", "es").unwrap();          // reads messages_es.yaml|yml|json
let msg = b.get_message("greeting.hello", &["World"], "es").unwrap();
let or_default = b.get_message_or_default("missing", "Hi {0}", &["World"], "es");
```

## Public surface

| Symbol                                       | Purpose                                                            |
|----------------------------------------------|--------------------------------------------------------------------|
| `Bundle::new(fallback)`                      | Empty bundle with the given fallback locale                        |
| `Bundle::add(locale, key, template)`         | Add one localised message                                          |
| `Bundle::load(locale, src)`                  | Bulk-add from any `(key, template)` iterator                       |
| `Bundle::load_json` / `Bundle::load_yaml`    | Bulk-add from a serialized `key → template` map (Rust convenience) |
| `Bundle::load_dir(base, locale)`             | Load `messages_{locale}.{yaml,yml,json}`, flattening nested keys   |
| `Bundle::t(locale, key, args)`               | Translate (named `{name}` args); falls back to `key` literal       |
| `Bundle::tn(locale, key, args)`              | Translate with positional `{0}`/`{1}` MessageFormat args           |
| `format_message(template, args)`             | Standalone positional MessageFormat formatter (quote-aware)        |
| `MessageSource`                              | Pluggable resolution port (`get_message` / `…_or_default`)         |
| `MessageNotFound`                            | Typed miss error                                                   |
| `LocaleResolver`                             | Pluggable locale-resolution port                                   |
| `FixedLocaleResolver` / `AcceptHeaderLocaleResolver` | Built-in resolvers (fixed locale / Accept-Language root)   |
| `LocaleLayer::new(&bundle)`                  | Tower layer — sets the request-extension locale per request        |
| `Locale`                                     | Resolved locale; axum extractor (empty string when absent)         |
| `with_locale(ext, locale)` / `locale_from`   | Manual extension propagation                                       |
| `pick_locale(header, fallback)`              | Standalone Accept-Language picker                                  |
| `I18nError`                                  | `thiserror` enum for `load_json` / `load_yaml` / `load_dir` errors |

## Testing

```bash
cargo test -p firefly-i18n
```

Covers placeholder substitution, region→language fallback, missing-key
literal pass-through, q-value-ranked Accept-Language picking, and the
middleware end-to-end (in-process via `tower::ServiceExt::oneshot`), plus
Rust-specific cases: serde round-trips, JSON/YAML bundle loading,
cross-thread sharing, and `Send + Sync` bounds.
