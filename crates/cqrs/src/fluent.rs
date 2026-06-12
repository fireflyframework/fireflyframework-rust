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

//! Fluent command/query builders — pyfly's `pyfly.cqrs.fluent` package
//! (Java's `CommandBuilder` / `QueryBuilder`).
//!
//! Python builds messages reflectively from `with_field` kwargs; Rust
//! constructs the struct directly and the builders wrap it, layering on
//! the dispatch metadata Python attaches to its `Command`/`Query` base
//! classes: a fresh message id, correlation id, initiating user,
//! timestamp, free-form metadata, an optional
//! [`ExecutionContext`], and (for queries) cache-control overrides.
//! `execute_with` dispatches through the [`Bus`] with everything
//! attached to the [`Envelope`].

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::bus::{Bus, Envelope, Message};
use crate::context::ExecutionContext;
use crate::CqrsError;

/// Dispatch metadata accumulated by [`CommandBuilder`] /
/// [`QueryBuilder`] and carried on the [`Envelope`] — the Rust home for
/// the identity fields pyfly's `Command`/`Query` base classes carry on
/// every message (`command_id`, `correlation_id`, `initiated_by`,
/// `timestamp`, `metadata`).
#[derive(Clone, Debug, PartialEq)]
pub struct MessageMetadata {
    /// Fresh unique id minted when the builder is created — pyfly's
    /// `command_id` / `query_id` (a 36-char UUID string).
    pub message_id: String,
    /// Correlation id linking the dispatch to a wider flow — pyfly's
    /// `correlated_by(...)`.
    pub correlation_id: Option<String>,
    /// User who initiated the dispatch — pyfly's `initiated_by(...)`.
    pub initiated_by: Option<String>,
    /// Dispatch timestamp (UTC); defaults to creation time — pyfly's
    /// `at(...)`.
    pub timestamp: DateTime<Utc>,
    /// Free-form metadata entries — pyfly's `with_metadata(k, v)`.
    pub extra: HashMap<String, serde_json::Value>,
}

impl MessageMetadata {
    /// Returns metadata with a fresh UUID message id, the current UTC
    /// timestamp, and everything else empty.
    pub fn new() -> Self {
        Self {
            message_id: uuid::Uuid::new_v4().to_string(),
            correlation_id: None,
            initiated_by: None,
            timestamp: Utc::now(),
            extra: HashMap::new(),
        }
    }

    /// Looks up a free-form metadata entry.
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.extra.get(key)
    }
}

impl Default for MessageMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Fluent builder for command dispatch — pyfly's `CommandBuilder`.
///
/// ```
/// use firefly_cqrs::{Bus, CommandBuilder, CqrsError, Message};
/// use serde::Serialize;
///
/// #[derive(Clone, Serialize)]
/// struct CreateOrder { customer_id: String, amount: f64 }
/// impl Message for CreateOrder {}
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let bus = Bus::new();
/// bus.register(|c: CreateOrder| async move { Ok::<_, CqrsError>(c.customer_id) });
///
/// let id: String = CommandBuilder::create(CreateOrder { customer_id: "cust-1".into(), amount: 99.9 })
///     .with(|c| c.amount = 100.0)          // pyfly: with_field
///     .correlated_by("corr-abc")
///     .initiated_by("user-42")
///     .execute_with(&bus)
///     .await
///     .unwrap();
/// assert_eq!(id, "cust-1");
/// # });
/// ```
#[derive(Clone, Debug)]
pub struct CommandBuilder<C: Message> {
    command: C,
    metadata: MessageMetadata,
    context: Option<ExecutionContext>,
}

impl<C: Message> CommandBuilder<C> {
    /// Starts a builder around an already-constructed command — the
    /// Rust spelling of pyfly's `CommandBuilder.create(CommandType)`
    /// (Rust constructs the struct directly; reflection-style
    /// `with_field` becomes the [`CommandBuilder::with`] closure).
    pub fn create(command: C) -> Self {
        Self {
            command,
            metadata: MessageMetadata::new(),
            context: None,
        }
    }

    /// Mutates the wrapped command in place — the typed replacement for
    /// pyfly's `with_field(name, value)` / `with_fields(**kwargs)`.
    #[must_use]
    pub fn with(mut self, f: impl FnOnce(&mut C)) -> Self {
        f(&mut self.command);
        self
    }

    /// Sets the correlation id — pyfly's `correlated_by`.
    #[must_use]
    pub fn correlated_by(mut self, correlation_id: impl Into<String>) -> Self {
        self.metadata.correlation_id = Some(correlation_id.into());
        self
    }

    /// Records the initiating user — pyfly's `initiated_by`.
    #[must_use]
    pub fn initiated_by(mut self, user_id: impl Into<String>) -> Self {
        self.metadata.initiated_by = Some(user_id.into());
        self
    }

    /// Pins the dispatch timestamp — pyfly's `at`.
    #[must_use]
    pub fn at(mut self, timestamp: DateTime<Utc>) -> Self {
        self.metadata.timestamp = timestamp;
        self
    }

    /// Adds a free-form metadata entry — pyfly's `with_metadata`.
    #[must_use]
    pub fn with_metadata(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.metadata.extra.insert(key.into(), value.into());
        self
    }

    /// Attaches an [`ExecutionContext`] consulted by
    /// [`Message::authorize`] and context-aware handlers.
    #[must_use]
    pub fn with_context(mut self, context: ExecutionContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Borrows the wrapped command.
    pub fn message(&self) -> &C {
        &self.command
    }

    /// Borrows the accumulated [`MessageMetadata`].
    pub fn metadata(&self) -> &MessageMetadata {
        &self.metadata
    }

    /// Borrows the attached [`ExecutionContext`], if any.
    pub fn context(&self) -> Option<&ExecutionContext> {
        self.context.as_ref()
    }

    /// Assembles the dispatch [`Envelope`] carrying the command,
    /// metadata, and context — pyfly's `build()` (which returns the
    /// command with metadata attached; Rust attaches it to the
    /// envelope instead).
    pub fn build(self) -> Envelope {
        let mut env = Envelope::new(self.command).with_metadata(self.metadata);
        if let Some(ctx) = self.context {
            env = env.with_context(ctx);
        }
        env
    }

    /// Builds and dispatches through `bus`, returning the typed result
    /// — pyfly's `execute_with(command_bus)`.
    pub async fn execute_with<R>(self, bus: &Bus) -> Result<R, CqrsError>
    where
        R: Clone + Send + Sync + 'static,
    {
        bus.dispatch_typed(self.build()).await
    }
}

/// Fluent builder for query dispatch — pyfly's `QueryBuilder`.
///
/// On top of the [`CommandBuilder`] surface it adds cache control:
/// [`QueryBuilder::cached_for`] / [`QueryBuilder::uncached`] override
/// the message's [`Message::cache_ttl`] for this dispatch (pyfly's
/// `cached(True/False)`), and [`QueryBuilder::with_cache_key`] replaces
/// the derived cache key (pyfly's `with_cache_key`).
///
/// ```
/// use std::time::Duration;
/// use firefly_cqrs::{Bus, CqrsError, Message, QueryBuilder, QueryCache};
/// use serde::Serialize;
///
/// #[derive(Clone, Serialize)]
/// struct GetOrder { order_id: String }
/// impl Message for GetOrder {}
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let bus = Bus::new();
/// let cache = QueryCache::new();
/// bus.use_middleware(cache.middleware());
/// bus.register(|q: GetOrder| async move { Ok::<_, CqrsError>(q.order_id) });
///
/// let order: String = QueryBuilder::create(GetOrder { order_id: "ord-1".into() })
///     .cached_for(Duration::from_secs(300))
///     .with_cache_key("order:ord-1")
///     .execute_with(&bus)
///     .await
///     .unwrap();
/// assert_eq!(order, "ord-1");
/// cache.invalidate("order:ord-1"); // after a mutation
/// # });
/// ```
#[derive(Clone, Debug)]
pub struct QueryBuilder<Q: Message> {
    query: Q,
    metadata: MessageMetadata,
    context: Option<ExecutionContext>,
    cache_ttl: Option<Option<Duration>>,
    cache_key: Option<String>,
}

impl<Q: Message> QueryBuilder<Q> {
    /// Starts a builder around an already-constructed query — the Rust
    /// spelling of pyfly's `QueryBuilder.create(QueryType)`.
    pub fn create(query: Q) -> Self {
        Self {
            query,
            metadata: MessageMetadata::new(),
            context: None,
            cache_ttl: None,
            cache_key: None,
        }
    }

    /// Mutates the wrapped query in place — the typed replacement for
    /// pyfly's `with_field` / `with_fields`.
    #[must_use]
    pub fn with(mut self, f: impl FnOnce(&mut Q)) -> Self {
        f(&mut self.query);
        self
    }

    /// Sets the correlation id — pyfly's `correlated_by`.
    #[must_use]
    pub fn correlated_by(mut self, correlation_id: impl Into<String>) -> Self {
        self.metadata.correlation_id = Some(correlation_id.into());
        self
    }

    /// Pins the dispatch timestamp — pyfly's `at`.
    #[must_use]
    pub fn at(mut self, timestamp: DateTime<Utc>) -> Self {
        self.metadata.timestamp = timestamp;
        self
    }

    /// Adds a free-form metadata entry — pyfly's `with_metadata`.
    #[must_use]
    pub fn with_metadata(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.metadata.extra.insert(key.into(), value.into());
        self
    }

    /// Attaches an [`ExecutionContext`] consulted by
    /// [`Message::authorize`] and context-aware handlers.
    #[must_use]
    pub fn with_context(mut self, context: ExecutionContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Opts this dispatch into caching for `ttl`, overriding the
    /// message's [`Message::cache_ttl`] — pyfly's `cached(True)` (the
    /// TTL is explicit in Rust; `Duration::ZERO` caches forever).
    #[must_use]
    pub fn cached_for(mut self, ttl: Duration) -> Self {
        self.cache_ttl = Some(Some(ttl));
        self
    }

    /// Opts this dispatch out of caching even when the message type
    /// declares a [`Message::cache_ttl`] — pyfly's `cached(False)`.
    #[must_use]
    pub fn uncached(mut self) -> Self {
        self.cache_ttl = Some(None);
        self
    }

    /// Replaces the derived cache key (`<type name>:<sha-256 of JSON>`)
    /// with an explicit one — pyfly's `with_cache_key`. Explicit keys
    /// pair naturally with
    /// [`EdaCacheInvalidationBridge`](crate::EdaCacheInvalidationBridge)
    /// rules.
    #[must_use]
    pub fn with_cache_key(mut self, key: impl Into<String>) -> Self {
        self.cache_key = Some(key.into());
        self
    }

    /// Borrows the wrapped query.
    pub fn message(&self) -> &Q {
        &self.query
    }

    /// Borrows the accumulated [`MessageMetadata`].
    pub fn metadata(&self) -> &MessageMetadata {
        &self.metadata
    }

    /// Borrows the attached [`ExecutionContext`], if any.
    pub fn context(&self) -> Option<&ExecutionContext> {
        self.context.as_ref()
    }

    /// Assembles the dispatch [`Envelope`] carrying the query,
    /// metadata, context, and cache overrides — pyfly's `build()`.
    pub fn build(self) -> Envelope {
        let mut env = Envelope::new(self.query).with_metadata(self.metadata);
        if let Some(ctx) = self.context {
            env = env.with_context(ctx);
        }
        if let Some(ttl) = self.cache_ttl {
            env = env.with_cache_ttl(ttl);
        }
        if let Some(key) = self.cache_key {
            env = env.with_cache_key(key);
        }
        env
    }

    /// Builds and dispatches through `bus`, returning the typed result
    /// — pyfly's `execute_with(query_bus)`.
    pub async fn execute_with<R>(self, bus: &Bus) -> Result<R, CqrsError>
    where
        R: Clone + Send + Sync + 'static,
    {
        bus.dispatch_typed(self.build()).await
    }
}
