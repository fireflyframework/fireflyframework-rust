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

//! The Lumen process entry point (book chapter 18, "Production").
//!
//! Boots the whole service — banner, public API on `127.0.0.1:8080`, the
//! actuator admin surface on `127.0.0.1:8081`, the scheduled housekeeping task,
//! and graceful SIGINT/SIGTERM shutdown through the lifecycle [`Application`].
//! Override the bind addresses with `LUMEN_ADDR` / `LUMEN_ADMIN_ADDR`.
//!
//! Infrastructure is **in-memory**: this is teaching code, so the binary runs
//! with no external dependencies. The tests drive
//! [`build_router`](firefly_sample_lumen::web::build_router) in-process instead
//! of this binary.

use firefly::starter_core::InfoContributor;
use firefly_sample_lumen::housekeeping::build_scheduler;
use firefly_sample_lumen::web::{build_app, APP_NAME, VERSION};

/// Default bind address of the public API server.
const DEFAULT_ADDR: &str = "127.0.0.1:8080";
/// Default bind address of the admin (actuator) server.
const DEFAULT_ADMIN_ADDR: &str = "127.0.0.1:8081";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = build_app().await;
    // Best-effort: a test harness may already own the global subscriber.
    let _ = app.web.init_logging();

    let api = app.router();
    let contributor: InfoContributor = Box::new(|| {
        let mut info = serde_json::Map::new();
        info.insert(
            "sample".into(),
            serde_json::json!({ "name": APP_NAME, "store": "in-memory", "eventBus": "in-memory" }),
        );
        info
    });
    let admin = app.web.actuator_router(vec![contributor]);

    // Register and start the scheduled housekeeping task on a background task
    // (`Scheduler::start` runs until the scheduler is stopped).
    let scheduler = build_scheduler();
    tokio::spawn(async move { scheduler.start().await });

    app.web.print_banner();
    println!(":: {APP_NAME} :: digital-wallet & ledger (v{VERSION})");

    let api_addr = std::env::var("LUMEN_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
    let admin_addr =
        std::env::var("LUMEN_ADMIN_ADDR").unwrap_or_else(|_| DEFAULT_ADMIN_ADDR.to_owned());

    let application = app
        .web
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

    if let Err(err) = application.run().await {
        if !err.is_cancelled() {
            eprintln!("application failed: {err}");
            std::process::exit(1);
        }
    }
    Ok(())
}
