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

//! EDA → CQRS cache-invalidation bridge — pyfly's
//! `pyfly.cqrs.cache.eda_bridge` (Java's `EventDrivenCacheInvalidator`
//! wired to the event bus).
//!
//! When domain events arrive on a [`firefly_eda`] broker the bridge
//! evicts matching [`QueryCache`] entries, keeping read models fresh
//! without manual `invalidate` calls after every mutation:
//!
//! 1. **Rules** registered via [`EdaCacheInvalidationBridge::register`]
//!    map an *event-type string* to cache-key patterns whose `{field}`
//!    placeholders are resolved from the event's JSON payload — exactly
//!    pyfly's behaviour.
//! 2. Events of type [`CacheInvalidationEvent::EVENT_TYPE`] carry
//!    explicit prefixes in their payload and are honoured without any
//!    registered rule — the dedicated invalidation topic the Rust port
//!    adds so services can broadcast evictions directly.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use firefly_eda::{handler, EdaResult, Event, Subscriber};
use serde::{Deserialize, Serialize};

use crate::cache::QueryCache;

/// Default topic the bridge subscribes to for explicit
/// [`CacheInvalidationEvent`]s — the Rust port's spelling of pyfly's
/// `cqrs.events` invalidation channel.
pub const CACHE_INVALIDATION_TOPIC: &str = "cqrs.cache.invalidation";

/// Wire payload for an explicit cache-invalidation broadcast.
///
/// Publish one as the JSON payload of a [`firefly_eda::Event`] with
/// `event_type` [`CacheInvalidationEvent::EVENT_TYPE`] (conventionally
/// on [`CACHE_INVALIDATION_TOPIC`]); every subscribed bridge calls
/// [`QueryCache::invalidate`] for each prefix — no rule registration
/// needed.
///
/// ```
/// use firefly_cqrs::CacheInvalidationEvent;
///
/// let ev = CacheInvalidationEvent::of(["order:42", "customer-orders:7"]);
/// let json = serde_json::to_vec(&ev).unwrap();
/// # let _ = json;
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheInvalidationEvent {
    /// Cache-key prefixes to evict, each passed to
    /// [`QueryCache::invalidate`].
    pub prefixes: Vec<String>,
}

impl CacheInvalidationEvent {
    /// The `event_type` string the bridge recognises for explicit
    /// invalidation events.
    pub const EVENT_TYPE: &'static str = "CacheInvalidationEvent";

    /// Builds an event evicting the given prefixes.
    pub fn of<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            prefixes: prefixes.into_iter().map(Into::into).collect(),
        }
    }
}

/// Evicts [`QueryCache`] entries when EDA events arrive — pyfly's
/// `EdaCacheInvalidationBridge`.
///
/// Rules map an event-type string (as carried in the
/// [`firefly_eda::Event`] envelope) to one or more cache-key patterns.
/// Patterns may contain `{field}` placeholders resolved from the
/// event's JSON-object payload.
///
/// ```
/// use firefly_cqrs::{EdaCacheInvalidationBridge, QueryCache};
/// use firefly_eda::InMemoryBroker;
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let cache = QueryCache::new();
/// let bridge = EdaCacheInvalidationBridge::new(cache.clone());
/// bridge.register("order.updated", "order:{order_id}");
///
/// let broker = InMemoryBroker::new();
/// bridge.subscribe(&broker, "cqrs.events").await.unwrap();
/// // publishing {"order_id":"42"} as "order.updated" now evicts "order:42"
/// # });
/// ```
///
/// The bridge is cheaply cloneable (`Arc`-backed); clones share the
/// same rules and cache.
#[derive(Clone)]
pub struct EdaCacheInvalidationBridge {
    cache: QueryCache,
    rules: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl EdaCacheInvalidationBridge {
    /// Wraps the [`QueryCache`] the bridge will evict from — pyfly's
    /// `EdaCacheInvalidationBridge(cache_adapter)`.
    pub fn new(cache: QueryCache) -> Self {
        Self {
            cache,
            rules: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Registers a cache-key pattern to evict when `event_type`
    /// arrives — pyfly's `register("order.updated", "order:{order_id}")`.
    ///
    /// `{field}` placeholders are resolved from the event's JSON-object
    /// payload; the resolved string is passed to
    /// [`QueryCache::invalidate`] as a prefix.
    pub fn register(&self, event_type: impl Into<String>, cache_key_pattern: impl Into<String>) {
        self.rules
            .write()
            .expect("firefly/cqrs: eda bridge lock poisoned")
            .entry(event_type.into())
            .or_default()
            .push(cache_key_pattern.into());
    }

    /// Wires the bridge into an EDA subscriber on `topic` — pyfly's
    /// `subscribe(event_bus)`.
    ///
    /// pyfly subscribes a `"*"` wildcard; the Rust [`Subscriber`] port
    /// is per-topic, so pass each topic the invalidating events flow on
    /// (call repeatedly for several topics, or use
    /// [`EdaCacheInvalidationBridge::subscribe_default`] for the
    /// dedicated [`CACHE_INVALIDATION_TOPIC`]).
    pub async fn subscribe<S>(&self, subscriber: &S, topic: &str) -> EdaResult<()>
    where
        S: Subscriber + ?Sized,
    {
        let bridge = self.clone();
        subscriber
            .subscribe(
                topic,
                handler(move |ev: Event| {
                    let bridge = bridge.clone();
                    async move {
                        bridge.on_event(&ev);
                        Ok(())
                    }
                }),
            )
            .await
    }

    /// [`EdaCacheInvalidationBridge::subscribe`] on the conventional
    /// [`CACHE_INVALIDATION_TOPIC`].
    pub async fn subscribe_default<S>(&self, subscriber: &S) -> EdaResult<()>
    where
        S: Subscriber + ?Sized,
    {
        self.subscribe(subscriber, CACHE_INVALIDATION_TOPIC).await
    }

    /// Handles one EDA event — pyfly's `on_envelope`.
    ///
    /// Explicit [`CacheInvalidationEvent`]s evict their prefixes
    /// directly; for every other event the registered rules for
    /// `ev.event_type` are resolved against the JSON payload and each
    /// resolved key is evicted. Unknown event types and undecodable
    /// payloads are ignored.
    pub fn on_event(&self, ev: &Event) {
        if ev.event_type == CacheInvalidationEvent::EVENT_TYPE {
            if let Some(payload) = &ev.payload {
                if let Ok(event) = serde_json::from_slice::<CacheInvalidationEvent>(payload) {
                    for prefix in &event.prefixes {
                        self.cache.invalidate(prefix);
                    }
                }
            }
            return;
        }
        let patterns = {
            let rules = self
                .rules
                .read()
                .expect("firefly/cqrs: eda bridge lock poisoned");
            match rules.get(&ev.event_type) {
                Some(patterns) => patterns.clone(),
                None => return,
            }
        };
        let payload = ev
            .payload
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
            .unwrap_or(serde_json::Value::Null);
        for pattern in &patterns {
            let key = resolve_pattern(pattern, &payload);
            self.cache.invalidate(&key);
        }
    }
}

/// Resolves `{field}` placeholders in `pattern` from a JSON-object
/// `payload` — pyfly's `_resolve_pattern`. Unresolvable placeholders
/// (missing field, `null` value, non-object payload) are left as-is.
///
/// ```
/// use firefly_cqrs::resolve_pattern;
///
/// let payload = serde_json::json!({"tenant_id": "acme", "order_id": 7});
/// assert_eq!(resolve_pattern("tenant:{tenant_id}:order:{order_id}", &payload), "tenant:acme:order:7");
/// assert_eq!(resolve_pattern("order:{missing}", &payload), "order:{missing}");
/// ```
pub fn resolve_pattern(pattern: &str, payload: &serde_json::Value) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // A placeholder is `{` + one-or-more word chars + `}` — pyfly's
        // `\{(\w+)\}`. Anything else is literal text.
        let close = after.find(|c: char| !c.is_alphanumeric() && c != '_');
        match close {
            Some(end) if end > 0 && after[end..].starts_with('}') => {
                let field = &after[..end];
                match field_as_string(payload, field) {
                    Some(value) => out.push_str(&value),
                    None => {
                        out.push('{');
                        out.push_str(field);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Stringifies a payload field the way Python's `str(value)` would for
/// the JSON scalar types the bridge cares about; `None` for missing
/// fields, `null`, or non-object payloads.
///
/// Booleans are the one scalar where serde_json's lowercase
/// `true`/`false` diverges from Python's title-cased `True`/`False`, so
/// they are rendered explicitly to keep cache keys byte-compatible with
/// pyfly's `_resolve_pattern` (which uses `str(value)`).
fn field_as_string(payload: &serde_json::Value, field: &str) -> Option<String> {
    match payload.get(field)? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Booleans must resolve to Python's title-cased `True`/`False` —
    /// pyfly's `_resolve_pattern` uses `str(value)`, so a JSON `true`
    /// becomes `"True"` (not serde_json's lowercase `"true"`). Diverging
    /// here yields a mismatched cache key and a missed eviction against
    /// pyfly-flavoured producers/consumers.
    #[test]
    fn resolve_pattern_stringifies_booleans_like_python_str() {
        let payload = serde_json::json!({"enabled": true, "disabled": false});
        assert_eq!(resolve_pattern("flag:{enabled}", &payload), "flag:True");
        assert_eq!(resolve_pattern("flag:{disabled}", &payload), "flag:False");
    }

    /// Integers, floats and strings already match Python `str(value)` on
    /// both runtimes; lock that in so the boolean fix does not regress the
    /// other scalar types.
    #[test]
    fn resolve_pattern_matches_python_str_for_other_scalars() {
        let payload = serde_json::json!({"id": 7, "ratio": 1.0, "name": "acme"});
        assert_eq!(resolve_pattern("order:{id}", &payload), "order:7");
        assert_eq!(resolve_pattern("ratio:{ratio}", &payload), "ratio:1.0");
        assert_eq!(resolve_pattern("tenant:{name}", &payload), "tenant:acme");
    }
}
