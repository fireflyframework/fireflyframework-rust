# `firefly-starter-data`

> **Tier:** Starter · **Status:** Stable

## Overview

`firefly-starter-data` is the **bring-your-own-DB starter**. It composes
[`firefly-starter-core`](../starter-core/) and leaves the persistence
pool to the consumer — perfect for services that already own their own
`sqlx::PgPool` / `rusqlite::Connection` configuration.

```rust,ignore
pub struct Data {
    pub core: Core, // Deref/DerefMut → Core
}

impl Data {
    pub fn new(cfg: CoreConfig) -> Data;
}
```

`starter_name` defaults to `"starter-data"` (an explicit
`CoreConfig.starter_name` is preserved). The `Data` struct holds the
`Core` as a public field and implements `Deref`/`DerefMut` to it, so
every core field and convenience method is available directly on the
starter (`data.bus`, `data.apply_middleware(..)`,
`data.actuator_router(..)`, `data.new_application()`, …). The full
`firefly-starter-core` surface is re-exported, so this crate is the only
starter dependency a data service needs.

## Quick start

```rust,ignore
use axum::{routing::get, Router};
use firefly_migrations as migrations;
use firefly_starter_data::{CoreConfig, Data};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The consumer owns the DB pool and migration timing.
    let conn = rusqlite::Connection::open(std::env::var("DATABASE_PATH")?)?;
    migrations::Runner::new(migrations::DirSource::new("db")?).run(&conn)?;

    let data = Data::new(CoreConfig {
        app_name: "orders".into(),
        ..CoreConfig::default()
    });
    data.init_logging()?;
    data.print_banner();

    let repo = OrderRepo::new(conn);
    orders_core::register(&data.bus, repo);

    // … wire HTTP via data.apply_middleware(..), run via data.new_application() …
    Ok(())
}
```

## Why a separate starter?

Three reasons:

1. The `Core` abstraction can't hold a typed DB pool without either
   pulling a driver into every service or losing type safety.
   `firefly-starter-data` lets the consumer keep their typed pool while
   still getting the standard `Core` facilities.
2. Services that don't need a database (read-side projections, thin
   BFFs, event consumers) should not be forced to depend on a DB
   driver. Use `firefly-starter-data` only when persistence is needed.
3. Migration timing is application-specific (some services run
   migrations elsewhere, some at startup) — leaving the choice in
   `main` keeps it explicit.

## Testing

```bash
cargo test -p firefly-starter-data
```

Covers the wired `Core` being live (a CQRS round-trip through `data.bus`)
and the `starter_name` override, plus: explicit starter names preserved,
core defaults flowing through the wrapper, validation middleware
pre-installed, `Deref` access to core methods, and `Send`/`Sync` bounds.
