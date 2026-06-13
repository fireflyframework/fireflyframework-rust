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

//! Container-level coverage for the introspection / lifecycle / condition /
//! `@Value` surface added for best-in-class DI. The macro-driven `scan()` path
//! is covered end-to-end in `firefly-macros/tests/di.rs`; here we exercise the
//! plain container API directly.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use firefly_container::{ConditionContext, Container, Scope};

#[derive(Default)]
struct Repo;

#[derive(Default)]
struct Service;

#[test]
fn beans_introspection_reports_name_scope_stereotype_primary() {
    let c = Container::new();
    c.register_factory_with::<Repo, _>(Scope::Singleton, "repo", false, 0, |_| Ok(Repo));
    c.set_stereotype::<Repo>("repository");
    c.register_factory_with::<Service, _>(Scope::Transient, "", true, 0, |_| Ok(Service));
    c.set_stereotype::<Service>("service");

    let beans = c.beans();
    assert_eq!(beans.len(), 2);

    let repo = beans.iter().find(|b| b.name == "repo").unwrap();
    assert_eq!(repo.scope, "singleton");
    assert_eq!(repo.stereotype.as_deref(), Some("repository"));
    assert!(!repo.primary);
    assert!(!repo.initialized, "singleton not yet resolved");

    let svc = beans
        .iter()
        .find(|b| b.type_name.ends_with("Service"))
        .unwrap();
    assert_eq!(svc.scope, "transient");
    assert!(svc.primary);

    // Resolving bumps the resolution count + marks the singleton initialized.
    let _ = c.resolve::<Repo>().unwrap();
    let beans2 = c.beans();
    let repo2 = beans2.iter().find(|b| b.name == "repo").unwrap();
    assert!(repo2.initialized);
    assert_eq!(repo2.resolution_count, 1);

    // Aggregate stats.
    let stats = c.bean_stats();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.stereotypes.get("repository").copied(), Some(1));
    assert_eq!(stats.stereotypes.get("service").copied(), Some(1));
}

#[test]
fn destroy_runs_pre_destroy_hooks_in_reverse_order() {
    static LOG: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

    struct A;
    struct B;

    let c = Container::new();
    c.register_factory::<A, _>(Scope::Singleton, |_| Ok(A));
    c.set_destroy_hook::<A, _>(|_| LOG.lock().unwrap().push("a"));
    c.register_factory::<B, _>(Scope::Singleton, |_| Ok(B));
    c.set_destroy_hook::<B, _>(|_| LOG.lock().unwrap().push("b"));

    // Build both singletons so the hooks have instances to run against.
    let _ = c.resolve::<A>().unwrap();
    let _ = c.resolve::<B>().unwrap();

    c.destroy();
    assert_eq!(
        *LOG.lock().unwrap(),
        vec!["b", "a"],
        "reverse registration order"
    );

    // After destroy, singletons rebuild on next resolve.
    static REBUILDS: AtomicUsize = AtomicUsize::new(0);
    struct Counter;
    let c2 = Container::new();
    c2.register_factory::<Counter, _>(Scope::Singleton, |_| {
        REBUILDS.fetch_add(1, Ordering::SeqCst);
        Ok(Counter)
    });
    let _ = c2.resolve::<Counter>().unwrap();
    c2.destroy();
    let _ = c2.resolve::<Counter>().unwrap();
    assert_eq!(
        REBUILDS.load(Ordering::SeqCst),
        2,
        "singleton rebuilt after destroy"
    );
}

#[test]
fn condition_context_round_trips_and_counts_by_name() {
    let c = Container::new();
    c.set_condition_context(
        ConditionContext::new()
            .with_profiles(["prod", "cloud"])
            .with_property("k", "v"),
    );
    let ctx = c.condition_context();
    assert!(ctx.accepts_profiles("prod & cloud"));
    assert_eq!(ctx.property("k"), Some("v"));

    // count_assignable_by_name is type-name based (used by pass-2 conditions).
    c.register_factory::<Repo, _>(Scope::Singleton, |_| Ok(Repo));
    assert_eq!(c.count_assignable_by_name("Repo"), 1);
    assert_eq!(c.count_assignable_by_name("Nope"), 0);
}

#[test]
fn config_properties_prefix_stripping() {
    let c = Container::new();
    c.set_condition_context(
        ConditionContext::new()
            .with_property("app.db.url", "x")
            .with_property("app.db.pool", "5")
            .with_property("other", "y"),
    );
    let map = c.config_properties("app.db");
    assert_eq!(map.get("url").map(String::as_str), Some("x"));
    assert_eq!(map.get("pool").map(String::as_str), Some("5"));
    assert!(!map.contains_key("other"));
}

#[test]
fn provider_for_requires_shared_container() {
    // A container created via `shared()` carries a self-handle, so a factory
    // can build a Provider.
    let c = Container::shared();
    c.register_factory::<Repo, _>(Scope::Transient, |_| Ok(Repo));
    let provider = c.provider_for::<Repo>();
    assert!(provider.get().is_ok());

    // install_shared_handle upgrades a plain Arc-wrapped container too.
    let plain = Arc::new(Container::new());
    plain.install_shared_handle();
    plain.register_factory::<Repo, _>(Scope::Transient, |_| Ok(Repo));
    assert!(plain.provider_for::<Repo>().get().is_ok());
}
