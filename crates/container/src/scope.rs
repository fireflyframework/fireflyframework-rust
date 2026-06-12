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

//! Bean lifecycle scopes and the custom-scope SPI.
//!
//! Ports pyfly's `container.types.Scope` / `ScopeHandler` and
//! `container.refresh_scope.RefreshScope`. The reflective REQUEST/SESSION
//! resolution (which reaches into a Python `RequestContext`) is adapted: those
//! scopes are still representable as [`Scope`] variants, but a Rust consumer
//! drives per-request/per-session caching through a custom [`ScopeHandler`]
//! (the same SPI pyfly exposes via `register_scope`).

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// An instance shared across the container, type-erased for storage.
pub type SharedInstance = Arc<dyn Any + Send + Sync>;

/// Built-in bean lifecycle scope.
///
/// Mirrors pyfly's `Scope` enum. A bean's effective scope is either one of
/// these built-ins or a custom scope name registered with
/// [`Container::register_scope`](crate::Container::register_scope) — see
/// [`ScopeSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// One shared instance per container (created lazily, cached).
    #[default]
    Singleton,
    /// A fresh instance on every resolution.
    Transient,
    /// One instance per request — managed by a custom [`ScopeHandler`].
    Request,
    /// One instance per session — managed by a custom [`ScopeHandler`].
    Session,
}

impl Scope {
    /// The display name of this scope (lower-case, matching pyfly's reserved
    /// scope names).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Scope::Singleton => "singleton",
            Scope::Transient => "transient",
            Scope::Request => "request",
            Scope::Session => "session",
        }
    }
}

/// A bean's scope: a built-in [`Scope`] or a custom scope name.
///
/// Mirrors pyfly's `ScopeSpec = Scope | str`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeSpec {
    /// A built-in scope.
    Builtin(Scope),
    /// A custom scope registered by name via
    /// [`Container::register_scope`](crate::Container::register_scope).
    Custom(String),
}

impl ScopeSpec {
    /// Display name for this scope — the enum member name or the custom string.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            ScopeSpec::Builtin(s) => s.name().to_string(),
            ScopeSpec::Custom(s) => s.clone(),
        }
    }
}

impl Default for ScopeSpec {
    fn default() -> Self {
        ScopeSpec::Builtin(Scope::Singleton)
    }
}

impl From<Scope> for ScopeSpec {
    fn from(s: Scope) -> Self {
        ScopeSpec::Builtin(s)
    }
}

impl From<&str> for ScopeSpec {
    fn from(s: &str) -> Self {
        ScopeSpec::Custom(s.to_string())
    }
}

impl From<String> for ScopeSpec {
    fn from(s: String) -> Self {
        ScopeSpec::Custom(s)
    }
}

/// SPI for a custom bean scope (Spring's `org.springframework...config.Scope`).
///
/// Ports pyfly's `ScopeHandler` protocol. Register an implementation with
/// [`Container::register_scope`](crate::Container::register_scope) and declare
/// beans with that scope name. Implementations must be `Send + Sync` so the
/// container can be shared as `Arc<Container>` across threads.
pub trait ScopeHandler: Send + Sync {
    /// Return the cached instance for `name`, or create it via `object_factory`
    /// (called at most once per key), cache it, and return it.
    ///
    /// `object_factory` returns a `Result` so a creation failure (e.g. a
    /// circular dependency surfaced by the container) propagates instead of
    /// being cached.
    fn get(
        &self,
        name: &str,
        object_factory: &dyn Fn() -> Result<SharedInstance, crate::ContainerError>,
    ) -> Result<SharedInstance, crate::ContainerError>;

    /// Evict `name` from the scope, returning the removed instance or `None`.
    fn remove(&self, name: &str) -> Option<SharedInstance>;
}

/// A thread-safe [`ScopeHandler`] that caches instances until [`RefreshScope::refresh`]
/// evicts them all.
///
/// Ports pyfly's `container.refresh_scope.RefreshScope` (Spring Cloud's
/// `@RefreshScope`). A refresh-scoped bean is cached like a singleton, but a
/// refresh evicts every refresh-scoped instance so the next resolution rebuilds
/// it — the hook a future `/actuator/refresh` calls on config reload.
#[derive(Debug, Default)]
pub struct RefreshScope {
    cache: Mutex<HashMap<String, SharedInstance>>,
}

/// The custom-scope name under which a [`RefreshScope`] handler is conventionally
/// registered. Mirrors pyfly's `REFRESH_SCOPE_NAME`.
pub const REFRESH_SCOPE_NAME: &str = "refresh";

impl RefreshScope {
    /// Create an empty refresh scope.
    #[must_use]
    pub fn new() -> Self {
        RefreshScope {
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Evict every cached refresh-scoped instance; returns the evicted cache keys.
    ///
    /// Mirrors pyfly's `RefreshScope.refresh()`.
    pub fn refresh(&self) -> Vec<String> {
        let mut cache = self.cache.lock().expect("RefreshScope mutex poisoned");
        let keys: Vec<String> = cache.keys().cloned().collect();
        cache.clear();
        keys
    }
}

impl ScopeHandler for RefreshScope {
    fn get(
        &self,
        name: &str,
        object_factory: &dyn Fn() -> Result<SharedInstance, crate::ContainerError>,
    ) -> Result<SharedInstance, crate::ContainerError> {
        if let Some(existing) = self
            .cache
            .lock()
            .expect("RefreshScope mutex poisoned")
            .get(name)
        {
            return Ok(existing.clone());
        }
        // Double-checked create, mirroring the container's SINGLETON path.
        let instance = object_factory()?;
        let mut cache = self.cache.lock().expect("RefreshScope mutex poisoned");
        let entry = cache.entry(name.to_string()).or_insert(instance);
        Ok(entry.clone())
    }

    fn remove(&self, name: &str) -> Option<SharedInstance> {
        self.cache
            .lock()
            .expect("RefreshScope mutex poisoned")
            .remove(name)
    }
}
