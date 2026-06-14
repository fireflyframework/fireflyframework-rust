# `firefly-session-mongodb`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-session-mongodb` is a **MongoDB-backed distributed `SessionRegistry`**
— the document-store sibling of `firefly-session-postgres` and
`firefly-session-redis`. It lets a horizontally-scaled service enforce a
maximum-concurrent-sessions policy across every instance: each
`(principal, session_id, created_at)` triple is one document in a sessions
collection, keyed uniquely by `session_id`.

It implements the `firefly_session::SessionRegistry` port, so it drops straight
into a `SessionConcurrencyController` alongside the in-memory, Postgres, and
Redis registries — swap the backend, keep the policy.

## Usage

```rust,no_run
use firefly_session_mongodb::MongoSessionRegistry;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
// Connect from a URI (uses the URI's default database, else `firefly`,
// and the `firefly_sessions` collection):
let registry = MongoSessionRegistry::connect("mongodb://localhost:27017").await?;
registry.init().await?; // create the unique `session_id` + `principal` indexes

// …or build it over a Collection you already own (DI):
// let registry = MongoSessionRegistry::from_collection(collection);
# let _ = registry;
# Ok(())
# }
```

## Contract

| Method | Behaviour |
|--------|-----------|
| `register(principal, session_id, created_at)` | upsert keyed by `session_id` |
| `deregister(principal, session_id)` | delete (idempotent) |
| `list_sessions(principal)` | `(session_id, created_at)`, **oldest first** |
| `count(principal)` | number of live sessions |

The `SessionRegistry` trait is **infallible by contract** — a backend hiccup
must never fail a login — so every method logs and swallows a MongoDB error
rather than propagating it; the concurrency cap simply isn't enforced for that
one operation. Constructors and `init()` return `RegistryError`.

## Testing

The round-trip test is **env-gated**: set `FIREFLY_TEST_MONGODB_URL` (fallback
`MONGODB_URL`) to run it against a live MongoDB; it skips cleanly otherwise.
