# `firefly-eda-rabbitmq`

> **Tier:** Platform · **Status:** Full (pyfly parity) · **Python original:** `pyfly.eda.adapters.rabbitmq.RabbitMqEventBus`

## Overview

`firefly-eda-rabbitmq` is the RabbitMQ transport for the
[`firefly-eda`](../eda) ports. It implements `Publisher` / `Subscriber`
/ `Broker` over [`lapin`](https://docs.rs/lapin) and is the registered
adapter the EDA starter calls in place of `firefly_eda::new_rabbitmq_broker`'s
`EdaError::RabbitMqUnavailable` sentinel when the configuration selects
RabbitMQ.

It is a faithful port of pyfly's `RabbitMqEventBus`: same topology, same
at-least-once delivery contract, same `fnmatch`-style subscription
matching, same wire format (the JSON-encoded `Event` envelope).

## Topology

On `RabbitMqBroker::start` the broker declares:

- one **durable `direct` exchange** (default `pyfly`), and
- one **durable queue `<group>.<destination>`** per configured
  destination, bound to the exchange with `<destination>` as the routing
  key and consumed with **manual ack**.

The publishing channel enables **publisher confirms**, so `publish`
resolves only once the broker has accepted the message.

`RabbitMqBrokerConfig::declaration_plan` exposes this topology as data
(`DeclarationPlan` / `ExchangeDeclaration` / `QueueDeclaration`) so the
declaration set is assertable in a unit test without a live broker.

## Delivery semantics (at-least-once)

`dispatch` / `decide` reproduce pyfly's `on_message` outcomes exactly:

| Outcome                       | pyfly call                      | AMQP action               |
|-------------------------------|---------------------------------|---------------------------|
| handled, or no matching pattern | `message.ack()`               | `basic_ack`               |
| a matching handler returns `Err` | `message.reject(requeue=True)` | `basic_nack(requeue=true)` |
| body could not deserialize    | `message.reject(requeue=False)` | `basic_reject(requeue=false)` |

A non-match is not a failure (the message is consumed). All matching
handlers run even when an earlier one fails — only the aggregate outcome
flips to nack-with-requeue.

## Pattern subscription

`Subscriber::subscribe(topic, handler)` registers `topic` as an
`fnmatch`-style pattern (`*`, `?`, `[...]`, with `!`-negated classes)
tested against the event's `type` — pyfly's
`subscribe(event_type_pattern, handler)`. `*` spans any character
including `.`, so `order.*` matches `order.created.v2`. Patterns added
after `start` take effect on the next delivery; every consumer reads the
shared subscription list per message. Use `pattern_matches` to test the
matcher directly.

## Quick start

```rust,no_run
use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_rabbitmq::{RabbitMqBroker, RabbitMqBrokerConfig};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let broker = RabbitMqBroker::new(
    RabbitMqBrokerConfig::default()
        .with_url("amqp://guest:guest@localhost:5672/")
        .with_destinations(["orders"])
        .with_group("svc"),
);

broker
    .subscribe(
        "order.*",
        handler(|ev: Event| async move {
            println!("got {}", ev.event_type);
            Ok(())
        }),
    )
    .await?;
broker.start().await?;

let ev = Event::new("orders", "order.created", "orders-svc", Some(b"{}".to_vec()));
broker.publish(ev).await?;
# Ok(())
# }
```

## pyfly parity

Ported surface and behavior, mapped from `pyfly.eda.adapters.rabbitmq`:

- `RabbitMqEventBus(url, exchange_name, destinations, group)` →
  `RabbitMqBroker::new(RabbitMqBrokerConfig)` with `with_url` /
  `with_exchange` / `with_destinations` / `with_group` builders
  (defaults `amqp://guest:guest@localhost/`, `pyfly`, `["pyfly.events"]`,
  `pyfly-default`).
- `start` is idempotent and closes a half-open connection on a declare
  failure; `stop` is safe to call when never started.
- `publish` auto-starts the broker, routes on `destination`
  (= `Event.topic`), and awaits the publisher confirm.
- Python decorators / DI map to explicit `subscribe` and `publish`
  calls, the Rust idiom.

Tests port pyfly's `tests/eda/test_rabbitmq_event_bus.py`: the
declaration plan (exchange + one bound queue per destination,
`<group>.<destination>` names), routing-key/envelope mapping, matching
vs non-matching dispatch, the undeserializable-body drop, and the
handler-failure nack-with-requeue. A live declare → publish → consume → ack
round-trip lives in `tests/roundtrip.rs` as an **env-gated** integration test
(no `#[ignore]`): it reads `FIREFLY_TEST_RABBITMQ_URL` (falling back to the
legacy `RABBITMQ_URL` then `AMQP_URL`) and skips with a one-line notice when
unset, so a bare `cargo test` stays green.
```sh
cargo test -p firefly-eda-rabbitmq            # unit + doc tests, round-trip skips (no broker)
FIREFLY_TEST_RABBITMQ_URL=amqp://guest:guest@localhost:5672/%2f \
  cargo test -p firefly-eda-rabbitmq          # runs the real round-trip
```
