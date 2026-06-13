# `firefly-session-redis`

> **Tier:** Platform · **Status:** Full · **pyfly original:** `pyfly.session.adapters.redis_registry.RedisSessionRegistry`

## Overview

`firefly-session-redis` is the **Redis-backed, distributed** implementation of
the [`firefly_session::SessionRegistry`](../session) port — the Rust port of
pyfly's `RedisSessionRegistry`. It is a shared, per-principal index of live
sessions: every application instance reads and writes the same Redis keys, so
the per-principal concurrency cap enforced by
[`firefly_session::SessionConcurrencyController`](../session) holds **across the
whole cluster**, not just within one process (the limit of the in-process
[`MemorySessionRegistry`](../session)).

Plug it into the existing session-concurrency machinery with **no API change**:
it implements the same `SessionRegistry` trait the in-memory registry does
(`register` / `deregister` / `list_sessions` / `count`), so the
`SessionConcurrencyController` is agnostic to which registry backs it.

## Data model

Each principal's live sessions are a single Redis **sorted set** keyed
`firefly:session:user:<principal>` (the `DEFAULT_KEY_PREFIX` plus the
principal). The sorted-set *score* is the session's `created_at` (epoch-millis)
and the *member* is the session id:

| `SessionRegistry` method | Redis command(s)                                          |
|--------------------------|-----------------------------------------------------------|
| `register`               | `ZADD key <created_at> <session_id>` + `EXPIRE key <ttl>` |
| `deregister`             | `ZREM key <session_id>`                                   |
| `list_sessions`          | `ZRANGE key 0 -1 WITHSCORES` (ascending → oldest-first)   |
| `count`                  | `ZCARD key`                                               |

Storing `created_at` as the score makes `list_sessions` naturally
**oldest-first** (`ZRANGE` is ascending) with no client-side sort — exactly
what the evict-oldest concurrency strategy needs — and a `deregister` of the
last member leaves an empty set that Redis drops automatically, so a
principal's key disappears once they have no live sessions (matching the
in-process registry's bucket pruning).

> A plain Redis set (`SADD`/`SMEMBERS`/`SREM`) cannot record each session's
> `created_at` nor return entries oldest-first, both of which the
> `SessionRegistry` contract requires; a **sorted set** is used instead. This
> is faithful to pyfly's actual `ZADD`/`ZRANGE`/`ZREM`/`ZCARD` implementation.

## TTL — bounding orphan growth

`register` slides an `EXPIRE` on the principal's key (default 24h,
`DEFAULT_TTL_SECS`) on every login. This bounds the growth of orphaned index
entries (e.g. a crashed instance that never deregistered): a principal who
stops logging in entirely has their stale index self-expire rather than linger
forever. The TTL slides forward on each `register`, so an actively-used
principal's index never expires out from under them. A `ttl_secs <= 0` disables
the per-principal expiry entirely.

## Usage

```rust,no_run
use std::sync::Arc;
use firefly_session::{SessionRegistry, SessionConcurrencyController, ConcurrencyPolicy, Strategy};
use firefly_session_redis::RedisSessionRegistry;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
// Connect from a URL …
let registry = Arc::new(RedisSessionRegistry::connect("redis://127.0.0.1:6379/0").await?);

// … or inject an already-built multiplexed connection (the DI entry point):
// let registry = Arc::new(RedisSessionRegistry::from_connection(conn));

// Plug the distributed registry into the cluster-wide concurrency cap.
let controller = SessionConcurrencyController::new(
    registry.clone(),
    ConcurrencyPolicy { max_sessions: 2, strategy: Strategy::EvictOldest },
);
controller.on_login("alice", "session-1", 1_700_000_000_000).await;
assert_eq!(registry.count("alice").await, 1);
# Ok(())
# }
```

### Construction

| Constructor                                 | Use                                                       |
|---------------------------------------------|-----------------------------------------------------------|
| `connect(url)`                              | Connect from a `redis://` URL, default prefix + 24h TTL.  |
| `connect_with(url, prefix, ttl_secs)`       | Connect with a custom key prefix and sliding TTL.         |
| `from_connection(conn)`                     | Wrap an existing `MultiplexedConnection` (DI).            |
| `from_connection_with(conn, prefix, ttl)`   | Wrap an existing connection with custom prefix + TTL.     |

The `SessionRegistry` trait methods are **infallible by contract** (the
controller can't surface an error mid-login), so a per-operation Redis failure
is logged via `tracing` and swallowed — the concurrency cap simply isn't
enforced for that one login (the key's TTL still bounds any drift). Only the
constructors return a `RegistryError` (connection setup).

## Testing

- **Always-on contract tests** (`tests/redis_registry_test.rs`) run the adapter
  end-to-end over a real TCP socket against an **in-process fake RESP2 server**
  (`tests/common/mod.rs`) implementing `ZADD`/`ZRANGE`/`ZREM`/`ZCARD`/`EXPIRE` —
  no external Redis, so they run on every `cargo test`.
- **Env-gated live tests** (`tests/redis_integration_test.rs`) exercise a real
  server. They read `FIREFLY_TEST_REDIS_URL` (falling back to `REDIS_URL`); when
  unset they print a one-line `skipping …` and pass, so `cargo test` on a bare
  machine is green.

```sh
export FIREFLY_TEST_REDIS_URL="redis://127.0.0.1:6379/0"
cargo test -p firefly-session-redis
```

## See also

- [`firefly-session`](../session) — the session tier and the `SessionRegistry`
  port + in-process `MemorySessionRegistry` this crate implements.
- [`firefly-session-postgres`](../session-postgres) — the durable, relational
  alternative for deployments without Redis.
- [`firefly-cache-redis`](../cache-redis) — the sibling Redis cache adapter
  whose connection / lifecycle convention this crate follows.
