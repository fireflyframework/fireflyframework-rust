# `firefly-eda-redis`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-eda-redis` is a **Redis Streams transport** for the Firefly
[`firefly-eda`](../eda) event-driven architecture.

`RedisStreamsBroker` implements `firefly_eda::Publisher` and
`firefly_eda::Subscriber` (and therefore `firefly_eda::Broker`) over the
[`redis`](https://crates.io/crates/redis) crate's async multiplexed
connection, using consumer groups for competing-consumer delivery:

- **subscribe** registers a glob topic pattern + handler;
- **publish** `XADD`s `{envelope: <json>}` to the stream named by the
  event's `topic`;
- **start** issues `XGROUP CREATE … MKSTREAM` per configured stream
  (tolerating the `BUSYGROUP` error a pre-existing group raises) and
  spawns an `XREADGROUP … BLOCK` consume loop;
- the loop dispatches each entry to matching handlers, `XACK`s on
  success, and **leaves the entry pending (unacked) on handler error**
  so Redis redelivers it later — at-least-once delivery achieved by
  skipping the `XACK`.

The on-stream record uses the field name `envelope` carrying the
`firefly_eda::Event` JSON, so any producer and consumer that speak this
record format interoperate.

## Usage

```rust,no_run
use firefly_eda::{handler, Event, Publisher};
use firefly_eda_redis::{RedisConfig, RedisStreamsBroker};

# async fn run() -> firefly_eda::EdaResult<()> {
let broker = RedisStreamsBroker::connect(
    RedisConfig::new("redis://localhost:6379/0")
        .with_streams(["orders"])
        .with_group("orders-svc"),
)?;

broker
    .subscribe(
        "orders.*", // glob over the event topic
        handler(|ev: Event| async move {
            println!("got {}", ev.event_type);
            Ok(())
        }),
    )
    .await?;
broker.start().await?;

broker
    .publish(Event::new("orders.created", "OrderCreated", "orders-svc", None))
    .await?;

Publisher::close(&broker).await?;
# Ok(())
# }
```

The starter selects this transport through the factory
`new_redis_broker(config) -> EdaResult<Box<dyn Broker>>`, paralleling
`firefly_eda::new_kafka_broker`.

## Configuration

`RedisConfig::new(url)` applies sensible defaults; every field has a
builder:

| Field         | Default            |
|---------------|--------------------|
| `url`         | (required)         |
| `streams`     | `["firefly.events"]` |
| `group`       | `"firefly-default"`  |
| `consumer_id` | machine hostname   |
| `block_ms`    | `5000`             |
| `count`       | `10`               |

## Delivery semantics

The transport drives the full Redis Streams consumer-group lifecycle:
`XGROUP CREATE … MKSTREAM` with `BUSYGROUP` tolerance, `XADD` publish,
`XREADGROUP` block loop, `XACK` on success, and leave-pending on handler
error. `publish` auto-starts the broker on first use, so events produced
before the first `subscribe` are not lost. Poison entries — missing the
`envelope` field or carrying undeserializable bytes — are `XACK`-ed and
skipped (logged), never redelivered forever.

Handler patterns match against the envelope `topic`, consistent with the
`firefly_eda::Subscriber` contract shared by every Firefly transport
(including `InMemoryBroker`). Glob patterns (`*`, `?`, `[..]`, `{a,b}`)
are honored via `globset`. Events carry the canonical wire-compatible
`Event` JSON serde used across Firefly.

## Testing

The test suite runs against an **in-process fake RESP2 server** on an
ephemeral `TcpListener` (`tests/common/mod.rs`, ~250 lines) implementing
only the commands the broker uses (`CLIENT SETINFO`, `XGROUP CREATE …
MKSTREAM`, `XADD`, `XREADGROUP`, `XACK`). The full lifecycle — connect →
group create → publish → consume → ack / leave-pending — is exercised
over a real socket with **no external Redis** and no test sleeping more
than a fraction of a second. Round-trips against a live Redis are out of
scope for the unit suite.
```bash
cargo test -p firefly-eda-redis
```
