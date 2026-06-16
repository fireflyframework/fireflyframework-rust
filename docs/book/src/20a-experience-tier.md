# The Experience Tier (BFF)

Lumen, as the book has built it, is a self-contained service: it owns the
wallet domain *and* the HTTP API that fronts it. Real Firefly systems split that
responsibility across **three service tiers**, and Lumen sits at the bottom two.
By the end of this chapter you will know where a frontend-facing API for Lumen
belongs — the **experience tier** — and you will build a small Backend-for-Frontend
that composes Lumen as a downstream domain SDK, drives a multi-step "fund a
wallet and confirm" journey with a signal-gated workflow, and survives a client
disconnect by persisting journey state.

This is the one Firefly tier Lumen itself never enters, so the chapter introduces
it from first principles and then wires it against the service you already have.

> **What the experience tier is.** The experience tier is Firefly's
> Backend-for-Frontend layer, built on the `firefly-starter-experience` crate: it
> composes downstream domain SDKs, drives multi-request journeys with signal-gated
> workflows, and persists journey state across requests. A signal gate parks a
> workflow step until a named signal arrives; a domain-SDK registry resolves each
> downstream client by logical name.

## The three service tiers

Firefly services are layered into three **service** tiers (distinct from the
crate-graph tiers in the architecture docs). The dependency direction is strict
and one-way: `channel → experience → domain → core`.

| Service tier | Owns | Talks to | Rust starter |
|--------------|------|----------|--------------|
| **core** | the database (R2DBC/sqlx, migrations, CRUD) | nothing below | `firefly-starter-core` / `firefly-starter-data` |
| **domain** | sagas, CQRS, event sourcing, third-party adapters | **core** SDKs | `firefly-starter-domain` |
| **experience (BFF)** | signal-driven workflows, stateless aggregation, atomic REST | **domain** SDKs *only* | `firefly-starter-experience` |

Lumen, with its event-sourced ledger, CQRS bus, and transfer saga, is a
**domain** service. An **experience** service is a Backend-for-Frontend: it
aggregates several domain SDKs into APIs shaped for *one* frontend or channel. It
**never** owns a database, **never** calls a core service directly, and **never**
calls a sibling experience service — it composes domain SDKs (over
`firefly-client`) and nothing else.

> **The tier boundary is a type.** The strict `channel → experience → domain →
> core` direction is enforced by the starters themselves: an experience stack can
> register domain SDKs and nothing else. A BFF never owns a database and never
> calls a core service or a sibling BFF directly — the dependency direction is
> one-way by construction.

## `ExperienceStack` — batteries for a BFF

`ExperienceStack::new(cfg)` builds a full `WebStack` (so it inherits every web
battery from the [production chapter](./20-production.md) — CORS, security
headers, request metrics, access log, correlation, idempotency, the actuator
surface) and adds the four experience-tier building blocks:

```rust,ignore
use firefly_starter_experience::{CoreConfig, ExperienceStack};

let bff = ExperienceStack::new(CoreConfig {
    app_name: "lumen-bff".into(),
    app_version: "1.0.0".into(),
    ..CoreConfig::default()
});
```

The stack adds five experience-tier public fields on top of the embedded `web: WebStack` (`Bff` is an alias for `ExperienceStack`):

| Field | Type | Role |
|-------|------|------|
| `clients` | `DomainClients` | the domain-SDK registry, resolved by logical name |
| `signals` | `Arc<SignalService>` | the signal gates a workflow step parks on |
| `state` | `WorkflowState` | Redis-capable persisted journey state, keyed by correlation id |
| `query` | `Arc<WorkflowQueryService>` | the journey-status query surface |
| `children` | `Arc<ChildWorkflowService>` | child-workflow composition for nested journeys |

`ExperienceStack` `Deref`s to its `WebStack` (which derefs to `Core`), so every
web + core method — `apply_middleware`, `actuator_router`, `new_application`,
`with_security`, `cache`, `bus`, `scheduler` — is reachable directly on the BFF
value. These are the lower-level building blocks `FireflyApplication` drives for
you on a plain domain service like Lumen; a BFF reaches for them directly because
its router is assembled by hand from workflow controllers rather than
auto-mounted. There is also a bootstrap pair, `register_experience_stack(cfg)`
(== `ExperienceStack::new`) and `enable_experience_stack(cfg)` (stamps the tier
defaults onto a `CoreConfig`), so you can reach the tier through whichever
spelling reads more naturally at the call site.

> **`FireflyApplication` is the primary bootstrap.** For an ordinary domain or
> experience service, the turnkey path is `FireflyApplication::new(name).run().await`
> — it component-scans beans, auto-mounts controllers, drains the inventory-registered
> handlers/listeners/scheduled tasks, applies security + middleware, self-hosts the
> admin dashboard, and serves both ports. The `ExperienceStack` methods below
> (`apply_middleware`, `with_security`, `new_application`) are the explicit
> building blocks you compose by hand when a BFF's signal-gated journey controllers
> need wiring the auto-mount path does not cover; they remain fully supported.

> **Two ways to build the stack.** `ExperienceStack::new(cfg)` and the equivalent
> `register_experience_stack(cfg)` both build the BFF; `enable_experience_stack(cfg)`
> stamps the tier defaults onto a `CoreConfig`. Because `ExperienceStack` derefs to
> `WebStack` (which derefs to `Core`), the BFF *is* a web service plus the journey
> machinery.

## Composing domain SDKs — `DomainClients`

A BFF reaches its downstream domain services through named `RestClient`s, one per
dependency. `DomainClients` is the registry; register Lumen under a logical name
and resolve it by that name from any handler or workflow step:

```rust,ignore
// Experience -> Domain only. Register Lumen as a downstream domain SDK.
bff.clients.register("wallets", "https://lumen.internal");

// later, in a handler or workflow step:
let wallets = bff.clients.get("wallets").expect("wallets SDK");
// wallets is an Arc<RestClient> with correlation-id propagation, JSON codec,
// RFC 7807 error decoding, and retry/backoff all inherited from firefly-client.
```

`register(name, base_url)` builds a default client; `register_client(name,
client)` takes a pre-tuned `RestClient` (custom timeout, headers, retry policy).
Re-registering a name replaces it (last wins), and `names()` lists every
registered SDK.

> **Resolve clients by name.** `DomainClients` lets a step resolve the right
> downstream client by logical name instead of threading a `RestBuilder` through
> every call site. The clients carry the same correlation propagation and RFC 7807
> decoding the [HTTP-clients chapter](./13-http-clients.md) covered.

## Signal-driven journeys

A BFF journey is rarely one request. "Fund a wallet, wait for the customer to
confirm the amount, then commit the transfer" is three interactions over time. A
**workflow** with a **signal gate** models exactly that: steps that call domain
SDKs, and a gate that parks until an atomic endpoint delivers a named signal.

The journey is a `Workflow` of `Node`s; `Node::wait_for_signal` parks on the
stack's `SignalService` until the signal arrives:

```rust,ignore
use std::sync::Arc;
use firefly_starter_experience::{Node, Workflow};

let signals = Arc::clone(&bff.signals);
let journey_id = "j-1".to_string();

let workflow = Workflow::new("fund-and-confirm")
    // 1. reserve: call the Lumen "wallets" SDK to open/lock the funds.
    .node(Node::new("reserve", || async { Ok(()) }))
    // 2. await-confirm: park until POST /journeys/{id}/data delivers "confirmed".
    .node(
        Node::wait_for_signal("await-confirm", &signals, journey_id.clone(), "confirmed")
            .depends_on(["reserve"]),
    )
    // 3. commit: call the Lumen "wallets" SDK to run the transfer.
    .node(Node::new("commit", || async { Ok(()) }).depends_on(["await-confirm"]));
```

`Workflow::run().await` executes the nodes in dependency order; when it reaches
`await-confirm` it parks. An atomic endpoint later calls
`bff.signals.deliver(&journey_id, "confirmed", payload)` and the parked node
resumes. Delivery is **buffered** — if the signal arrives before the gate parks,
there is no lost wakeup. `signals.list_active()` lists the journeys currently
parked on a gate.

> **Signal gates.** `Node::wait_for_signal(...)` parks a workflow node until
> `signals.deliver(...)` delivers the named signal — the external event that
> satisfies the gate. Delivery is buffered, so a signal that arrives before the
> gate parks is not lost. The workflow is the same DAG-with-compensation engine
> from the [sagas chapter](./12-sagas.md) (`firefly-orchestration`), here driven
> by signals rather than run to completion in one call.

## Persisting journey state — `WorkflowState`

Because a journey spans several requests, its state must outlive any one of them.
`WorkflowState` round-trips a workflow run's `StepContext` snapshot through the
stack's cache `Adapter`, keyed by correlation id. The in-memory adapter is the
default; point it at `firefly-cache-redis`'s `RedisAdapter` for cross-restart
durability — the convention the experience tier is built around.

```rust,ignore
use firefly_starter_experience::CoreConfig;
use firefly_orchestration::StepContext;

// Save when a journey parks:
let ctx = StepContext::new();
ctx.set_correlation_id("j-1");
ctx.set_variable("phase", serde_json::json!("AWAITING_CONFIRM"));
bff.state.save(&ctx).await?;

// Rehydrate from a later request to advance it:
if let Some(ctx) = bff.state.load("j-1").await? {
    // ... advance the journey ...
}

// Discard when the journey completes:
bff.state.delete("j-1").await?;
```

A miss on an unknown journey is `Ok(None)`, not an error — so a status check on a
journey that never existed is a clean 404, not a 500.

> **Durable journey state.** `WorkflowState` persists a workflow run's
> `StepContext` through the **cache** `Adapter`, keyed by correlation id — point
> it at Redis for cross-restart durability, the Firefly convention of "hold
> workflow state in Redis." A parked journey saves its `StepContext`; a later
> request loads it and resumes, surviving the client disconnect an in-memory
> waiter would not.

## Querying journey status — `WorkflowQueryService`

The frontend polls "where is my journey?" while it waits. `WorkflowQueryService`
holds the live `StepContext` per run and answers named queries against it — the
main recovery mechanism when a client reconnects:

```rust,ignore
bff.query.register(&journey_id, ctx.clone());            // on start
bff.query.register_query(&journey_id, "phase", |ctx| {    // a named query
    ctx.variable("phase").unwrap_or(serde_json::json!("UNKNOWN"))
});
let phase = bff.query.query(&journey_id, "phase")?;       // GET /journeys/{id}
bff.query.unregister(&journey_id);                        // on completion
```

## The atomic-endpoint contract

Put together, an experience controller exposes one request per journey phase —
the **atomic REST** shape:

| Method & path | Does |
|---------------|------|
| `POST /journeys` | start the workflow (calls the "wallets" SDK to reserve), persist `WorkflowState`, park on the gate, return the journey id |
| `POST /journeys/:id/data` | deliver the `confirmed` signal — the parked workflow resumes and commits the transfer via the "wallets" SDK |
| `GET  /journeys/:id` | report the persisted phase (or 404 if unknown) |

Each phase is one HTTP request; state lives in the cache (Redis-capable), so a
client can resume a journey after a disconnect. Because the controller runs
through `bff.apply_middleware(routes)`, every response inherits the web batteries
— the start response carries `X-Frame-Options: DENY` and a correlation id just as
Lumen's own responses do, and the same `ExperienceStack::with_security(chain)`
filter chain from the [security chapter](./14-security.md) guards the mutating
routes.

This is exactly the contract the crate's own boot test proves end to end (its
checkout journey uses an `AWAITING_PAYMENT` phase rather than Lumen's
`AWAITING_CONFIRM`, but the shape is identical): two mock domain SDKs composed
through a signal-gated workflow, driven with `tower::oneshot` — start parks on
the gate, the status endpoint reports the persisted phase, delivering the signal
advances the workflow off-task, and the final status flips to `COMPLETED`.

## Where Lumen fits

Drawn as the full estate, Lumen is one of the domain services a BFF composes:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 300" role="img"
     aria-label="The estate around Lumen, top to bottom: web/mobile app on the channel tier, lumen-bff on the experience tier, lumen on the domain tier, accounts on the core tier; calls flow downward, experience to domain only"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <rect x="24" y="14" width="320" height="44" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="184" y="41" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">web / mobile app</text>
  <text x="360" y="41" font-size="11.5" fill="#7a6450">channel tier</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="184" y1="58" x2="184" y2="78"/><polygon points="184,86 180,78 188,78"/>
  </g>
  <rect x="24" y="88" width="320" height="44" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="184" y="115" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">lumen-bff
    <tspan font-size="11" fill="#7a6450"> (experience)</tspan></text>
  <text x="360" y="106" font-size="11.5" fill="#7a6450">this chapter:</text>
  <text x="360" y="121" font-size="10.5" fill="#7a6450">DomainClients + signals + state</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="184" y1="132" x2="184" y2="152"/><polygon points="184,160 180,152 188,152"/>
  </g>
  <text x="200" y="150" font-size="11" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">Experience &#8594; Domain only</text>
  <rect x="24" y="162" width="320" height="44" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="184" y="189" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">lumen
    <tspan font-size="11" fill="#7a6450"> (domain)</tspan></text>
  <text x="360" y="180" font-size="11.5" fill="#7a6450">the service the book built</text>
  <text x="360" y="195" font-size="10.5" fill="#7a6450">(ledger, CQRS, saga)</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="184" y1="206" x2="184" y2="226"/><polygon points="184,234 180,226 188,226"/>
  </g>
  <rect x="24" y="236" width="320" height="44" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="184" y="263" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">accounts
    <tspan font-size="11" fill="#7a6450"> (core)</tspan></text>
  <text x="360" y="263" font-size="11.5" fill="#7a6450">owns the database</text>
</svg>
<figcaption>Lumen as one service in the estate. Calls flow strictly downward; the BFF on the experience tier composes Lumen on the domain tier, which owns its logic, while the core service owns the database.</figcaption>
</figure>

The BFF never reaches into Lumen's event store or bus — it speaks to Lumen's
public HTTP API through the registered `"wallets"` SDK, exactly as any external
client would, and adds only the journey orchestration a frontend needs.

## What changed in Lumen

Lumen itself is unchanged — it is a *domain* service, and this chapter is about
the tier *above* it. What you built is the mental model and the wiring for a
frontend-facing BFF: an `ExperienceStack` inheriting Lumen's web batteries,
`DomainClients` registering Lumen as the `"wallets"` SDK (Experience → Domain
only), a signal-gated `Workflow` modeling a multi-request "fund and confirm"
journey, `WorkflowState` persisting that journey through a Redis-capable cache
adapter, and `WorkflowQueryService` answering status polls — every API drawn from
the real `firefly-starter-experience` surface.

## Exercises

1. **Register Lumen as a domain SDK.** Build an `ExperienceStack`, call
   `bff.clients.register("wallets", "http://localhost:8080")`, and confirm
   `bff.clients.get("wallets")` returns a client and `bff.clients.names()`
   lists it.
2. **Park and resume.** Build a two-node workflow whose second node is a
   `Node::wait_for_signal` gate. Run it on a task, assert
   `bff.signals.list_active()` contains the journey id, then `deliver` the signal
   and confirm the workflow completes.
3. **Persist a journey.** Save a `StepContext` with a `phase` variable via
   `bff.state.save`, `load` it back from a fresh handle, and confirm the variable
   survives. Then `delete` it and confirm `load` returns `Ok(None)`.
4. **Atomic endpoints.** Wire the three-route controller above with
   `bff.apply_middleware(routes)` and drive it with `tower::oneshot`: start,
   poll status (`AWAITING_CONFIRM`), confirm, poll again (`COMPLETED`). Assert the
   start response carries the inherited `X-Frame-Options` header.

With Lumen in production (the previous chapter) and its place in the tiered
estate clear, the capstone re-reads the whole service through the
declarative-macro lens. Continue to
[Declarative Services with Macros](./21-declarative-macros.md).
