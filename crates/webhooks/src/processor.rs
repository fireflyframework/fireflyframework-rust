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

//! SPI surface for service-supplied per-provider processors — the Rust
//! spelling of the Go `webhooks/processor` package.
//!
//! Like its Go counterpart, this module deliberately contains no
//! implementation: services implement
//! [`Processor`](crate::interfaces::Processor) (re-exported here for
//! discoverability) and register the implementations on the
//! [`Pipeline`](crate::Pipeline) at boot. The framework supplies the
//! plumbing — validation, enrichment, dispatch, DLQ — and stays out of
//! the business logic.
//!
//! # Example
//!
//! ```
//! use async_trait::async_trait;
//! use firefly_webhooks::{Inbound, Pipeline, Processor, WebhookError};
//!
//! struct ChargeProcessor;
//!
//! #[async_trait]
//! impl Processor for ChargeProcessor {
//!     fn provider(&self) -> &str {
//!         "stripe"
//!     }
//!
//!     async fn process(&self, ev: &Inbound) -> Result<(), WebhookError> {
//!         if ev.payload.is_empty() {
//!             return Err(WebhookError::processor("empty payload"));
//!         }
//!         Ok(())
//!     }
//! }
//!
//! let pipeline = Pipeline::without_dlq();
//! pipeline.register_processor(ChargeProcessor);
//! ```

pub use crate::interfaces::Processor;
