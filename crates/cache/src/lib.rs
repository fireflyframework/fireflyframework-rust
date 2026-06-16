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

//! firefly-cache — the Firefly Framework's distributed-cache abstraction.
//!
//! This crate exposes a single port — [`Adapter`] — and ships three
//! implementations ([`MemoryAdapter`], [`NoOpAdapter`], [`FallbackAdapter`])
//! plus a typed wrapper ([`Typed`]) with `get_or_set` memoisation. Every
//! consumer (CQRS query cache, idempotency middleware, custom service code)
//! talks to the same [`Adapter`] regardless of whether it is running an
//! in-process map during local dev or — once the Redis adapter ships in the
//! next minor — a Redis cluster in production.
//!
//! [`Typed`] also offers the declarative-cache conveniences pyfly exposes
//! as decorators: [`get_or_set`](Typed::get_or_set) (`@cacheable`),
//! [`put`](Typed::put) (`@cache_put`), and [`delete`](Typed::delete) /
//! [`delete_prefix`](Typed::delete_prefix) (`@cache_evict`).
//!
//! [`CacheHealthIndicator`] is a [`firefly_observability::Indicator`] that
//! probes the cache with an active put/get/evict round-trip and reports
//! the round-trip `latencyMs` — pyfly's `CacheHealthIndicator` — degrading
//! to `DEGRADED` past a latency threshold and `DOWN` on a probe mismatch
//! or adapter error.
//!
//! Values cross the port as raw bytes; the [`Typed`] facade layers JSON
//! encoding (via `serde_json`) on top so services work with their own types.
//! The JSON wire format matches the Go/`encoding/json` port byte-for-byte for
//! equivalently-annotated types, keeping cached entries portable across the
//! sibling framework ports.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//! use std::time::Duration;
//! use firefly_cache::{MemoryAdapter, Typed};
//!
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct Order { id: String }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_cache::CacheError> {
//! let typed: Typed<Order> = Typed::new(Arc::new(MemoryAdapter::new()));
//! let order = typed
//!     .get_or_set("order:42", Some(Duration::from_secs(60)), || async {
//!         Ok(Order { id: "42".into() })
//!     })
//!     .await?;
//! assert_eq!(order.id, "42");
//! # Ok(())
//! # }
//! ```

mod adapter;
mod fallback;
mod health;
mod manager;
mod memory;
mod noop;
mod typed;

pub use adapter::{Adapter, CacheError, CacheStats};
pub use fallback::FallbackAdapter;
pub use health::CacheHealthIndicator;
pub use manager::{cache_adapter, register_cache};
pub use memory::MemoryAdapter;
pub use noop::NoOpAdapter;
pub use typed::Typed;

/// Framework version stamp.
pub const VERSION: &str = "26.6.23";
