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

//! # firefly-starter-experience
//!
//! The **experience (BFF) tier starter**: a [`firefly_starter_web::WebStack`]
//! with the building blocks a Backend-for-Frontend service needs to compose
//! several **domain** SDKs into journey-specific, atomic REST endpoints —
//! the Rust spelling of the Firefly `generate-execution-plan-experience`
//! contract (Java: `firefly-starter-application` driving signal-driven
//! `@Workflow`s; pyfly: the `transactional/workflow` + `client` building
//! blocks composed into a BFF tier).
//!
//! ## The three service tiers
//!
//! Firefly services are layered into **three tiers** (distinct from the
//! crate-graph tiers documented in `docs/ARCHITECTURE.md`):
//!
//! | Service tier | Owns | Talks to | Rust starter |
//! |--------------|------|----------|--------------|
//! | **core** | the database (R2DBC/sqlx, migrations, CRUD) | nothing below | [`firefly_starter_core`] / `firefly-starter-data` |
//! | **domain** | sagas, CQRS, event sourcing, third-party adapters | **core** SDKs | `firefly-starter-domain` |
//! | **experience (BFF)** | signal-driven workflows, stateless aggregation, atomic REST | **domain** SDKs only | **this crate** |
//!
//! The dependency direction is strict — `channel → experience → domain →
//! core`. An experience service **never** owns a database, **never** calls
//! a core service directly, and **never** calls a sibling experience
//! service. It aggregates domain SDKs (over [`firefly_client`]) into APIs
//! shaped for one frontend / channel.
//!
//! ## What [`ExperienceStack`] wires
//!
//! [`ExperienceStack::new`] builds a full [`WebStack`] (so it inherits the
//! web batteries — CORS, security headers, request metrics, access-log,
//! correlation, idempotency, the actuator surface) and adds the
//! experience-tier building blocks:
//!
//! | Field | Type | Role |
//! |-------|------|------|
//! | [`clients`](ExperienceStack::clients) | [`DomainClients`] | the **domain-SDK composition** surface — a registry of named [`RestClient`]s (the `ClientFactory` equivalent) for calling downstream domain services |
//! | [`signals`](ExperienceStack::signals) | `Arc<`[`SignalService`]`>` | `@WaitForSignal`-style gates — a workflow step parks until an external caller delivers a named signal |
//! | [`state`](ExperienceStack::state) | [`WorkflowState`] | **Redis-capable** persisted workflow state, keyed by correlation id, over the [`firefly_cache`] `Arc<dyn Adapter>` abstraction (swap in `firefly-cache-redis`'s `RedisAdapter` for cross-restart durability) |
//! | [`query`](ExperienceStack::query) | `Arc<`[`WorkflowQueryService`]`>` | the journey-status query surface — derive a phase / next-step DTO from the live step statuses (the main recovery mechanism) |
//! | [`children`](ExperienceStack::children) | `Arc<`[`ChildWorkflowService`]`>` | child-workflow composition for nested journeys |
//!
//! [`ExperienceStack`] [`Deref`]s to its [`WebStack`] (which itself derefs
//! to [`Core`]), so every web + core field and method
//! (`apply_middleware`, `actuator_router`, `new_application`,
//! `with_security`, `cache`, `bus`, `scheduler`, …) is reachable directly
//! on the experience value. `starter_name` defaults to
//! `"starter-experience"`. `Bff` is a type alias for the same struct.
//!
//! ## Atomic REST + signal-driven workflows
//!
//! A BFF journey is a [`Workflow`] whose steps call domain SDKs and whose
//! gates park on signals. The controller exposes **atomic** endpoints:
//!
//! * `POST /journeys` — start the workflow; persist its [`WorkflowState`].
//! * `POST /journeys/{id}/data` — deliver a signal that **advances** the
//!   parked workflow ([`SignalService::deliver`]).
//! * `GET /journeys/{id}` — query the journey status
//!   ([`WorkflowQueryService`]).
//!
//! Each phase is one request; state lives in Redis (or any
//! [`Adapter`](firefly_cache::Adapter)), so a client can resume a journey
//! after a disconnect.
//!
//! ## pyfly-parity APIs
//!
//! [`register_experience_stack`] / [`enable_experience_stack`] mirror
//! pyfly's `register_*_stack` / `@enable_*_stack` bootstrap pair (and
//! .NET's `services.AddFireflyExperience`), so a migrating service reaches
//! the tier by the spelling it already knows.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use firefly_starter_experience::{CoreConfig, ExperienceStack, Node, Workflow};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Batteries-included BFF from one dependency.
//!     let bff = ExperienceStack::new(CoreConfig {
//!         app_name: "exp-onboarding".into(),
//!         app_version: "1.0.0".into(),
//!         ..CoreConfig::default()
//!     });
//!
//!     // Register the downstream domain SDKs (Experience → Domain only).
//!     bff.clients
//!         .register("orders", "https://domain-orders.internal");
//!     bff.clients
//!         .register("fulfillment", "https://domain-fulfillment.internal");
//!
//!     // A signal-driven journey: reserve → wait for payment → ship.
//!     let signals = Arc::clone(&bff.signals);
//!     let journey_id = "j-1".to_string();
//!     let workflow = Workflow::new("checkout")
//!         .node(Node::new("reserve", || async { Ok(()) }))
//!         .node(
//!             Node::wait_for_signal("await-payment", &signals, journey_id.clone(), "paid")
//!                 .depends_on(["reserve"]),
//!         )
//!         .node(Node::new("ship", || async { Ok(()) }).depends_on(["await-payment"]));
//!     let _ = workflow; // run it on a task; deliver `paid` from POST /journeys/{id}/data
//!
//!     bff.init_logging()?;
//!     bff.print_banner();
//!     Ok(())
//! }
//! ```

#![warn(missing_docs)]

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use firefly_cache::{Adapter, CacheError, Typed};
use firefly_client::{RestBuilder, RestClient};
use firefly_orchestration::{
    ChildWorkflowService, SignalService, StepContext, WorkflowQueryService,
};

// Re-export the orchestration building blocks a BFF journey is built from,
// so a workflow file needs only a single `use firefly_starter_experience::*`.
pub use firefly_orchestration::{
    CompensationPolicy, ExecutionState, ExecutionStatus, Node, SignalError, Workflow,
    WorkflowError, WorkflowQueryError,
};
// Re-export the domain-SDK client surface.
pub use firefly_client::{
    ClientError, RestBuilder as DomainClientBuilder, RestClient as DomainClient,
};
// Re-export the web + core surface so a controller file imports from here.
pub use firefly_starter_web::{
    Authentication, BearerConfig, BearerLayer, Core, CoreConfig, CorsConfig, FilterChain, WebStack,
};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = firefly_starter_web::VERSION;

/// Cache key prefix under which [`WorkflowState`] persists each run's
/// [`StepContext`] snapshot. Namespaced so a shared Redis instance can host
/// many services without collision.
const STATE_KEY_PREFIX: &str = "firefly:experience:workflow:";

// ─────────────────────────────────────────────────────────────────────────
// DomainClients — the ClientFactory equivalent (domain-SDK composition).
// ─────────────────────────────────────────────────────────────────────────

/// A registry of named domain-service [`RestClient`]s — the Rust spelling
/// of the Firefly `ClientFactory` an experience service uses to reach its
/// downstream **domain** SDKs.
///
/// An experience service composes several domain SDKs into one journey API.
/// [`DomainClients`] holds one [`RestClient`] per domain dependency, keyed
/// by a logical name (`"orders"`, `"fulfillment"`, …), so a workflow step or
/// composition handler resolves the right client by name rather than
/// threading a builder through every call site.
///
/// Per the experience-tier invariants, every registered client points at a
/// **domain** service; an experience service never registers a core-tier or
/// sibling-experience SDK here.
///
/// ```
/// use firefly_starter_experience::DomainClients;
///
/// let clients = DomainClients::new();
/// clients.register("orders", "https://domain-orders.internal");
/// assert!(clients.get("orders").is_some());
/// assert_eq!(clients.names(), vec!["orders".to_string()]);
/// ```
#[derive(Clone, Default)]
pub struct DomainClients {
    inner: Arc<Mutex<HashMap<String, Arc<RestClient>>>>,
}

impl std::fmt::Debug for DomainClients {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainClients")
            .field("names", &self.names())
            .finish()
    }
}

impl DomainClients {
    /// Returns an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<RestClient>>> {
        self.inner
            .lock()
            .expect("firefly/experience: client registry lock poisoned")
    }

    /// Registers a domain SDK under `name`, building a default
    /// [`RestClient`] for `base_url` (correlation-id propagation, JSON
    /// codec, RFC 7807 error decoding, retry/backoff — all inherited from
    /// [`firefly_client`]). Replaces any client previously registered under
    /// the same name. Returns the built client for immediate use.
    ///
    /// Use [`Self::register_client`] to register a pre-tuned client (custom
    /// timeout, headers, retry policy).
    pub fn register(&self, name: impl Into<String>, base_url: impl AsRef<str>) -> Arc<RestClient> {
        let client = Arc::new(RestBuilder::new(base_url).build());
        self.locked().insert(name.into(), Arc::clone(&client));
        client
    }

    /// Registers a pre-built [`RestClient`] under `name` — use when the
    /// domain SDK needs a custom timeout, default headers, or retry policy.
    /// Replaces any client previously registered under the same name.
    pub fn register_client(&self, name: impl Into<String>, client: RestClient) {
        self.locked().insert(name.into(), Arc::new(client));
    }

    /// Resolves the domain client registered under `name`, or `None` when no
    /// such client is registered.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<RestClient>> {
        self.locked().get(name).map(Arc::clone)
    }

    /// The logical names of every registered domain client, sorted for
    /// deterministic output.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.locked().keys().cloned().collect();
        names.sort();
        names
    }

    /// The number of registered domain clients.
    #[must_use]
    pub fn len(&self) -> usize {
        self.locked().len()
    }

    /// Whether no domain client is registered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.locked().is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// WorkflowState — Redis-capable persisted workflow state.
// ─────────────────────────────────────────────────────────────────────────

/// Redis-capable persisted workflow state — a thin store over the
/// [`firefly_cache`] [`Adapter`] abstraction that round-trips a workflow
/// run's [`StepContext`] snapshot keyed by correlation id.
///
/// This is the experience-tier analog of
/// [`DurableWorkflowState`](firefly_orchestration::DurableWorkflowState),
/// but persisted through the **cache** [`Adapter`] (the in-memory default,
/// or `firefly-cache-redis`'s `RedisAdapter` for cross-restart durability),
/// matching the Firefly experience-tier convention of holding workflow state
/// in Redis. A parked journey [`save`](Self::save)s its context before
/// suspending; a later request [`load`](Self::load)s it back to advance the
/// workflow.
///
/// State is JSON-encoded via [`StepContext::to_snapshot`] /
/// [`StepContext::from_snapshot`], so any [`Adapter`] backend can store it.
#[derive(Clone)]
pub struct WorkflowState {
    /// The raw cache backend — held so [`delete`](Self::delete) can issue an
    /// eviction (the [`Typed`] view only exposes `get`/`set`).
    adapter: Arc<dyn Adapter>,
    /// A JSON-typed view over the same `adapter` for the snapshot round-trip.
    snapshots: Typed<serde_json::Value>,
}

impl std::fmt::Debug for WorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowState")
            .field("backend", &self.adapter.name())
            .finish_non_exhaustive()
    }
}

impl WorkflowState {
    /// Wraps a cache [`Adapter`] for workflow-state persistence. Pass the
    /// stack's [`Core::cache`] (in-memory by default) or a `RedisAdapter`
    /// for durability.
    #[must_use]
    pub fn new(adapter: Arc<dyn Adapter>) -> Self {
        Self {
            snapshots: Typed::new(Arc::clone(&adapter)),
            adapter,
        }
    }

    fn key(correlation_id: &str) -> String {
        format!("{STATE_KEY_PREFIX}{correlation_id}")
    }

    /// Persists `ctx` under its
    /// [`correlation_id`](StepContext::correlation_id), so a parked journey
    /// can be resumed from a later request — the experience-tier "save
    /// workflow state to Redis" step.
    pub async fn save(&self, ctx: &StepContext) -> Result<(), CacheError> {
        let key = Self::key(&ctx.correlation_id());
        self.snapshots.set(&key, &ctx.to_snapshot(), None).await
    }

    /// Loads the [`StepContext`] persisted under `correlation_id`, or
    /// `Ok(None)` when no state is stored for that journey — the
    /// experience-tier "rehydrate workflow state from Redis" step.
    pub async fn load(&self, correlation_id: &str) -> Result<Option<StepContext>, CacheError> {
        let key = Self::key(correlation_id);
        match self.snapshots.get(&key).await {
            Ok(snapshot) => Ok(Some(StepContext::from_snapshot(&snapshot))),
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Discards the state persisted for `correlation_id` — call when a
    /// journey completes (or is cancelled) so finished runs don't linger.
    pub async fn delete(&self, correlation_id: &str) -> Result<(), CacheError> {
        let key = Self::key(correlation_id);
        self.adapter.delete(&key).await
    }
}

// ─────────────────────────────────────────────────────────────────────────
// ExperienceStack — the experience (BFF) tier starter.
// ─────────────────────────────────────────────────────────────────────────

/// The experience (BFF) tier starter — a [`WebStack`] plus the domain-SDK
/// composition + signal-driven workflow building blocks an experience
/// service needs.
///
/// Build it with [`ExperienceStack::new`]. The struct [`Deref`]s to its
/// embedded [`WebStack`] (which derefs to [`Core`]), so the whole web + core
/// surface is reachable directly on the experience value.
///
/// See the [crate docs](crate) for the three-tier service model and the role
/// of each field. [`Bff`] is a type alias for this struct.
pub struct ExperienceStack {
    /// The wired web tier (batteries on + the inherited core). The
    /// [`ExperienceStack`] also [`Deref`]s to this field, mirroring Go's
    /// embedded `*starterweb.WebStack`.
    pub web: WebStack,
    /// The domain-SDK composition surface — register one [`RestClient`] per
    /// downstream **domain** service and resolve it by name in a workflow
    /// step or composition handler.
    pub clients: DomainClients,
    /// The signal router for `@WaitForSignal`-style gates: a workflow step
    /// parks on [`SignalService::wait_for`] / [`Node::wait_for_signal`]
    /// until an atomic endpoint delivers the named signal.
    pub signals: Arc<SignalService>,
    /// Redis-capable persisted workflow state, keyed by correlation id, over
    /// the stack's cache [`Adapter`]. Defaults to the in-memory
    /// [`Core::cache`]; point it at a `RedisAdapter` for durability.
    pub state: WorkflowState,
    /// The journey-status query surface — register a run's [`StepContext`]
    /// and derive a phase / next-step DTO from its live step statuses (the
    /// main recovery mechanism).
    pub query: Arc<WorkflowQueryService>,
    /// Child-workflow composition for nested journeys (pyfly's
    /// `ChildWorkflowService`).
    pub children: Arc<ChildWorkflowService>,
}

/// Idiomatic alias for [`ExperienceStack`] — the Backend-for-Frontend tier.
pub type Bff = ExperienceStack;

impl ExperienceStack {
    /// Wires the experience starter — a [`WebStack`] (HTTP batteries on)
    /// plus the BFF building blocks (domain-SDK registry, signal service,
    /// Redis-capable workflow state over the core cache, query service,
    /// child-workflow service).
    ///
    /// Delegates to [`WebStack::new`] for the web + core tiers, then — like
    /// the sibling starters — renames a `starter_name` that resolved to the
    /// `"starter-core"` / `"starter-web"` default to `"starter-experience"`.
    /// A custom starter name passed in `cfg` is preserved untouched.
    ///
    /// The [`WorkflowState`] is wired over the same cache [`Adapter`] the
    /// [`Core`] holds (in-memory by default). To make journey state survive a
    /// restart, pass a `RedisAdapter` as `cfg.cache` (or rebuild
    /// [`state`](Self::state) with [`WorkflowState::new`] afterwards).
    #[must_use]
    pub fn new(cfg: CoreConfig) -> Self {
        let mut web = WebStack::new(cfg);
        if web.starter_name == "starter-web" {
            web.core.starter_name = "starter-experience".to_string();
        }
        let state = WorkflowState::new(Arc::clone(&web.core.cache));
        ExperienceStack {
            web,
            clients: DomainClients::new(),
            signals: Arc::new(SignalService::new()),
            state,
            query: Arc::new(WorkflowQueryService::new()),
            children: Arc::new(ChildWorkflowService::new()),
        }
    }

    /// Attaches a security [`FilterChain`] to the inherited web tier (the
    /// builder spelling of [`WebStack::with_security`]). Returns `self` so it
    /// chains after [`Self::new`].
    #[must_use]
    pub fn with_security(mut self, chain: FilterChain) -> Self {
        self.web = self.web.with_security(chain);
        self
    }
}

impl Deref for ExperienceStack {
    type Target = WebStack;

    fn deref(&self) -> &WebStack {
        &self.web
    }
}

impl DerefMut for ExperienceStack {
    fn deref_mut(&mut self) -> &mut WebStack {
        &mut self.web
    }
}

// ─────────────────────────────────────────────────────────────────────────
// register_experience_stack / enable_experience_stack — pyfly-parity APIs.
// ─────────────────────────────────────────────────────────────────────────

/// Imperative experience-tier bootstrap — the Rust spelling of pyfly's
/// `register_experience_stack(app)` (and .NET's
/// `services.AddFireflyExperience(...)`).
///
/// Builds an [`ExperienceStack`] from `cfg` with every experience-tier
/// battery wired, ready for a controller file to register routes and an
/// [`Application`](firefly_starter_core::Core::new_application) to serve
/// them. Equivalent to [`ExperienceStack::new`]; provided under the
/// pyfly-parity `register_*_stack` name so a migrating service reaches it by
/// the spelling it already knows.
#[must_use]
pub fn register_experience_stack(cfg: CoreConfig) -> ExperienceStack {
    ExperienceStack::new(cfg)
}

/// Declarative experience-tier configuration — the Rust spelling of pyfly's
/// `@enable_experience_stack` decorator.
///
/// Where pyfly's decorator merges the tier's `pyfly.*.enabled` property dict
/// into the live config before auto-configuration runs, the typed-Rust idiom
/// applies the experience tier's defaults to a [`CoreConfig`] and hands it
/// back. The experience tier inherits the **web** tier's batteries (CORS,
/// security headers, request metrics, access-log — all on by default via
/// [`WebStack::new`]), so this fills in the same web-tier defaults
/// [`WebStack::new`] would, and stamps `starter_name` to
/// `"starter-experience"` when it was left at a default — so the banner and
/// actuator `info` surface report the experience tier even before the stack
/// is built.
///
/// Pass the result to [`register_experience_stack`] / [`ExperienceStack::new`].
///
/// ```
/// use firefly_starter_experience::{enable_experience_stack, CoreConfig};
///
/// let cfg = enable_experience_stack(CoreConfig {
///     app_name: "exp-dashboard".into(),
///     ..CoreConfig::default()
/// });
/// assert_eq!(cfg.starter_name, "starter-experience");
/// assert!(cfg.cors.is_some()); // web batteries inherited
/// ```
#[must_use]
pub fn enable_experience_stack(mut cfg: CoreConfig) -> CoreConfig {
    // Inherit the web tier's batteries (only fill gaps so an explicit
    // override survives), mirroring WebStack::new's get_or_insert_with.
    cfg.cors
        .get_or_insert_with(firefly_starter_web::CorsConfig::permit_defaults);
    cfg.security_headers
        .get_or_insert_with(firefly_starter_core::SecurityHeadersConfig::default);
    cfg.request_metrics
        .get_or_insert_with(firefly_starter_core::RequestMetricsConfig::default);
    cfg.request_log
        .get_or_insert_with(firefly_starter_core::RequestLogLayer::new);
    if cfg.starter_name.is_empty()
        || cfg.starter_name == "starter-core"
        || cfg.starter_name == "starter-web"
    {
        cfg.starter_name = "starter-experience".to_string();
    }
    cfg
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::{Path, State};
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use firefly_kernel::HEADER_CORRELATION_ID;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    use super::*;

    fn bff_for(app_name: &str) -> ExperienceStack {
        ExperienceStack::new(CoreConfig {
            app_name: app_name.into(),
            ..CoreConfig::default()
        })
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    // ---- wiring / naming parity with the sibling starters -----------------

    #[test]
    fn experience_stack_wires_the_bff_building_blocks() {
        let bff = bff_for("exp-onboarding");
        assert_eq!(bff.app_name, "exp-onboarding");
        assert_eq!(bff.starter_name, "starter-experience");
        // Inherits the web batteries (cache wired live, security off).
        assert_eq!(bff.cache.name(), "memory");
        assert!(bff.web.security.is_none());
        // The BFF building blocks are wired and empty.
        assert!(bff.clients.is_empty());
        assert!(bff.signals.list_active().is_empty());
        assert!(bff.query.active().is_empty());
    }

    #[test]
    fn defaults_fall_back_to_canonical_names() {
        let bff = ExperienceStack::new(CoreConfig::default());
        assert_eq!(bff.app_name, "firefly-app");
        assert_eq!(bff.starter_name, "starter-experience");
        assert_eq!(bff.log.service, "firefly-app");
    }

    /// A custom starter name passes through; an explicit `"starter-core"` or
    /// `"starter-web"` (indistinguishable from the default after the inner
    /// constructors) is renamed — exactly like the sibling starters.
    #[test]
    fn starter_name_rules_match_siblings() {
        let custom = ExperienceStack::new(CoreConfig {
            starter_name: "starter-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(custom.starter_name, "starter-custom");

        for seed in ["starter-core", "starter-web"] {
            let renamed = ExperienceStack::new(CoreConfig {
                starter_name: seed.into(),
                ..CoreConfig::default()
            });
            assert_eq!(renamed.starter_name, "starter-experience");
        }
    }

    #[test]
    fn register_and_enable_helpers_mirror_pyfly() {
        // register_experience_stack == ExperienceStack::new.
        let bff = register_experience_stack(CoreConfig {
            app_name: "exp-x".into(),
            ..CoreConfig::default()
        });
        assert_eq!(bff.starter_name, "starter-experience");

        // enable_experience_stack stamps the tier + inherits web batteries.
        let cfg = enable_experience_stack(CoreConfig::default());
        assert_eq!(cfg.starter_name, "starter-experience");
        assert!(cfg.cors.is_some());
        assert!(cfg.security_headers.is_some());
        assert!(cfg.request_metrics.is_some());

        // An explicit non-default starter name survives enable_*.
        let cfg = enable_experience_stack(CoreConfig {
            starter_name: "exp-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(cfg.starter_name, "exp-custom");
    }

    // ---- DomainClients (ClientFactory equivalent) -------------------------

    #[test]
    fn domain_clients_register_resolve_and_replace() {
        let bff = bff_for("exp-onboarding");
        bff.clients
            .register("orders", "https://domain-orders.internal");
        bff.clients
            .register("fulfillment", "https://domain-fulfillment.internal");
        assert_eq!(bff.clients.len(), 2);
        assert_eq!(
            bff.clients.names(),
            vec!["fulfillment".to_string(), "orders".to_string()]
        );
        assert!(bff.clients.get("orders").is_some());
        assert!(bff.clients.get("unknown").is_none());

        // Re-registering the same name replaces (last wins, no growth).
        bff.clients
            .register("orders", "https://domain-orders-v2.internal");
        assert_eq!(bff.clients.len(), 2);
    }

    // ---- WorkflowState (Redis-capable persisted state) --------------------

    #[tokio::test]
    async fn workflow_state_round_trips_through_the_cache() {
        let bff = bff_for("exp-onboarding");
        // A miss on an unknown journey is Ok(None), not an error.
        assert!(bff.state.load("ghost").await.unwrap().is_none());

        let ctx = StepContext::new();
        ctx.set_correlation_id("j-42");
        ctx.set_variable("entityId", json!("E-7"));
        ctx.set_result("reserve", json!({ "ok": true }));
        bff.state.save(&ctx).await.unwrap();

        let restored = bff.state.load("j-42").await.unwrap().expect("persisted");
        assert_eq!(restored.correlation_id(), "j-42");
        assert_eq!(restored.variable("entityId").unwrap(), json!("E-7"));
        assert_eq!(restored.result("reserve").unwrap(), json!({ "ok": true }));

        // Completing the journey clears the state.
        bff.state.delete("j-42").await.unwrap();
        assert!(bff.state.load("j-42").await.unwrap().is_none());
    }

    // ---- Deref promotion --------------------------------------------------

    #[test]
    fn deref_promotes_web_and_core_surface() {
        let bff = bff_for("exp-dashboard");
        let banner = bff.banner();
        assert!(banner.contains("exp-dashboard"));
        assert!(banner.contains("starter-experience"));
        assert_eq!(bff.new_application().name(), "exp-dashboard");

        // DerefMut reaches the inner core fields.
        let mut bff = bff;
        bff.app_version = "2.0.0".into();
        assert_eq!(bff.web.core.app_version, "2.0.0");
    }

    // ---- the headline boot test (W2 plan) ---------------------------------

    // Two mock domain SDKs, composed through a signal-gated workflow, driven
    // via tower::oneshot. This is the experience-tier BFF contract:
    //   POST /checkout          -> start the workflow (calls domain-orders),
    //                              park on the `paid` gate, persist state.
    //   POST /checkout/{id}/pay -> deliver the `paid` signal (advances the
    //                              workflow: calls domain-fulfillment, ships).
    //   GET  /checkout/{id}     -> query the journey status.

    #[derive(Clone)]
    struct AppState {
        bff: Arc<ExperienceStack>,
        // Mock domain SDK call recorders (stand in for two domain services).
        orders_reserved: Arc<AtomicU32>,
        fulfillment_shipped: Arc<AtomicU32>,
    }

    async fn start_checkout(State(st): State<AppState>) -> impl IntoResponse {
        let journey_id = uuid::Uuid::new_v4().to_string();

        // Seed + persist the journey's workflow state.
        let ctx = StepContext::new();
        ctx.set_correlation_id(&journey_id);
        ctx.set_variable("phase", json!("AWAITING_PAYMENT"));
        st.bff.query.register(&journey_id, ctx.clone());
        st.bff.state.save(&ctx).await.unwrap();

        // Build the signal-gated journey: reserve (domain-orders) -> wait for
        // `paid` -> ship (domain-fulfillment).
        let signals = Arc::clone(&st.bff.signals);
        let orders = Arc::clone(&st.orders_reserved);
        let fulfillment = Arc::clone(&st.fulfillment_shipped);
        let jid = journey_id.clone();
        let workflow = Workflow::new("checkout")
            .node(Node::new("reserve", move || {
                let orders = Arc::clone(&orders);
                async move {
                    // Mock domain-orders SDK call.
                    orders.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }))
            .node(
                Node::wait_for_signal("await-payment", &signals, jid.clone(), "paid")
                    .depends_on(["reserve"]),
            )
            .node(
                Node::new("ship", move || {
                    let fulfillment = Arc::clone(&fulfillment);
                    async move {
                        // Mock domain-fulfillment SDK call.
                        fulfillment.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                })
                .depends_on(["await-payment"]),
            );

        // Run the journey on a task; it parks on the `paid` gate.
        let runner = st.clone();
        let completed_id = journey_id.clone();
        tokio::spawn(async move {
            if workflow.run().await.is_ok() {
                // Journey done: advance the recorded phase + persist.
                if let Ok(Some(ctx)) = runner.bff.state.load(&completed_id).await {
                    ctx.set_variable("phase", json!("COMPLETED"));
                    let _ = runner.bff.state.save(&ctx).await;
                }
            }
        });

        (
            StatusCode::ACCEPTED,
            Json(json!({ "journeyId": journey_id })),
        )
    }

    async fn pay_checkout(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
        // The atomic "advance the workflow" endpoint: deliver the gate
        // signal. Buffered if the gate hasn't parked yet (no lost wakeup).
        st.bff
            .signals
            .deliver(&id, "paid", json!({ "amount": 100 }));
        StatusCode::ACCEPTED
    }

    async fn status_checkout(
        State(st): State<AppState>,
        Path(id): Path<String>,
    ) -> impl IntoResponse {
        match st.bff.state.load(&id).await.unwrap() {
            Some(ctx) => (
                StatusCode::OK,
                Json(json!({
                    "journeyId": id,
                    "phase": ctx.variable("phase").unwrap_or(json!("UNKNOWN")),
                })),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    #[tokio::test]
    async fn boot_two_domain_sdks_through_a_signal_gated_workflow() {
        let bff = Arc::new(bff_for("exp-checkout"));
        let st = AppState {
            bff: Arc::clone(&bff),
            orders_reserved: Arc::new(AtomicU32::new(0)),
            fulfillment_shipped: Arc::new(AtomicU32::new(0)),
        };

        // The atomic-endpoint router, run through the inherited web
        // middleware (CORS / security headers / correlation / metrics).
        let routes = Router::new()
            .route("/checkout", post(start_checkout))
            .route("/checkout/:id/pay", post(pay_checkout))
            .route("/checkout/:id", get(status_checkout))
            .with_state(st.clone());
        let api = bff.apply_middleware(routes);

        // 1. Start the journey. The workflow reserves (domain-orders) and
        //    parks on the `paid` gate.
        let res = api
            .clone()
            .oneshot(
                Request::post("/checkout")
                    .header("Origin", "https://app.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        // The inherited web batteries decorate the response.
        assert_eq!(res.headers().get("x-frame-options").unwrap(), "DENY");
        assert!(res.headers().contains_key(HEADER_CORRELATION_ID));
        let started = body_json(res).await;
        let journey_id = started["journeyId"].as_str().unwrap().to_string();

        // Wait for the reserve step + gate park (no sleep > 200ms).
        for _ in 0..200 {
            if st.orders_reserved.load(Ordering::SeqCst) == 1
                && bff.signals.list_active().contains(&journey_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            st.orders_reserved.load(Ordering::SeqCst),
            1,
            "domain-orders reserve ran"
        );
        assert_eq!(
            st.fulfillment_shipped.load(Ordering::SeqCst),
            0,
            "domain-fulfillment is gated behind the signal"
        );

        // 2. The status endpoint reports the persisted (Redis-capable) phase.
        let res = api
            .clone()
            .oneshot(
                Request::get(format!("/checkout/{journey_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let status = body_json(res).await;
        assert_eq!(status["phase"], "AWAITING_PAYMENT");

        // 3. Deliver the `paid` signal — the atomic "advance" endpoint. The
        //    parked workflow resumes and ships (domain-fulfillment).
        let res = api
            .clone()
            .oneshot(
                Request::post(format!("/checkout/{journey_id}/pay"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);

        // The workflow completes off-task: fulfillment ships + phase flips.
        for _ in 0..200 {
            if st.fulfillment_shipped.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            st.fulfillment_shipped.load(Ordering::SeqCst),
            1,
            "domain-fulfillment shipped after the signal"
        );

        // 4. The final status reflects completion + the gate is released.
        let mut completed = false;
        for _ in 0..200 {
            let res = api
                .clone()
                .oneshot(
                    Request::get(format!("/checkout/{journey_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = body_json(res).await;
            if status["phase"] == "COMPLETED" {
                completed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(completed, "journey phase reached COMPLETED");
        assert!(!bff.signals.list_active().contains(&journey_id));
    }

    #[test]
    fn version_matches_workspace() {
        assert_eq!(VERSION, firefly_starter_web::VERSION);
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn experience_stack_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExperienceStack>();
        assert_send_sync::<DomainClients>();
        assert_send_sync::<WorkflowState>();
    }
}
