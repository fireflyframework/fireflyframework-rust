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

//! # firefly-webhooks
//!
//! The framework's **inbound webhook ingestion subsystem** — the Rust
//! port of the Go `webhooks` module. HTTP requests arriving at
//! `POST /api/webhooks/{provider}` are validated against the
//! registered signature scheme, optionally enriched, and dispatched to
//! per-provider [`Processor`]s; failures are sent to a dead-letter
//! queue for replay.
//!
//! Sub-modules mirror the Go sub-packages:
//!
//! | Module                | What it provides                                                  |
//! |-----------------------|-------------------------------------------------------------------|
//! | [`interfaces`]        | [`Inbound`] DTO + [`Validator`], [`Processor`] ports              |
//! | [`core`](self::core)  | [`Pipeline`], in-memory [`MemoryDlq`], idempotency [`EventStore`], four validators |
//! | [`processor`]         | SPI surface for service-supplied per-provider processors          |
//! | [`web`]               | `POST /api/webhooks/{provider}` ingestion [`axum::Router`]        |
//! | [`sdk`]               | Typed forwarder (replay DLQ entries; cross-service composition)   |
//!
//! ## Built-in validators
//!
//! | Validator            | Header(s)              | Algorithm                                        |
//! |----------------------|------------------------|--------------------------------------------------|
//! | [`HmacValidator`]    | `X-Signature` (default)| HMAC-SHA256 hex (with optional `sha256=` prefix) |
//! | [`StripeValidator`]  | `Stripe-Signature`     | `t=<unix>,v1=<hmac-hex>` with 5-min tolerance    |
//! | [`GitHubValidator`]  | `X-Hub-Signature-256`  | HMAC-SHA256 hex                                  |
//! | [`TwilioValidator`]  | `X-Twilio-Signature`   | HMAC-SHA1 base64 of `URL + sorted(form k+v)`     |
//!
//! Bring your own by implementing the [`Validator`] trait. The
//! signature wire formats are byte-identical to the Go port — the
//! `firefly-testkit` signers (`sign_hmac`, `sign_stripe`,
//! `sign_github`, `sign_twilio`) produce header values these
//! validators accept.
//!
//! ## Pipeline
//!
//! ```text
//! Pipeline::process(ev)
//!    │
//!    ├─ enrich (optional hook)
//!    │
//!    ├─ for each registered Processor for ev.provider:
//!    │     │  process(&ev)
//!    │     │  on error → Dlq::push, abort downstream processors
//!    │
//!    └─ return first error
//! ```
//!
//! ## Quick start
//!
//! ```
//! use std::sync::Arc;
//!
//! use firefly_webhooks::{web, MemoryDlq, Pipeline, StripeValidator};
//!
//! let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
//! pipeline.register_validator(StripeValidator::new(b"whsec_test"));
//! // pipeline.register_processor(StripeProcessor); // your business handler
//!
//! let app: axum::Router = web::router(pipeline);
//! // axum::serve(listener, app).await …
//! # let _ = app;
//! ```

pub mod core;
mod error;
pub mod interfaces;
pub mod processor;
pub mod sdk;
pub mod web;

pub use self::core::{
    Dlq, DlqEntry, EventStore, GitHubValidator, HmacValidator, MemoryDlq, MemoryEventStore,
    Pipeline, StripeValidator, TwilioValidator, DEFAULT_IDEMPOTENCY_HEADER,
};
#[cfg(feature = "redis")]
pub use self::core::{RedisEventStore, DEFAULT_KEY_PREFIX, DEFAULT_TTL_SECONDS};
pub use error::WebhookError;
pub use interfaces::{Inbound, Processor, Validator};
pub use sdk::Client;
pub use web::router;

/// The released framework version (CalVer expressed as semver).
pub const VERSION: &str = "26.6.21";
