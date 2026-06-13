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

//! The macro-quickstart process entry point.
//!
//! Boots the macro-generated service over the [`firefly`] facade: builds the
//! starter [`Core`](firefly::prelude::Core), starts the `#[scheduled]` task,
//! applies the canonical middleware chain to the `#[rest_controller]` router,
//! and serves it on `127.0.0.1:8080` (override with `QUICKSTART_ADDR`). Shuts
//! down gracefully on SIGINT/SIGTERM via the lifecycle application.

use firefly::prelude::*;
use firefly_sample_macro_quickstart::{build_router, build_scheduler, VERSION};

/// Default bind address of the public API server.
const DEFAULT_ADDR: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() {
    // One-call wiring of the framework core (logging, banner, middleware,
    // lifecycle) from the facade prelude.
    let core = Core::new(CoreConfig {
        app_name: "macro-quickstart".into(),
        app_version: VERSION.into(),
        ..CoreConfig::default()
    });
    let _ = core.init_logging();
    core.print_banner();

    // Start the macro-generated scheduled task.
    let scheduler = build_scheduler();
    let runner = scheduler.clone();
    tokio::spawn(async move { runner.start().await });

    // The macro-generated router, behind the canonical middleware chain.
    let api = core.apply_middleware(build_router());
    let addr = std::env::var("QUICKSTART_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());

    let app = core
        .new_application()
        .on_server("api", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            axum::serve(listener, api)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        });

    if let Err(err) = app.run().await {
        if !err.is_cancelled() {
            eprintln!("application failed: {err}");
            std::process::exit(1);
        }
    }
}
