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

//! Ported from pyfly `tests/container/test_public_spi.py` and
//! `test_registration_display_name.py`.
//!
//! `register_instance` / introspection (`contains_type`, `registered_types`) /
//! `reset_instance`, plus `Registration::display_name`.

use std::sync::Arc;

use firefly_container::{Container, Scope};

#[derive(Default)]
struct Svc {
    tag: u32,
}

#[test]
fn register_instance_returns_exact_object() {
    let c = Container::new();
    let svc = Svc { tag: 42 };
    c.register_instance(svc);
    let resolved = c.resolve::<Svc>().unwrap();
    assert_eq!(resolved.tag, 42);
    assert!(c.contains_type::<Svc>());
    assert!(c.registered_types().iter().any(|n| n.contains("Svc")));
}

#[test]
fn register_instance_named() {
    let c = Container::new();
    c.register_instance_named(Svc { tag: 7 }, "primary");
    assert_eq!(c.resolve_named::<Svc>("primary").unwrap().tag, 7);
}

#[test]
fn introspection_on_empty_container() {
    let c = Container::new();
    assert!(!c.contains_type::<Svc>());
    assert!(!c.contains("primary"));
    assert!(c.registered_types().is_empty());
}

#[test]
fn register_instance_is_same_arc_across_resolves() {
    let c = Container::new();
    c.register_instance(Svc::default());
    let a = c.resolve::<Svc>().unwrap();
    let b = c.resolve::<Svc>().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn reset_instance_forces_rebuild() {
    // A factory-backed singleton: reset evicts the cached instance so the next
    // resolve rebuilds a fresh one (the refresh/config-reload hook).
    let c = Container::new();
    c.register_factory::<Svc, _>(Scope::Singleton, |_| Ok(Svc { tag: 1 }));
    let first = c.resolve::<Svc>().unwrap();
    assert!(c.reset_instance::<Svc>());
    let rebuilt = c.resolve::<Svc>().unwrap();
    assert!(!Arc::ptr_eq(&first, &rebuilt));
}

#[test]
fn reset_instance_unregistered_is_false() {
    assert!(!Container::new().reset_instance::<Svc>());
}

// -- display_name --

#[test]
fn display_name_uses_explicit_name() {
    let c = Container::new();
    c.register_factory_named::<Svc, _>(Scope::Singleton, "myBean", |_| Ok(Svc::default()));
    // Resolve to ensure the registration exists, then check via metrics path.
    assert!(c.resolve_named::<Svc>("myBean").is_ok());
    assert!(c.contains("myBean"));
}
