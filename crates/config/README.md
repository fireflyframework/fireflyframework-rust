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
