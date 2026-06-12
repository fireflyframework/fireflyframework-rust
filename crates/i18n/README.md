# `firefly-i18n`

> **Tier:** Foundational · **Status:** Full · **Java original:** Spring `MessageSource` · **Go module:** `i18n`

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

## Public surface

| Symbol                                       | Purpose                                                            |
|----------------------------------------------|--------------------------------------------------------------------|
| `Bundle::new(fallback)`                      | Empty bundle with the given fallback locale                        |
| `Bundle::add(locale, key, template)`         | Add one localised message                                          |
| `Bundle::load(locale, src)`                  | Bulk-add from any `(key, template)` iterator                       |
| `Bundle::load_json` / `Bundle::load_yaml`    | Bulk-add from a serialized `key → template` map (Rust convenience) |
| `Bundle::t(locale, key, args)`               | Translate; falls back to `key` literal when no match               |
| `LocaleLayer::new(&bundle)`                  | Tower layer — sets the request-extension locale per request        |
| `Locale`                                     | Resolved locale; axum extractor (empty string when absent)         |
| `with_locale(ext, locale)` / `locale_from`   | Manual extension propagation (Go ctx analogue)                     |
| `pick_locale(header, fallback)`              | Standalone Accept-Language picker                                  |
| `I18nError`                                  | `thiserror` enum for `load_json` / `load_yaml` failures            |

## Testing

```bash
cargo test -p firefly-i18n
```

Covers placeholder substitution, region→language fallback, missing-key
literal pass-through, q-value-ranked Accept-Language picking, and the
middleware end-to-end (in-process via `tower::ServiceExt::oneshot`), plus
Rust-specific cases: serde round-trips, JSON/YAML bundle loading,
cross-thread sharing, and `Send + Sync` bounds.
