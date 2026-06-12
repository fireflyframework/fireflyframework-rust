# Configuration

Unlike the Go port's first release, the Rust port ships a full typed
configuration loader from day one: [`firefly-config`](../crates/config/README.md)
brings Spring Boot-style layered binding — YAML files, environment
variables, profile selection — onto plain `serde`-deserializable
structs.

This document covers the loader itself, and then the canonical mapping
from the Java `application.yml` keys onto Rust wiring, so operators can
translate ops-runbooks across runtimes without re-learning the field
names.

## The loader — `firefly-config`

Declare a struct, list your sources, call `load`:

```rust
use firefly_config::{from_env, from_optional_yaml, load, Source};
use serde::Deserialize;

#[derive(Deserialize)]
struct Web { port: u16, host: String }

#[derive(Deserialize)]
struct Cache { adapter: String, ttl: i64 }

#[derive(Deserialize)]
struct AppCfg { web: Web, cache: Cache, tags: Vec<String> }

let sources: Vec<Box<dyn Source>> = vec![
    Box::new(from_optional_yaml("application.yaml")),
    Box::new(from_env("FIREFLY")),
];
let cfg: AppCfg = load(&sources)?;
```

The binder is type-driven: `"9090"` binds onto an integer field,
`"alpha,beta"` splits onto a `Vec<String>`, `"true"` parses onto a
`bool` (Go `strconv.ParseBool` acceptance set), and missing keys
produce zero values — plain `#[derive(Deserialize)]` structs bind
without `#[serde(default)]`. Supported leaf kinds: `String`, `bool`,
all integer widths, `f32`/`f64`, `char`, unit enums, `Option<T>`,
sequences of scalars (comma-separated), and `HashMap<String, _>`
subtrees. Durations travel as an `i64` field plus your own conversion
(`Duration::from_millis(cfg.timeout_ms as u64)`), matching the Go port.

## Source precedence

Sources merge left to right — **last write wins**. The canonical chain:

| Order | Source                       | Constructor                                  |
|-------|------------------------------|----------------------------------------------|
| 1     | Defaults                     | `StaticSource::new(name, entries)`           |
| 2     | Base YAML                    | `from_optional_yaml("application.yaml")`     |
| 3     | Profile YAML                 | `from_optional_yaml("application-prod.yaml")`|
| 4     | Environment                  | `from_env("FIREFLY")`                        |
| 5     | CLI / programmatic flags     | `FlagSource::new()` + `.set(key, value)`     |

So an environment override always beats a YAML file, and a flag
override always beats both.

Environment mapping: `FIREFLY_WEB_PORT` → `web.port` — prefix stripped,
underscores become dots, lower-cased.

## Profiles

`FIREFLY_PROFILE` selects the profile-specific YAML file. The
convenience helper builds the whole chain:

```rust
let cfg: AppCfg = load_from_profile("/etc/orders", "application", "dev")?;
```

reads `application.yaml`, then
`application-{FIREFLY_PROFILE, falling back to "dev"}.yaml`, then
`FIREFLY_*` environment variables. `active_profile(fallback)` and
`profile_sources(dir, app, profile)` expose the pieces individually.

YAML files are flattened to the same dot-keyed shape the Go port's
scanner produces: nested mappings become dot-joined lower-cased keys,
sequences of scalars are comma-joined. Sequences containing mappings
are rejected — the configuration contract is "sequences of scalars
only", exactly as documented for the Go module.

## Wiring it into a service

`firefly-config` produces values; the starter consumes them. The
pattern is: bind your `AppCfg`, then build the `CoreConfig`:

```rust
use firefly_config::load_from_profile;
use firefly_starter_core::{Core, CoreConfig};

let app: AppCfg = load_from_profile(".", "application", "dev")?;
let core = Core::new(CoreConfig {
    app_name: app.name.clone(),
    ..CoreConfig::default()
});
```

## Java key → Rust wiring

### Top level — `firefly_starter_core::CoreConfig`

| Java key               | Rust field / wiring                          |
|------------------------|----------------------------------------------|
| `firefly.app.name`     | `CoreConfig.app_name`                        |
| `firefly.app.version`  | `CoreConfig.app_version`                     |
| `firefly.starter.name` | `CoreConfig.starter_name`                    |

### Cache — `firefly_cache::Adapter`

| Java key                                | Rust wiring                                  |
|-----------------------------------------|----------------------------------------------|
| `firefly.cache.adapter=memory`          | `MemoryAdapter::new()` (default)             |
| `firefly.cache.adapter=noop`            | `NoOpAdapter`                                |
| `firefly.cache.adapter=redis`           | (next release) Redis adapter crate           |
| `firefly.cache.fallback.adapter=memory` | `FallbackAdapter::new(primary, secondary)`   |
| `firefly.cache.ttl`                     | Per-call TTL on `set` / `Typed::get_or_set`  |

### Idempotency — `firefly_web::IdempotencyConfig`

| Java key                            | Rust field / wiring                                |
|-------------------------------------|----------------------------------------------------|
| `firefly.idempotency.enabled`       | Don't apply `IdempotencyLayer`                     |
| `firefly.idempotency.ttl`           | `IdempotencyConfig.ttl` (default 24 h)             |
| `firefly.idempotency.methods`       | `IdempotencyConfig.methods` (default POST/PUT/PATCH)|
| `firefly.idempotency.store=memory`  | `MemoryIdempotencyStore` (default)                 |
| `firefly.idempotency.store=redis`   | Implement the `IdempotencyStore` trait             |

### Observability — `firefly_observability::LogConfig`

| Java key                       | Rust field                                  |
|--------------------------------|---------------------------------------------|
| `firefly.logging.level`        | `LogConfig.level`                           |
| `firefly.logging.format=json`  | `LogConfig.format = LogFormat::Json` (default) |
| `firefly.logging.format=text`  | `LogConfig.format = LogFormat::Text`        |
| `firefly.app.name`             | `LogConfig.service`                         |

### EDA — `firefly_eda::Broker`

| Java key                          | Rust wiring                                            |
|-----------------------------------|--------------------------------------------------------|
| `firefly.eda.broker=in-memory`    | `InMemoryBroker::new()` (default)                      |
| `firefly.eda.broker=kafka`        | `new_kafka_broker(KafkaConfig { .. })` — returns `EdaError::KafkaUnavailable` until the transport crate ships |
| `firefly.eda.broker=rabbitmq`     | `new_rabbitmq_broker(RabbitMqConfig { .. })` — returns `EdaError::RabbitMqUnavailable` until the transport crate ships |
| `firefly.eda.kafka.brokers`       | `KafkaConfig.brokers`                                  |
| `firefly.eda.rabbitmq.url`        | `RabbitMqConfig.url`                                   |

### IDP — `firefly_idp::Adapter`

| Java key                              | Rust wiring                                       |
|---------------------------------------|---------------------------------------------------|
| `firefly.idp.adapter=internal-db`     | `firefly_idp_internal_db` adapter + `Config { .. }` |
| `firefly.idp.adapter=keycloak`        | (next release) `firefly-idp-keycloak`             |
| `firefly.idp.internal-db.jwt.secret`  | `Config.jwt_secret`                               |
| `firefly.idp.internal-db.token.ttl`   | `Config.token_ttl` (default 1 h)                  |

### Callbacks — `firefly_callbacks::DispatcherConfig`

| Java key                                     | Rust field                       |
|----------------------------------------------|----------------------------------|
| `firefly.callbacks.dispatcher.timeout`       | `DispatcherConfig.http_client` (a tuned `reqwest::Client`) |
| `firefly.callbacks.dispatcher.retries`       | `DispatcherConfig.max_attempts`  |
| `firefly.callbacks.dispatcher.initialDelay`  | `DispatcherConfig.initial_delay` |

### Webhooks — `firefly_webhooks` pipeline

Validators are registered explicitly per provider, as in the Go port —
see [`crates/webhooks/README.md`](../crates/webhooks/README.md) for the
registration shape.

## Config server

[`firefly-config-server`](../crates/config-server/README.md) serves the
Spring-Cloud-Config-compatible REST shape, so a Java, .NET, Go, Python,
or Rust service can pull its environment from the same endpoint. A
pulled environment is just another `Source` in the layered chain.
