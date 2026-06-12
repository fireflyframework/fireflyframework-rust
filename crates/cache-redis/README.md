# `firefly-cache-redis`

> **Tier:** Platform · **Status:** Full · **pyfly original:** `pyfly.cache.adapters.redis.RedisCacheAdapter`

## Overview

`firefly-cache-redis` is the Redis implementation of the
[`firefly_cache::Adapter`](../cache) port — the Rust port of pyfly's
`RedisCacheAdapter`. It speaks the native Redis verbs over the
[`redis`](https://crates.io/crates/redis) crate's multiplexed async
connection, so a `RedisAdapter` drops in wherever an `Arc<dyn Adapter>`
is expected (CQRS query cache, idempotency guards, `FallbackAdapter`
primaries, the `Typed<T>` facade).

```
firefly_cache::Adapter  (port)
        ▲
        │  impl
   RedisAdapter ──► redis::aio::MultiplexedConnection ──► Redis
```

## Command mapping

| Port method      | Redis command(s)                          |
|------------------|-------------------------------------------|
| `get`            | `GET`                                     |
| `set`            | `SET key value [PX ttl_ms]`               |
| `set_if_absent`  | `SET key value [PX ttl_ms] NX`            |
| `delete`         | `DEL`                                     |
| `exists`         | `EXISTS`                                  |
| `delete_prefix`  | `SCAN MATCH <escaped-prefix>*` loop + `DEL` |
| `clear`          | `FLUSHDB`                                 |
| `stats`          | `DBSIZE` + in-process hit/miss/eviction counters |
| `health_check`   | `PING`                                    |

Plus two pyfly-parity helpers beyond the port:

- `keys(pattern, limit) -> Vec<String>` — `SCAN MATCH` collecting up to
  `limit` keys (pyfly's `get_keys(pattern, limit)`).
- `is_available() -> bool` — a fail-soft `PING` (pyfly's
  `is_available()`); `health_check` is the erroring variant.

## Construction

Unlike pyfly — whose adapter is handed an already-connected
`redis.asyncio.Redis` client and has `start()`/`stop()` lifecycle hooks —
`RedisAdapter` follows the Rust port's adapter-crate convention:

```rust,no_run
use std::sync::Arc;
use std::time::Duration;
use firefly_cache::{Adapter, Typed};
use firefly_cache_redis::RedisAdapter;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
// From a URL (multiplexed connection established eagerly):
let adapter = Arc::new(RedisAdapter::connect("redis://127.0.0.1:6379/0").await?);

adapter.set("user:1", b"alice", Some(Duration::from_secs(60))).await?;
assert_eq!(adapter.get("user:1").await?, b"alice");
assert_eq!(adapter.delete_prefix("user:").await?, 1);
# Ok(())
# }
```

Or inject a pre-built connection with `RedisAdapter::from_connection`
(the DI entry point matching pyfly's `RedisCacheAdapter(client)`).

Values cross the port as raw bytes; layer `firefly_cache::Typed<T>` on
top for JSON encoding — the stored bytes are byte-identical to the
in-process `MemoryAdapter`, keeping cache entries portable across the
sibling framework ports.

## Notes

- **Prefix safety:** `delete_prefix` escapes the Redis glob
  metacharacters (`* ? [ ] \`) in the literal prefix before appending the
  `*` wildcard, so `delete_prefix("a*b:")` removes only keys literally
  starting with `a*b:`.
- **TTL:** `None` (or a zero `Duration`) means no expiry. A positive TTL
  is forwarded as whole-millisecond `PX`; sub-millisecond TTLs round up
  to `1ms` so they never silently become persistent.
- **Stats:** `size` comes from `DBSIZE`; hits/misses/evictions are
  in-process counters (Redis exposes no per-adapter hit counters), as in
  pyfly.

## Testing

```bash
cargo test -p firefly-cache-redis
```

Every unit test runs against an **in-process fake RESP server** (a
`TcpListener` speaking just enough RESP2) — there is no external Redis
dependency, mirroring pyfly's `FakeRedis` stub in
`tests/cache/test_redis_adapter.py`. A docker-gated round-trip against a
real Redis (mirroring pyfly's
`tests/integration/test_cache_redis_integration.py`) runs with:

```bash
REDIS_URL=redis://127.0.0.1:6379/0 cargo test -p firefly-cache-redis -- --ignored
```
