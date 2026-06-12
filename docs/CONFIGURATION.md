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

## Placeholder resolution (PyFly parity)

`load` / `bind` run a post-merge pass that resolves `${...}`
placeholders inside values (also available standalone as
`resolve_placeholders(&flat)`):

| Syntax              | Resolves to                                                       |
|---------------------|-------------------------------------------------------------------|
| `${ENV_VAR}`        | the literal environment variable named `ENV_VAR`                  |
| `${app.name}`       | another config key (kebab/snake segments are interchangeable), resolved recursively with a depth-10 guard against cycles |
| `${key:default}`    | `default` when neither environment nor config resolves `key`      |

Environment beats config: `${app.name}` honors `FIREFLY_APP_NAME`
(a leading `firefly.` segment is stripped, dots/dashes → `_`) before
falling back to the merged map. An unresolvable placeholder with no
default raises `ConfigError::Placeholder`.

Keys are also normalized **kebab ↔ snake** (`-` → `_`, lower-cased), so
`graceful-timeout:` in YAML binds a `graceful_timeout` serde field.

## Runtime reload / refresh

```rust
let cfg: ReloadableConfig<AppCfg> = ReloadableConfig::load(sources)?;
let snapshot = cfg.get();              // Arc<AppCfg>
let mut rx = cfg.subscribe();          // tokio watch receiver
let changed: Vec<String> = cfg.reload()?; // changed top-level keys, sorted
```

`ReloadableConfig<T>` replays the full merge → placeholder-resolution →
bind pipeline and atomically swaps the snapshot; a failed reload keeps
the previous snapshot. The object-safe `Refresher` trait
(`refresh() -> Result<Vec<String>, ConfigError>`) is what an actuator
`POST /actuator/refresh` endpoint wires up —
`Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>`, and the
actuator `/actuator/refresh` endpoint returns `{"refreshed": [keys…]}`.

## Property-source introspection + masking

`Layered::property_sources()` returns ordered `PropertySourceView`s
(highest precedence first, Spring `/actuator/env` style): a synthetic
`systemEnvironment` source leads, then the chain's sources in reverse
merge order, each property carrying `{value, origin}`. Values are
sanitized by the public `mask` module (Spring Boot `Sanitizer` parity):
keys naming secrets (`password`, `secret`, `token`, `credential`,
`*key`, …) mask wholesale to `******`; URI-shaped values get the
userinfo password redacted (`postgresql://user:******@host`). This is
what backs `/actuator/env`.

## Multi-profile + config server

`active_profiles("dev")` reads a **comma-separated** `FIREFLY_PROFILE`
(`dev,cloud` → `["dev", "cloud"]`); `multi_profile_sources` overlays one
`application-{p}.yaml` per profile in order. A `ConfigClient`
(`fetch_source()` / `fetch_source_or_empty()`) pulls a Spring-Cloud-Config
`/{app}/{profile}/{label}` document into a `StaticSource` you insert into
the layered chain (above defaults, below env/flags); a non-2xx response
soft-misses to an empty map.

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
| `firefly.cache.adapter=redis`           | `firefly_cache_redis::RedisAdapter::connect(url)` (real adapter — see the **Redis** section below) |
| `firefly.cache.adapter=postgres`        | `firefly-cache-postgres` (Postgres key/value table with TTL — **port pending**) |
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
| `firefly.eda.broker=kafka`        | `firefly_eda_kafka::new_kafka_broker(KafkaConfig { .. })` (real transport) |
| `firefly.eda.broker=rabbitmq`     | `firefly_eda_rabbitmq::RabbitMqBroker::new(..)` (real transport)           |
| `firefly.eda.broker=postgres`     | `firefly_eda_postgres::PostgresBroker::new(..)` (durable outbox)           |
| `firefly.eda.broker=redis`        | `firefly_eda_redis::new_redis_broker(RedisConfig { .. })` (Redis Streams)  |
| `firefly.eda.kafka.brokers`       | `KafkaConfig.brokers`                                  |
| `firefly.eda.rabbitmq.url`        | `RabbitMqConfig.url`                                   |

See the **Message brokers** section below for each transport's full
connection-config surface. When `firefly.eda.broker` selects a
transport but the corresponding crate is not linked,
`firefly_eda::new_kafka_broker` / `new_rabbitmq_broker` return the typed
`EdaError::{KafkaUnavailable, RabbitMqUnavailable}` sentinels so the
deployment fails loud at startup.

### IDP — `firefly_idp::Adapter`

| Java key                              | Rust wiring                                       |
|---------------------------------------|---------------------------------------------------|
| `firefly.idp.adapter=internal-db`     | `firefly_idp_internal_db` adapter + `Config { .. }` |
| `firefly.idp.adapter=keycloak`        | `firefly-idp-keycloak` (real: OIDC + admin REST)  |
| `firefly.idp.adapter=azure-ad`        | `firefly-idp-azure-ad` (real: Microsoft Graph)    |
| `firefly.idp.adapter=aws-cognito`     | `firefly-idp-aws-cognito` (real: JSON API + SigV4)|
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

## HTTP server — `firefly_web::ServerProperties`

`ServerProperties` is serde-bound under the `server.*` prefix (the
Spring Boot `server.*` namespace), feeding `firefly_web::Server::bind` /
`serve`, which drops into `Application::on_server`:

| Key                                  | Field                          | Default        |
|--------------------------------------|--------------------------------|----------------|
| `server.host`                        | `host`                         | `0.0.0.0`      |
| `server.port`                        | `port`                         | `8080`         |
| `server.graceful-timeout`            | `graceful_timeout`             | drain window   |
| `server.keep-alive-timeout`          | `keep_alive_timeout`           | —              |
| `server.backlog`                     | `backlog` (socket2 listen backlog) | —          |
| `server.max-concurrent-connections`  | `max_concurrent_connections` (`tower` `ConcurrencyLimitLayer`) | — |
| `server.tls.cert-file`               | `tls.cert_file` (`TlsConfig`)  | (plain HTTP)   |
| `server.tls.key-file`                | `tls.key_file`                 | (plain HTTP)   |

When `server.tls.*` is set the listener terminates TLS via
`axum-server`'s `tls-rustls`; otherwise it serves plain HTTP.

## CORS / security headers / CSRF — `firefly_web` layers

These layers are serde-bound config structs, applied as explicit
`tower::Layer`s; field names are kebab-case under their respective
prefixes.

`CorsConfig` (`CorsLayer`):

| Field               | Notes                                                       |
|---------------------|-------------------------------------------------------------|
| `allowed-origins`   | `*` by default; reflected when `allow-credentials`          |
| `allowed-methods`   | `GET` by default; `permit_defaults()` = `GET`/`HEAD`/`POST` |
| `allowed-headers`   | `*` by default                                              |
| `allow-credentials` | reflect origin instead of `*`                               |
| `exposed-headers`   | `Access-Control-Expose-Headers`                             |
| `max-age`           | preflight cache seconds (default `600`)                     |

`SecurityHeadersConfig` (`SecurityHeadersLayer`) — same 7 fields/defaults
as pyfly: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
HSTS, `X-XSS-Protection: 0`, `Referrer-Policy`, optional CSP and
Permissions-Policy.

`CsrfLayer` — double-submit cookie: `XSRF-TOKEN` cookie vs
`X-XSRF-TOKEN` header, safe-method pass-through with cookie refresh,
`Authorization: Bearer` bypass, timing-safe compare, `403 problem+json`
on mismatch. (A second `CsrfLayer` with the same semantics ships in
`firefly-security` for the OAuth2 stack — see
[`crates/security/README.md`](../crates/security/README.md).)

## Message brokers — `firefly-eda-*`

Each transport is configured by its own struct, handed to the broker
constructor. The `firefly.eda.broker` key (above) selects which one the
starter builds; the per-transport fields:

**Kafka** (`firefly_eda_kafka::KafkaConfig` — same shape as
`firefly_eda::KafkaConfig`):

| Field            | Notes                                                  |
|------------------|--------------------------------------------------------|
| `brokers`        | bootstrap server list (`firefly.eda.kafka.brokers`)    |
| `client_id`      | producer/consumer client id                            |
| `consumer_group` | consumer-group id                                      |
| `with_property`  | escape hatch for arbitrary `librdkafka` keys (`acks`, SASL, `auto.offset.reset`, …) |

**RabbitMQ** (`firefly_eda_rabbitmq::RabbitMqBrokerConfig`, builder):

| Builder          | Default                                  |
|------------------|------------------------------------------|
| `with_url`       | `amqp://guest:guest@localhost/`          |
| `with_exchange`  | `pyfly` (durable `direct`)               |
| `with_destinations` | `["pyfly.events"]`                    |
| `with_group`     | `pyfly-default` (→ queue `<group>.<destination>`) |

**Postgres outbox** (`firefly_eda_postgres::PostgresConfig`, builder):

| Builder          | Notes                                              |
|------------------|----------------------------------------------------|
| `new(dsn)`       | libpq DSN (`postgresql+asyncpg://` etc. normalized)|
| `listen_dsn`     | dedicated `LISTEN` connection (defaults to `dsn`)  |
| `channel`        | `pg_notify` channel (identifier-validated)         |
| `destinations`   | event topics to drain                              |
| `group`          | consumer group (folds to the advisory-lock key)    |
| `poll_interval`  | fallback drain cadence                             |

**Redis Streams** (`firefly_eda_redis::RedisConfig::new(url)`, builder):

| Field         | Default              |
|---------------|----------------------|
| `url`         | (required)           |
| `streams`     | `["firefly.events"]` |
| `group`       | `firefly-default`    |
| `consumer_id` | machine hostname     |
| `block_ms`    | `5000`               |
| `count`       | `10`                 |

## Redis — `firefly-cache-redis` / `firefly-eda-redis`

The Redis cache adapter is constructed from a URL:

```rust,ignore
let adapter = Arc::new(
    firefly_cache_redis::RedisAdapter::connect("redis://127.0.0.1:6379/0").await?,
);
```

or `RedisAdapter::from_connection(conn)` to inject a pre-built
multiplexed connection (the DI entry point). It drops in wherever an
`Arc<dyn cache::Adapter>` is expected. The Redis Streams *transport*
shares the same URL shape via `RedisConfig::new(url)` above.

## SMTP email — `firefly_notifications_smtp::SmtpConfig`

| Field      | Default | Notes                                  |
|------------|---------|----------------------------------------|
| `host`     | —       | SMTP server host                       |
| `port`     | `587`   | submission port                        |
| `username` | `None`  | SMTP AUTH user (credentials attached only when both user + password present) |
| `password` | `None`  | SMTP AUTH password                     |
| `use_tls`  | `true`  | STARTTLS                               |

`SmtpEmailProvider::from_config(get)` parses these from flat config
keys; `SmtpEmailProvider::new(SmtpConfig { .. })` takes them directly.
It implements both `EmailProvider` and a thin
`firefly_notifications::Channel` (name `notificationssmtp`).

## Admin dashboard — `firefly.admin.*`

`AdminConfig` / `AdminServerConfig` / `AdminClientConfig`
(`firefly-admin`) bind from a `firefly-config` document, mirroring
pyfly's `AdminProperties` family.

| Key                                  | Field            | Default          |
|--------------------------------------|------------------|------------------|
| `firefly.admin.enabled`              | `enabled`        | `true`           |
| `firefly.admin.path`                 | `path`           | `/admin`         |
| `firefly.admin.title`                | `title`          | `Firefly Admin`  |
| `firefly.admin.theme`                | `theme`          | `auto`           |
| `firefly.admin.require-auth`         | `require_auth`   | `false`          |
| `firefly.admin.allowed-roles`        | `allowed_roles`  | `["ADMIN"]`      |
| `firefly.admin.refresh-interval`     | `refresh_interval` (ms) | `5000`    |
| `firefly.admin.server.enabled`       | server-mode instance registry | `false` |
| `firefly.admin.server.poll-interval` | `poll_interval` (ms) | `10000`      |
| `firefly.admin.server.connect-timeout` | `connect_timeout` (ms) | `2000`   |
| `firefly.admin.server.read-timeout`  | `read_timeout` (ms) | `5000`        |
| `firefly.admin.server.instances`     | seeded `InstanceConfig` list (`name` + `url` + `metadata`) | `[]` |
| `firefly.admin.client.url`           | remote admin server to register with | `""`  |
| `firefly.admin.client.auto-register` | self-register on lifecycle start | `false` |

When `require_auth` is on, every `/admin/api/*` route is guarded by a
`firefly-security` `Authentication` carrying one of `allowed_roles`.

## Config server

[`firefly-config-server`](../crates/config-server/README.md) serves the
Spring-Cloud-Config-compatible REST shape, so a Java, .NET, Go, Python,
or Rust service can pull its environment from the same endpoint. A
pulled environment is just another `Source` in the layered chain.
