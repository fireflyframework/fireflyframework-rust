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
