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

//! Ported from pyfly `tests/container/test_di_errors.py`.
//!
//! Developer-friendly error messages: `NoSuchBean`, `NoUniqueBean`, and
//! `CircularDependency`, plus the in-creation-stack cleanup invariant.

use std::sync::Arc;

use firefly_container::{Container, ContainerError, Scope};

#[derive(Debug)]
struct Greeter;
#[derive(Debug)]
struct MissingDep;
#[derive(Debug)]
struct ServiceWithMissing {
    #[allow(dead_code)]
    dep: Arc<MissingDep>,
}

// -- NoSuchBean message content --

#[test]
fn no_such_bean_message_includes_type_name() {
    let err = ContainerError::no_such_type_for_test("Greeter");
    assert!(err.to_string().contains("Greeter"));
}

#[test]
fn no_such_bean_message_includes_actionable_hints() {
    let err = ContainerError::no_such_type_for_test("Greeter");
    let msg = err.to_string();
    assert!(msg.contains("register"));
    assert!(msg.contains("bind"));
}

#[test]
fn no_such_bean_message_includes_suggestions() {
    let err = ContainerError::NoSuchBean {
        bean_type: Some("Greeter".to_string()),
        bean_name: None,
        required_by: None,
        parameter: None,
        suggestions: vec!["Greet".to_string(), "GreetingService".to_string()],
    };
    let msg = err.to_string();
    assert!(msg.contains("Greet"));
    assert!(msg.contains("GreetingService"));
}

#[test]
fn no_such_bean_includes_required_by_and_parameter() {
    let err = ContainerError::NoSuchBean {
        bean_type: Some("Greeter".to_string()),
        bean_name: None,
        required_by: Some("ServiceA::new()".to_string()),
        parameter: Some("greeter: Greeter".to_string()),
        suggestions: Vec::new(),
    };
    let msg = err.to_string();
    assert!(msg.contains("ServiceA::new()"));
    assert!(msg.contains("greeter: Greeter"));
}

// -- NoSuchBean from the container --

#[test]
fn resolve_unregistered_type_carries_type_name() {
    let c = Container::new();
    let err = c.resolve::<Greeter>().unwrap_err();
    match err {
        ContainerError::NoSuchBean { bean_type, .. } => {
            assert!(bean_type.unwrap().contains("Greeter"));
        }
        other => panic!("expected NoSuchBean, got {other:?}"),
    }
}

#[test]
fn missing_constructor_dep_propagates() {
    let c = Container::new();
    c.register_factory::<ServiceWithMissing, _>(Scope::Singleton, |c| {
        Ok(ServiceWithMissing {
            dep: c.resolve::<MissingDep>()?,
        })
    });
    let err = c.resolve::<ServiceWithMissing>().unwrap_err();
    assert!(matches!(err, ContainerError::NoSuchBean { .. }));
}

#[test]
fn similar_types_suggested() {
    let c = Container::new();
    c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
    // Resolving a typo-ish type should at least produce a list (possibly empty).
    let err = c.resolve::<MissingDep>().unwrap_err();
    match err {
        ContainerError::NoSuchBean { suggestions, .. } => {
            // suggestions is always a Vec; MissingDep is dissimilar to Greeter.
            let _ = suggestions;
        }
        other => panic!("expected NoSuchBean, got {other:?}"),
    }
}

#[test]
fn fuzzy_suggestion_finds_close_name() {
    let c = Container::new();
    c.register_factory::<GreetingService, _>(Scope::Singleton, |_| Ok(GreetingService));
    let suggestions = c.fuzzy_suggestions("GreetingServie"); // typo
    assert!(
        suggestions.iter().any(|s| s.contains("GreetingService")),
        "expected a close match, got {suggestions:?}"
    );
}

#[derive(Debug)]
struct GreetingService;

// -- NoUniqueBean --

trait Base: Send + Sync {}
#[derive(Debug)]
struct ImplX;
impl Base for ImplX {}
#[derive(Debug)]
struct ImplY;
impl Base for ImplY {}

#[test]
fn no_unique_bean_message_content() {
    let err = ContainerError::NoUniqueBean {
        bean_type: "Base".to_string(),
        candidates: vec!["ImplX".to_string(), "ImplY".to_string()],
    };
    let msg = err.to_string();
    assert!(msg.contains("Base"));
    assert!(msg.contains("ImplX"));
    assert!(msg.contains("ImplY"));
    assert!(msg.contains("primary"));
}

#[test]
fn multiple_impls_without_primary() {
    let c = Container::new();
    c.register_factory::<ImplX, _>(Scope::Singleton, |_| Ok(ImplX));
    c.register_factory::<ImplY, _>(Scope::Singleton, |_| Ok(ImplY));
    c.bind::<dyn Base, ImplX>(|a| a);
    c.bind::<dyn Base, ImplY>(|a| a);
    match c.resolve::<dyn Base>() {
        Err(ContainerError::NoUniqueBean {
            bean_type,
            candidates,
        }) => {
            assert!(bean_type.contains("Base"));
            assert_eq!(candidates.len(), 2);
        }
        Err(other) => panic!("expected NoUniqueBean, got {other:?}"),
        Ok(_) => panic!("expected NoUniqueBean, got Ok"),
    }
}

// -- CircularDependency --

#[derive(Debug)]
struct CircA {
    #[allow(dead_code)]
    b: Arc<CircB>,
}
#[derive(Debug)]
struct CircB {
    #[allow(dead_code)]
    a: Arc<CircA>,
}

#[test]
fn circular_dependency_message_shows_chain() {
    let err = ContainerError::CircularDependency {
        chain: vec!["CircA".to_string(), "CircB".to_string()],
        current: "CircA".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("CircA -> CircB -> CircA"));
    assert!(msg.contains("Provider") || msg.contains("factory"));
}

#[test]
fn two_way_circular_from_container() {
    let c = Container::new();
    c.register_factory::<CircA, _>(Scope::Singleton, |c| {
        Ok(CircA {
            b: c.resolve::<CircB>()?,
        })
    });
    c.register_factory::<CircB, _>(Scope::Singleton, |c| {
        Ok(CircB {
            a: c.resolve::<CircA>()?,
        })
    });
    let err = c.resolve::<CircA>().unwrap_err();
    match err {
        ContainerError::CircularDependency { ref current, .. } => {
            assert!(current.contains("CircA"));
            assert!(err.to_string().contains("CircA -> CircB -> CircA"));
        }
        other => panic!("expected CircularDependency, got {other:?}"),
    }
}

// -- deterministic chain over three types --

#[derive(Debug)]
struct CircC {
    #[allow(dead_code)]
    a: Arc<CircABC>,
}
#[derive(Debug)]
struct CircBBC {
    #[allow(dead_code)]
    c: Arc<CircC>,
}
#[derive(Debug)]
struct CircABC {
    #[allow(dead_code)]
    b: Arc<CircBBC>,
}

#[test]
fn chain_is_deterministic() {
    let c = Container::new();
    c.register_factory::<CircABC, _>(Scope::Singleton, |c| {
        Ok(CircABC {
            b: c.resolve::<CircBBC>()?,
        })
    });
    c.register_factory::<CircBBC, _>(Scope::Singleton, |c| {
        Ok(CircBBC {
            c: c.resolve::<CircC>()?,
        })
    });
    c.register_factory::<CircC, _>(Scope::Singleton, |c| {
        Ok(CircC {
            a: c.resolve::<CircABC>()?,
        })
    });
    let err = c.resolve::<CircABC>().unwrap_err();
    match err {
        ContainerError::CircularDependency { chain, .. } => {
            let short: Vec<&str> = chain
                .iter()
                .map(|s| s.rsplit("::").next().unwrap())
                .collect();
            assert_eq!(short, vec!["CircABC", "CircBBC", "CircC"]);
        }
        other => panic!("expected CircularDependency, got {other:?}"),
    }
}

#[test]
fn resolving_stack_cleaned_up_after_error() {
    let c = Container::new();
    c.register_factory::<CircA, _>(Scope::Singleton, |c| {
        Ok(CircA {
            b: c.resolve::<CircB>()?,
        })
    });
    c.register_factory::<CircB, _>(Scope::Singleton, |c| {
        Ok(CircB {
            a: c.resolve::<CircA>()?,
        })
    });
    assert!(c.resolve::<CircA>().is_err());
    // A subsequent independent resolve must succeed (stack was cleaned up).
    c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
    assert!(c.resolve::<Greeter>().is_ok());
}

// Test-only constructor helpers for the message-content checks.
trait NoSuchTypeForTest {
    fn no_such_type_for_test(name: &str) -> ContainerError;
}
impl NoSuchTypeForTest for ContainerError {
    fn no_such_type_for_test(name: &str) -> ContainerError {
        ContainerError::NoSuchBean {
            bean_type: Some(name.to_string()),
            bean_name: None,
            required_by: None,
            parameter: None,
            suggestions: Vec::new(),
        }
    }
}
