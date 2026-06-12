//! Ported from pyfly `tests/container/test_container_basics.py`.
//!
//! Constructor injection is expressed as explicit factory closures that call
//! `resolve` (the Rust idiom replacing reflective autowiring). Optional/`list`
//! injection becomes the factory's own `resolve().ok()` / `resolve_all()` calls.

use std::sync::Arc;

use firefly_container::{Container, ContainerError, Scope};

#[derive(Debug)]
struct Greeter;
impl Greeter {
    fn greet(&self) -> &'static str {
        "hello"
    }
}

struct UserService {
    greeter: Arc<Greeter>,
}

#[test]
fn register_and_resolve() {
    let c = Container::new();
    c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
    let instance = c.resolve::<Greeter>().unwrap();
    assert_eq!(instance.greet(), "hello");
}

#[test]
fn resolve_with_dependency() {
    let c = Container::new();
    c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
    c.register_factory::<UserService, _>(Scope::Singleton, |c| {
        Ok(UserService {
            greeter: c.resolve::<Greeter>()?,
        })
    });
    let service = c.resolve::<UserService>().unwrap();
    assert_eq!(service.greeter.greet(), "hello");
}

#[test]
fn singleton_scope_returns_same_instance() {
    let c = Container::new();
    c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
    let a = c.resolve::<Greeter>().unwrap();
    let b = c.resolve::<Greeter>().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn transient_scope_returns_new_instance() {
    let c = Container::new();
    c.register_factory::<Greeter, _>(Scope::Transient, |_| Ok(Greeter));
    let a = c.resolve::<Greeter>().unwrap();
    let b = c.resolve::<Greeter>().unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
}

#[test]
fn resolve_unregistered_raises() {
    let c = Container::new();
    let err = c.resolve::<Greeter>().unwrap_err();
    assert!(matches!(err, ContainerError::NoSuchBean { .. }));
}

// -- bind interface to implementation --

trait Cache: Send + Sync {
    fn kind(&self) -> &'static str;
}
struct RedisCache;
impl Cache for RedisCache {
    fn kind(&self) -> &'static str {
        "redis"
    }
}

#[test]
fn bind_interface_to_implementation() {
    let c = Container::new();
    c.register_factory::<RedisCache, _>(Scope::Singleton, |_| Ok(RedisCache));
    c.bind::<dyn Cache, RedisCache>(|a| a);
    let instance = c.resolve::<dyn Cache>().unwrap();
    assert_eq!(instance.kind(), "redis");
}

// -- parameter defaults: a missing optional dependency falls back --

struct DefaultService {
    name: String,
}

#[test]
fn respects_param_defaults() {
    // pyfly: a constructor param with a default is skipped when unresolvable.
    // Rust: the factory chooses the default explicitly when resolve fails.
    let c = Container::new();
    c.register_factory::<DefaultService, _>(Scope::Singleton, |c| {
        let name = c
            .resolve::<String>()
            .map(|s| (*s).clone())
            .unwrap_or_else(|_| "default".to_string());
        Ok(DefaultService { name })
    });
    let svc = c.resolve::<DefaultService>().unwrap();
    assert_eq!(svc.name, "default");
}

// -- optional injection --

struct OptionalGreeterService {
    greeter: Option<Arc<Greeter>>,
}

#[test]
fn optional_resolves_to_none_when_missing() {
    let c = Container::new();
    c.register_factory::<OptionalGreeterService, _>(Scope::Singleton, |c| {
        Ok(OptionalGreeterService {
            greeter: c.resolve::<Greeter>().ok(),
        })
    });
    let svc = c.resolve::<OptionalGreeterService>().unwrap();
    assert!(svc.greeter.is_none());
}

#[test]
fn optional_resolves_to_instance_when_registered() {
    let c = Container::new();
    c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
    c.register_factory::<OptionalGreeterService, _>(Scope::Singleton, |c| {
        Ok(OptionalGreeterService {
            greeter: c.resolve::<Greeter>().ok(),
        })
    });
    let svc = c.resolve::<OptionalGreeterService>().unwrap();
    assert_eq!(svc.greeter.as_ref().unwrap().greet(), "hello");
}

// -- list injection --

trait Validator: Send + Sync {}
struct EmailValidator;
impl Validator for EmailValidator {}
struct PhoneValidator;
impl Validator for PhoneValidator {}

struct ValidationService {
    validators: Vec<Arc<dyn Validator>>,
}

#[test]
fn list_collects_all_implementations() {
    let c = Container::new();
    c.register_factory::<EmailValidator, _>(Scope::Singleton, |_| Ok(EmailValidator));
    c.register_factory::<PhoneValidator, _>(Scope::Singleton, |_| Ok(PhoneValidator));
    c.bind::<dyn Validator, EmailValidator>(|a| a);
    c.bind::<dyn Validator, PhoneValidator>(|a| a);
    c.register_factory::<ValidationService, _>(Scope::Singleton, |c| {
        Ok(ValidationService {
            validators: c.resolve_all::<dyn Validator>()?,
        })
    });
    let svc = c.resolve::<ValidationService>().unwrap();
    assert_eq!(svc.validators.len(), 2);
}

#[test]
fn list_returns_empty_when_no_bindings() {
    let c = Container::new();
    c.register_factory::<ValidationService, _>(Scope::Singleton, |c| {
        Ok(ValidationService {
            validators: c.resolve_all::<dyn Validator>()?,
        })
    });
    let svc = c.resolve::<ValidationService>().unwrap();
    assert!(svc.validators.is_empty());
}
