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
//! Lumen is the running example of *Firefly Framework for Rust* (the Rust analog
//! of pyfly's "Lumen") — a single wallet domain grown into a secure, observable,
//! event-sourced CQRS service. Every listing in the book is a slice of this
//! crate, verified against code that compiles and passes tests.
//!
//! ## A single-line `main`, the rest declarative
//!
//! `main()` is **one line** over [`FireflyApplication`](firefly::FireflyApplication)
//! — the Rust analog of Spring Boot's `SpringApplication.run(App.class, args)`.
//! Everything else is declarative app code the framework discovers: there is **no
//! composition root and no bootstrap file**. The framework component-scans the
//! beans below, auto-mounts every `#[rest_controller]`, auto-discovers security +
//! middleware, drains the inventory-registered CQRS handlers / EDA listeners /
//! `#[scheduled]` tasks, self-hosts the admin dashboard, originates W3C trace
//! context, prints a startup report, and serves the public + management ports
//! with graceful shutdown.
//!
//! ## The model
//!
//! | Building block | Module | Key Firefly surface |
//! |----------------|--------|---------------------|
//! | [`Money`](money::Money) value object | [`money`] | — (pure domain) |
//! | [`Wallet`](domain::Wallet) aggregate | [`domain`] | `#[derive(AggregateRoot)]`, `#[derive(DomainEvent)]` |
//! | Event-sourced [`Ledger`](ledger::Ledger) + projection | [`ledger`] | `EventStore`, `Broker`, `#[event_listener]` |
//! | CQRS commands / queries + handlers | [`commands`] | `#[derive(Command)]`, `#[command_handler]` / `#[query_handler]` |
//! | [Transfer saga](transfer::run_transfer) | [`transfer`] | `#[saga]` / `#[saga_step]` |
//! | [Compliance workflow](compliance::run_compliance) | [`compliance`] | `#[workflow]` / `#[workflow_step]` |
//! | [Two-phase transfer](tcc_transfer::run_tcc_transfer) | [`tcc_transfer`] | `#[tcc]` / `#[participant]` |
//! | JWT security beans | [`security`] | `JwtService`, `BearerLayer`, `FilterChain` |
//! | HTTP surface (declarative beans) | [`web`] | `#[rest_controller]`, `#[derive(Configuration)]` + `#[bean]` |
//! | Scheduled housekeeping | [`housekeeping`] | `#[scheduled]` |
//!
//! Override the bind addresses with `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR`.

// Lumen is a teaching binary: some domain/API items (e.g. `Money::is_zero`,
// `mint_token`, the generated `register_*`/`schedule_*` helpers) are exercised
// by the test modules or kept as illustrative API the book references, so they
// read as "unused" from the binary's entry point alone.
#![allow(dead_code)]

mod commands;
mod compliance;
mod domain;
mod housekeeping;
mod ledger;
mod money;
mod security;
mod tcc_transfer;
mod transfer;
mod web;

// The HTTP tests drive the framework-assembled router in-process (no socket).
#[cfg(test)]
mod http_test;
#[cfg(all(test, feature = "streaming"))]
mod streaming_test;

/// The testable in-process router, re-exported at the crate root for the test
/// modules (assembled by `FireflyApplication::bootstrap`, no socket bound).
#[cfg(test)]
pub(crate) use web::build_router;

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen").run().await
}
