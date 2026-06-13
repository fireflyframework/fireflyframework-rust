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

//! # firefly-callbacks
//!
//! The framework's **outbound webhook subsystem** — the Rust port of
//! the Go `callbacks` module. Services publish business events; the
//! [`HmacDispatcher`] signs each payload with HMAC-SHA256, retries
//! with exponential backoff, and records every attempt to a pluggable
//! [`Store`] for audit. A REST admin endpoint ([`handler`]) manages
//! targets; an SDK ([`CallbacksClient`]) type-safely calls the admin
//! endpoint from upstream services.
//!
//! Sub-modules mirror the Go sub-packages (and the .NET project split):
//!
//! | Module         | What it provides                                                         |
//! |----------------|--------------------------------------------------------------------------|
//! | [`interfaces`] | DTOs ([`Target`], [`CallbackEvent`], [`Attempt`]) + [`Store`], [`Dispatcher`] ports |
//! | [`models`]     | In-memory [`MemoryStore`] implementing [`Store`]                         |
//! | [`core`]       | HMAC-signing [`HmacDispatcher`] with retry, audit-log recording          |
//! | [`web`]        | REST admin handler (CRUD targets, list attempts)                         |
//! | [`sdk`]        | Typed client for the admin REST API                                      |
//!
//! ## Wire format
//!
//! `POST <target.url>` with body == `event.payload`, plus headers:
//!
//! | Header                | Value                                          |
//! |-----------------------|------------------------------------------------|
//! | `Content-Type`        | `application/json`                             |
//! | `X-Firefly-Event`     | `event.event_type`                             |
//! | `X-Firefly-Event-Id`  | `event.id`                                     |
//! | `X-Firefly-Timestamp` | Unix seconds when the request was sent         |
//! | `X-Firefly-Signature` | `sha256=<hmac-hex>` keyed on `target.secret`   |
//! | `X-Correlation-Id`    | When `event.correlation_id` is set             |
//! | (custom)              | Anything from `target.headers`                 |
//!
//! Header names and the `sha256=<lowercase hex>` HMAC encoding are
//! byte-identical to the Java / .NET / Go / Python ports — a webhook
//! receiver written against any of them verifies this crate's
//! deliveries unchanged.
//!
//! ## Retry policy
//!
//! [`DispatcherConfig`]`{max_attempts, initial_delay}` — defaults:
//! 3 attempts, 200 ms initial delay, doubling. Each attempt records an
//! [`Attempt`] audit row regardless of outcome.
//!
//! ## Quick start
//!
//! ```no_run
//! # async fn demo() -> Result<(), firefly_callbacks::CallbackError> {
//! use std::sync::Arc;
//!
//! use firefly_callbacks::{
//!     CallbackEvent, Dispatcher, DispatcherConfig, HmacDispatcher, MemoryStore, Store, Target,
//! };
//!
//! let store = Arc::new(MemoryStore::new());
//! store
//!     .upsert_target(Target {
//!         id: "customers".into(),
//!         url: "https://customer.example.com/cb".into(),
//!         secret: "shared-secret".into(),
//!         active: true,
//!         event_types: vec!["order.placed".into(), "order.shipped".into()],
//!         ..Target::default()
//!     })
//!     .await?;
//!
//! let dispatcher = HmacDispatcher::new(store.clone(), DispatcherConfig::default());
//! dispatcher
//!     .dispatch(CallbackEvent {
//!         id: uuid::Uuid::new_v4().to_string(),
//!         event_type: "order.placed".into(),
//!         payload: br#"{"id":"o1","customer":"alice"}"#.to_vec(),
//!         ..CallbackEvent::default()
//!     })
//!     .await?;
//!
//! // Audit trail:
//! let attempts = store.list_attempts("…event id…").await?;
//! # let _ = attempts;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod core;
pub mod interfaces;
pub mod models;
pub mod sdk;
pub mod web;

pub use crate::core::{
    DispatcherConfig, HmacDispatcher, HEADER_EVENT, HEADER_EVENT_ID, HEADER_SIGNATURE,
    HEADER_TIMESTAMP,
};
pub use crate::interfaces::{
    Attempt, AuthorizedDomain, CallbackError, CallbackEvent, Dispatcher, Store, Target,
};
pub use crate::models::MemoryStore;
pub use crate::sdk::CallbacksClient;
pub use crate::web::handler;

/// The released framework version stamp shared by every Firefly crate.
pub const VERSION: &str = "26.6.3";
