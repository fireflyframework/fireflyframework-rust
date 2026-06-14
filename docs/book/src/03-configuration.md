# Configuration

> By the end of this chapter Lumen reads its identity and its bind addresses
> from configuration instead of hard-coding them: the `app_name` and
> `app_version` that flow into the banner and `/actuator/info`, and the
> `LUMEN_ADDR` / `LUMEN_ADMIN_ADDR` overrides `main` already honors. You will
> also see the typed, layered, profile-aware machinery Lumen grows into as it
> moves toward production.

In the last chapter Lumen named itself with two `pub const` strings and pulled
its ports straight off the environment with `std::env::var`. That is the right
starting point — but a real wallet service runs in dev, in CI, and in prod, and
each environment wants different ports, log levels, and (eventually) database
URLs. `firefly-config` provides **typed, layered configuration binding**: you
declare a `serde`-deserializable struct, call `load`/`load_from_profile`, and the
loader merges sources in precedence order, resolves the active profile, resolves
`${...}` placeholders, and binds the flat dot-keyed map onto your struct. If
you've used a batteries-included framework before, the shape will feel familiar.

> **Design note.** Firefly binds a profile-aware `application.yaml` → profile →
> environment hierarchy onto typed structs. The flattening and binding rules are
> specified precisely (see below) so the same `application.yaml` produces the
> same keys deterministically — Firefly treats this determinism as a guarantee,
> not an accident.

## Where Lumen is today: app identity

Recall the composition root from the Quickstart:

```rust,ignore
// src/web.rs
let web = WebStack::new(firefly::starter_web::CoreConfig {
    app_name: APP_NAME.into(),       // "lumen"
    app_version: VERSION.into(),     // firefly::VERSION
    ..Default::default()
});
```

`CoreConfig` is itself plain configuration: every field is a knob, and the two
Lumen sets — `app_name` and `app_version` — are exactly the values
`/actuator/info` reports and the banner prints. The remaining fields default
(in-memory cache, in-process broker, a fresh CQRS bus), which is why a bare
`cargo run` needs no infrastructure. Promoting any of those to real
infrastructure is a one-field change you will make in
[Production & Deployment](./20-production.md); the config story in this chapter
is how those values stop being literals and start coming from files and the
environment.

## Defining configuration

A configuration struct is plain `serde`. Nested structs become nested dot-keyed
sections (`web.port`, `cache.adapter`). Here is the shape Lumen would adopt as
it outgrows the two constants:

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Web {
    /// Public API bind address — the typed home of LUMEN_ADDR.
    addr: String,
    /// Admin/actuator bind address — the typed home of LUMEN_ADMIN_ADDR.
    admin_addr: String,
}

#[derive(Debug, Deserialize)]
struct LumenConfig {
    name: String,
    web: Web,
    tags: Vec<String>,
}
```

The binder is **type-driven**: `"9090"` binds onto a `u16`, `"alpha,beta"`
splits onto a `Vec<String>`, `"true"` parses onto a `bool`, and missing keys
produce zero values — so plain `#[derive(Deserialize)]` structs bind without
`#[serde(default)]`.

## Loading with profiles

The canonical helper reads `application.yaml`, then the profile-specific
`application-{profile}.yaml`, then `LUMEN_*` environment variables:

```rust,ignore
use firefly_config::{load_from_profile, ConfigError};

fn main() -> Result<(), ConfigError> {
    // dir, app basename, fallback profile (FIREFLY_PROFILE overrides).
    let cfg: LumenConfig = load_from_profile("/etc/lumen", "application", "dev")?;
    println!("public API on {}", cfg.web.addr);
    Ok(())
}
```

`FIREFLY_PROFILE` selects the profile file at runtime — `FIREFLY_PROFILE=prod`
reads `application-prod.yaml`. A comma-separated value
(`FIREFLY_PROFILE=dev,cloud`) overlays one file per profile in order. This is
how Lumen would carry an in-memory event store in `dev` and a Postgres event
store in `prod` without a single `if` in the wiring code.

## Source precedence

`Layered::new(vec![s1, s2, ...])` merges from left to right — **last write
wins**. The canonical chain is:

| Order | Source                                          | Beats        |
|-------|-------------------------------------------------|--------------|
| 1     | Defaults — `StaticSource::new(name, entries)`   | nothing      |
| 2     | Base YAML — `from_optional_yaml("application.yaml")` | defaults |
| 3     | Profile YAML — `from_optional_yaml("application-prod.yaml")` | base |
| 4     | Environment — `from_env("LUMEN")`               | YAML files   |
| 5     | CLI flags — `FlagSource::new().set("web.addr", "0.0.0.0:80")` | everything |

So an environment override (`LUMEN_WEB_ADDR=0.0.0.0:80`) always beats a YAML
file, and a CLI override always beats both. That precedence is precisely why
`main` can read `LUMEN_ADDR` and have it win over any baked-in default. Build
the chain explicitly when you need full control:

```rust,ignore
use firefly_config::{from_env, from_optional_yaml, load, Source, StaticSource};

let sources: Vec<Box<dyn Source>> = vec![
    Box::new(StaticSource::new("defaults", [("web.addr".into(), "127.0.0.1:8080".into())])),
    Box::new(from_optional_yaml("application.yaml")),
    Box::new(from_env("LUMEN")),
];
let cfg: LumenConfig = load(&sources)?;
```

## YAML subset and value rules

Files are parsed by a line-by-line YAML-subset scanner (no general-purpose YAML
dependency), so the flattened output is deterministic and stable for any given
`application.yaml`:

```yaml
name: lumen
web:
  addr: 127.0.0.1:8080
  admin-addr: 127.0.0.1:8081
tags: wallet, ledger, demo   # sequences of scalars are comma-joined
```

- nested mappings become dot-joined, lower-cased keys;
- scalar lexemes are preserved verbatim until the binder parses them against the
  target field type;
- duplicate keys follow last-write-wins;
- aliases, anchors, multi-doc, tags, and flow sequences are deliberately not
  interpreted.

Supported leaf kinds: `String`, `bool` (accepts `1`/`0`, `t`/`f`, and
`true`/`false` in any case), every integer
width, `f32`/`f64`, `char`, unit enums (by variant name), `Option<T>`, sequences
of scalars, and `HashMap<String, _>` subtrees. For durations, use an `i64` field
plus a conversion: `Duration::from_millis(cfg.cache.ttl as u64)`.

> **Note** — Keys are normalized kebab ↔ snake, so `admin-addr:` in YAML binds
> an `admin_addr` serde field.

## Placeholders

`load` / `bind` run a post-merge pass resolving `${...}` placeholders in values
(also exposed standalone as `resolve_placeholders(&flat)`):

```yaml
name: lumen
datasource:
  url: ${DATABASE_URL:postgres://localhost/lumen}   # env, else default
  pool: ${name}-pool                                 # config reference
```

- `${ENV_VAR}` — a literal environment variable;
- `${name}` — a config reference, resolved recursively with a depth-10 guard
  against cycles;
- `${key:default}` — a fallback when neither environment nor config resolves
  `key`;
- **environment beats config**: `${name}` honors `FIREFLY_NAME` before the
  merged map.

An unresolvable placeholder without a default raises `ConfigError::Placeholder`.

## Binding config straight into a bean — `#[derive(ConfigProperties)]`

Loading a struct by hand is fine for `main`. But Lumen's services want their
configuration *injected*, not threaded through every constructor. The
`#[derive(ConfigProperties)]` macro turns a `serde` struct into a
container-managed, prefix-bound bean — the exact pattern the dependency-injection
chapter builds on:

```rust,ignore
use firefly::prelude::*;
use serde::Deserialize;

/// Binds the `lumen.web.*` config subtree into an injectable bean.
#[derive(Deserialize, ConfigProperties, Default)]
#[firefly(prefix = "lumen.web")]
pub struct WebProperties {
    pub addr: String,
    #[serde(default)]
    pub admin_addr: String,
}
```

Any `#[derive(Service)]` bean can then `#[autowired] props: Arc<WebProperties>`
and receive the bound values — no manual `load`, no global. You will wire one in
[Dependency Wiring](./04-dependency-wiring.md). For one-off scalars there is an
even lighter touch: a `#[firefly(value = "${lumen.web.addr:127.0.0.1:8080}")]`
field injects a single resolved value with a default.

> **Design note.** Firefly offers two binding styles. A prefix-bound bean
> (`#[derive(ConfigProperties)]` + `#[firefly(prefix = "...")]`) pulls a whole
> config subtree into one injectable struct, while single-value injection
> (`#[firefly(value = "${...}")]`) wires one resolved scalar onto a field. Both
> bind against the same merged, profile-resolved, placeholder-expanded map
> described above.

## Config-driven auto-configuration — datasource and security from `application.yaml`

The properties-binding machinery so far hands you a typed struct. Firefly's
infrastructure crates take the next step: a handful of subsystems are
**config-driven and DI-free** — you bind a plain `serde` struct from
`application.yaml`/env, then `await` a single auto-configure call at boot, and
the subsystem stands itself up. No container, no manual builder chains, no
`if scheme == "postgres"` branching in your wiring. Two subsystems Lumen leans
on this way are its datasource and its security layer.

Both feed off one YAML tree. `firefly.datasource.*` binds onto
`DataSourceProperties` and `firefly.security.*` onto `SecurityProperties`:

```yaml
firefly:
  datasource:
    url: ${DATABASE_URL:postgres://localhost/lumen}  # scheme picks the backend
    max-connections: 16
    min-connections: 2
    acquire-timeout-ms: 5000
    idle-timeout-ms: 600000
    max-lifetime-ms: 1800000
  security:
    jwt:
      jwk-set-uri: https://idp.example.com/.well-known/jwks.json
      issuer-uri: https://idp.example.com/
      audience: lumen-api
    bearer:
      header-name: Authorization
      allow-anonymous: false
```

### Datasource — `DataSourceProperties` → pool → transaction manager

`DataSourceProperties` is a plain `serde` struct — `{ url, max_connections,
min_connections, acquire_timeout_ms, idle_timeout_ms, max_lifetime_ms }`. The
**URL scheme selects the backend**, each behind its own cargo feature:
`postgres://` / `postgresql://` → PostgreSQL, `mysql://` → MySQL, `sqlite:` →
SQLite. A `0` for any pool setting leaves the `sqlx` default in place.

`firefly_data_sqlx::auto_configure(&props)` does the one thing you want at boot:
it builds the connection pool **and** registers a `SqlxTransactionManager` over
it, so `#[transactional]` resolves with no manual wiring. The returned `Db` is
the same pool, ready to build typed repositories. (For finer control,
`Db::connect(url)` and `Db::connect_with(&props)` build just the pool.)

```rust,ignore
use firefly_data_sqlx::{auto_configure, DataSourceProperties};

// `Db` carries the pool; auto_configure also registers the tx manager.
let db = auto_configure(&props).await?;     // Result<Db, FireflyError>
```

### Security — `SecurityProperties` → verifier → bearer layer

`SecurityProperties` nests `{ jwt: JwtProperties, bearer: BearerProperties }`.
`JwtProperties` holds `{ jwk_set_uri, issuer_uri, audience, secret, algorithm,
expiration_seconds }`; `BearerProperties` holds `{ header_name, allow_anonymous }`.
Two functions turn that into running middleware:

- `verifier_from_config(&JwtProperties)` returns
  `Result<Option<Arc<dyn Verifier>>, SecurityError>`. A non-empty `jwk_set_uri`
  builds a JWKS (RS256) resource-server verifier; otherwise a non-empty `secret`
  builds an HMAC verifier (`HS256`/`HS384`/`HS512`); otherwise `None`.
- `bearer_layer_from_config(&SecurityProperties)` returns
  `Result<Option<BearerLayer>, SecurityError>` — the ready-to-mount layer with
  the configured header name and anonymous policy already applied, or `None`
  when no verifier is configured.

### The one-call startup wiring

Bind one config struct, then drive both subsystems from it. The whole wiring is
a load plus two awaited calls:

```rust,ignore
use firefly_config::{load_from_profile, ConfigError};
use firefly_data_sqlx::{auto_configure, DataSourceProperties};
use firefly_security::{bearer_layer_from_config, SecurityProperties};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Firefly {
    datasource: DataSourceProperties,
    security: SecurityProperties,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LumenConfig {
    firefly: Firefly,   // binds the `firefly.datasource.*` / `firefly.security.*` subtree
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load + merge + profile-resolve + placeholder-expand, then bind.
    let cfg: LumenConfig = load_from_profile("/etc/lumen", "application", "dev")?;

    // 2. Build the pool AND register the transaction manager in one await.
    let db = auto_configure(&cfg.firefly.datasource).await?;

    // 3. Build the ready-to-mount bearer layer (None if no JWT settings).
    let bearer = bearer_layer_from_config(&cfg.firefly.security)?;

    // `db` builds typed repositories; mount `bearer` on the web stack.
    // ...
    Ok(())
}
```

This is the same precedence chain from earlier in the chapter doing real work:
`DATABASE_URL` in the environment overrides the YAML default for the pool, and
the JWKS endpoint can be re-pointed per profile without touching code. The
`#[transactional]` machinery and the bearer middleware both pick up what
`auto_configure` and `bearer_layer_from_config` registered — no globals threaded
through your constructors.

> **Design note.** Firefly deliberately keeps this path DI-free: the config
> structs are ordinary `serde` types and the auto-configure calls are ordinary
> `async fn`s you `await` at boot. You can adopt the full
> `#[derive(ConfigProperties)]` container later without rewriting any of it —
> the same bound values flow either way.

## Profile expressions

`accepts_profiles(&active, &exprs)` evaluates a profile-expression grammar — AND
(`&`), OR (`|`), negation (`!`), and grouping with parentheses — against an
active-profile list, useful for gating a bean that should exist only in some
environments:

```rust,ignore
use firefly_config::{accepts_profiles, active_profiles};

let active = active_profiles("dev");                  // e.g. ["prod", "cloud"]
accepts_profiles(&active, &["prod & cloud"]);         // AND
accepts_profiles(&active, &["prod | qa"]);            // OR
accepts_profiles(&active, &["!test"]);                // negation
accepts_profiles(&active, &["(prod & cloud) | qa"]);  // grouping
```

It returns `true` when any expression matches; a malformed expression evaluates
to `false` (it never panics). The dependency-injection chapter shows how a bean
declares `#[firefly(profile = "prod")]` so the container applies exactly this
rule at scan time.

## Runtime reload — the `/actuator/refresh` contract

`ReloadableConfig<T>` replays the full merge → placeholder-resolution → bind
pipeline and atomically swaps the snapshot; a failed reload keeps the previous
snapshot. This is the hook a `POST /actuator/refresh` endpoint wires up — so an
operator could re-point Lumen's datasource without a restart.

```rust,ignore
use firefly_config::ReloadableConfig;

let cfg: ReloadableConfig<LumenConfig> = ReloadableConfig::load(sources)?;
let snapshot = cfg.get();              // Arc<LumenConfig>
let mut rx = cfg.subscribe();          // tokio watch receiver
let changed: Vec<String> = cfg.reload()?; // sorted, changed top-level keys
```

`Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>` — the object-safe
trait the actuator refresh endpoint depends on.

## Property-source introspection and masking

`Layered::property_sources()` returns ordered, origin-attributed
`PropertySourceView`s (highest precedence first) — the data Firefly's
`/actuator/env` view renders, with secrets masked: keys naming secrets (`password`, `secret`, `token`,
`credential`, `*key`) mask as `******`, and URI userinfo passwords are redacted
(`postgresql://user:******@host`). The `mask` module exposes `mask_value`,
`is_sensitive_key`, and `sanitize_uri` directly. This matters for Lumen the
moment it has a JWT signing key (chapter 14) and a datasource URL — neither
should ever appear in plaintext on `/actuator/env`.

## In-process application events

`ApplicationEventBus` is an **in-process, `TypeId`-dispatched, order-sorted,
synchronous** pub/sub for lifecycle and local notification events. This is
distinct from the asynchronous `firefly-eda` broker Lumen uses for domain events
(no transport, no topics; listeners run on the publishing thread):

```rust,ignore
use firefly_config::{ApplicationEventBus, ApplicationReadyEvent};

let bus = ApplicationEventBus::new();
bus.subscribe::<ApplicationReadyEvent, _>(|_e| { /* on ready */ });
bus.publish(&ApplicationReadyEvent);
```

Lifecycle events ship: `ContextRefreshedEvent`, `ApplicationReadyEvent`,
`ContextClosedEvent`. Any `'static` type can be published as a domain event.

> **Note** — Do not confuse this with [Event-Driven Architecture](./10-eda-messaging.md):
> the `ApplicationEventBus` is a *local* lifecycle/notification channel; Lumen's
> wallet domain events ride the `firefly-eda` `Broker` over a topic, with a real
> Kafka/RabbitMQ adapter waiting behind the in-memory default.

## Pulling config from a config server

`ConfigClient` fetches a remote configuration document (compatible with the
Spring Cloud Config server wire format) and flattens it into a `StaticSource`
you slot into your chain above the defaults:

```rust,ignore
use firefly_config::ConfigClient;

let remote = ConfigClient::new("http://config:8888", "lumen")
    .with_profile("prod")
    .with_label("main")
    .with_basic_auth("user", "pass")
    .fetch_source()           // fail-fast; .fetch_source_or_empty() = soft fallback
    .await?;
sources.insert(1, Box::new(remote)); // above defaults, below env/flags
```

A non-2xx response logs a warning and yields an empty map (soft miss); transport
or decode failures raise `ConfigError::Remote`. The standalone config server
lives in [`firefly-config-server`](./91-appendix-modules.md).

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| identity hard-coded in two `pub const` strings | the same values understood as `CoreConfig` knobs that feed the banner and `/actuator/info` |
| ports read with bare `std::env::var` | the typed home of `LUMEN_ADDR` / `LUMEN_ADMIN_ADDR`, sitting at the top of a documented precedence chain |
| no path to per-environment settings | profiles, placeholders, and `#[derive(ConfigProperties)]` ready for injection in the next chapter |
| secrets unconsidered | masking + `/actuator/env` redaction in place before Lumen ever holds a signing key |

## Exercises

1. **Promote the ports to YAML.** Write an `application.yaml` with
   `web.addr` / `web.admin-addr`, load it with `load_from_profile`, and confirm
   a `LUMEN_WEB_ADDR` environment variable still wins (precedence row 4 beats
   row 2).
2. **Add a profile.** Create `application-prod.yaml` that overrides `web.addr`
   to `0.0.0.0:80`, run with `FIREFLY_PROFILE=prod`, and verify the prod value
   takes effect while `dev` keeps the localhost binding.
3. **Bind a `ConfigProperties` bean.** Define the `WebProperties` struct above,
   set its keys via a `ConditionContext`, and resolve it from a `Container`
   (you will recognize this pattern in the next chapter's DI tests).
4. **Mask a secret.** Add a `datasource.password` key and call
   `Layered::property_sources()`; confirm the value renders as `******` rather
   than in plaintext.

Next, see how Lumen's composition root resolves its collaborators — explicitly
today, with the best-in-class DI container as the scan-driven alternative — in
[Dependency Wiring](./04-dependency-wiring.md).
