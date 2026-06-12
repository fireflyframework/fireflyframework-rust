//! Ported from pyfly `tests/container/test_ordering.py` and
//! `test_same_type_beans.py`.
//!
//! `@order` becomes the `order` argument to `register_factory_with`; pyfly's
//! list[T] ordering and "two beans of the same concrete type both survive"
//! regression map onto `resolve_all` ordered by `order`.

use firefly_container::{Container, Scope, HIGHEST_PRECEDENCE, LOWEST_PRECEDENCE};

#[test]
fn ordering_constants() {
    assert_eq!(HIGHEST_PRECEDENCE, i32::MIN);
    assert_eq!(LOWEST_PRECEDENCE, i32::MAX);
    const { assert!(HIGHEST_PRECEDENCE < LOWEST_PRECEDENCE) };
}

trait Step: Send + Sync {
    fn label(&self) -> &'static str;
}
struct First;
impl Step for First {
    fn label(&self) -> &'static str {
        "first"
    }
}
struct Second;
impl Step for Second {
    fn label(&self) -> &'static str {
        "second"
    }
}
struct Third;
impl Step for Third {
    fn label(&self) -> &'static str {
        "third"
    }
}

#[test]
fn resolve_all_honors_order() {
    let c = Container::new();
    // Register out of order; resolve_all must sort by `order`.
    c.register_factory_with::<Second, _>(Scope::Singleton, "second", false, 10, |_| Ok(Second));
    c.register_factory_with::<Third, _>(Scope::Singleton, "third", false, 20, |_| Ok(Third));
    c.register_factory_with::<First, _>(Scope::Singleton, "first", false, -5, |_| Ok(First));
    c.bind::<dyn Step, Second>(|a| a);
    c.bind::<dyn Step, Third>(|a| a);
    c.bind::<dyn Step, First>(|a| a);

    let labels: Vec<&str> = c
        .resolve_all::<dyn Step>()
        .unwrap()
        .iter()
        .map(|s| s.label())
        .collect();
    assert_eq!(labels, vec!["first", "second", "third"]);
}

// -- same concrete type, two distinct named registrations both survive --

struct Widget {
    label: &'static str,
}

#[test]
fn two_same_type_beans_both_resolvable_by_name() {
    let c = Container::new();
    c.register_factory_named::<Widget, _>(Scope::Singleton, "widget_one", |_| {
        Ok(Widget { label: "one" })
    });
    c.register_factory_named::<Widget, _>(Scope::Singleton, "widget_two", |_| {
        Ok(Widget { label: "two" })
    });
    assert_eq!(
        c.resolve_named::<Widget>("widget_one").unwrap().label,
        "one"
    );
    assert_eq!(
        c.resolve_named::<Widget>("widget_two").unwrap().label,
        "two"
    );
}

#[test]
fn resolve_all_returns_every_same_type_bean() {
    let c = Container::new();
    c.register_factory_named::<Widget, _>(Scope::Singleton, "widget_one", |_| {
        Ok(Widget { label: "one" })
    });
    c.register_factory_named::<Widget, _>(Scope::Singleton, "widget_two", |_| {
        Ok(Widget { label: "two" })
    });
    let mut labels: Vec<&str> = c
        .resolve_all::<Widget>()
        .unwrap()
        .iter()
        .map(|w| w.label)
        .collect();
    labels.sort_unstable();
    assert_eq!(labels, vec!["one", "two"]);
}

#[test]
fn re_registering_same_name_overwrites() {
    // pyfly: a re-registration under the same (type, name) replaces the prior one.
    let c = Container::new();
    c.register_factory_named::<Widget, _>(Scope::Singleton, "w", |_| Ok(Widget { label: "old" }));
    c.register_factory_named::<Widget, _>(Scope::Singleton, "w", |_| Ok(Widget { label: "new" }));
    assert_eq!(c.resolve_named::<Widget>("w").unwrap().label, "new");
    assert_eq!(c.resolve_all::<Widget>().unwrap().len(), 1);
}
