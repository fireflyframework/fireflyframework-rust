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

//! In-memory query-result memoisation keyed by message type + value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::bus::{AnyResult, DynHandler, Envelope, HandlerFuture, Middleware};
use crate::CqrsError;

/// Memoises query results by message type + JSON-encoded value.
///
/// Mirrors the `@Cacheable` behaviour of the Java/.NET CQRS modules and
/// the Go `QueryCache` — only queries (read-only messages whose
/// [`Message::cache_ttl`](crate::Message::cache_ttl) returns `Some`) are
/// cached; commands pass straight through. Failed dispatches are never
/// cached.
///
/// The handle is cheap to clone (`Arc`-backed), so keep one alongside the
/// bus and call [`QueryCache::invalidate`] after mutations while the
/// [`QueryCache::middleware`] registered on the bus shares the same
/// entries.
#[derive(Clone, Default)]
pub struct QueryCache {
    entries: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

#[derive(Clone)]
struct CacheEntry {
    value: AnyResult,
    expires_at: Option<Instant>,
}

impl QueryCache {
    /// Returns an empty in-memory query cache — Go's `NewQueryCache()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a [`Middleware`] that consults this cache for any message
    /// opting in via [`Message::cache_ttl`](crate::Message::cache_ttl) —
    /// Go's `QueryCache.Middleware()`.
    pub fn middleware(&self) -> QueryCacheMiddleware {
        QueryCacheMiddleware {
            cache: self.clone(),
        }
    }

    /// Removes every entry whose key starts with `prefix` — useful for
    /// invalidating a query family after a mutation, exactly like Go's
    /// `QueryCache.Invalidate`.
    ///
    /// Keys have the shape `<message type name>:<sha-256 of JSON>`, so
    /// passing a message type name (see [`QueryCache::invalidate_type`])
    /// clears all cached results for that query type.
    pub fn invalidate(&self, prefix: &str) {
        self.entries
            .lock()
            .expect("firefly/cqrs: query cache lock poisoned")
            .retain(|key, _| !key.starts_with(prefix));
    }

    /// Removes every cached result for query type `Q` — the typed
    /// convenience over [`QueryCache::invalidate`], sparing callers from
    /// spelling out [`std::any::type_name`] (Go callers pass the
    /// `reflect.Type` string by hand).
    ///
    /// The prefix includes the trailing `:` key separator so only `Q`'s
    /// entries are evicted — sibling types whose names merely share a
    /// prefix (e.g. `GetUser` vs `GetUserById`) stay cached.
    pub fn invalidate_type<Q: 'static>(&self) {
        self.invalidate(&format!("{}:", std::any::type_name::<Q>()));
    }

    fn get(&self, key: &str) -> Option<AnyResult> {
        let entries = self
            .entries
            .lock()
            .expect("firefly/cqrs: query cache lock poisoned");
        let entry = entries.get(key)?;
        if let Some(expires_at) = entry.expires_at {
            if Instant::now() > expires_at {
                return None;
            }
        }
        Some(entry.value.clone())
    }

    fn set(&self, key: String, value: AnyResult, ttl: Duration) {
        let expires_at = if ttl > Duration::ZERO {
            Some(Instant::now() + ttl)
        } else {
            // Go: ttl <= 0 leaves exp at the zero time — cache forever.
            None
        };
        self.entries
            .lock()
            .expect("firefly/cqrs: query cache lock poisoned")
            .insert(key, CacheEntry { value, expires_at });
    }
}

/// The bus middleware handle produced by [`QueryCache::middleware`]. It
/// shares the parent cache's entries, so invalidations through the
/// [`QueryCache`] are visible immediately.
#[derive(Clone)]
pub struct QueryCacheMiddleware {
    cache: QueryCache,
}

impl Middleware for QueryCacheMiddleware {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        let cache = self.cache.clone();
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            let cache = cache.clone();
            Box::pin(async move {
                // Not cacheable → pass through (Go: msg doesn't implement
                // Cacheable).
                let Some(ttl) = env.cache_ttl() else {
                    return next(env).await;
                };
                // Key derivation failed → dispatch uncached (Go: keyOf
                // error falls through to next).
                let key = match key_of(&env) {
                    Ok(key) => key,
                    Err(_) => return next(env).await,
                };
                if let Some(hit) = cache.get(&key) {
                    return Ok(hit);
                }
                let result = next(env).await?;
                cache.set(key, result.clone(), ttl);
                Ok(result)
            })
        })
    }
}

/// Builds the cache key: `<message type name>:<hex sha-256 of JSON>` —
/// the same construction as Go's `keyOf` (`reflect.Type.String() + ":" +
/// hex(sha256(json.Marshal(msg)))`), with [`std::any::type_name`]
/// standing in for the `reflect.Type` string.
///
/// An explicit key set on the envelope (pyfly's
/// `QueryBuilder.with_cache_key`, Rust's
/// [`Envelope::with_cache_key`](crate::Envelope::with_cache_key)) is
/// used verbatim instead — pyfly's `get_cache_key()` override.
fn key_of(envelope: &Envelope) -> Result<String, CqrsError> {
    if let Some(key) = envelope.cache_key() {
        return Ok(key.to_string());
    }
    let json = envelope.cache_json()?;
    let digest = Sha256::digest(&json);
    Ok(format!("{}:{}", envelope.type_name(), hex::encode(digest)))
}
