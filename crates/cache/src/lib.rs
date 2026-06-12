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
mod memory;
mod noop;
mod typed;

pub use adapter::{Adapter, CacheError};
pub use fallback::FallbackAdapter;
pub use memory::MemoryAdapter;
pub use noop::NoOpAdapter;
pub use typed::Typed;

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
