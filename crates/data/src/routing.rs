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

//! Read/write datasource routing ([`RoutingPolicy`]) and a registry of
//! additional named datasources ([`NamedDataSources`]).
//!
//! This is the Rust port of pyfly's `RoutingSessionFactory` /
//! `read_only()` context manager and its `NamedDataSources` holder — the
//! Spring `AbstractRoutingDataSource` and "multiple `DataSource` beans"
//! equivalents.
//!
//! - [`RoutingPolicy`] wraps a primary and an optional read-replica
//!   session-factory; calling it routes to the replica inside a
//!   [`read_only`] scope (when a replica is configured) and to the
//!   primary otherwise. Routing is opt-in: with no replica, the policy
//!   always uses the primary.
//! - [`NamedDataSources`] is a registry of secondary session factories,
//!   keyed by name, the Rust analogue of injecting an extra
//!   `async_sessionmaker` bean.
//!
//! pyfly threads the read-only flag through a `contextvar`; the Rust
//! idiom is a thread-local guard ([`ReadOnlyGuard`]) returned by
//! [`read_only`], restoring the prior value when dropped — nesting and
//! all.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{read_only, RoutingPolicy};
//!
//! let policy = RoutingPolicy::with_replica(|| "PRIMARY", || "REPLICA");
//! assert_eq!(policy.route(), "PRIMARY"); // default: read/write -> primary
//! {
//!     let _ro = read_only();
//!     assert_eq!(policy.route(), "REPLICA"); // read-only -> replica
//! }
//! assert_eq!(policy.route(), "PRIMARY"); // scope restored
//! ```

use std::cell::Cell;
use std::collections::BTreeMap;

thread_local! {
    static READ_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// Whether the current thread is inside a [`read_only`] scope — the Rust
/// equivalent of pyfly's `is_read_only()`.
pub fn is_read_only() -> bool {
    READ_ONLY.with(|c| c.get())
}

/// RAII guard returned by [`read_only`]. While it is alive the current
/// thread is flagged read-only; dropping it restores the previous flag.
///
/// Nesting is supported: an inner guard's drop restores the outer
/// guard's state, so a nested `read_only` scope leaves the outer scope
/// read-only — exactly pyfly's `contextvar` token reset semantics.
#[derive(Debug)]
#[must_use = "the read-only scope ends as soon as the guard is dropped"]
pub struct ReadOnlyGuard {
    previous: bool,
}

impl Drop for ReadOnlyGuard {
    fn drop(&mut self) {
        READ_ONLY.with(|c| c.set(self.previous));
    }
}

/// Marks the enclosing scope read-only so [`RoutingPolicy::route`] uses
/// the replica (when one is configured). The Rust analogue of pyfly's
/// `with read_only():` context manager and Spring's
/// `@Transactional(readOnly = true)`.
///
/// The returned [`ReadOnlyGuard`] must be held for the duration of the
/// scope; dropping it restores the prior read-only state.
pub fn read_only() -> ReadOnlyGuard {
    let previous = READ_ONLY.with(|c| c.replace(true));
    ReadOnlyGuard { previous }
}

/// Routes session creation between a primary (read/write) and an
/// optional read-replica session factory, based on whether the current
/// scope is [`read_only`].
///
/// `S` is the session (or session-factory) type the closures produce —
/// the crate stays storage-agnostic, so this is whatever a concrete
/// adapter hands back (an `async_sessionmaker` analogue, a pool handle,
/// …). The Rust port of pyfly's `RoutingSessionFactory`.
pub struct RoutingPolicy<S> {
    primary: Box<dyn Fn() -> S + Send + Sync>,
    replica: Option<Box<dyn Fn() -> S + Send + Sync>>,
}

impl<S> std::fmt::Debug for RoutingPolicy<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingPolicy")
            .field("has_replica", &self.has_replica())
            .finish_non_exhaustive()
    }
}

impl<S> RoutingPolicy<S> {
    /// Builds a policy with only a primary factory; every route — even
    /// inside a [`read_only`] scope — uses the primary.
    pub fn primary_only(primary: impl Fn() -> S + Send + Sync + 'static) -> Self {
        RoutingPolicy {
            primary: Box::new(primary),
            replica: None,
        }
    }

    /// Builds a policy with a primary and a read-replica factory;
    /// read-only scopes route to the replica.
    pub fn with_replica(
        primary: impl Fn() -> S + Send + Sync + 'static,
        replica: impl Fn() -> S + Send + Sync + 'static,
    ) -> Self {
        RoutingPolicy {
            primary: Box::new(primary),
            replica: Some(Box::new(replica)),
        }
    }

    /// Whether a read-replica factory is configured — pyfly's
    /// `has_replica`.
    pub fn has_replica(&self) -> bool {
        self.replica.is_some()
    }

    /// Forces a primary (read/write) session regardless of scope.
    pub fn primary(&self) -> S {
        (self.primary)()
    }

    /// Forces a replica session, falling back to the primary when no
    /// replica is configured — pyfly's `replica()`.
    pub fn replica(&self) -> S {
        match &self.replica {
            Some(r) => r(),
            None => (self.primary)(),
        }
    }

    /// Routes by scope: the replica when read-only and a replica is
    /// configured, otherwise the primary. The Rust equivalent of
    /// pyfly's `RoutingSessionFactory.__call__`.
    pub fn route(&self) -> S {
        match &self.replica {
            Some(r) if is_read_only() => r(),
            _ => (self.primary)(),
        }
    }
}

/// Registry of named secondary datasource session factories — the Rust
/// port of pyfly's `NamedDataSources`.
///
/// `S` is the secondary session-factory type. Names are stored sorted
/// (a `BTreeMap`), so [`NamedDataSources::names`] returns them in the
/// same sorted order pyfly's `names()` does.
pub struct NamedDataSources<S> {
    factories: BTreeMap<String, S>,
}

impl<S> std::fmt::Debug for NamedDataSources<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NamedDataSources")
            .field("names", &self.names())
            .finish()
    }
}

impl<S> Default for NamedDataSources<S> {
    fn default() -> Self {
        NamedDataSources {
            factories: BTreeMap::new(),
        }
    }
}

impl<S> FromIterator<(String, S)> for NamedDataSources<S> {
    /// Builds a registry from an iterator of `(name, factory)` pairs.
    /// Duplicate names keep the last-seen factory.
    fn from_iter<I: IntoIterator<Item = (String, S)>>(entries: I) -> Self {
        NamedDataSources {
            factories: entries.into_iter().collect(),
        }
    }
}

impl<S> NamedDataSources<S> {
    /// Returns an empty registry.
    pub fn new() -> Self {
        NamedDataSources::default()
    }

    /// Registers (or replaces) the factory for `name`, returning `self`
    /// for chaining.
    pub fn register(mut self, name: impl Into<String>, factory: S) -> Self {
        self.factories.insert(name.into(), factory);
        self
    }

    /// Inserts the factory for `name` in place.
    pub fn insert(&mut self, name: impl Into<String>, factory: S) {
        self.factories.insert(name.into(), factory);
    }

    /// Returns the factory registered for `name`, or
    /// [`RoutingError::UnknownDataSource`] when none is — pyfly's `get`
    /// (which raises `KeyError`).
    pub fn get(&self, name: &str) -> Result<&S, RoutingError> {
        self.factories
            .get(name)
            .ok_or_else(|| RoutingError::UnknownDataSource {
                name: name.to_string(),
                configured: self.names(),
            })
    }

    /// Sorted names of all registered datasources — pyfly's `names()`.
    pub fn names(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }

    /// Whether a datasource named `name` is registered — pyfly's
    /// `__contains__`.
    pub fn contains(&self, name: &str) -> bool {
        self.factories.contains_key(name)
    }

    /// The number of registered datasources — pyfly's `__len__`.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// Iterates over `(name, factory)` pairs in sorted name order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &S)> {
        self.factories.iter()
    }
}

/// Errors raised by [`NamedDataSources`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoutingError {
    /// No datasource was registered under the requested name. The
    /// message mirrors pyfly's `KeyError("No datasource named …")`.
    #[error("firefly/data: no datasource named {name:?}; configured: {configured:?}")]
    UnknownDataSource {
        /// The requested (missing) datasource name.
        name: String,
        /// The sorted list of names that *are* registered.
        configured: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- RoutingPolicy (pyfly test_routing.py) ----

    /// Port of `test_routes_to_replica_only_inside_read_only`.
    #[test]
    fn test_routes_to_replica_only_inside_read_only() {
        let policy = RoutingPolicy::with_replica(|| "PRIMARY", || "REPLICA");
        assert!(policy.has_replica());
        assert_eq!(policy.route(), "PRIMARY"); // default: read/write -> primary
        {
            let _ro = read_only();
            assert_eq!(policy.route(), "REPLICA"); // read-only -> replica
        }
        assert_eq!(policy.route(), "PRIMARY"); // context restored
    }

    /// Port of `test_no_replica_always_primary`.
    #[test]
    fn test_no_replica_always_primary() {
        let policy = RoutingPolicy::primary_only(|| "PRIMARY");
        assert!(!policy.has_replica());
        {
            let _ro = read_only();
            assert_eq!(policy.route(), "PRIMARY"); // no replica -> primary even read-only
        }
        assert_eq!(policy.replica(), "PRIMARY"); // explicit replica() falls back to primary
    }

    /// Port of `test_explicit_accessors_and_nesting`.
    #[test]
    fn test_explicit_accessors_and_nesting() {
        let policy = RoutingPolicy::with_replica(|| "PRIMARY", || "REPLICA");
        assert_eq!(policy.primary(), "PRIMARY");
        assert_eq!(policy.replica(), "REPLICA");
        assert!(!is_read_only());
        {
            let _outer = read_only();
            assert!(is_read_only());
            {
                let _inner = read_only();
                assert!(is_read_only());
            }
            assert!(is_read_only()); // inner exit keeps the outer read-only state
        }
        assert!(!is_read_only());
    }

    // ---- NamedDataSources (pyfly test_named_datasources.py) ----

    /// Port of `test_holder_get_names_contains_len`.
    #[test]
    fn test_holder_get_names_contains_len() {
        let nds = NamedDataSources::from_iter([
            ("reporting".to_string(), "F_r"),
            ("audit".to_string(), "F_a"),
        ]);
        assert_eq!(*nds.get("reporting").unwrap(), "F_r");
        assert_eq!(nds.names(), vec!["audit", "reporting"]);
        assert!(nds.contains("audit"));
        assert_eq!(nds.len(), 2);
        let err = nds.get("missing").unwrap_err();
        assert!(err.to_string().contains("no datasource named"), "{err}");
    }

    /// Building skips entries without a usable factory — the Rust
    /// analogue of pyfly's "broken (no url) -> skipped".
    #[test]
    fn test_build_skips_entries_without_url() {
        // simulate config: name -> Option<url>; only the ones with a url
        // produce a factory, exactly as build_named_data_sources does.
        let config: Vec<(&str, Option<&str>)> = vec![
            ("reporting", Some("sqlite:///r.db")),
            ("audit", Some("sqlite:///a.db")),
            ("broken", None),
        ];
        let mut created: Vec<String> = Vec::new();
        let mut nds = NamedDataSources::new();
        for (name, url) in config {
            if let Some(url) = url {
                created.push(url.to_string());
                nds.insert(name, format!("sm:engine:{url}"));
            }
        }
        assert_eq!(nds.names(), vec!["audit", "reporting"]); // "broken" skipped
        assert_eq!(*nds.get("reporting").unwrap(), "sm:engine:sqlite:///r.db");
        assert!(created.contains(&"sqlite:///a.db".to_string()));
    }

    /// Port of `test_build_empty_when_no_datasources_configured`.
    #[test]
    fn test_empty_registry() {
        let nds: NamedDataSources<String> = NamedDataSources::new();
        assert_eq!(nds.names(), Vec::<String>::new());
        assert_eq!(nds.len(), 0);
        assert!(nds.is_empty());
    }

    #[test]
    fn test_register_chaining_and_iter() {
        let nds = NamedDataSources::new()
            .register("b", 2)
            .register("a", 1)
            .register("c", 3);
        let collected: Vec<(&String, &i32)> = nds.iter().collect();
        assert_eq!(collected[0].0, "a"); // sorted order
        assert_eq!(*collected[0].1, 1);
        assert_eq!(nds.len(), 3);
    }

    #[test]
    fn test_register_replaces_existing() {
        let nds = NamedDataSources::new()
            .register("a", "first")
            .register("a", "second");
        assert_eq!(*nds.get("a").unwrap(), "second");
        assert_eq!(nds.len(), 1);
    }

    /// Read-only flag is per-thread, so a separate thread is not
    /// affected by this thread's scope.
    #[test]
    fn test_read_only_is_thread_local() {
        let _ro = read_only();
        assert!(is_read_only());
        let other = std::thread::spawn(is_read_only).join().unwrap();
        assert!(!other);
    }
}
