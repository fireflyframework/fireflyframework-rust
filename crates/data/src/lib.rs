//! firefly-data — persistence abstractions every Firefly service shares.
//!
//! This crate is the Rust port of the Go module `data` (Java original:
//! `firefly-common-data`). It provides three things:
//!
//! - a generic **filter DSL** ([`Filter`], [`Predicate`], [`Op`],
//!   [`Sort`], [`Direction`]) that renders to parameterised PostgreSQL
//!   (`$1`, `$2`, … placeholders, double-quoted identifiers),
//! - the canonical [`Page`] paged-result envelope, wire-compatible with
//!   the Java/.NET/Go ports' `Page<T>` JSON shape, and
//! - a typed [`Repository`] CRUD contract with an in-memory
//!   implementation ([`MemoryRepository`]) for tests.
//!
//! Services that talk to a real database implement [`Repository`]
//! against their driver of choice, using [`Filter::to_sql`] to render
//! the WHERE / ORDER BY / LIMIT clauses.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Direction, Filter, MemoryRepository, Repository};
//!
//! #[derive(Clone)]
//! struct User {
//!     id: String,
//!     name: String,
//! }
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let repo = MemoryRepository::new(|u: &User| u.id.clone());
//! repo.save(User { id: "u1".into(), name: "alice".into() })
//!     .await
//!     .unwrap();
//!
//! let f = Filter::new()
//!     .where_eq("name", "alice")
//!     .order_by("id", Direction::Asc)
//!     .paged(0, 10);
//! let page = repo.find(&f).await.unwrap();
//! assert_eq!(page.total_elements, 1);
//! # });
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod filter;
mod page;
mod repository;

pub use filter::{Direction, Filter, Op, Predicate, Sort};
pub use page::Page;
pub use repository::{DataError, MemoryRepository, Repository};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
