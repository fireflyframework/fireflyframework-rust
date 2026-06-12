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

//! Ported from pyfly `tests/container/test_custom_scope.py`,
//! `test_request_scope.py`, and `test_session_scope.py`.
//!
//! The custom-scope SPI (`register_scope` + `ScopeHandler`) is identical. The
//! REQUEST/SESSION scopes — which in pyfly reach into a thread/context-local
//! `RequestContext` — are adapted to the same SPI: a per-request/per-session
//! `ScopeHandler` is registered under the reserved built-in name, since Rust
//! drives request lifecycle explicitly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use firefly_container::{Container, ContainerError, ScopeHandler, SharedInstance};

/// A trivial `ScopeHandler` caching one instance per key.
#[derive(Default)]
struct DictScope {
    cache: Mutex<HashMap<String, SharedInstance>>,
}

impl ScopeHandler for DictScope {
    fn get(
        &self,
        name: &str,
        object_factory: &dyn Fn() -> Result<SharedInstance, ContainerError>,
    ) -> Result<SharedInstance, ContainerError> {
        if let Some(existing) = self.cache.lock().unwrap().get(name) {
            return Ok(existing.clone());
        }
        let instance = object_factory()?;
        let mut cache = self.cache.lock().unwrap();
        Ok(cache.entry(name.to_string()).or_insert(instance).clone())
    }

    fn remove(&self, name: &str) -> Option<SharedInstance> {
        self.cache.lock().unwrap().remove(name)
    }
}

#[derive(Debug)]
struct Widget;

#[test]
fn custom_scope_caches_via_handler() {
    let c = Container::new();
    c.register_scope("tenant", Arc::new(DictScope::default()))
        .unwrap();
    c.register_factory_scoped::<Widget, _>("tenant", "", |_| Ok(Widget));
    let first = c.resolve::<Widget>().unwrap();
    let second = c.resolve::<Widget>().unwrap();
    assert!(Arc::ptr_eq(&first, &second)); // handler returns the cached instance
}

#[test]
fn unregistered_custom_scope_raises() {
    let c = Container::new();
    c.register_factory_scoped::<Widget, _>("ghost", "", |_| Ok(Widget));
    let err = c.resolve::<Widget>().unwrap_err();
    assert!(err.to_string().contains("not registered"));
}

#[test]
fn register_scope_rejects_builtin_and_empty_names() {
    let c = Container::new();
    for reserved in ["singleton", "transient", "request", "session"] {
        let err = c
            .register_scope(reserved, Arc::new(DictScope::default()))
            .unwrap_err();
        assert!(err.to_string().contains("built-in"));
    }
    let err = c
        .register_scope("", Arc::new(DictScope::default()))
        .unwrap_err();
    assert!(err.to_string().contains("non-empty"));
}

#[test]
fn unregister_scope() {
    let c = Container::new();
    c.register_scope("tenant", Arc::new(DictScope::default()))
        .unwrap();
    c.register_factory_scoped::<Widget, _>("tenant", "", |_| Ok(Widget));
    assert!(c.resolve::<Widget>().is_ok());
    c.unregister_scope("tenant");
    assert!(c.resolve::<Widget>().is_err());
}

#[test]
fn handler_remove_evicts() {
    let scope = Arc::new(DictScope::default());
    let c = Container::new();
    c.register_scope("tenant", scope.clone()).unwrap();
    c.register_factory_scoped::<Widget, _>("tenant", "", |_| Ok(Widget));
    let first = c.resolve::<Widget>().unwrap();
    let key = scope.cache.lock().unwrap().keys().next().unwrap().clone();
    assert!(scope.remove(&key).is_some());
    let rebuilt = c.resolve::<Widget>().unwrap();
    assert!(!Arc::ptr_eq(&first, &rebuilt)); // rebuilt after eviction
}

// -- REQUEST scope driven through a per-request ScopeHandler --

struct DummyRequestService;

#[test]
fn request_scope_via_handler() {
    let c = Container::new();
    // The REQUEST built-in resolves through a handler registered under "request".
    // A fresh DictScope per "request" yields fresh instances.
    c.register_factory::<DummyRequestService, _>(firefly_container::Scope::Request, |_| {
        Ok(DummyRequestService)
    });

    // No handler installed yet -> resolution fails (pyfly: "No active request context").
    assert!(c.resolve::<DummyRequestService>().is_err());

    // Install a request-scoped handler.
    c.register_request_scope(Arc::new(DictScope::default()));
    let a = c.resolve::<DummyRequestService>().unwrap();
    let b = c.resolve::<DummyRequestService>().unwrap();
    assert!(Arc::ptr_eq(&a, &b)); // same instance within the request
}
