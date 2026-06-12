//! # firefly-sample-reactive-banking
//!
//! The **flagship full-ecosystem reference service** for the Firefly
//! Framework for Rust: a reactive (WebFlux/Reactor-style) banking service
//! that wires *the entire ecosystem together* and proves it works end to
//! end. It is the "everything wired together" sample.
//!
//! ```text
//!   HTTP (firefly-starter-web + JWT)
//!        │  POST /accounts /deposit /withdraw /transfers   GET /accounts/:id   GET /accounts/:id/events
//!        ▼
//!   reactive CQRS bus (firefly-cqrs: send_mono / query_mono → Mono<R>)
//!        │
//!        ├── command handlers ──► Bank (application service)
//!        │                          │  rehydrate ─► run domain command ─► append (optimistic concurrency)
//!        │                          ▼
//!        │                    event store (firefly-eventsourcing)
//!        │                          │  publish each DomainEvent
//!        │                          ▼
//!        │                    EDA broker (firefly-eda; Kafka via firefly-eda-kafka)
//!        │                          │  subscribe
//!        │                          ▼
//!        │                    projection ─► reactive read model (firefly-data ReactiveCrudRepository)
//!        │                                     ▲
//!        │   query handler ────────────────────┘  (GET serves the projected view)
//!        │
//!        └── POST /transfers ──► SAGA (firefly-orchestration): debit ─► credit, compensate on failure
//!
//!   GET /accounts/:id/events ──► Flux<AccountEvent> ──► application/x-ndjson | text/event-stream
//!                                    (reactive server push with backpressure)
//! ```
//!
//! ## What this demonstrates
//!
//! - **Reactive everywhere** — [`firefly_reactive`]'s `Mono`/`Flux` thread
//!   through the CQRS bus ([`Bus::send_mono`](firefly_cqrs::Bus::send_mono)),
//!   the read-model repository
//!   ([`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository)), the
//!   streaming HTTP responder ([`NdJson`](firefly_web::NdJson) /
//!   [`Sse`](firefly_web::Sse)), and the SDK client
//!   ([`WebClient`](firefly_client::WebClient)).
//! - **Event sourcing + CQRS** — the [`Account`](domain::Account) aggregate
//!   is the write model; an EDA-driven [`projection`](projections) keeps the
//!   read model in sync.
//! - **Sagas with compensation** — a money [`transfer`](saga) debits then
//!   credits, rolling the debit back on failure.
//! - **Pluggable infrastructure** — in-memory by default; a real Postgres
//!   reactive repo (`FIREFLY_TEST_POSTGRES_URL`) and Kafka event bus
//!   (`FIREFLY_TEST_KAFKA_BROKERS`) when configured.
//! - **Security** — JWT-protected mutating routes ([`security`]).
//! - **Observability + actuator + lifecycle** — inherited from
//!   [`firefly_starter_web`].
//!
//! ## Module map
//!
//! | Module             | Contents                                                        |
//! |--------------------|-----------------------------------------------------------------|
//! | [`domain`]         | The `Account` aggregate, domain events, read-model view         |
//! | [`repository`]     | `ReactiveCrudRepository` read model (in-memory + Postgres)      |
//! | [`commands`]       | CQRS messages + the `Bank` application service + handlers        |
//! | [`saga`]           | The money-transfer saga (debit → credit, compensation)         |
//! | [`projections`]    | The EDA subscriber that rebuilds the read model                 |
//! | [`security`]       | JWT verifier + token minting + the RBAC filter chain            |
//! | [`web`]            | Router composition + reactive HTTP handlers                     |
//! | [`sdk`]            | The reactive `WebClient` SDK                                     |
//!
//! ## Quick start
//!
//! Serve the app (public API `127.0.0.1:8080`, actuator `127.0.0.1:8081`):
//!
//! ```bash
//! cargo run -p firefly-sample-reactive-banking
//! ```
//!
//! Or compose the full router in-process and drive it with
//! `tower::ServiceExt::oneshot`:
//!
//! ```
//! use firefly_sample_reactive_banking::build_router;
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let app: axum::Router = build_router().await;
//! # let _ = app;
//! # });
//! ```

#![warn(missing_docs)]

pub mod commands;
pub mod domain;
pub mod projections;
pub mod repository;
pub mod saga;
pub mod sdk;
pub mod security;
pub mod web;

pub use web::{build_app, build_app_with, build_router, BankingApp};

/// The released framework version, mirroring [`firefly_kernel::VERSION`].
pub const VERSION: &str = firefly_kernel::VERSION;
