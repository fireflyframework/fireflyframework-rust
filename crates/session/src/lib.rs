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

//! `firefly-session` — server-side HTTP session management, the Rust port
//! of pyfly's `session` package.
//!
//! # What it provides
//! * [`Session`] / [`SessionInner`] — a session handle with typed
//!   attribute get/set/remove, anti-fixation [`SessionInner::rotate_id`],
//!   [`SessionInner::invalidate`], and modified-tracking (pyfly
//!   `HttpSession`).
//! * [`SessionStore`] — the async persistence port, with
//!   [`MemorySessionStore`] (idle TTL eviction) and [`CacheSessionStore`]
//!   bridging any [`firefly_cache::Adapter`] (pyfly `SessionStore` +
//!   `InMemorySessionStore` + `RedisSessionStore`).
//! * [`SessionConfig`] / [`SameSite`] — cookie name/path/secure/http-only/
//!   same-site and idle/absolute timeouts, serde-bindable from
//!   `firefly.session.*`.
//! * [`SessionLayer`] / [`SessionService`] — the tower middleware that
//!   loads-or-creates on request and saves-if-modified (+ `Set-Cookie`,
//!   id-rotation migration, invalidation delete) on response (pyfly
//!   `SessionFilter`).
//! * [`SessionExt`] — an axum extractor for the request [`Session`].
//! * [`SessionSigner`] — optional HMAC-SHA256 signing of the session-id
//!   cookie value (a Rust hardening).
//! * Concurrency control ([`SessionRegistry`], [`MemorySessionRegistry`],
//!   [`ConcurrencyPolicy`], [`Strategy`], [`SessionConcurrencyController`])
//!   — Spring Security's `maximumSessions` (pyfly `session.concurrency`).
//!
//! # Example
//! ```no_run
//! use std::sync::Arc;
//! use axum::{routing::get, Router};
//! use firefly_session::{Session, SessionLayer, MemorySessionStore, SessionStore};
//!
//! async fn handler(session: axum::Extension<Session>) -> &'static str {
//!     session.set_attribute("user", "ada").await.unwrap();
//!     "ok"
//! }
//!
//! let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
//! let app: Router = Router::new()
//!     .route("/", get(handler))
//!     .layer(SessionLayer::new(store));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod concurrency;
mod config;
mod extract;
mod layer;
mod session;
mod signing;
mod store;

pub use concurrency::{
    ConcurrencyPolicy, MemorySessionRegistry, SessionConcurrencyController, SessionRegistry,
    Strategy,
};
pub use config::{SameSite, SessionConfig, DEFAULT_COOKIE_NAME, DEFAULT_TTL_SECONDS};
pub use extract::{SessionExt, SessionLayerMissing};
pub use layer::{SessionLayer, SessionService};
pub use session::{new_session_id, Session, SessionInner};
pub use signing::SessionSigner;
pub use store::{
    CacheSessionStore, MemorySessionStore, SessionData, SessionStore, SessionStoreError,
    DEFAULT_CACHE_PREFIX,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.19";
