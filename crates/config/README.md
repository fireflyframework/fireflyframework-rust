# `firefly-config`

> **Tier:** Foundational · **Status:** Full · **Java original:** Spring Boot `@ConfigurationProperties` · **Go module:** `config`

## Overview

`firefly-config` brings Spring Boot–style **typed, layered configuration
binding** to Rust. Application authors declare a `serde`-deserializable
struct and call `load::<T>(&sources)`; the loader merges the sources in
precedence order, resolves the active profile, and binds the flat
dot-keyed map onto the struct.

```rust,ignore
#[derive(serde::Deserialize)]
struct AppCfg {
    web: Web,     // struct { port: u16, host: String }
    cache: Cache, // struct { adapter: String }
}

let sources: Vec<Box<dyn Source>> = vec![
    Box::new(from_yaml("application.yaml")),
    Box::new(from_env("FIREFLY")),
];
let cfg: AppCfg = load(&sources)?;
```

The binder is **type-driven** (the Rust analog of the Go port's
reflection binder): `"9090"` binds onto an integer field, `"alpha,beta"`
splits onto a `Vec<String>`, `"true"` parses onto a `bool`, and missing
keys produce zero values (`0`, `""`, `false`, empty vec) — plain
`#[derive(Deserialize)]` structs bind without `#[serde(default)]`.

## Source precedence

`Layered::new(vec![s1, s2, ...])` merges from left to right — **last
write wins**. The canonical chain is:

1. **Defaults** (`StaticSource::new(name, entries)`)
2. **Base YAML** (`from_optional_yaml("application.yaml")`)
3. **Profile YAML** (`from_optional_yaml("application-prod.yaml")`)
4. **Environment** (`from_env("FIREFLY")` — `FIREFLY_WEB_PORT` → `web.port`)
5. **CLI flags** (`FlagSource::new()` — `flags.set("web.port", "9090")`)

So an environment override always beats a YAML file, and a CLI
override always beats both.

## Profile selection

`FIREFLY_PROFILE` selects the profile-specific YAML file. The
canonical helper:

```rust,ignore
let cfg: AppCfg = load_from_profile("/etc/firefly", "application", "dev")?;
```

reads `application.yaml`, then `application-{FIREFLY_PROFILE,fallback}.yaml`,
then `FIREFLY_*` env vars.

## Public surface

| Symbol                                                       | Purpose                                                |
|--------------------------------------------------------------|--------------------------------------------------------|
| `Source` trait                                               | Anything producing a flat `HashMap<String, String>`    |
| `StaticSource::new(name, entries)`                           | Hard-coded source                                      |
| `from_yaml(path) -> YamlSource`                              | Required YAML file                                     |
| `from_optional_yaml(path) -> YamlSource`                     | Tolerates absent file                                  |
| `from_env(prefix) -> EnvSource`                              | Reads `<PREFIX>_FOO_BAR` → `foo.bar`                   |
| `FlagSource::new()` + `.set(key, value)`                     | Programmatic / CLI overrides (clones share entries)    |
| `Layered::new(sources).map()`                                | Compute the merged map                                 |
| `load::<T>(&sources) -> Result<T, ConfigError>`              | Merge + bind onto a fresh `T`                          |
| `load_from_profile::<T>(dir, app, fallback)`                 | Profile-aware convenience                              |
| `bind::<T>(&flat) -> Result<T, ConfigError>`                 | Bind a pre-merged map                                  |
| `active_profile(fallback) -> String`                         | `FIREFLY_PROFILE` lookup                               |
| `profile_sources(dir, app, profile) -> Vec<Box<dyn Source>>` | Build the YAML chain for a profile                     |
| `ConfigError`                                                | `thiserror` enum: source, I/O, YAML, and bind failures |

## Supported leaf kinds (struct binder)

`String`, `bool` (Go `strconv.ParseBool` syntax: `1/t/T/true/TRUE/True`,
`0/f/F/false/FALSE/False`), all integer widths, `f32`/`f64`, `char`,
unit enums (by variant name), `Option<T>` (`None` when no key or section
is present), sequences of scalars (comma-separated, items trimmed), and
`HashMap<String, _>` subtrees. Use `Duration` via an `i64` field plus
your own conversion (`Duration::from_millis(cfg.timeout_ms as u64)`) —
keeps the binder dependency-light, matching the Go port.

## YAML subset

Files are parsed by a line-by-line port of the Go module's embedded
YAML-subset scanner — no general-purpose YAML dependency — so the
flattened output is identical to the Go port for any given
`application.yaml`:

- nested mappings become dot-joined lower-cased keys (each parent
  mapping key also yields an empty-string entry, as in Go);
- **scalar lexemes are preserved verbatim**: `1.10`, `0x1A`, `1e3`, and
  `2.50` bind onto `String` fields exactly as written — values are
  never parsed into typed numbers and re-rendered (one pair of
  surrounding quotes is stripped);
- duplicate keys follow last-write-wins, and out-of-range numeric
  literals are fine — every value stays a string until the binder
  parses it against the target field's type;
- sequences of scalars are comma-joined; sequence items are taken
  verbatim (the configuration contract is "sequences of scalars only");
- empty values render as `""`;
- aliases / anchors / multi-doc / tags / flow sequences are not
  interpreted (deliberate, matching the Go scanner — bring your own
  parser if you need them).

## Quick start

```rust
use firefly_config::{load_from_profile, ConfigError};
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

fn main() -> Result<(), ConfigError> {
    let cfg: AppCfg = load_from_profile("/etc/orders", "application", "dev")?;
    println!("{}", cfg.web.port);
    Ok(())
}
```

## pyfly parity

On top of the Go-parity surface, the crate ports pyfly's configuration
subsystem (`pyfly.core.config` + `pyfly.config_server.client`):

### `${...}` placeholder resolution

`load`/`bind` run a post-merge pass resolving placeholders in values
(also exposed standalone as `resolve_placeholders(&flat)`):

- `${ENV_VAR}` — literal environment variable name;
- `${app.name}` — config reference (relaxed: kebab/snake segments are
  interchangeable), resolved recursively with a **depth-10 guard**
  against circular references;
- `${key:default}` — fallback when neither environment nor config
  resolves `key`;
- **environment beats config**: `${app.name}` honors `FIREFLY_APP_NAME`
  (a leading `firefly.` segment is stripped, dots/dashes map to `_`)
  before falling back to the merged map.

Unresolvable placeholders without a default raise
`ConfigError::Placeholder`.

### Relaxed (kebab ↔ snake) keys

The merge and the binder normalize keys (`-` → `_`, lower-case), so
`graceful-timeout:` in YAML binds a `graceful_timeout` serde field.

### Runtime reload (`/actuator/refresh` contract)

```rust,ignore
let cfg: ReloadableConfig<AppCfg> = ReloadableConfig::load(sources)?;
let snapshot = cfg.get();           // Arc<AppCfg>, re-read per call
let mut rx = cfg.subscribe();       // tokio watch receiver
let changed: Vec<String> = cfg.reload()?; // changed top-level keys, sorted
```

`ReloadableConfig<T>` replays the exact merge → placeholder-resolution →
bind pipeline and atomically swaps the snapshot; failed reloads keep the
previous snapshot. The object-safe `Refresher` trait
(`refresh() -> Result<Vec<String>, ConfigError>`) is the hook an
actuator `POST /actuator/refresh` endpoint wires up —
`Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>`.

### Property-source introspection + masking

`Layered::property_sources()` returns ordered `PropertySourceView`s
(highest precedence first, Spring `/actuator/env` style): a synthetic
`systemEnvironment` source with every `FIREFLY_*` variable leads the
list, then the chain's sources in reverse merge order, each property
carrying `{value, origin}`. Values are sanitized by the public `mask`
module (Spring Boot `Sanitizer` parity): keys naming secrets
(`password`, `secret`, `token`, `credential`, `*key`, …) mask fully as
`******`; URI-shaped values get the userinfo password redacted
(`postgresql://user:******@host`).

### Multi-profile

`active_profiles("dev")` reads a **comma-separated** `FIREFLY_PROFILE`
(`dev,cloud` → `["dev", "cloud"]`); `multi_profile_sources` overlays
one `application-{p}.yaml` per profile in order, and
`load_from_profile` now composes both (single-profile behavior is
unchanged).

### Profile expressions (Spring Boot 2.4+)

`accepts_profiles(&active, &exprs)` evaluates the Spring Boot 2.4+
profile-expression grammar against an active-profile list — negation,
boolean operators with grouping, and the legacy comma-OR:

```rust,ignore
let active = active_profiles("dev"); // e.g. ["prod", "cloud"]
accepts_profiles(&active, &["prod & cloud"]);        // AND
accepts_profiles(&active, &["prod | qa"]);           // OR
accepts_profiles(&active, &["!test"]);               // negation
accepts_profiles(&active, &["(prod & cloud) | qa"]); // grouping
accepts_profiles(&active, &["dev,test"]);            // legacy comma-OR
```

It returns `true` when **any** expression matches; a malformed
expression evaluates to `false` (never panics). Where pyfly reads the
active list off the `Environment`, the Rust port takes it as a slice so
it composes with `active_profiles`.

### In-process application events

`ApplicationEventBus` is a **synchronous, in-process, `TypeId`-dispatched,
`@order`-sorted** pub/sub — Spring's `ApplicationEventPublisher` model,
distinct from the asynchronous `firefly-eda` broker (no transport, no
topics, listeners run on the publishing thread):

```rust,ignore
let bus = ApplicationEventBus::new();
bus.subscribe::<ApplicationReadyEvent, _>(|_e| { /* … */ });
bus.subscribe_ordered::<ContextRefreshedEvent, _>(1, |_e| { /* runs first */ });
bus.publish(&ApplicationReadyEvent);

// ApplicationEventPublisher fans into a shared Rc<ApplicationEventBus>.
let publisher = ApplicationEventPublisher::new(Rc::new(bus));
```

Lifecycle events: `ContextRefreshedEvent`, `ApplicationReadyEvent`,
`ContextClosedEvent`. Any `'static` type can be published as a domain
event. Dispatch is keyed on the concrete `TypeId` (Rust has no runtime
subclass relationship, so a listener receives exactly the type it
subscribed to).

### Spring-Cloud-Config client

```rust,ignore
let remote = ConfigClient::new("http://config:8888", "orders")
    .with_profile("prod")
    .with_label("main")
    .with_basic_auth("user", "pass")
    .fetch_source()           // -> StaticSource, fail-fast
    .await?;                  // .fetch_source_or_empty() = soft fallback
sources.insert(1, Box::new(remote)); // above defaults, below env/flags
```

`fetch()` GETs `/{application}/{profile}/{label}` and flattens the
document's `propertySources` (highest priority first on the wire, so
applied in reverse) into a flat map. Non-2xx responses log a warning
and yield an empty map (pyfly soft-miss parity); transport/decode
failures raise `ConfigError::Remote`.

| New symbol | Purpose |
|---|---|
| `resolve_placeholders(&flat)` | Post-merge `${...}` pass (called by `load`/`bind`) |
| `ReloadableConfig<T>` / `Refresher` | Runtime reload + actuator refresh hook |
| `Layered::property_sources()` | Ordered, masked, origin-attributed view |
| `PropertySourceView` / `PropertyView` | `/actuator/env`-shaped rows (serde-serializable) |
| `mask::{mask_value, is_sensitive_key, sanitize_uri, MASK}` | Spring Sanitizer parity |
| `active_profiles(fallback)` | Comma-separated `FIREFLY_PROFILE` list |
| `multi_profile_sources(dir, app, &profiles)` | One overlay per active profile |
| `accepts_profiles(&active, &exprs)` | Spring Boot 2.4+ profile-expression evaluator (`!`/`&`/`\|`/grouping/comma-OR) |
| `ApplicationEventBus` / `ApplicationEventPublisher` | Synchronous in-process `TypeId`-dispatched, `@order`-sorted pub/sub |
| `ContextRefreshedEvent` / `ApplicationReadyEvent` / `ContextClosedEvent` | Lifecycle event types |
| `ConfigClient` | Spring-Cloud-Config `/{app}/{profile}/{label}` fetch → `StaticSource` |
| `ConfigError::{Placeholder, Remote}` | New failure shapes |

## Testing

```bash
cargo test -p firefly-config
```

Suite ports every Go test (static + YAML + env merge order, profile
selection, optional-YAML absence tolerance, flag precedence, duration
via `i64`) and adds Rust-specific cases: leaf-kind coverage for every
integer width and float, Go `ParseBool` acceptance set, `Option`/enum/
map binding, `serde_json::Value` binding through `deserialize_any`,
YAML flattening edge cases, `Send + Sync` bounds, and `load` inside a
tokio task.

The pyfly-parity layer ports the pyfly test contract
(`test_placeholder_resolution.py`, `test_config_reload.py`,
`test_config.py` property-sources/masking, `test_wave_config_relaxed.py`,
and the config-server client): placeholder env/config/default/recursion
cases, kebab↔snake binding, reload-on-file-change with changed-key
reporting, ordered+masked property sources, multi-profile overlays, and
`ConfigClient` against an in-process axum mock (flattening precedence,
basic auth, soft-miss on non-2xx, transport-error fallback).
