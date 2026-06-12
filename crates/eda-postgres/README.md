# firefly-eda-postgres

A Postgres-backed [`Broker`](../eda) for the Firefly EDA port: a durable
**transactional outbox** plus `LISTEN`/`NOTIFY` wake-ups, with per-group
offset cursors and a single advisory-lock-gated drain loop. It implements
the `firefly-eda` `Publisher` / `Subscriber` / `Broker` ports over
`tokio-postgres`, so a service tested against the in-memory broker switches
to durable Postgres delivery with no handler changes.

## How it works

- **Outbox.** Every `publish` appends a row to `firefly_eda_outbox`
  (monotonic `BIGSERIAL` id, `JSONB` payload/headers) and fires
  `pg_notify(channel, '<id>')`.
- **Offsets.** Each consumer **group** keeps a cursor row in
  `firefly_eda_offsets`, so subscribers survive restarts and catch up on
  events they missed.
- **Drain loop.** A background task wakes on `NOTIFY`, on `subscribe`, and
  on a fixed `poll_interval` fallback. It is gated by
  `pg_try_advisory_lock` on a stable signed-`i64` key folded from the
  consumer-group name (SHA-256, first 8 bytes, big-endian), so two replicas
  sharing a group never double-advance the cursor.
- **At-least-once.** The cursor is advanced only **after** a handler
  returns successfully; a crash mid-dispatch re-delivers from the last
  committed id. The session-level advisory lock auto-releases on connection
  close, so a crashed worker never zombies the lock.

## Quick start

```rust
use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_postgres::{PostgresBroker, PostgresConfig};

let broker = PostgresBroker::new(
    PostgresConfig::new("host=db user=app dbname=app")
        .destinations(["orders.created"])
        .group("orders-workers"),
);
broker.start().await?;
broker
    .subscribe(
        "OrderCreated", // event-type glob, e.g. "Order*"
        handler(|ev: Event| async move {
            println!("got {}", ev.event_type);
            Ok(())
        }),
    )
    .await?;
broker
    .publish(Event::new("orders.created", "OrderCreated", "orders-svc", None))
    .await?;
```

## pyfly parity

This crate ports `pyfly.eda.adapters.postgres.PostgresEventBus`.

| pyfly | firefly-eda-postgres | Notes |
|-------|----------------------|-------|
| `PostgresEventBus(dsn=…, listen_dsn=…, channel=…, destinations=…, group=…, poll_interval_s=…)` | `PostgresBroker::new(PostgresConfig::new(dsn).listen_dsn(…).channel(…).destinations(…).group(…).poll_interval(…))` | builder replaces keyword args |
| `_quote_ident` (channel validation) | `quote_ident` / `PostgresBroker::try_new` returning `IdentError` | `new` panics on an invalid channel; `try_new` returns the error |
| `_group_lock_key` (SHA-256 fold) | `group_lock_key` | identical value; `i64::from_be_bytes` of the first 8 digest bytes is the two's-complement equal of pyfly's unsigned-then-subtract fold |
| `_normalise_dsn` | `normalise_dsn` | strips `postgresql+asyncpg://` / `postgresql+psycopg://` / `postgres+asyncpg://` |
| `subscribe(event_type_pattern, handler)` | `subscribe_pattern(pattern, handler)` (and the `Subscriber::subscribe` trait method, which treats `topic` as the glob) | `fnmatch` → `globset` glob over the event's `event_type` |
| `publish` (INSERT + NOTIFY) | `PostgresBroker::publish` | same SQL shape, `$n::jsonb` casts |
| `start` / `stop` | `start` / `close` | DDL + offset seed + listener + drain loop / abort + release |

### Deliberate divergences

- **Table prefix.** Tables are `firefly_eda_outbox` / `firefly_eda_offsets`
  (vs pyfly's `pyfly_eda_*`), matching this framework's naming. The column
  layout is byte-identical, so a pyfly producer and a Rust consumer
  interoperate when both target the same table name.
- **Hash.** SHA-256 (in the workspace dependency catalog) rather than
  pyfly's `hashlib.sha256` — same algorithm, same bytes, same key.
- **No connection pool.** A single pipelined `tokio-postgres::Client`
  stands in for pyfly's asyncpg pool; tokio-postgres pipelines concurrent
  queries on one connection. A dedicated session connection runs `LISTEN`.
- **JSONB as text.** Payload/headers are bound and read as JSON text
  (`$n::jsonb` / `::text`) so the adapter needs no optional tokio-postgres
  type features.

## Testing

SQL/DDL strings, identifier validation, DSN normalisation, the advisory-key
fold (cross-checked against pyfly's exact arithmetic), and payload/header
round-trips are unit-tested with no database. The end-to-end publish →
NOTIFY-driven drain → cursor-advance round trip is **env-gated**: it reads
`FIREFLY_TEST_POSTGRES_URL` (falling back to `DATABASE_URL` / `POSTGRES_URL`),
skips with a one-line notice when unset, and runs the genuine outbox
INSERT + NOTIFY/LISTEN → consume round-trip when set (a per-test consumer
group, NOTIFY channel, and event type keep it parallel-safe):

```sh
FIREFLY_TEST_POSTGRES_URL='postgres://firefly:firefly@localhost:5432/firefly' \
  cargo test -p firefly-eda-postgres
```
