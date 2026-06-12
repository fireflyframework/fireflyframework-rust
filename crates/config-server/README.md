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

## Testing

```bash
cargo test -p firefly-config-server
```

Covers seeded `Environment` lookup, soft-miss behaviour for unknown
applications, and JSON wire-shape compatibility — byte-for-byte
against the Go encoder's output, including sorted `source` keys,
`version`/`state` omission, and the trailing newline.
