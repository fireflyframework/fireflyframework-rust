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

//! # firefly-sample-orders
//!
//! The reference Firefly Framework Rust service — the port of the Go
//! `samples/orders` module. It demonstrates:
//!
//! - The five-package layout, mirrored as the five Rust modules
//!   [`interfaces`], [`models`], [`core`], [`web`], and [`sdk`].
//! - [`firefly_starter_core::Core::new`] one-call composition.
//! - CQRS dispatch with validation + query caching.
//! - Idempotency replay on `POST /api/v1/orders`.
//! - RFC 7807 `application/problem+json` error rendering.
//! - Correlation-id propagation.
//! - Startup banner.
//!
//! ## Module map (Go package → Rust module)
//!
//! | Go package                    | Rust module             | Contents                              |
//! |-------------------------------|-------------------------|---------------------------------------|
//! | `orders/interfaces`           | [`interfaces`]          | Wire shapes + CQRS messages           |
//! | `orders/models`               | [`models`]              | `Order` entity + `Repository` port    |
//! | `orders/core`                 | [`core`]                | CQRS handler registration             |
//! | `orders/web` (package `main`) | [`web`] + `src/main.rs` | Router composition + HTTP entry point |
//! | `orders/sdk`                  | [`sdk`]                 | Typed client over `/api/v1/orders`    |
//!
//! ## Quick start
//!
//! Serve the app (binds `127.0.0.1:8080` public + `127.0.0.1:8081`
//! admin by default):
//!
//! ```bash
//! cargo run -p firefly-sample-orders
//! ```
//!
//! Or compose the full router in-process — the Rust spelling of the Go
//! sample's `BuildHandler()` — and drive it with
//! `tower::ServiceExt::oneshot`:
//!
//! ```
//! use firefly_sample_orders::build_router;
//!
//! let app: axum::Router = build_router();
//! # let _ = app;
//! ```

#![warn(missing_docs)]

pub mod core;
pub mod interfaces;
pub mod models;
pub mod sdk;
pub mod web;

pub use web::build_router;

/// The released framework version, mirroring [`firefly_kernel::VERSION`].
pub const VERSION: &str = firefly_kernel::VERSION;
