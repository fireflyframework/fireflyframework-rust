# `firefly-starter-experience`

> **Tier:** Starter · **Status:** Stable · **Service tier:** Experience (BFF) · **Skill contract:** `generate-execution-plan-experience`

## The three service tiers

Firefly services are layered into **three service tiers** — distinct from the
crate-graph tiers in [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md). The
dependency direction is strict: `channel → experience → domain → core`.

| Service tier | Owns | Talks to | Rust starter |
|--------------|------|----------|--------------|
| **core** | the database (sqlx/R2DBC, migrations, CRUD) | nothing below | [`firefly-starter-core`](../starter-core/) / [`firefly-starter-data`](../starter-data/) |
| **domain** | sagas, CQRS, event sourcing, third-party adapters | **core** SDKs | [`firefly-starter-domain`](../starter-domain/) |
| **experience (BFF)** | signal-driven workflows, stateless aggregation, atomic REST | **domain** SDKs *only* | **this crate** |

An **experience** service is a Backend-for-Frontend (BFF): it aggregates
several **domain** SDKs into APIs shaped for one frontend / channel. It
**never** owns a database, **never** calls a core service directly, and
**never** calls a sibling experience service.

## Overview

`firefly-starter-experience` composes
[`firefly-starter-web`](../starter-web/) (so it inherits every web battery —
CORS, security headers, request metrics, access-log, correlation, idempotency,
and the actuator surface) with the four experience-tier building blocks:

* `clients` — `DomainClients`: the **domain-SDK composition** surface, a
  registry of named `RestClient`s (a `ClientFactory` over
  [`firefly-client`](../client/)) for calling downstream domain services.
* `signals` — `Arc<SignalService>`: `@WaitForSignal`-style gates. A workflow
  step parks until an atomic endpoint delivers the named signal
  ([`firefly-orchestration`](../orchestration/)).
* `state` — `WorkflowState`: **Redis-capable** persisted workflow state, keyed
  by correlation id, over the [`firefly-cache`](../cache/) `Arc<dyn Adapter>`
  abstraction. The in-memory adapter is the default; swap in
  [`firefly-cache-redis`](../cache-redis/)'s `RedisAdapter` for cross-restart
  durability.
* `query` — `Arc<WorkflowQueryService>`: the journey-status query surface —
  derive a phase / next-step DTO from the live step statuses (the main
  recovery mechanism).
* `children` — `Arc<ChildWorkflowService>`: child-workflow composition for
  nested journeys.

`ExperienceStack` dereferences to `WebStack` (which derefs to `Core`), so
every web + core field
and method — `apply_middleware`, `actuator_router`, `new_application`,
`with_security`, `cache`, `bus`, `scheduler`, … — is available directly on the
experience value. `starter_name` defaults to `"starter-experience"`. `Bff` is
a type alias for `ExperienceStack`.

## Public surface

```rust,ignore
pub struct ExperienceStack {     // alias: pub type Bff = ExperienceStack;
    pub web: WebStack,           // Deref/DerefMut target (→ Core)
    pub clients: DomainClients,  // ClientFactory
    pub signals: Arc<SignalService>,
    pub state: WorkflowState,    // Redis-capable persisted workflow state
    pub query: Arc<WorkflowQueryService>,
    pub children: Arc<ChildWorkflowService>,
}

impl ExperienceStack {
    pub fn new(cfg: CoreConfig) -> Self;
    pub fn with_security(self, chain: FilterChain) -> Self;
}

// Bootstrap pair:
pub fn register_experience_stack(cfg: CoreConfig) -> ExperienceStack;
pub fn enable_experience_stack(cfg: CoreConfig) -> CoreConfig;

// Domain-SDK composition registry.
pub struct DomainClients { /* … */ }
impl DomainClients {
    pub fn register(&self, name, base_url) -> Arc<RestClient>;
    pub fn register_client(&self, name, client: RestClient);
    pub fn get(&self, name: &str) -> Option<Arc<RestClient>>;
    pub fn names(&self) -> Vec<String>;
}

// Redis-capable persisted workflow state.
pub struct WorkflowState { /* … */ }
impl WorkflowState {
    pub fn new(adapter: Arc<dyn Adapter>) -> Self;
    pub async fn save(&self, ctx: &StepContext) -> Result<(), CacheError>;
    pub async fn load(&self, correlation_id: &str) -> Result<Option<StepContext>, CacheError>;
    pub async fn delete(&self, correlation_id: &str) -> Result<(), CacheError>;
}
```

`Core`, `CoreConfig`, `WebStack`, `FilterChain`, the orchestration journey
primitives (`Workflow`, `Node`, `SignalService` types, `WorkflowQueryError`,
`CompensationPolicy`, …), and the domain-SDK client surface (`DomainClient` =
`RestClient`, `DomainClientBuilder` = `RestBuilder`, `ClientError`) are
re-exported flat, so an experience service can depend on
`firefly-starter-experience` alone.

## Atomic REST + signal-driven workflows

A BFF journey is a `Workflow` whose steps call domain SDKs and whose gates
park on signals. The controller exposes **atomic** endpoints (one per phase),
each backed by the wired building blocks:

| Endpoint | Building block | Effect |
|----------|----------------|--------|
| `POST /journeys` | `state.save` + run a `Workflow` | start the journey; persist its state; the workflow reserves via a domain SDK then parks on a `@WaitForSignal` gate |
| `POST /journeys/{id}/data` | `signals.deliver` | the **advance** endpoint — deliver the gate signal; the parked workflow resumes and calls the next domain SDK |
| `GET /journeys/{id}` | `state.load` / `query` | report the persisted phase / next step — the recovery mechanism |

Because state lives in Redis (or any `Adapter`), a client can resume a journey
after a disconnect.

## Quick start

```rust,ignore
use std::sync::Arc;
use firefly_starter_experience::{CoreConfig, ExperienceStack, Node, Workflow};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bff = ExperienceStack::new(CoreConfig {
        app_name: "exp-onboarding".into(),
        ..CoreConfig::default()
    });

    // Experience → Domain only: register the downstream domain SDKs.
    bff.clients.register("orders", "https://domain-orders.internal");
    bff.clients.register("fulfillment", "https://domain-fulfillment.internal");

    // A signal-driven journey: reserve → wait for payment → ship.
    let signals = Arc::clone(&bff.signals);
    let workflow = Workflow::new("checkout")
        .node(Node::new("reserve", || async { Ok(()) }))
        .node(Node::wait_for_signal("await-payment", &signals, "j-1", "paid")
            .depends_on(["reserve"]))
        .node(Node::new("ship", || async { Ok(()) }).depends_on(["await-payment"]));

    bff.init_logging()?;
    bff.print_banner();
    Ok(())
}
```

## Testing

```bash
cargo test -p firefly-starter-experience
```

Covers: the BFF building blocks are wired and named `"starter-experience"`;
starter-name rules (custom names survive, an
explicit `"starter-core"`/`"starter-web"` is renamed); the
`register_experience_stack` / `enable_experience_stack` bootstrap pair;
the `DomainClients` register/resolve/replace contract; `WorkflowState`
round-trips a `StepContext` snapshot through the cache (miss → `Ok(None)`);
`Deref`/`DerefMut` promotion of the web + core surface; `Send + Sync` bounds;
and the **headline boot test** — two mock domain SDKs composed through a
signal-gated workflow, driven via `tower::oneshot`: `POST /checkout` reserves
(domain-orders) and parks on the `paid` gate, `POST /checkout/{id}/pay`
delivers the signal so the workflow ships (domain-fulfillment), and
`GET /checkout/{id}` reports the persisted phase — proving the
experience-tier atomic-REST + Redis-capable-state + `@WaitForSignal` contract
end to end.
