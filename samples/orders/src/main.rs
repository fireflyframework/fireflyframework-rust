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

//! The orders sample's process entry point — the port of the Go
//! sample's `web/main.go`. Serves the public API on
//! [`DEFAULT_ADDR`](firefly_sample_orders::web::DEFAULT_ADDR)
//! (`127.0.0.1:8080`) and the actuator admin surface on
//! [`DEFAULT_ADMIN_ADDR`](firefly_sample_orders::web::DEFAULT_ADMIN_ADDR)
//! (`127.0.0.1:8081`); override with the `ORDERS_ADDR` /
//! `ORDERS_ADMIN_ADDR` environment variables. Shuts down gracefully on
//! SIGINT/SIGTERM via the lifecycle application, like Go's
//! `signal.NotifyContext`.

use std::sync::Arc;

use firefly_starter_core::InfoContributor;

use firefly_sample_orders::web::{
    api_router, build_core, wire_orders, DEFAULT_ADDR, DEFAULT_ADMIN_ADDR,
};

#[tokio::main]
async fn main() {
    let core = build_core();
    let _query_cache = wire_orders(&core);
    // Best-effort: a test harness may already own the global subscriber.
    let _ = core.init_logging();

    // Public traffic.
    let api = core.apply_middleware(api_router(Arc::clone(&core.bus)));

    // Management endpoints — bind on a separate port so /actuator/*
    // never leaks onto the public network unintentionally.
    let contributor: InfoContributor = Box::new(|| {
        let mut info = serde_json::Map::new();
        info.insert(
            "sample".into(),
            serde_json::json!({ "orders": "in-memory" }),
        );
        info
    });
    let admin = core.actuator_router(vec![contributor]);

    core.print_banner();

    let api_addr = std::env::var("ORDERS_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
    let admin_addr =
        std::env::var("ORDERS_ADMIN_ADDR").unwrap_or_else(|_| DEFAULT_ADMIN_ADDR.to_owned());

    let app = core
        .new_application()
        .on_server("api", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind(&api_addr).await?;
            axum::serve(listener, api)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        })
        .on_server("admin", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind(&admin_addr).await?;
            axum::serve(listener, admin)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        });

    // Go: `if err := app.Run(ctx); err != nil && err != context.Canceled`.
    if let Err(err) = app.run().await {
        if !err.is_cancelled() {
            eprintln!("application failed: {err}");
            std::process::exit(1);
        }
    }
}
