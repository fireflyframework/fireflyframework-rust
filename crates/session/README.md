# `firefly-session`

> **Tier:** Foundational · **Status:** Full · **Python original:** `pyfly.session`

## Overview

`firefly-session` is the framework's **server-side HTTP session tier** —
a session handle with typed attributes, a pluggable async session store,
and a [`tower::Layer`] that loads-or-creates a session from a cookie on
each request and saves-if-modified (with `Set-Cookie`, id-rotation
migration, and invalidation) on the response. It is the Rust port of
pyfly's `session` package (itself modelled on Spring Session + Spring
Security's `maximumSessions`).

Every middleware is a [`tower::Layer`], so it composes with axum and any
tower-compatible router.

## Mental model

```
incoming request
      │
      ▼  parse Cookie → (verify HMAC) → load from store or mint new id
┌──────────────────────────────────────────────┐
│ SessionLayer   (inserts Extension<Session>)   │
└──────────────────────────────────────────────┘
      │  handler mutates / rotates / invalidates
      ▼  persist: delete previous_id, delete-if-invalidated, save-if-modified
   Set-Cookie (sliding Max-Age; Secure auto over HTTPS)
```

## Quick start

```rust,no_run
use std::sync::Arc;
use axum::{routing::get, Extension, Router};
use firefly_session::{Session, SessionLayer, MemorySessionStore, SessionStore};

async fn handler(session: Extension<Session>) -> &'static str {
    session.set_attribute("user", "ada").await.unwrap();
    "ok"
}

let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
let app: Router = Router::new()
    .route("/", get(handler))
    .layer(SessionLayer::new(store));
```

## pyfly parity

| pyfly | firefly-session |
|---|---|
| `HttpSession` (untyped `get/set_attribute`) | [`Session`] / [`SessionInner`] with **typed** `attribute::<T>` / `set_attribute::<T>` (serde) |
| `rotate_id`, `invalidate`, `modified`, `previous_id` | same, preserved |
| `SessionStore` protocol | [`SessionStore`] async trait (`get`/`save`/`delete`/`exists`, TTL `Duration`) |
| `InMemorySessionStore` | [`MemorySessionStore`] (lazy TTL eviction + eager `sweep`) |
| `RedisSessionStore` (+ `allow_session_type` allowlist) | [`CacheSessionStore`] bridging any `firefly_cache::Adapter`; serde typing subsumes the importlib allowlist (no gadget to guard) |
| `SessionFilter(store, cookie_name, ttl, secure)` | [`SessionLayer`] / [`SessionService`] (`new` / `from_config` / `with_signer`) |
| cookie: `PYFLY_SESSION`, `HttpOnly`, `SameSite=Lax`, sliding `Max-Age`, auto-`Secure` via `X-Forwarded-Proto`/scheme | [`SessionConfig`] / [`SameSite`], identical defaults & wire format |
| `SessionRegistry` / `InMemorySessionRegistry` | [`SessionRegistry`] / [`MemorySessionRegistry`] (oldest-first) |
| `ConcurrencyControlPolicy(max_sessions, strategy)` | [`ConcurrencyPolicy`] + [`Strategy::EvictOldest`] / [`Strategy::RejectNew`] |
| `SessionConcurrencyController(on_login → bool, on_logout)` | [`SessionConcurrencyController`] (+ `with_session_store` ≈ `session_deleter`) |
| `@auto_configuration` / `@conditional_on_property` beans | explicit `SessionLayer::from_config(cfg, store)` (the workspace DI pattern) |

### Rust-only additions

* **Typed attributes** — `attribute::<T>` deserializes; wrong-type reads
  yield `None` rather than panicking.
* **HMAC-signed cookies** — [`SessionSigner`] (`with_signer`) signs the
  session-id cookie value (`<id>.<base64url(hmac)>`); a tampered cookie
  fails constant-time verification and starts a fresh session. Off by
  default for pyfly cookie-wire parity.
* **Absolute timeout** — [`SessionConfig::absolute_timeout_seconds`] caps
  total session lifetime from `_created_at`, beyond pyfly's sliding-only
  TTL. Off by default.
* **`SessionExt` extractor** — an axum [`SessionExt`] newtype yielding a
  clear `500` when the layer is not installed.

## Distributed registry adapters

The in-process [`MemorySessionRegistry`] bounds session concurrency within one
process. For cluster-wide caps, two leaf adapter crates implement the same
[`SessionRegistry`] port over shared storage (pyfly's `RedisSessionRegistry` /
`PostgresSessionRegistry`):

* [`firefly-session-redis`](../session-redis) — `RedisSessionRegistry` over a
  Redis sorted set (score = `created_at`, oldest-first via `ZRANGE`, sliding
  `EXPIRE`).
* [`firefly-session-postgres`](../session-postgres) — `PostgresSessionRegistry`
  over a Postgres table (idempotent `ON CONFLICT` upsert, `ORDER BY created_at`).

For the session *store* (not the registry), pyfly's `RedisSessionStore` is
covered by [`CacheSessionStore`], which bridges any `firefly_cache::Adapter`
(including `firefly-cache-redis`) without a hard `redis` dependency.

## Tests

`cargo test -p firefly-session` — unit tests per module (session,
store, config, signing, concurrency) plus `tests/layer_oneshot.rs`
exercising the full `SessionLayer` via `tower::ServiceExt::oneshot`
(cookie issuance, secure-over-HTTPS / `X-Forwarded-Proto`, existing-load,
invalidation delete-cookie, rotation store+cookie migration, signed
round-trip, absolute-timeout expiry). All in-process — no external
servers.

[`tower::Layer`]: https://docs.rs/tower/latest/tower/trait.Layer.html
