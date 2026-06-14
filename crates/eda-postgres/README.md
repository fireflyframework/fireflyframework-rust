# firefly-eda-postgres

A Postgres-backed [`Broker`](../eda) for Firefly EDA: a durable
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

## API and behavior

- **Builder configuration.** `PostgresBroker::new(PostgresConfig::new(dsn)
  .listen_dsn(…).channel(…).destinations(…).group(…).poll_interval(…))`
  drives all setup. `new` panics on an invalid channel identifier; the
  fallible `PostgresBroker::try_new` returns an `IdentError` instead, backed
  by `quote_ident` channel validation.
- **Subscriptions.** `subscribe_pattern(pattern, handler)` (and the
  `Subscriber::subscribe` trait method, which treats its `topic` argument as
  the glob) match a `globset` glob against the event's `event_type`.
- **Advisory key.** `group_lock_key` folds the consumer-group name with
  SHA-256 and reads `i64::from_be_bytes` of the first 8 digest bytes, giving
  a stable signed-`i64` advisory-lock key per group.
- **DSN normalisation.** `normalise_dsn` strips
  `postgresql+asyncpg://` / `postgresql+psycopg://` / `postgres+asyncpg://`
  scheme prefixes before connecting.
- **Lifecycle.** `start` runs DDL, seeds offsets, opens the listener, and
  spawns the drain loop; `close` aborts the loop and releases the advisory
  lock.

### Implementation notes

- **Table naming.** Tables are `firefly_eda_outbox` / `firefly_eda_offsets`.
  The column layout is stable JSONB, so any producer and consumer
  interoperate when both target the same table name.
- **Hashing.** The advisory key uses SHA-256 from the workspace dependency
  catalog.
- **No connection pool.** A single pipelined `tokio-postgres::Client`
  handles publishing and draining — tokio-postgres pipelines concurrent
  queries on one connection — while a dedicated session connection runs
  `LISTEN`.
- **JSONB as text.** Payload/headers are bound and read as JSON text
  (`$n::jsonb` / `::text`) so the adapter needs no optional tokio-postgres
  type features.

## Testing

SQL/DDL strings, identifier validation, DSN normalisation, the advisory-key
fold, and payload/header
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
