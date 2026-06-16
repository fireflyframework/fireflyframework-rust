# The Experience Tier (BFF)

Lumen, as the book has built it, is a self-contained service: it owns the wallet
domain *and* the HTTP API that fronts it. Real Firefly estates split that
responsibility across **three service tiers**, and Lumen sits in the lower two.
This chapter introduces the one tier Lumen itself never enters — the
**experience tier**, Firefly's Backend-for-Frontend layer — and then wires a
small BFF against the service you already have. The BFF composes Lumen as a
downstream domain SDK, drives a multi-step "fund a wallet and confirm" journey
with a signal-gated workflow, and survives a client disconnect by persisting the
journey's state.

Because this tier is new ground, the chapter teaches it from first principles:
what the three tiers are and why the dependency direction is one-way, what an
`ExperienceStack` gives you, how to register a domain SDK, how a signal gate
parks a workflow until an external event arrives, and how to persist and query a
journey that spans several HTTP requests. Every API here is drawn from the real
`firefly-starter-experience` crate — the same surface its own boot test exercises
end to end.

By the end of this chapter you will:

- Explain the `channel → experience → domain → core` tier model and why the
  experience tier may compose *domain* SDKs only — never a database, a core
  service, or a sibling BFF.
- Build an `ExperienceStack` (the BFF starter) and understand how it inherits the
  full web batteries while adding five experience-tier building blocks.
- Register Lumen as a named domain SDK through `DomainClients` and resolve it by
  logical name from any handler or workflow step.
- Model a multi-request journey as a `Workflow` whose gate parks on a named
  signal, and resume it by delivering that signal from a later request.
- Persist a journey's state with `WorkflowState` (Redis-capable) and answer
  "where is my journey?" with `WorkflowQueryService`, so a client can reconnect
  after a disconnect.
- Assemble the three-endpoint atomic-REST controller that ties it all together.

## Concepts you will meet

These are the ideas the chapter leans on. Each is reintroduced in context the
first time it is used; this is the short version.

> **Note** **Key term — Backend-for-Frontend (BFF).** A BFF is an HTTP service
> that exists to serve *one* frontend or channel: it aggregates several
> downstream services into endpoints shaped exactly for that UI's screens and
> flows. It owns no database of its own. The Spring analog is a Spring Cloud
> Gateway / aggregation service sitting in front of your domain microservices —
> here it is a first-class, batteries-included tier.

> **Note** **Key term — domain SDK.** A *domain SDK* is just an HTTP client
> pointed at a downstream domain service's public API, dressed up with the
> framework's correlation propagation, JSON codec, error decoding, and
> retry/backoff. A BFF calls its dependencies through these SDKs exactly as any
> external client would — it never reaches into their internals.

> **Note** **Key term — signal gate.** A *signal gate* is a workflow step that
> parks (suspends) until a named *signal* is delivered from outside the workflow
> — typically by a later HTTP request. It models "wait for the customer to
> confirm" inside an otherwise sequential journey. The Java/Firefly analog is a
> `@WaitForSignal` step; there is no direct Spring Boot equivalent.

## Step 1 — Understand the three service tiers

Firefly estates are layered into three **service** tiers (distinct from the
crate-graph tiers in the architecture docs). The dependency direction is strict
and one-way: `channel → experience → domain → core`. A tier may only call the
tier directly below it.

| Service tier | Owns | Talks to | Rust starter |
|--------------|------|----------|--------------|
| **core** | the database (sqlx, migrations, CRUD) | nothing below | `firefly-starter-core` / `firefly-starter-data` |
| **domain** | sagas, CQRS, event sourcing, third-party adapters | **core** SDKs | `firefly-starter-domain` |
| **experience (BFF)** | signal-driven workflows, stateless aggregation, atomic REST | **domain** SDKs *only* | `firefly-starter-experience` |

Lumen — with its event-sourced ledger, its CQRS bus, and its transfer saga — is a
**domain** service. An **experience** service is the BFF that fronts it: it
aggregates one or more domain SDKs into endpoints shaped for a single frontend or
channel. It **never** owns a database, **never** calls a core service directly,
and **never** calls a sibling experience service. It composes domain SDKs (over
`firefly-client`) and nothing else.

What just happened: you placed Lumen on the map. The book has been building a
domain service all along; this chapter builds the tier *above* it. Knowing the
direction matters because it is not a convention you remember — it is enforced by
the starters themselves.

> **Design note.** The tier boundary is a *type*, not a code-review rule. An
> experience stack exposes a registry that holds domain SDKs and nothing else
> (you will meet it in Step 3), and it has no data-access surface to register a
> database against. A BFF that tried to own a table or dial a core service simply
> has no API to do so — the dependency direction is one-way by construction.

> **Tip** **Checkpoint.** You can state, without looking, what each tier owns and
> who it may call. The experience tier composes domain SDKs only; Lumen is the
> domain service the BFF in this chapter will call.

## Step 2 — Build the BFF stack with `ExperienceStack`

A BFF lives in its own crate. Unlike a plain domain service — which depends only
on the one `firefly` facade — a BFF depends on the experience-tier starter
directly, because the facade carries the core and web starters but not the
experience one.

> **Note** **Key term — experience starter.** `firefly-starter-experience` is the
> crate that turns a web service into a BFF. It builds on the web starter (so you
> get every HTTP battery) and adds the journey machinery. The Spring analog is a
> Spring Boot *starter* that bundles a coherent slice of capability behind one
> dependency.

```toml
# Cargo.toml of a BFF crate (e.g. lumen-bff). Note: the experience starter is a
# direct dependency — the `firefly` facade carries the core + web starters but
# not this one.
[dependencies]
firefly-starter-experience = { version = "26.6.28" }
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
```

With the dependency in place, one call builds the whole stack:

```rust,ignore
use firefly_starter_experience::{CoreConfig, ExperienceStack};

let bff = ExperienceStack::new(CoreConfig {
    app_name: "lumen-bff".into(),
    app_version: "1.0.0".into(),
    ..CoreConfig::default()
});
```

What just happened: `ExperienceStack::new(cfg)` builds a full `WebStack`
underneath — so the BFF inherits every web battery from the
[production chapter](./20-production.md): CORS, security headers, request
metrics, the access log, correlation-id propagation, idempotency, and the
actuator surface — and then layers the five experience-tier building blocks on
top. The `CoreConfig` is the same typed configuration value the rest of the book
has used; the experience starter stamps `starter_name` to `"starter-experience"`
when you leave it at its default, so the banner and `/actuator/info` report the
tier.

The five experience-tier fields sit on top of the embedded `web: WebStack`:

| Field | Type | Role |
|-------|------|------|
| `clients` | `DomainClients` | the domain-SDK registry, resolved by logical name |
| `signals` | `Arc<SignalService>` | the signal gates a workflow step parks on |
| `state` | `WorkflowState` | Redis-capable persisted journey state, keyed by correlation id |
| `query` | `Arc<WorkflowQueryService>` | the journey-status query surface |
| `children` | `Arc<ChildWorkflowService>` | child-workflow composition for nested journeys |

`ExperienceStack` `Deref`s to its `WebStack` (which in turn derefs to `Core`), so
every web + core method and field — `apply_middleware`, `actuator_router`,
`new_application`, `with_security`, and the `cache`, `bus`, `scheduler` fields —
is reachable directly on the `bff` value. `Bff` is a type alias for
`ExperienceStack`, so you can spell the type whichever way reads better at the
call site.

> **Note** There are two more spellings for the same wiring, kept for services
> migrating from other Firefly ports. `register_experience_stack(cfg)` is an
> alias for `ExperienceStack::new(cfg)`, and `enable_experience_stack(cfg)` takes
> a `CoreConfig`, stamps the tier's defaults onto it (inheriting the web
> batteries), and hands it back — you then pass that to `ExperienceStack::new`.
> Reach for whichever reads most naturally; they wire the identical stack.

> **Design note.** On a plain domain service like Lumen,
> `FireflyApplication::new(name).run().await` is the turnkey path — it
> component-scans beans, auto-mounts controllers, drains the inventory-registered
> handlers and listeners, applies security and middleware, self-hosts the admin
> dashboard, and serves both ports. A BFF reaches for the lower-level building
> blocks (`apply_middleware`, `with_security`, `new_application`) directly because
> its router is assembled by hand from signal-gated journey controllers rather
> than auto-mounted. Those methods are the same ones `FireflyApplication` drives
> for you under the hood, and they remain fully supported.

> **Tip** **Checkpoint.** `ExperienceStack::new(...)` returns a value on which
> `bff.app_name` reports your app name and `bff.starter_name` is
> `"starter-experience"`. `bff.clients.is_empty()`, `bff.signals.list_active()`,
> and `bff.query.active()` are all empty — the building blocks are wired and
> waiting.

## Step 3 — Register Lumen as a domain SDK

A BFF reaches each downstream domain service through a named REST client — one
per dependency. `DomainClients` is the registry: you register Lumen under a
logical name, then resolve it by that name from any handler or workflow step,
without threading a builder through every call site.

> **Note** **Key term — `RestClient`.** The `RestClient` is `firefly-client`'s
> HTTP client. It carries correlation-id propagation, a JSON codec, RFC 9457
> `application/problem+json` error decoding (a non-2xx response becomes a typed
> error), and retry/backoff — the same client the
> [HTTP-clients chapter](./13-http-clients.md) covered. `register` hands you back
> an `Arc<RestClient>` for immediate use.

```rust,ignore
// Experience -> Domain only. Register Lumen as a downstream domain SDK.
bff.clients.register("wallets", "https://lumen.internal");

// Later, in a handler or workflow step, resolve it by its logical name:
let wallets = bff.clients.get("wallets").expect("wallets SDK");
// `wallets` is an Arc<RestClient> with correlation-id propagation, a JSON codec,
// RFC 9457 problem decoding, and retry/backoff — all inherited from firefly-client.
```

What just happened: `register(name, base_url)` builds a default `RestClient` for
that base URL, stores it under the logical name, and returns the `Arc<RestClient>`
it built. `get(name)` resolves it later, returning `None` when nothing is
registered under that name. Because every registered client points at a domain
service, this registry is exactly where the "experience → domain only" rule lives.

Once you hold a client, you call the downstream API through `request`:

```rust,ignore
use http::Method;
use serde_json::json;

// Call Lumen's public API as any external client would. A non-2xx response
// decodes into a typed ClientError (RFC 9457 problem document).
let _: serde_json::Value = wallets
    .request(Method::POST, "/wallets/w-1/reserve", Some(&json!({ "amount": 5000 })))
    .await?;
```

The registry has a small, predictable surface:

- `register(name, base_url)` builds and stores a default client (last write wins
  if the name already exists).
- `register_client(name, client)` stores a pre-tuned `RestClient` — use it when a
  domain SDK needs a custom timeout, default headers, or retry policy.
- `get(name)` resolves a client, or `None`.
- `names()` lists every registered SDK (sorted), and `len()` / `is_empty()`
  report the registry size.

> **Design note.** Resolving a client by logical name — `"wallets"` rather than a
> hard-coded URL — is what keeps the BFF's journey code decoupled from where Lumen
> actually runs. Point the name at `https://lumen.internal` in production and at
> `http://localhost:8080` in a test, and not one line of the workflow changes.

> **Tip** **Checkpoint.** After `bff.clients.register("wallets", ...)`,
> `bff.clients.get("wallets")` returns `Some(_)`, `bff.clients.names()` is
> `["wallets"]`, and `bff.clients.len()` is `1`. Resolving an unregistered name
> returns `None`, never a panic.

## Step 4 — Model the journey as a signal-gated workflow

A BFF journey is rarely one request. "Fund a wallet, wait for the customer to
confirm the amount, then commit the transfer" is three interactions spread over
time. A **workflow** with a **signal gate** models exactly that: steps that call
domain SDKs, and a gate that parks until an external caller delivers a named
signal.

> **Note** **Key term — workflow and node.** A `Workflow` is a directed graph of
> `Node`s, each an async step, executed in dependency order. It is the same
> DAG-with-compensation engine from the [sagas chapter](./12-sagas.md)
> (`firefly-orchestration`) — here driven by signals rather than run to
> completion in one call. `Node::new(name, action)` defines a step;
> `.depends_on([...])` declares which steps must finish first.

The journey is a `Workflow` of `Node`s; `Node::wait_for_signal` builds the gate
node, parking on the stack's `SignalService` until the named signal arrives:

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

What just happened, node by node:

- `Node::new("reserve", || async { Ok(()) })` is the first step. In a real BFF its
  body resolves `bff.clients.get("wallets")` and calls Lumen's reserve endpoint;
  here the body is a stub that returns `Ok(())` so the shape is clear. A node
  action returns `Result<(), BoxError>`.
- `Node::wait_for_signal("await-confirm", &signals, journey_id.clone(),
  "confirmed")` builds the **gate** node. It takes the node name, a reference to
  the stack's `Arc<SignalService>`, the journey's correlation id, and the signal
  name to wait for. `.depends_on(["reserve"])` makes it run after `reserve`.
- `Node::new("commit", ...).depends_on(["await-confirm"])` is the final step,
  which runs only once the gate releases.

`workflow.run().await` executes the nodes in dependency order and returns
`Result<(), WorkflowError>`. When the run reaches `await-confirm` it **parks** —
the future suspends inside the gate node and does not progress. You typically
spawn the run on a task so the HTTP handler that started it can return
immediately:

```rust,ignore
// Run the journey on a task; it parks on the `await-confirm` gate.
tokio::spawn(async move {
    let _ = workflow.run().await;
});
```

Later, an atomic endpoint delivers the signal and the parked node resumes:

```rust,ignore
// From a later request (POST /journeys/{id}/data):
bff.signals.deliver(&journey_id, "confirmed", serde_json::json!({ "ok": true }));
```

> **Note** **Key term — signal delivery (buffered).** `signals.deliver(id,
> signal, payload)` wakes the parked gate. Delivery is **buffered**: if the
> signal arrives *before* the gate has parked, the payload is held and the next
> `wait_for_signal` for that pair resolves immediately — so there is no lost
> wakeup in a race. `deliver` returns `true` when a live waiter consumed the
> signal and `false` when it was buffered (do not treat `false` as an error).
> `signals.list_active()` lists every journey currently parked on, or holding a
> buffered signal for, a gate.

> **Tip** **Checkpoint.** Spawn `workflow.run()`, then poll
> `bff.signals.list_active()` — once the run reaches the gate it contains
> `journey_id`. Call `bff.signals.deliver(&journey_id, "confirmed", payload)` and
> the workflow completes; `list_active()` no longer lists the id.

## Step 5 — Persist the journey with `WorkflowState`

A journey spans several requests, so its state must outlive any single one. If
the customer closes the tab between "reserve" and "confirm," a purely in-memory
waiter would be lost. `WorkflowState` solves this by round-tripping a workflow
run's `StepContext` snapshot through the stack's cache `Adapter`, keyed by
correlation id.

> **Note** **Key term — `StepContext`.** A `StepContext` is the per-run bag of
> facts a workflow carries: its correlation id, the inputs, each step's result,
> and free-form variables. It serializes to and from a JSON snapshot, which is
> what `WorkflowState` stores. It lives in `firefly-orchestration`, so you import
> it from there.

> **Note** **Key term — cache `Adapter`.** The cache `Adapter` is Firefly's
> pluggable key/value backend. The in-memory adapter is the default; point it at
> `firefly-cache-redis`'s `RedisAdapter` for cross-restart durability — the
> convention the experience tier is built around. `ExperienceStack` wires
> `state` over the same adapter the `Core` holds, so swapping in Redis is a config
> change, not a code change.

```rust,ignore
use firefly_orchestration::StepContext;

// Save when a journey parks:
let ctx = StepContext::new();
ctx.set_correlation_id("j-1");
ctx.set_variable("phase", serde_json::json!("AWAITING_CONFIRM"));
bff.state.save(&ctx).await?;

// Rehydrate from a later request to advance it:
if let Some(ctx) = bff.state.load("j-1").await? {
    // ... advance the journey using ctx ...
    let _ = ctx;
}

// Discard when the journey completes:
bff.state.delete("j-1").await?;
```

What just happened:

- `StepContext::new()` makes an empty context; `set_correlation_id` keys it (this
  is the journey id `WorkflowState` stores under), and `set_variable("phase", …)`
  records where the journey is. You read it back with `ctx.variable("phase")`.
- `bff.state.save(&ctx).await?` persists the snapshot under the context's
  correlation id, returning `Result<(), CacheError>`.
- `bff.state.load("j-1").await?` returns `Result<Option<StepContext>,
  CacheError>`. A **miss on an unknown journey is `Ok(None)`, not an error** — so a
  status check on a journey that never existed renders as a clean 404, never a
  500.
- `bff.state.delete("j-1").await?` evicts the state when the journey finishes, so
  completed runs do not linger.

> **Design note.** This is the seam that makes a BFF resilient to client
> disconnects. A parked journey saves its `StepContext` before suspending; a later
> request — possibly from a fresh browser session, possibly after the BFF
> restarted (with the Redis adapter) — loads it back and resumes. The in-memory
> waiter alone could not survive that gap; the persisted state can.

> **Tip** **Checkpoint.** `save` a `StepContext` carrying a `phase` variable,
> then `load` it from a fresh handle: the variable survives the round-trip.
> `delete` it, and `load` returns `Ok(None)`.

## Step 6 — Answer status polls with `WorkflowQueryService`

While the customer waits at the confirm screen, the frontend polls "where is my
journey?" `WorkflowQueryService` holds the live `StepContext` per run and answers
*named* queries against it — the main recovery mechanism when a client reconnects.

```rust,ignore
let journey_id = "j-1".to_string();

// On start: register the run's live context.
bff.query.register(&journey_id, ctx.clone());

// Register a named query that projects a value out of the context.
bff.query.register_query(&journey_id, "phase", |ctx| {
    ctx.variable("phase").unwrap_or(serde_json::json!("UNKNOWN"))
});

// On GET /journeys/{id}: run the named query.
let phase = bff.query.query(&journey_id, "phase")?;

// On completion: drop the run.
bff.query.unregister(&journey_id);
```

What just happened: `register(id, ctx)` enrolls a run by correlation id with its
live `StepContext`. `register_query(id, name, |ctx| value)` attaches a named
projection — a closure that derives a JSON value from the context (here, the
`phase` variable). `query(id, name)` runs that projection and returns
`Result<Value, WorkflowQueryError>` — an unknown run or an unknown query name is a
typed error, which the controller maps to a 404. `unregister(id)` removes the run
when the journey ends; `active()` lists every registered run.

> **Design note.** Two surfaces answer "where is my journey?" and they
> complement each other. `WorkflowState` (Step 5) is the *durable* record — it
> survives a restart and backs the 404-or-phase decision. `WorkflowQueryService`
> is the *live* projection over the in-process run — richer, cheaper to query, and
> the natural place to derive a "next step" DTO while the process is up. A
> production status endpoint reads the live query when the run is in memory and
> falls back to the persisted state otherwise.

> **Tip** **Checkpoint.** `register` a run, `register_query` a `"phase"`
> projection, and `query(id, "phase")` returns the phase value. Querying an
> unregistered id or an unknown query name returns an `Err`, not a panic.

## Step 7 — Assemble the atomic-endpoint controller

Put the pieces together and an experience controller exposes one HTTP request per
journey phase — the **atomic REST** shape.

> **Note** **Key term — atomic endpoint.** An *atomic endpoint* performs exactly
> one phase of a journey and returns. State lives in the cache (Redis-capable)
> between calls, so the client drives the journey one request at a time and can
> resume after a disconnect — instead of holding one long-lived connection open
> across the whole flow.

| Method & path | Does |
|---------------|------|
| `POST /journeys` | start the workflow (calls the `"wallets"` SDK to reserve), persist `WorkflowState`, park on the gate, return the journey id |
| `POST /journeys/:id/data` | deliver the `confirmed` signal — the parked workflow resumes and commits the transfer via the `"wallets"` SDK |
| `GET  /journeys/:id` | report the persisted phase (or 404 if the journey is unknown) |

You build these as ordinary `axum` routes, then run the router through the BFF's
inherited middleware so every response carries the web batteries:

```rust,ignore
use axum::routing::{get, post};
use axum::Router;

// `routes` is an axum Router with the three handlers and the BFF as state.
let routes = Router::new()
    .route("/journeys", post(start_journey))
    .route("/journeys/:id/data", post(deliver_journey_signal))
    .route("/journeys/:id", get(journey_status))
    .with_state(app_state);

// Inherit the web batteries: CORS, security headers, correlation, metrics, …
let api = bff.apply_middleware(routes);
```

What just happened: each phase is one HTTP request, and the workflow state lives
in the cache between them, so a client can resume the journey after a disconnect.
Because the router runs through `bff.apply_middleware(routes)`, every response
inherits the web batteries — the start response carries `X-Frame-Options: DENY`
and an `X-Correlation-Id` just as Lumen's own responses do — and the same
`ExperienceStack::with_security(chain)` filter chain from the
[security chapter](./14-security.md) guards the mutating routes.

This is exactly the contract the crate's own boot test proves end to end. (Its
checkout journey uses an `AWAITING_PAYMENT` phase rather than Lumen's
`AWAITING_CONFIRM`, but the shape is identical.) Two mock domain SDKs are composed
through a signal-gated workflow and driven with `tower::oneshot`: starting the
journey reserves through the first SDK and parks on the gate; the status endpoint
reports the persisted phase; delivering the signal advances the workflow off-task
and ships through the second SDK; and the final status flips to `COMPLETED`.

> **Tip** **Checkpoint.** Driving the router with three calls — `POST /journeys`,
> then `GET /journeys/:id` (reports `AWAITING_CONFIRM`), then `POST
> /journeys/:id/data`, then `GET /journeys/:id` again (reports `COMPLETED`) —
> walks the whole journey. The first response carries the inherited
> `X-Frame-Options: DENY` header.

## Step 8 — See where Lumen fits

Drawn as the full estate, Lumen is one of the domain services a BFF composes. The
channel tier (a web or mobile app) calls the experience tier (`lumen-bff`), which
calls the domain tier (`lumen`), which calls the core tier (`accounts`, which
owns the database). Every arrow points strictly downward, and the experience tier
only ever reaches the domain tier:

```text
  web / mobile app          (channel tier)
        │
        ▼   Experience → Domain only
  lumen-bff                 (experience: DomainClients + signals + state)
        │
        ▼
  lumen                     (domain: ledger, CQRS, transfer saga)
        │
        ▼
  accounts                  (core: owns the database)
```

What just happened: you placed your BFF in the estate. The BFF never reaches into
Lumen's event store or its CQRS bus — it speaks to Lumen's public HTTP API
through the registered `"wallets"` SDK, exactly as any external client would, and
adds only the journey orchestration a frontend needs. Lumen, the domain service,
owns its own logic; the core service below it owns the database.

## Recap — what this chapter built

You did not change Lumen — it is a *domain* service, and this chapter is about the
tier *above* it. What you built is the mental model and the wiring for a
frontend-facing BFF:

- The `channel → experience → domain → core` tier model, and why the experience
  tier composes *domain* SDKs only — never a database, a core service, or a
  sibling BFF.
- An `ExperienceStack` (`Bff`) that inherits Lumen's web batteries and adds five
  building blocks: `clients`, `signals`, `state`, `query`, and `children`.
- `DomainClients` registering Lumen as the `"wallets"` SDK and resolving it by
  logical name, over a `RestClient` with correlation propagation and RFC 9457
  problem decoding.
- A signal-gated `Workflow` whose `Node::wait_for_signal` gate parks the
  "fund and confirm" journey until `signals.deliver(...)` arrives — with buffered
  delivery so there is no lost wakeup.
- `WorkflowState` persisting that journey through a Redis-capable cache adapter
  (a miss is `Ok(None)`, not an error), so a client can resume after a disconnect.
- `WorkflowQueryService` answering "where is my journey?" status polls from the
  live `StepContext`.
- The three-endpoint atomic-REST controller, run through
  `bff.apply_middleware(...)` so every response inherits the web batteries.

Every API here is drawn from the real `firefly-starter-experience` surface — the
same one its boot test exercises end to end.

## Exercises

1. **Register Lumen as a domain SDK.** Build an `ExperienceStack`, call
   `bff.clients.register("wallets", "http://localhost:8080")`, and confirm
   `bff.clients.get("wallets")` returns `Some(_)` and `bff.clients.names()` lists
   `"wallets"`. Then re-register the same name with a different URL and confirm
   `bff.clients.len()` stays `1` (last write wins).
2. **Park and resume.** Build a two-node `Workflow` whose second node is a
   `Node::wait_for_signal` gate. Spawn `workflow.run()` on a task, poll until
   `bff.signals.list_active()` contains the journey id, then
   `bff.signals.deliver(&id, "confirmed", json!({}))` and confirm the workflow
   completes and `list_active()` no longer lists the id.
3. **Race the gate.** Repeat exercise 2 but call `deliver` *before* spawning the
   run. Confirm the workflow still completes — buffered delivery means the signal
   is not lost when it beats the gate.
4. **Persist a journey.** `save` a `StepContext` carrying a `phase` variable via
   `bff.state.save`, `load` it back from a fresh handle, and confirm the variable
   survives. Then `delete` it and confirm `bff.state.load(...)` returns `Ok(None)`.
5. **Atomic endpoints.** Wire the three-route controller from Step 7 with
   `bff.apply_middleware(routes)` and drive it with `tower::oneshot`: start, poll
   status (`AWAITING_CONFIRM`), deliver the signal, poll again (`COMPLETED`).
   Assert the start response carries the inherited `X-Frame-Options: DENY` header
   and an `X-Correlation-Id`.

## Where to go next

- Revisit how a domain service like Lumen is taken to production — real Postgres,
  Kafka, and the management surface — in
  **[Production & Deployment](./20-production.md)**.
- See how the workflow engine the BFF's journey rides on also powers Lumen's own
  compensating transfers in **[Sagas, Workflows & TCC](./12-sagas.md)**.
- With Lumen's place in the tiered estate now clear, the capstone re-reads the
  whole service through the declarative-macro lens. Continue to
  **[Declarative Services with Macros](./21-declarative-macros.md)**.
