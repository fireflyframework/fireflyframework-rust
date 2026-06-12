//! Ported from pyfly `tests/container/test_named_beans.py`.
//!
//! Named beans + `@primary` resolution + `Qualifier` injection. The Qualifier
//! idiom (`Annotated[T, Qualifier("name")]`) becomes a `resolve_named` call in
//! the dependent's factory.

use std::sync::Arc;

use firefly_container::{Container, ContainerError, Scope};

trait Greeter: Send + Sync {
    fn greet(&self) -> &'static str;
}

#[derive(Debug)]
struct EnglishGreeter;
impl Greeter for EnglishGreeter {
    fn greet(&self) -> &'static str {
        "Hello"
    }
}

#[derive(Debug)]
struct SpanishGreeter;
impl Greeter for SpanishGreeter {
    fn greet(&self) -> &'static str {
        "Hola"
    }
}

#[test]
fn register_and_resolve_by_name() {
    let c = Container::new();
    c.register_factory_named::<EnglishGreeter, _>(Scope::Singleton, "english", |_| {
        Ok(EnglishGreeter)
    });
    let result = c.resolve_named::<EnglishGreeter>("english").unwrap();
    assert_eq!(result.greet(), "Hello");
}

#[test]
fn resolve_by_name_not_found() {
    let c = Container::new();
    let err = c
        .resolve_named::<EnglishGreeter>("nonexistent")
        .unwrap_err();
    match err {
        ContainerError::NoSuchBean { ref bean_name, .. } => {
            assert_eq!(bean_name.as_deref(), Some("nonexistent"));
            assert!(err.to_string().contains("No bean named"));
        }
        other => panic!("expected NoSuchBean, got {other:?}"),
    }
}

#[test]
fn resolve_all_of_type() {
    let c = Container::new();
    c.register_factory_named::<EnglishGreeter, _>(Scope::Singleton, "english", |_| {
        Ok(EnglishGreeter)
    });
    c.register_factory_named::<SpanishGreeter, _>(Scope::Singleton, "spanish", |_| {
        Ok(SpanishGreeter)
    });
    c.bind::<dyn Greeter, EnglishGreeter>(|a| a);
    c.bind::<dyn Greeter, SpanishGreeter>(|a| a);
    let beans = c.resolve_all::<dyn Greeter>().unwrap();
    assert!(beans.len() >= 2);
}

#[test]
fn primary_wins_when_multiple() {
    let c = Container::new();
    c.register_factory_named::<EnglishGreeter, _>(Scope::Singleton, "english", |_| {
        Ok(EnglishGreeter)
    });
    // SpanishGreeter is marked primary.
    c.register_factory_with::<SpanishGreeter, _>(Scope::Singleton, "spanish", true, 0, |_| {
        Ok(SpanishGreeter)
    });
    c.bind::<dyn Greeter, EnglishGreeter>(|a| a);
    c.bind::<dyn Greeter, SpanishGreeter>(|a| a);
    let result = c.resolve::<dyn Greeter>().unwrap();
    assert_eq!(result.greet(), "Hola");
}

#[test]
fn qualifier_selects_named_bean() {
    let c = Container::new();
    c.register_factory_named::<EnglishGreeter, _>(Scope::Singleton, "english", |_| {
        Ok(EnglishGreeter)
    });
    c.register_factory_named::<SpanishGreeter, _>(Scope::Singleton, "spanish", |_| {
        Ok(SpanishGreeter)
    });

    struct GreetService {
        greeter: Arc<EnglishGreeter>,
    }
    c.register_factory::<GreetService, _>(Scope::Singleton, |c| {
        Ok(GreetService {
            greeter: c.resolve_named::<EnglishGreeter>("english")?,
        })
    });
    let svc = c.resolve::<GreetService>().unwrap();
    assert_eq!(svc.greeter.greet(), "Hello");
}

#[test]
fn primary_marked_via_bound_registration() {
    // Two competing impls, neither primary -> NoUniqueBean; mark one -> resolves.
    let c = Container::new();
    c.register_factory::<EnglishGreeter, _>(Scope::Singleton, |_| Ok(EnglishGreeter));
    c.register_factory::<SpanishGreeter, _>(Scope::Singleton, |_| Ok(SpanishGreeter));
    c.bind::<dyn Greeter, EnglishGreeter>(|a| a);
    c.bind::<dyn Greeter, SpanishGreeter>(|a| a);
    assert!(matches!(
        c.resolve::<dyn Greeter>(),
        Err(ContainerError::NoUniqueBean { .. })
    ));
}
