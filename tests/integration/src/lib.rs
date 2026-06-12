//! # firefly-integration-tests
//!
//! The Firefly Framework's **cross-module integration suite** — the Rust
//! port of the Go `tests` module (Java original: `firefly-it`, .NET:
//! `tests/FireflyFramework.Tests/`).
//!
//! This crate intentionally exports nothing. The suite lives in
//! `tests/integration_test.rs` and proves that several framework crates
//! compose end-to-end:
//!
//! | Scenario | Crates exercised |
//! |----------|------------------|
//! | Command → HMAC-signed callback → audit | `firefly-starter-core` + `firefly-cqrs` + `firefly-callbacks` (verified by `firefly-webhooks`) |
//! | Webhook ingestion round trip | `firefly-webhooks` core + web |
//! | Saga compensation rollback | `firefly-orchestration` + `firefly-kernel` |
//! | Health composite over starter-core | `firefly-observability` + `firefly-starter-core` |
//! | Correlation id seam | `firefly-kernel` + `firefly-web` (via starter-core) + `firefly-callbacks` |
//!
//! Per-crate unit tests live alongside their sources (`#[cfg(test)]`
//! modules, the Rust idiom mirroring Go's `_test.go` files). This crate
//! is reserved for tests that span **three or more** crates.
//!
//! Every collaborator is wired in-memory or on a loopback socket: the
//! callback receiver is a real `axum` server bound to port `0` (the
//! analog of Go's `httptest.NewServer`), ingestion endpoints are driven
//! in-process through `tower::ServiceExt::oneshot`, and stores, DLQs,
//! and buses are the framework's in-memory implementations. No external
//! services are required.
//!
//! Run with:
//!
//! ```sh
//! cargo test -p firefly-integration-tests
//! ```

#![warn(missing_docs)]
