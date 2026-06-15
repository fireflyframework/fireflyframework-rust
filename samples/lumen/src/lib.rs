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

//! # Lumen — the digital-wallet & ledger service the book builds chapter by chapter
//!
//! Lumen is the running example of *Firefly Framework for Rust* (the Rust
//! analog of pyfly's "Lumen"). Every listing in the book is a slice of this
//! crate, so the prose is verified against code that **compiles and passes
//! tests**. It is deliberately clean and pedagogical — a single wallet domain
//! grown from a bare scaffold into a secure, observable, event-sourced CQRS
//! service — not a kitchen-sink demo.
//!
//! ## The one-dependency facade
//!
//! Lumen depends on exactly one Firefly crate — the [`firefly`] facade — plus
//! the two ecosystem crates any service writes against (`axum`, `serde`). The
//! whole framework (CQRS, DI, the reactive web stack, event-driven messaging,
//! event sourcing, saga orchestration, scheduling, resilience, security,
//! observability) and every `#[derive(...)]` / `#[...]` macro come in through
//! `use firefly::prelude::*;`.
//!
//! ## The model
//!
//! | Building block | Module | Key Firefly surface |
//! |----------------|--------|---------------------|
//! | [`Money`](money::Money) value object | [`money`] | — (pure domain) |
//! | [`Wallet`](domain::Wallet) aggregate (open / deposit / withdraw) | [`domain`] | `#[derive(AggregateRoot)]`, `#[derive(DomainEvent)]` |
//! | Event-sourced [`Ledger`](ledger::Ledger) + read-model projection | [`ledger`] | `EventStore`, `Broker`, `#[event_listener]` |
//! | CQRS commands / queries + handlers | [`commands`] | `#[derive(Command)]` / `#[derive(Query)]`, `#[command_handler]` / `#[query_handler]` |
//! | [Transfer saga](transfer::run_transfer) (debit→credit + compensation) | [`transfer`] | `#[saga]` / `#[saga_step]` |
//! | [Compliance workflow](compliance::run_compliance) (parallel checks → approve) | [`compliance`] | `#[workflow]` / `#[workflow_step]` |
//! | [Two-phase transfer](tcc_transfer::run_tcc_transfer) (reserve → capture / release) | [`tcc_transfer`] | `#[tcc]` / `#[participant]` |
//! | JWT-secured endpoints | [`security`] | `JwtService`, `BearerLayer`, `FilterChain` |
//! | HTTP surface + composition root | [`web`] | `#[rest_controller]`, `WebStack`, actuator |
//! | Scheduled housekeeping | [`housekeeping`] | `#[scheduled]` |
//!
//! ## How it grows
//!
//! The chapter-by-chapter map of what each part teaches lives in
//! `docs/book/LUMEN-ARC.md`. [`web::build_router`] is the testable composition
//! root the HTTP tests drive with `tower::oneshot`.

pub mod commands;
pub mod compliance;
pub mod domain;
pub mod housekeeping;
pub mod ledger;
pub mod money;
pub mod security;
pub mod tcc_transfer;
pub mod transfer;
pub mod web;

/// The composition root: the fully-wired in-memory Lumen router. Re-exported
/// at the crate root so tests can `use firefly_sample_lumen::build_router`.
pub use web::{build_app, build_router};

/// The released framework version Lumen targets (matches every `firefly-*`
/// crate's `VERSION`).
pub const VERSION: &str = firefly::VERSION;
