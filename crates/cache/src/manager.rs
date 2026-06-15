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

//! The process-global cache adapter the declarative `#[cacheable]` /
//! `#[cache_put]` / `#[cache_evict]` macros resolve at call time — the Rust
//! analog of Spring's `CacheManager` bean. An application registers one cache
//! once at startup (the same first-wins pattern as
//! [`register_transaction_manager`](firefly_transactional::register_transaction_manager));
//! a method annotated with a cache macro then transparently reads/writes it,
//! and is a plain method call when no cache is registered.

use std::sync::{Arc, OnceLock};

use crate::adapter::Adapter;

static CACHE: OnceLock<Arc<dyn Adapter>> = OnceLock::new();

/// Registers the process-global cache adapter used by the declarative cache
/// macros. First-wins: a later call is a no-op and returns `false`, so an
/// application's explicit registration is never clobbered by a default.
pub fn register_cache(adapter: Arc<dyn Adapter>) -> bool {
    CACHE.set(adapter).is_ok()
}

/// The registered process-global cache adapter, or `None` when none has been
/// registered (in which case a `#[cacheable]` method just runs its body).
pub fn cache_adapter() -> Option<Arc<dyn Adapter>> {
    CACHE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryAdapter;

    #[test]
    fn register_is_first_wins_and_readable() {
        // A fresh process would start empty; this test runs in a binary that
        // may share the global, so only assert the first-wins contract once set.
        let first = Arc::new(MemoryAdapter::new());
        if register_cache(first) {
            assert!(cache_adapter().is_some());
            // A second registration must not replace the first.
            assert!(!register_cache(Arc::new(MemoryAdapter::new())));
        }
    }
}
