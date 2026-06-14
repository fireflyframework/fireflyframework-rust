# `firefly-utils`

> **Tier:** Foundational · **Status:** Stable

## Overview

`firefly-utils` is the framework's **general-purpose helper grab-bag** — the
small set of primitives every module reaches for and that don't fit
into a more specific module:

- **try_run / try_of** — panic-safe function execution with the panic
  value surfaced as a `TryError`.
- **retry / retry_if** — async exponential-backoff retry with jitter
  and a pluggable retryable-error predicate (`RetryConfig`).
- **slugify** — URL-safe lower-case slug from any UTF-8 string. Input
  is canonically decomposed (NFD) and non-spacing combining marks
  (Unicode `Mn`) are dropped, so every canonically decomposable
  letter — Latin-1, Latin Extended-A/B, Vietnamese, pinyin, … — folds
  to its ASCII base.
- **AES-256-GCM crypto** — `encrypt_aes_gcm`, `decrypt_aes_gcm`, plus
  `derive_key256` (SHA-256 KDF) and base64 helpers. The wire format is
  `nonce || ciphertext || tag`.
- **Templates** — `render_text` and `render_html` (auto-escaping)
  over any `serde::Serialize` data.

## Why a separate crate?

Without a shared helper crate, every other crate ends up
re-implementing the same patterns slightly differently — `try`-style
panic recovery is a one-off variant in three places, retry policies
disagree on jitter semantics, and crypto wrappers leak the underlying
nonce semantics. `firefly-utils` lifts these into a single canonical
set so the platform behaves uniformly.

## Public surface

| Group     | Function / type                                                              |
|-----------|------------------------------------------------------------------------------|
| Try       | `try_run(f) -> Result<(), TryError<E>>`, `try_of(f) -> Result<T, TryError<E>>` — panic-recovering wrappers |
| Retry     | `retry(cfg, f).await -> Result<T, E>`, `retry_if(cfg, pred, f).await`        |
| Retry     | `RetryConfig { max_attempts, initial_delay, max_delay, multiplier, jitter_ratio }` |
| Slug      | `slugify(s: &str) -> String`                                                 |
| Crypto    | `derive_key256(passphrase) -> [u8; 32]`                                      |
| Crypto    | `encrypt_aes_gcm(key, plaintext) -> Result<Vec<u8>, CryptoError>`            |
| Crypto    | `decrypt_aes_gcm(key, payload)` — fails with `CryptoError::CipherText`       |
| Crypto    | `encode_base64(b)`, `decode_base64(s)` — URL-safe (with or without padding)  |
| Templates | `render_text(name, source, &data) -> Result<String, TemplateError>`          |
| Templates | `render_html(name, source, &data)` — auto-escaping                           |

Design notes:

- Retry is a single async `retry` over `Result<T, E>`; cancellation is
  the async-native kind — wrap the call in `tokio::time::timeout`.
- The retryable-error predicate is the explicit `retry_if` variant,
  keeping `RetryConfig` `Copy` and error-type agnostic.
- Templates implement the field-interpolation subset the framework
  uses (`{{.Field}}`, `{{.User.Name}}`, `{{.}}`); missing fields fail
  fast rather than rendering a placeholder.

## Quick start

```rust
use firefly_utils::{
    decrypt_aes_gcm, derive_key256, encrypt_aes_gcm, render_html, retry, slugify, RetryConfig,
};

#[tokio::main]
async fn main() {
    let cfg = RetryConfig::default(); // 3 attempts, 100ms→5s, ×2, ±20% jitter
    let result = retry(cfg, || async { place_order().await }).await;
    assert_eq!(result.unwrap(), "order-001");

    let slug = slugify("Cañón del Río"); // "canon-del-rio"
    assert_eq!(slug, "canon-del-rio");

    let key = derive_key256("super-secret");
    let ct = encrypt_aes_gcm(&key, b"hi").unwrap();
    let pt = decrypt_aes_gcm(&key, &ct).unwrap(); // b"hi"
    assert_eq!(pt, b"hi");

    let body = render_html(
        "welcome",
        "<p>Hello {{.Name}}</p>",
        &serde_json::json!({"Name": "world"}),
    )
    .unwrap();
    assert_eq!(body, "<p>Hello world</p>");
}

async fn place_order() -> Result<String, std::io::Error> {
    Ok("order-001".to_string())
}
```

## Testing

```bash
cargo test -p firefly-utils
```

The suite covers panic-as-error, retry attempts + non-retryable
short-circuit, slug edge cases (combining marks, leading/trailing
separators, Unicode), AES-GCM round-trip + tamper detection, base64
round-trip, HTML-escape preservation, all three AES key sizes, backoff
growth/cap/jitter bounds, template parse/execute errors, and
`Send + Sync` bounds on every error type.
