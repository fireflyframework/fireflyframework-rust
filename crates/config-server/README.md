# `firefly-config-server`

> **Tier:** Adapter · **Status:** Full · **Java original:** `firefly-config-server` · **Go module:** `configserver`

## Overview

`firefly-config-server` exposes a **Spring-Cloud-Config-compatible REST
endpoint** serving `Environment` payloads keyed by
`(application, profile, label)`. Existing Java / .NET SDKs that
already speak Spring Cloud Config talk to it without modification.

The default `MemoryStore` is suitable for tests and development;
production deployments back onto a Git repository or a database via
the `Store` trait.

## Wire format

`GET /{application}/{profile}[/{label}]` returns:

```json
{
  "name": "orders",
  "profiles": ["prod"],
  "label": "main",
  "version": "",
  "state": "",
  "propertySources": [
    {
      "name": "default",
      "source": { "db.url": "jdbc:postgres://…" }
    }
  ]
}
```

Field names match Spring Cloud Config exactly; `version` and `state`
are omitted when empty (the Go port's `omitempty`), and the label
defaults to `main` when the third path segment is absent.

A missing application/profile is a **soft miss** — the server returns
an empty `propertySources` array with the queried name and profile
echoed back. This matches Spring Cloud Config's behaviour so SDKs
don't break.

The request path is percent-decoded **before** routing, exactly as
Go's `net/http` hands the handler a decoded `r.URL.Path`: an encoded
slash (`%2F`) separates segments, and a path containing an invalid
percent-escape is rejected with `400 Bad Request` (Go's server rejects
such request lines before the handler runs).

## Public surface

| Symbol                                  | Purpose                                                          |
|-----------------------------------------|------------------------------------------------------------------|
| `PropertySource { name, source }`       | One logical source of properties (file, profile, db row)         |
| `Environment { name, profiles, … }`     | The wire-shape returned by `/{application}/{profile}`            |
| `Store` (async trait)                   | The persistence boundary the server queries                      |
| `ConfigServerError`                     | Typed lookup failure; mapped to `500` with the error text        |
| `MemoryStore::new()`                    | Empty in-process store                                           |
| `MemoryStore::put(app, profile, label, env)` | Seeds the store with an `Environment`                       |
| `router(store: Arc<dyn Store>)`         | axum `Router` serving `/{app}/{profile}[/{label}]`               |

## Quick start

```rust
use std::sync::Arc;
use firefly_config_server::{router, Environment, MemoryStore, PropertySource};

let store = Arc::new(MemoryStore::new());
store.put(
    "orders",
    "prod",
    "main",
    Environment {
        name: "orders".into(),
        profiles: vec!["prod".into()],
        label: "main".into(),
        property_sources: vec![PropertySource {
            name: "default".into(),
            source: [("db.url".to_string(), "jdbc:postgres://db:5432/orders".into())]
                .into_iter()
                .collect(),
        }],
        ..Environment::default()
    },
);

let app: axum::Router = router(store);
// axum::serve(tokio::net::TcpListener::bind("0.0.0.0:8888").await?, app).await?;
```

## Plugging in a Git-backed store

Implement the `Store` trait — the rest of the framework, including
existing Spring Cloud Config clients, doesn't need to change:

```rust,ignore
#[async_trait::async_trait]
impl firefly_config_server::Store for GitStore {
    async fn lookup(
        &self,
        app: &str,
        profile: &str,
        label: &str,
    ) -> Result<Environment, ConfigServerError> {
        // read `{app}-{profile}.yml` from branch `label` …
    }
}
```

## pyfly parity

In addition to the Go-parity `Store`/`MemoryStore`/`router` surface
above, the crate ships the **pyfly `config_server` backends** — the
filesystem and Git stores pyfly exposes via `ConfigBackend`. Where
`Store` answers a fully composed `Environment` lookup, a
`ConfigBackend` works one tier lower: it reads, writes, and lists
individual `ConfigSource` bundles keyed by
`(application, profile, label)`, and `ConfigServer` composes those
bundles into the Spring-Cloud-Config overlay set.

| Symbol                                          | Purpose                                                                 |
|-------------------------------------------------|-------------------------------------------------------------------------|
| `ConfigSource { application, profile, label, properties }` | One config bundle (the `Properties = serde_json::Map` payload) |
| `ConfigBackend` (async trait)                   | `fetch` / `save` / `list`; `save` defaults to `BackendError::Unsupported` |
| `BackendError`                                  | Typed `Io` / `Parse` / `Git` / `Unsupported` failure                    |
| `MemoryBackend::new()`                          | Map-backed backend for tests (pyfly `InMemoryConfigBackend`)            |
| `FsStore::new(root)`                            | Reads `<root>/<app>-<profile>.{yaml,yml,json}` (label = sub-directory)  |
| `FsStore::with_search_locations(root, [dirs…])` | **Tiered search**: domain overrides core overrides common (fill-in keys) |
| `GitStore::new(uri).label(..).clone_dir(..)`    | Clones/reuses a Git working tree, delegates to `FsStore`; `refresh()` pulls |
| `ConfigServer::new(backend)`                    | Composes the `(app,profile)` → `(app,default)` → `(application,profile)` → `(application,default)` overlay |

### Tiered filesystem store

```rust,no_run
# async fn run() -> Result<(), firefly_config_server::BackendError> {
use firefly_config_server::{ConfigBackend, FsStore};

// Highest precedence first: domain overrides core overrides common.
let store = FsStore::with_search_locations(
    "/etc/firefly/domain",
    [
        "/etc/firefly/domain".into(),
        "/etc/firefly/core".into(),
        "/etc/firefly/common".into(),
    ],
)?;
let source = store.fetch("orders", "prod", "main").await?;
# let _ = source;
# Ok(())
# }
```

Keys present only in a lower-precedence location are inherited
(fill-in semantics); `save()` and `list()` operate on the primary
(first / highest-precedence) location.

### Git-backed store

```rust,no_run
# async fn run() -> Result<(), firefly_config_server::BackendError> {
use firefly_config_server::{ConfigBackend, ConfigSource, GitStore, Properties};

let store = GitStore::new("https://github.com/acme/config.git")
    .label("main")
    .clone_dir("/var/lib/firefly/config-clone");

let source = store.fetch("orders", "prod", "main").await?;       // clones lazily
store.save(ConfigSource::with_label(                              // writes + local commit
    "payments", "prod", "main", Properties::new(),
)).await?;
store.refresh().await?;                                          // git pull origin
# let _ = source;
# Ok(())
# }
```

`GitStore` shells out to the system `git` binary (no extra crate); it
clones (or reuses an existing clone in a persistent `clone_dir`) and
delegates all file-search and merge logic to an `FsStore`. Writes are
committed locally — pushing to the remote is out of scope.

### Optional write path on `Store`

The Go-parity `Store` trait now carries a default `save` method that
returns `ConfigServerError::Unsupported`. Read-only stores (including
`MemoryStore`) need not implement it and keep compiling unchanged; a
writable store overrides `save`.

## Testing

```bash
cargo test -p firefly-config-server
```

Covers seeded `Environment` lookup, soft-miss behaviour for unknown
applications, and JSON wire-shape compatibility — byte-for-byte
against the Go encoder's output, including sorted `source` keys,
`version`/`state` omission, and the trailing newline. The pyfly-parity
suite (`tests/backend.rs`) ports pyfly's `test_config_server`,
`test_tiered_overlay`, and `test_git_backend` cases — the Git tests
build a **local** repository in a tempdir via the system `git` binary,
so no network access is required.
