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

//! # Lumen Ledger — `-models`
//!
//! The persistence layer: the [`Wallet`](entities::wallet::v1::Wallet) entity
//! and the [`WalletRepository`](repositories::wallet::v1::WalletRepository) — a
//! **real sqlx repository** built on the framework's
//! [`SqlxReactiveRepository`](firefly::data_sqlx::SqlxReactiveRepository) with
//! the `#[firefly::repository]` derived-query engine (Spring Data's
//! `findByOwner` / `countByStatus`, generated from the method name). This is the
//! Rust analog of firefly-oss's `-models` Maven module (R2DBC entities +
//! repositories).
//!
//! The repository is published as an **async DI bean**: its
//! [`#[bean] async fn`](config::WalletPersistenceConfig) opens the connection
//! pool and runs the schema migration with `await`, the Spring Boot way — the
//! framework parks the factory during the scan and resolves it during
//! `Container::init_async_beans`. By default it targets an in-memory SQLite
//! database (so the sample runs and tests with no external server); set
//! `DATABASE_URL=postgres://…` to point it at real PostgreSQL.

#![forbid(unsafe_code)]

pub mod config;
pub mod entities;
pub mod repositories;

/// Re-export of the framework predicate that detects an optimistic-lock
/// conflict, so the `-core` service can map a stale `@Version` write to a domain
/// `Conflict` without itself depending on the sqlx adapter (the `-models` layer
/// owns the persistence concern).
pub use firefly::data_sqlx::is_optimistic_lock;
