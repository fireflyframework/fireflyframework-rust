# `firefly-session`

> **Tier:** Foundational · **Status:** Stable

## Overview

`firefly-session` is the framework's **server-side HTTP session tier** —
a session handle with typed attributes, a pluggable async session store,
and a [`tower::Layer`] that loads-or-creates a session from a cookie on
each request and saves-if-modified (with `Set-Cookie`, id-rotation
migration, and invalidation) on the response. Its design is inspired by
mature server-side session frameworks, including Spring Session-style
stores and `maximumSessions`-style concurrency control.

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

## Capabilities

The session handle ([`Session`] / [`SessionInner`]) exposes **typed**
attributes via `attribute::<T>` / `set_attribute::<T>` (serde-backed), plus
`rotate_id`, `invalidate`, `modified`, and `previous_id` for id rotation and
teardown. The pluggable [`SessionStore`] async trait (`get`/`save`/`delete`/
`exists`, TTL as `Duration`) has two built-in implementations:

* [`MemorySessionStore`] — in-process, with lazy TTL eviction plus an eager
  `sweep`.
* [`CacheSessionStore`] — bridges any `firefly_cache::Adapter` (including
  `firefly-cache-redis`) without a hard `redis` dependency; serde typing
  gates which session types deserialize, so there is no deserialization
  gadget to guard.

The HTTP integration is wired through [`SessionLayer`] / [`SessionService`]
(`new` / `from_config` / `with_signer`). Cookie defaults are `HttpOnly`,
`SameSite=Lax`, a sliding `Max-Age`, and auto-`Secure` detection via
`X-Forwarded-Proto` or the request scheme; the cookie name and other knobs
live in [`SessionConfig`] / [`SameSite`]. Configuration is explicit —
`SessionLayer::from_config(cfg, store)` — following the workspace's
constructor-injection pattern rather than implicit auto-configuration.

Concurrency control combines [`SessionRegistry`] / [`MemorySessionRegistry`]
(oldest-first ordering), a [`ConcurrencyPolicy`] with
[`Strategy::EvictOldest`] / [`Strategy::RejectNew`], and a
[`SessionConcurrencyController`] (`on_login → bool`, `on_logout`, plus
`with_session_store` to delete evicted sessions).

### Notable features

* **Typed attributes** — `attribute::<T>` deserializes; wrong-type reads
  yield `None` rather than panicking.
* **HMAC-signed cookies** — [`SessionSigner`] (`with_signer`) signs the
  session-id cookie value (`<id>.<base64url(hmac)>`); a tampered cookie
  fails constant-time verification and starts a fresh session. Off by
  default to keep the plain cookie wire format.
* **Absolute timeout** — [`SessionConfig::absolute_timeout_seconds`] caps
  total session lifetime from `_created_at`, complementing the sliding TTL.
  Off by default.
* **`SessionExt` extractor** — an axum [`SessionExt`] newtype yielding a
  clear `500` when the layer is not installed.

## Distributed registry adapters

The in-process [`MemorySessionRegistry`] bounds session concurrency within one
process. For cluster-wide caps, two leaf adapter crates implement the same
[`SessionRegistry`] port over shared storage:

* [`firefly-session-redis`](../session-redis) — `RedisSessionRegistry` over a
  Redis sorted set (score = `created_at`, oldest-first via `ZRANGE`, sliding
  `EXPIRE`).
* [`firefly-session-postgres`](../session-postgres) — `PostgresSessionRegistry`
  over a Postgres table (idempotent `ON CONFLICT` upsert, `ORDER BY created_at`).

For the session *store* (not the registry), Redis-backed persistence is
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
