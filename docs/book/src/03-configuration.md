# Configuration

`firefly-config` brings Spring Boot–style **typed, layered configuration
binding** to Rust. You declare a `serde`-deserializable struct, call
`load`/`load_from_profile`, and the loader merges sources in precedence order,
resolves the active profile, resolves `${...}` placeholders, and binds the flat
dot-keyed map onto your struct.

> **Spring parity** — This is `@ConfigurationProperties` + the
> `application.yaml` → profile → environment hierarchy, re-expressed for Rust.

## Defining configuration

A configuration struct is plain `serde`. Nested structs become nested
dot-keyed sections (`web.port`, `cache.adapter`):

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Web {
    port: u16,
}

#[derive(Debug, Deserialize)]
struct Cache {
    adapter: String,
    ttl: i64,
}

#[derive(Debug, Deserialize)]
struct AppCfg {
    web: Web,
    cache: Cache,
    tags: Vec<String>,
}
```

The binder is **type-driven**: `"9090"` binds onto a `u16`, `"alpha,beta"`
splits onto a `Vec<String>`, `"true"` parses onto a `bool`, and missing keys
produce zero values — so plain `#[derive(Deserialize)]` structs bind without
`#[serde(default)]`.

## Loading with profiles

The canonical helper reads `application.yaml`, then the profile-specific
`application-{profile}.yaml`, then `FIREFLY_*` environment variables:

```rust,ignore
use firefly_config::{load_from_profile, ConfigError};

fn main() -> Result<(), ConfigError> {
    // dir, app basename, fallback profile (FIREFLY_PROFILE overrides).
    let cfg: AppCfg = load_from_profile("/etc/orders", "application", "dev")?;
    println!("listening on :{}", cfg.web.port);
    Ok(())
}
```

`FIREFLY_PROFILE` selects the profile file at runtime — `FIREFLY_PROFILE=prod`
reads `application-prod.yaml`. A comma-separated value
(`FIREFLY_PROFILE=dev,cloud`) overlays one file per profile in order.

## Source precedence

`Layered::new(vec![s1, s2, ...])` merges from left to right — **last write
wins**. The canonical chain is:

| Order | Source                                          | Beats        |
|-------|-------------------------------------------------|--------------|
| 1     | Defaults — `StaticSource::new(name, entries)`   | nothing      |
| 2     | Base YAML — `from_optional_yaml("application.yaml")` | defaults |
| 3     | Profile YAML — `from_optional_yaml("application-prod.yaml")` | base |
| 4     | Environment — `from_env("FIREFLY")`             | YAML files   |
| 5     | CLI flags — `FlagSource::new().set("web.port", "9090")` | everything |

So an environment override (`FIREFLY_WEB_PORT=9090`) always beats a YAML file,
and a CLI override always beats both. Build the chain explicitly when you need
the full control:

```rust,ignore
use firefly_config::{from_env, from_optional_yaml, load, Source, StaticSource};

let sources: Vec<Box<dyn Source>> = vec![
    Box::new(StaticSource::new("defaults", [("web.port".into(), "8080".into())])),
    Box::new(from_optional_yaml("application.yaml")),
    Box::new(from_env("FIREFLY")),
];
let cfg: AppCfg = load(&sources)?;
```

## YAML subset and value rules

Files are parsed by a line-by-line YAML-subset scanner (no general-purpose YAML
dependency), so the flattened output is identical across the Java/Go/Rust ports
for any given `application.yaml`:

```yaml
web:
  port: 8080
cache:
  adapter: memory
  ttl: 60000
tags: alpha, beta, gamma   # sequences of scalars are comma-joined
```

- nested mappings become dot-joined, lower-cased keys;
- scalar lexemes are preserved verbatim until the binder parses them against the
  target field type;
- duplicate keys follow last-write-wins;
- aliases, anchors, multi-doc, tags, and flow sequences are deliberately not
  interpreted.

Supported leaf kinds: `String`, `bool` (Go `ParseBool` syntax), every integer
width, `f32`/`f64`, `char`, unit enums (by variant name), `Option<T>`, sequences
of scalars, and `HashMap<String, _>` subtrees. For durations, use an `i64` field
plus a conversion: `Duration::from_millis(cfg.cache.ttl as u64)`.

## Placeholders

`load` / `bind` run a post-merge pass resolving `${...}` placeholders in values
(also exposed standalone as `resolve_placeholders(&flat)`):

```yaml
app:
  name: orders
datasource:
  url: ${DATABASE_URL:postgres://localhost/orders}   # env, else default
  pool: ${app.name}-pool                              # config reference
```

- `${ENV_VAR}` — a literal environment variable;
- `${app.name}` — a config reference, resolved recursively with a depth-10 guard
  against cycles;
- `${key:default}` — a fallback when neither environment nor config resolves
  `key`;
- **environment beats config**: `${app.name}` honours `FIREFLY_APP_NAME` before
  the merged map.

An unresolvable placeholder without a default raises `ConfigError::Placeholder`.

> **Note** — Keys are normalized kebab ↔ snake, so `graceful-timeout:` in YAML
> binds a `graceful_timeout` serde field.

## Runtime reload — the `/actuator/refresh` contract

`ReloadableConfig<T>` replays the full merge → placeholder-resolution → bind
pipeline and atomically swaps the snapshot; a failed reload keeps the previous
snapshot. This is the hook a `POST /actuator/refresh` endpoint wires up.

```rust,ignore
use firefly_config::ReloadableConfig;

let cfg: ReloadableConfig<AppCfg> = ReloadableConfig::load(sources)?;
let snapshot = cfg.get();              // Arc<AppCfg>
let mut rx = cfg.subscribe();          // tokio watch receiver
let changed: Vec<String> = cfg.reload()?; // sorted, changed top-level keys
```

`Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>` — the object-safe
trait the actuator refresh endpoint depends on.

## Property-source introspection and masking

`Layered::property_sources()` returns ordered, origin-attributed
`PropertySourceView`s (highest precedence first, Spring `/actuator/env` style),
with secrets masked: keys naming secrets (`password`, `secret`, `token`,
`credential`, `*key`) mask as `******`, and URI userinfo passwords are redacted
(`postgresql://user:******@host`). The `mask` module exposes `mask_value`,
`is_sensitive_key`, and `sanitize_uri` directly.

## Profile expressions

`accepts_profiles(&active, &exprs)` evaluates the Spring Boot 2.4+
profile-expression grammar against an active-profile list:

```rust,ignore
use firefly_config::{accepts_profiles, active_profiles};

let active = active_profiles("dev");                  // e.g. ["prod", "cloud"]
accepts_profiles(&active, &["prod & cloud"]);         // AND
accepts_profiles(&active, &["prod | qa"]);            // OR
accepts_profiles(&active, &["!test"]);                // negation
accepts_profiles(&active, &["(prod & cloud) | qa"]);  // grouping
```

It returns `true` when any expression matches; a malformed expression evaluates
to `false` (it never panics).

## In-process application events

`ApplicationEventBus` is a **synchronous, in-process, `TypeId`-dispatched,
`@order`-sorted** pub/sub — Spring's `ApplicationEventPublisher` model. This is
distinct from the asynchronous `firefly-eda` broker (no transport, no topics;
listeners run on the publishing thread):

```rust,ignore
use firefly_config::{ApplicationEventBus, ApplicationReadyEvent};

let bus = ApplicationEventBus::new();
bus.subscribe::<ApplicationReadyEvent, _>(|_e| { /* on ready */ });
bus.publish(&ApplicationReadyEvent);
```

Lifecycle events ship: `ContextRefreshedEvent`, `ApplicationReadyEvent`,
`ContextClosedEvent`. Any `'static` type can be published as a domain event.

## Pulling config from a config server

`ConfigClient` fetches a Spring-Cloud-Config document and flattens it into a
`StaticSource` you slot into your chain above the defaults:

```rust,ignore
use firefly_config::ConfigClient;

let remote = ConfigClient::new("http://config:8888", "orders")
    .with_profile("prod")
    .with_label("main")
    .with_basic_auth("user", "pass")
    .fetch_source()           // fail-fast; .fetch_source_or_empty() = soft fallback
    .await?;
sources.insert(1, Box::new(remote)); // above defaults, below env/flags
```

A non-2xx response from the server logs a warning and yields an empty map (soft
miss); transport or decode failures raise `ConfigError::Remote`. The standalone
config server itself lives in [`firefly-config-server`](./91-appendix-modules.md).

The next chapter covers how to wire your application's components together —
both explicitly and with the optional DI container. See
[Dependency Wiring](./04-dependency-wiring.md).
