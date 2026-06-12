// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # firefly-starter-data
//!
//! The **bring-your-own-DB starter** — the port of the Go `starterdata`
//! module (Java original: `firefly-starter-data`, .NET:
//! `FireflyFramework.Starter.Data`).
//!
//! [`Data`] composes [`firefly_starter_core::Core`] and leaves the
//! persistence pool to the consumer — perfect for services that already
//! own their own connection pool (`sqlx::PgPool`, `rusqlite::Connection`,
//! …). The starter name defaults to `"starter-data"`; everything else is
//! the standard [`Core`] wiring (bus, broker, cache, health, metrics,
//! scheduler, middleware chain, actuator router).
//!
//! Where the Go struct embeds `*startercore.Core`, the Rust [`Data`]
//! holds the [`Core`] as a public field and implements
//! [`Deref`]/[`DerefMut`] to it, so every `Core` field and method is
//! reachable directly on the starter (`data.bus`, `data.actuator_router(..)`,
//! …). The full `firefly-starter-core` surface is re-exported, so this
//! crate is the only starter dependency a data service needs — including
//! the pyfly-parity batteries on `CoreConfig` (CORS, security headers,
//! CSRF, request-log, request-metrics, http-exchanges, loggers,
//! redaction — all OFF by default), which flow through the inherited
//! `apply_middleware` / `actuator_router`.
//!
//! ## Quick start
//!
//! ```no_run
//! use axum::{routing::get, Router};
//! use firefly_starter_data::{CoreConfig, Data};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // The consumer composes their own DB pool / repository and
//!     // registers it on the bus or hands it to their handlers — the
//!     // starter never touches the database.
//!     let data = Data::new(CoreConfig {
//!         app_name: "orders".into(),
//!         ..CoreConfig::default()
//!     });
//!     data.init_logging()?;
//!     data.print_banner();
//!
//!     let api = data.apply_middleware(
//!         Router::new().route("/orders", get(|| async { "[]" })),
//!     );
//!     let app = data.new_application().on_server("api", move |shutdown| async move {
//!         let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
//!         axum::serve(listener, api)
//!             .with_graceful_shutdown(shutdown.wait())
//!             .await?;
//!         Ok(())
//!     });
//!     app.run().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Why a separate starter?
//!
//! 1. The [`Core`] abstraction can't hold a typed DB pool without either
//!    pulling a driver into every service or losing type safety —
//!    `firefly-starter-data` lets the consumer keep their typed pool
//!    while still getting the standard `Core` facilities.
//! 2. Services that don't need a database (read-side projections, thin
//!    BFFs, event consumers) should not be forced to depend on a DB
//!    driver. Use this starter only when persistence is needed.
//! 3. Migration timing is application-specific (some services run
//!    migrations elsewhere, some at startup) — leaving the choice in
//!    `main` keeps it explicit.

#![warn(missing_docs)]

use std::ops::{Deref, DerefMut};

pub use firefly_starter_core::*;

/// The data starter: [`Core`] for services that supply their own DB —
/// the Rust spelling of the Go `starterdata.Data` struct, which embeds
/// `*startercore.Core`. The consumer composes their own repository / DB
/// driver and registers it on the bus or via DI.
///
/// Dereferences to [`Core`], so every core field and convenience method
/// is available directly on the starter.
pub struct Data {
    /// The wired core — Go's embedded `*startercore.Core`.
    pub core: Core,
}

impl Data {
    /// Wires the data starter — Go's `starterdata.New(cfg)`.
    ///
    /// Delegates to [`Core::new`] and then rebrands the starter name to
    /// `"starter-data"` **only** when the core resolved it to the
    /// `"starter-core"` default — an explicit `starter_name` in the
    /// config is preserved, exactly like the Go module.
    pub fn new(cfg: CoreConfig) -> Self {
        let mut core = Core::new(cfg);
        if core.starter_name == "starter-core" {
            core.starter_name = "starter-data".to_string();
        }
        Data { core }
    }
}

impl Deref for Data {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

impl DerefMut for Data {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}

#[cfg(test)]
mod tests {
    use firefly_cqrs::{CqrsError, Message};
    use serde::Serialize;

    use super::*;

    #[derive(Clone, Serialize)]
    struct CreateOrder {
        id: String,
    }

    impl Message for CreateOrder {
        fn validate(&self) -> Result<(), CqrsError> {
            if self.id.is_empty() {
                return Err(CqrsError::validation("id required"));
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct OrderCreated {
        id: String,
    }

    // ---- ports of the Go test suite ----------------------------------------

    /// Go: TestDataWiring — the core bus is wired and the starter name
    /// is rebranded to "starter-data".
    #[tokio::test]
    async fn data_wiring() {
        let d = Data::new(CoreConfig {
            app_name: "svc".into(),
            ..CoreConfig::default()
        });
        assert_eq!(d.starter_name, "starter-data");

        // Go asserts d.Bus != nil; here the bus is proven live by a
        // round-trip dispatch.
        d.bus.register(|cmd: CreateOrder| async move {
            Ok::<_, CqrsError>(OrderCreated { id: cmd.id })
        });
        let created: OrderCreated = d.bus.send(CreateOrder { id: "o1".into() }).await.unwrap();
        assert_eq!(created, OrderCreated { id: "o1".into() });
    }

    // ---- Rust-specific coverage ---------------------------------------------

    /// The rebrand only fires on the "starter-core" default — an
    /// explicit starter name from the config is preserved (the Go `if`
    /// guard).
    #[test]
    fn explicit_starter_name_preserved() {
        let d = Data::new(CoreConfig {
            app_name: "svc".into(),
            starter_name: "starter-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(d.starter_name, "starter-custom");
    }

    /// Core defaults shine through the wrapper: empty app name falls
    /// back to "firefly-app", the log service tracks it, and the cache
    /// is the in-memory default.
    #[test]
    fn core_defaults_flow_through() {
        let d = Data::new(CoreConfig::default());
        assert_eq!(d.app_name, "firefly-app");
        assert_eq!(d.starter_name, "starter-data");
        assert_eq!(d.log.service, "firefly-app");
        assert_eq!(d.cache.name(), "memory");
        assert!(d.app_version.is_empty());
    }

    /// The validation middleware that Core::new installs is honoured
    /// through the data starter's bus.
    #[tokio::test]
    async fn validation_middleware_wired_by_default() {
        let d = Data::new(CoreConfig {
            app_name: "svc".into(),
            ..CoreConfig::default()
        });
        d.bus.register(|cmd: CreateOrder| async move {
            Ok::<_, CqrsError>(OrderCreated { id: cmd.id })
        });
        let err = d
            .bus
            .send::<CreateOrder, OrderCreated>(CreateOrder { id: String::new() })
            .await
            .expect_err("invalid command must be rejected");
        assert!(matches!(err, CqrsError::Validation(_)));
    }

    /// Deref exposes the Core convenience methods directly on Data.
    #[test]
    fn deref_exposes_core_methods() {
        let d = Data::new(CoreConfig {
            app_name: "orders".into(),
            ..CoreConfig::default()
        });
        assert_eq!(d.new_application().name(), "orders");
        let banner = d.banner();
        assert!(banner.contains("starter-data"));
        assert!(banner.contains("orders"));
    }

    #[test]
    fn version_matches_workspace() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn data_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Data>();
    }
}
