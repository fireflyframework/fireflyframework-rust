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

//! End-to-end DI behavioral tests against the real `firefly` facade, proving
//! the best-in-class dependency-injection surface: stereotype derives,
//! `Container::scan()`, `#[bean]` factories, qualifier/primary/order
//! disambiguation, `Vec`/`Provider`/`Option` injection, `#[post_construct]` /
//! `#[pre_destroy]` ordering, conditional/profile gating, interface
//! auto-binding, `@Value` config injection, and `#[derive(ConfigProperties)]`.
//!
//! NOTE: `inventory`/`scan()` collects EVERY thunk in this test binary, so the
//! whole app is co-located here and the scan-based test asserts on the beans it
//! owns. Disambiguation/lifecycle/value tests use an isolated container and
//! `register_all!` (or direct `firefly_register`) to avoid scan cross-talk.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use firefly::prelude::*;
use serde::Deserialize;

// ===========================================================================
// Stereotypes + Container::scan() + interface auto-binding + conditionals
// ===========================================================================

trait Clock: Send + Sync {
    fn now(&self) -> u64;
}

#[derive(Repository, Default)]
struct ScanRepo;

impl ScanRepo {
    fn name(&self) -> &'static str {
        "scan-repo"
    }
}

#[derive(Service)]
struct ScanService {
    #[autowired]
    repo: Arc<ScanRepo>,
}

impl ScanService {
    fn describe(&self) -> String {
        format!("svc->{}", self.repo.name())
    }
}

// Auto-binds `dyn Clock` to this impl via `provides`.
#[derive(Component, Default)]
#[firefly(provides = "dyn Clock", primary)]
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        42
    }
}

// Gated ON: present-and-not-false property.
#[derive(Service, Default)]
#[firefly(condition_on_property = "feature.audit=on")]
struct AuditService;

// Gated OFF: profile that is not active in the scan test.
#[derive(Service, Default)]
#[firefly(profile = "staging")]
struct StagingOnlyService;

#[test]
fn scan_registers_stereotypes_and_honors_conditions() {
    let ctx = ApplicationContext::builder()
        .profiles(["test"])
        .property("feature.audit", "on")
        .eager(false)
        .build();
    let c = ctx.container();

    // Stereotype beans are discovered and wired.
    let svc = c.resolve::<ScanService>().expect("scan registered service");
    assert_eq!(svc.describe(), "svc->scan-repo");

    // Interface auto-bind: `dyn Clock` resolves to SystemClock.
    let clock = c.resolve::<dyn Clock>().expect("auto-bound interface");
    assert_eq!(clock.now(), 42);

    // condition_on_property=on → registered.
    assert!(c.resolve::<AuditService>().is_ok());

    // profile "staging" not active → NOT registered.
    assert!(
        c.resolve::<StagingOnlyService>().is_err(),
        "staging-only bean must be excluded under the test profile"
    );

    // The bean introspection reports the discovered beans with stereotypes.
    let beans = ctx.beans();
    let repo = beans.iter().find(|b| b.type_name.ends_with("ScanRepo"));
    assert_eq!(repo.unwrap().stereotype.as_deref(), Some("repository"));
    assert!(ctx.bean_count() >= 4);
    let stats = ctx.bean_stats();
    assert!(stats.total >= 4);
    assert!(stats.stereotypes.get("service").copied().unwrap_or(0) >= 1);
}

#[test]
fn condition_on_property_absent_excludes() {
    // Without `feature.audit`, AuditService must be excluded.
    let ctx = ApplicationContext::builder()
        .profiles(["test"])
        .eager(false)
        .build();
    assert!(
        ctx.container().resolve::<AuditService>().is_err(),
        "audit bean must be excluded when feature.audit is absent"
    );
}

// ===========================================================================
// Qualifier / primary / order disambiguation + Vec injection
// ===========================================================================

trait Handler: Send + Sync {
    fn kind(&self) -> &'static str;
}

#[derive(Component, Default)]
struct FirstHandler;
impl Handler for FirstHandler {
    fn kind(&self) -> &'static str {
        "first"
    }
}

#[derive(Component, Default)]
struct SecondHandler;
impl Handler for SecondHandler {
    fn kind(&self) -> &'static str {
        "second"
    }
}

#[test]
fn primary_disambiguates_and_vec_injects_all_ordered() {
    let c = Container::new();
    // Register two impls and bind both to `dyn Handler`, ordering them.
    c.register_factory_with::<FirstHandler, _>(Scope::Singleton, "", false, 20, |_| {
        Ok(FirstHandler)
    });
    c.register_factory_with::<SecondHandler, _>(Scope::Singleton, "", true, 10, |_| {
        Ok(SecondHandler)
    });
    c.bind::<dyn Handler, FirstHandler>(|a| a);
    c.bind::<dyn Handler, SecondHandler>(|a| a);

    // `resolve` picks the primary (SecondHandler).
    let chosen = c.resolve::<dyn Handler>().expect("primary chosen");
    assert_eq!(chosen.kind(), "second");

    // `resolve_all` returns both, ordered by `order` (10 then 20).
    let all = c.resolve_all::<dyn Handler>().expect("resolve all");
    let kinds: Vec<&str> = all.iter().map(|h| h.kind()).collect();
    assert_eq!(kinds, vec!["second", "first"]);
}

// A bean that injects `Vec<Arc<T>>` and an `Option<Arc<T>>` (required=false).
#[derive(Component, Default)]
struct WidgetA;
#[derive(Component, Default)]
struct WidgetB;

#[derive(Service)]
struct Aggregator {
    #[autowired]
    widgets: Vec<Arc<WidgetA>>,
    #[autowired]
    maybe: Option<Arc<WidgetB>>,
    #[autowired]
    missing: Option<Arc<MissingDep>>,
}

struct MissingDep;

impl Aggregator {
    fn counts(&self) -> (usize, bool, bool) {
        (
            self.widgets.len(),
            self.maybe.is_some(),
            self.missing.is_some(),
        )
    }
}

#[test]
fn vec_and_optional_injection() {
    let c = Container::new();
    WidgetA::firefly_register(&c);
    WidgetB::firefly_register(&c);
    Aggregator::firefly_register(&c);

    let agg = c.resolve::<Aggregator>().expect("aggregator resolves");
    // one WidgetA, Some(WidgetB), None for the unregistered MissingDep.
    assert_eq!(agg.counts(), (1, true, false));
}

// ===========================================================================
// Provider<T> injection (deferred resolution)
// ===========================================================================

#[derive(Component, Default)]
struct Ticket {
    seq: usize,
}

static TICKET_SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Service)]
struct TicketDesk {
    #[autowired]
    tickets: Provider<Ticket>,
}

impl TicketDesk {
    fn issue(&self) -> usize {
        self.tickets.get().unwrap().seq
    }
}

#[test]
fn provider_injection_defers_resolution() {
    let c = Container::shared();
    // A transient Ticket whose seq increments each construction.
    c.register_factory::<Ticket, _>(Scope::Transient, |_| {
        Ok(Ticket {
            seq: TICKET_SEQ.fetch_add(1, Ordering::SeqCst),
        })
    });
    TicketDesk::firefly_register(&c);

    let desk = c.resolve::<TicketDesk>().expect("desk resolves");
    let a = desk.issue();
    let b = desk.issue();
    assert_ne!(a, b, "Provider yields a fresh transient on each get()");
}

// ===========================================================================
// #[bean] factory methods on a #[derive(Configuration)] holder
// ===========================================================================

#[derive(Configuration, Default)]
struct AppConfig;

struct Greeting(String);
struct Stamp(u64);

#[firefly::bean]
impl AppConfig {
    #[bean(name = "greeting", primary)]
    fn greeting(&self) -> Greeting {
        Greeting("hello".into())
    }

    // Depends on another bean (the auto-bound clock from the scan test is not
    // in this isolated container; depend on a locally-registered Stamp source).
    #[bean]
    fn stamp(&self, base: Arc<StampBase>) -> Stamp {
        Stamp(base.value + 1)
    }

    // Profile-gated bean: excluded unless "prod" is active.
    #[bean(profile = "prod")]
    fn prod_only(&self) -> ProdBean {
        ProdBean
    }
}

struct ProdBean;

#[derive(Component, Default)]
struct StampBase {
    value: u64,
}

#[test]
fn bean_factories_register_by_return_type_with_deps() {
    let c = Container::new();
    AppConfig::firefly_register(&c);
    StampBase::firefly_register(&c);
    AppConfig::firefly_register_beans(&c);

    let g = c.resolve::<Greeting>().expect("greeting bean");
    assert_eq!(g.0, "hello");
    // The named bean is also reachable by name.
    let g2 = c
        .resolve_named::<Greeting>("greeting")
        .expect("named greeting");
    assert_eq!(g2.0, "hello");

    // The dependent @bean resolved its Arc<StampBase> argument.
    let s = c.resolve::<Stamp>().expect("stamp bean");
    assert_eq!(s.0, 1);

    // The profile-gated @bean is excluded (no active "prod" profile).
    assert!(
        c.resolve::<ProdBean>().is_err(),
        "#[bean(profile = \"prod\")] must be excluded without the prod profile"
    );

    // With the prod profile active, it registers.
    let c2 = Container::new();
    c2.set_condition_context(ConditionContext::new().with_profiles(["prod"]));
    AppConfig::firefly_register(&c2);
    StampBase::firefly_register(&c2);
    AppConfig::firefly_register_beans(&c2);
    assert!(c2.resolve::<ProdBean>().is_ok());
}

// ===========================================================================
// #[post_construct] / #[pre_destroy] lifecycle ordering
// ===========================================================================

static LIFECYCLE_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Service, Default)]
#[firefly(post_construct = "started", pre_destroy = "stopped")]
struct Alpha {
    ready: AtomicBool,
}

impl Alpha {
    fn started(&mut self) {
        self.ready.store(true, Ordering::SeqCst);
        LIFECYCLE_LOG.lock().unwrap().push("alpha:post".into());
    }
    fn stopped(&self) {
        LIFECYCLE_LOG.lock().unwrap().push("alpha:pre".into());
    }
}

#[derive(Service, Default)]
#[firefly(post_construct = "started", pre_destroy = "stopped")]
struct Beta;

impl Beta {
    fn started(&mut self) {
        LIFECYCLE_LOG.lock().unwrap().push("beta:post".into());
    }
    fn stopped(&self) {
        LIFECYCLE_LOG.lock().unwrap().push("beta:pre".into());
    }
}

#[test]
fn lifecycle_post_construct_runs_and_pre_destroy_reverses() {
    LIFECYCLE_LOG.lock().unwrap().clear();
    let c = Container::new();
    Alpha::firefly_register(&c);
    Beta::firefly_register(&c);

    // Eagerly resolve in registration order → post_construct fires per bean.
    let alpha = c.resolve::<Alpha>().expect("alpha");
    assert!(alpha.ready.load(Ordering::SeqCst), "post_construct ran");
    let _beta = c.resolve::<Beta>().expect("beta");

    // pre_destroy runs in REVERSE construction order on destroy().
    c.destroy();
    let log = LIFECYCLE_LOG.lock().unwrap().clone();
    assert_eq!(
        log,
        vec!["alpha:post", "beta:post", "beta:pre", "alpha:pre"],
        "post_construct in order, pre_destroy reversed"
    );
}

// ===========================================================================
// @Value config-field injection
// ===========================================================================

#[derive(Service, Default)]
struct TunedService {
    #[firefly(value = "${svc.batch:25}")]
    batch: usize,
    #[firefly(value = "${svc.name}")]
    name: String,
}

#[test]
fn value_injection_from_config_with_default() {
    let c = Container::new();
    c.set_condition_context(ConditionContext::new().with_property("svc.name", "tuned"));
    TunedService::firefly_register(&c);

    let svc = c.resolve::<TunedService>().expect("tuned service");
    assert_eq!(svc.batch, 25, "default used when key absent");
    assert_eq!(svc.name, "tuned", "present key injected");
}

// ===========================================================================
// #[derive(ConfigProperties)] — config-bound injectable bean
// ===========================================================================

#[derive(Deserialize, ConfigProperties, Default)]
#[firefly(prefix = "app.db")]
struct DbProperties {
    url: String,
    #[serde(default)]
    pool_size: u32,
}

#[derive(Service)]
struct DbClient {
    #[autowired]
    props: Arc<DbProperties>,
}

impl DbClient {
    fn url(&self) -> &str {
        &self.props.url
    }
}

#[test]
fn config_properties_binds_and_injects() {
    let c = Container::new();
    c.set_condition_context(
        ConditionContext::new()
            .with_property("app.db.url", "postgres://x")
            .with_property("app.db.pool_size", "8"),
    );
    DbProperties::firefly_register(&c);
    DbClient::firefly_register(&c);

    let client = c.resolve::<DbClient>().expect("db client");
    assert_eq!(client.url(), "postgres://x");
    assert_eq!(client.props.pool_size, 8);
}

// ===========================================================================
// register_all! still works for the explicit-list fallback
// ===========================================================================

#[derive(Repository, Default)]
struct LegacyRepo;

#[derive(Service)]
struct LegacyService {
    #[autowired]
    repo: Arc<LegacyRepo>,
}

impl LegacyService {
    fn has_repo(&self) -> bool {
        Arc::strong_count(&self.repo) >= 1
    }
}

#[test]
fn register_all_explicit_list_still_works() {
    let c = Container::new();
    firefly::register_all!(&c, [LegacyRepo, LegacyService]);
    let svc = c.resolve::<LegacyService>().expect("legacy service");
    assert!(svc.has_repo());
    assert!(c.resolve::<LegacyRepo>().is_ok());
}

// ===========================================================================
// Every Spring/pyfly stereotype registers a USER-defined bean with its label
// ===========================================================================

#[derive(Component, Default)]
struct StereoComponent;

#[derive(Service, Default)]
struct StereoService;

#[derive(Repository, Default)]
struct StereoRepository;

#[derive(Clone, Controller, Default)]
struct StereoController;

#[derive(Configuration, Default)]
struct StereoConfig;
struct ConfigWidget;

#[firefly::bean]
impl StereoConfig {
    #[bean]
    fn config_widget(&self) -> ConfigWidget {
        ConfigWidget
    }
}

#[derive(AutoConfiguration, Default)]
struct StereoAutoConfig;
struct AutoWidget;

#[firefly::bean]
impl StereoAutoConfig {
    #[bean]
    fn auto_widget(&self) -> AutoWidget {
        AutoWidget
    }
}

/// One user-defined bean for every stereotype the framework supports —
/// `@Component` / `@Service` / `@Repository` / `@Controller` / `@Configuration`
/// (+ `@Bean`) / `@AutoConfiguration` — registers, resolves, and is reported
/// under its Spring/pyfly stereotype label, proving the DI container manages
/// **all** kinds of user beans like Spring Boot.
#[test]
fn every_stereotype_registers_a_user_bean_with_its_label() {
    let c = Container::new();
    StereoComponent::firefly_register(&c);
    StereoService::firefly_register(&c);
    StereoRepository::firefly_register(&c);
    StereoController::firefly_register(&c);
    StereoConfig::firefly_register(&c);
    StereoConfig::firefly_register_beans(&c);
    StereoAutoConfig::firefly_register(&c);
    StereoAutoConfig::firefly_register_beans(&c);

    // Every user-defined bean resolves — including the `@Bean` factory products.
    assert!(c.resolve::<StereoComponent>().is_ok());
    assert!(c.resolve::<StereoService>().is_ok());
    assert!(c.resolve::<StereoRepository>().is_ok());
    assert!(c.resolve::<StereoController>().is_ok());
    assert!(
        c.resolve::<ConfigWidget>().is_ok(),
        "@Bean product resolves"
    );
    assert!(
        c.resolve::<AutoWidget>().is_ok(),
        "@AutoConfiguration @Bean product resolves"
    );

    // ...and each is reported under its stereotype label in the bean registry
    // (the labels the admin dashboard groups by).
    let beans = c.beans();
    let label = |suffix: &str| -> Option<String> {
        beans
            .iter()
            .find(|b| b.type_name.ends_with(suffix))
            .unwrap_or_else(|| panic!("bean `{suffix}` not registered"))
            .stereotype
            .clone()
    };
    assert_eq!(label("StereoComponent").as_deref(), Some("component"));
    assert_eq!(label("StereoService").as_deref(), Some("service"));
    assert_eq!(label("StereoRepository").as_deref(), Some("repository"));
    assert_eq!(label("StereoController").as_deref(), Some("controller"));
    assert_eq!(label("StereoConfig").as_deref(), Some("configuration"));
    assert_eq!(
        label("StereoAutoConfig").as_deref(),
        Some("autoconfiguration")
    );
}

// ===========================================================================
// #[handlers] bean — a #[scheduled] task method autowires its collaborators
// (Spring's `@Scheduled` on a `@Component`) and is drained from the container.
// ===========================================================================

#[derive(Component, Default)]
struct TickCounter {
    ticks: AtomicUsize,
}

#[derive(Service)]
struct HeartbeatBean {
    #[autowired]
    counter: Arc<TickCounter>,
}

#[handlers]
impl HeartbeatBean {
    #[scheduled(fixed_rate = "60s")]
    async fn beat(&self) -> Result<(), std::io::Error> {
        self.counter.ticks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn handlers_bean_scheduled_task_registers_from_the_container() {
    let c = Container::new();
    TickCounter::firefly_register(&c);
    HeartbeatBean::firefly_register(&c);

    // The bean `#[scheduled]` task is drained from the container onto a scheduler
    // — the bean is resolved (autowiring `TickCounter`) and its method scheduled.
    let scheduler = Scheduler::new();
    let n = firefly::scheduling::register_discovered_scheduled_beans(&scheduler, &c);
    assert!(n >= 1, "the bean #[scheduled] task was drained");
    let names: Vec<String> = scheduler.tasks().into_iter().map(|t| t.name).collect();
    assert!(
        names.contains(&"beat".to_string()),
        "the autowired #[scheduled] task registered, got {names:?}"
    );
}
