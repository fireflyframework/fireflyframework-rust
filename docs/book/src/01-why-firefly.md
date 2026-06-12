# Why Firefly for Rust

By the end of this chapter you will understand the problem Firefly solves, how
its four tiers fit together, and why a single `Core::new` call replaces the two
weeks of architectural decisions that usually precede your first handler.

## The cohesion problem

Picture your first day on a new Rust microservice. Before you write one line of
business logic, you face a cascade of choices. Which HTTP layer — axum, actix,
warp, poem? Which database story — sqlx, SeaORM, diesel, raw `tokio-postgres`?
How do you wire dependencies — a hand-rolled `AppState`, a DI crate, lazy
statics? How do you handle configuration, errors, correlation IDs, metrics,
graceful shutdown? Every team invents its own answer.

You assemble a bespoke stack, glue it together with good intentions, and ship.
Six months later a second team starts a second service and makes entirely
different choices. Now you have two codebases with incompatible conventions,
different error shapes, different observability stories, and no shared
understanding of how anything works.

**Rust gives you infinite choice. What it does not give you is cohesion.**

The stack-assembly problem is not a skills failure — it is a tooling gap. Java
developers solved it with Spring Boot: one opinionated framework that makes
sensible choices, lets you override what matters, and enforces a consistent
idiom across every service. Firefly brings that same discipline to Rust.

## What is Firefly?

Firefly is a **cohesive, reactive, async-native framework** for building
production-grade Rust services. It makes the cross-cutting decisions for you —
HTTP middleware, configuration, caching, CQRS, messaging, security,
observability — all integrated, all consistent, with production-ready defaults
from the very first `cargo run`.

Under the hood Firefly delegates to battle-tested libraries — `tokio` for the
runtime, `axum`/`tower` for HTTP, `serde` for serialization, `tracing` for
logging, RustCrypto for crypto — but you depend on **Firefly's ports** (object-
safe `async_trait` traits), and you select concrete adapters at wiring time.
Swap PostgreSQL for an in-memory store, or Kafka for RabbitMQ, without touching
a single line of business logic.

Firefly's defining principles:

- **Composed, not constructed.** A single `Core::new(CoreConfig { .. })` call
  wires the whole infrastructure tier — middleware chain, cache, CQRS bus, event
  broker, health composite, metrics, scheduler, lifecycle. You write commands,
  queries, handlers, and routes; nothing more.
- **Symmetric across runtimes.** The wire contract, the
  `application/problem+json` shape, the `Idempotency-Key` semantics, the saga
  step definitions, the event envelopes, the HMAC webhook signatures — all
  identical to the Java, .NET, Go, and Python siblings.
- **Pluggable at the adapter layer.** Each integration point (cache, broker,
  IDP, ECM, notification channel) is an object-safe port with multiple adapter
  implementations selected at wiring time as an `Arc<dyn Port>`.
- **Observable by default.** `tracing` structured logging with correlation-id
  enrichment, actuator health/metrics endpoints, RFC 7807 error envelopes, and a
  startup banner are all on out of the box.
- **Reactive to the core.** A first-class `Mono`/`Flux` reactive surface — the
  Rust analog of Project Reactor — runs from reactive endpoints through reactive
  repositories, the reactive `WebClient`, and reactive EDA/CQRS.

> **Spring parity** — If you come from Spring Boot, `Core::new` is your
> `@SpringBootApplication` auto-configuration. The `firefly.yaml` configuration
> hierarchy (defaults → profile → env vars) maps directly to `application.yaml`
> + profiles. Returning a `Mono<T>` / `Flux<T>` from a handler is exactly the
> WebFlux `@RestController` model. A **Spring parity** callout appears wherever
> the concepts align closely enough to save you the translation.

## The four tiers

The framework is organised into four strictly-layered tiers, with a
left-to-right dependency direction. Each tier may depend on the tiers to its
left, never to its right; the Cargo crate graph enforces the layering.

```text
┌──────────────┐   ┌────────────────┐   ┌──────────────┐   ┌──────────────────────┐
│ FOUNDATIONAL │ → │    PLATFORM    │ → │   ADAPTERS   │ → │       STARTERS       │
│              │   │                │   │              │   │                      │
│  kernel      │   │  cache         │   │  client      │   │  starter-core        │
│  utils       │   │  observability │   │  idp-*       │   │  starter-application │
│  validators  │   │  data          │   │  ecm-*       │   │  starter-domain      │
│  web         │   │  cqrs          │   │  notif.-*    │   │  starter-data        │
│  config      │   │  eda · eda-*   │   │  callbacks   │   │  backoffice          │
│  i18n        │   │  eventsourcing │   │  webhooks    │   │                      │
│  session     │   │  orchestration │   │  config-srv  │   │  admin               │
│  reactive    │   │  resilience    │   │  cache-redis │   │  cli                 │
│              │   │  security · …  │   │  notif.-smtp │   │                      │
└──────────────┘   └────────────────┘   └──────────────┘   └──────────────────────┘
```

- **Foundational** crates are the vocabulary: `firefly-kernel` (errors, clock,
  correlation scopes, DDD kit), `firefly-reactive` (Mono/Flux), `firefly-web`
  (middleware), `firefly-config`, `firefly-validators`, `firefly-i18n`.
- **Platform** crates are the capabilities: caching, CQRS, EDA, event sourcing,
  orchestration, scheduling, resilience, security, observability, migrations,
  SSE, WebSockets.
- **Adapters** are the concrete integrations: the REST/reactive `WebClient`, the
  IDP vendors (Keycloak, Azure AD, Cognito), ECM (S3, Blob, e-sign),
  notifications (SMTP, Twilio, Firebase), and the event transports (Kafka,
  RabbitMQ, Postgres outbox, Redis Streams).
- **Starters** bundle a sensible default stack so a service depends on one crate.
  `firefly-starter-core` is the one most services begin with.

For the full per-crate catalogue see the [Module Index](./91-appendix-modules.md).

## Choosing your tier and adapters

Start from a **starter** and add only the adapters you need.

- **Default, zero infrastructure.** `firefly-starter-core` boots with the
  in-process `MemoryAdapter` cache and `InMemoryBroker` event bus. Nothing
  external is required — a service runs against pure-Rust defaults.
- **Pick a cache backend.** Drop in `firefly-cache-redis` (`RedisAdapter`)
  wherever an `Arc<dyn cache::Adapter>` is expected.
- **Pick an event transport.** `firefly-eda-kafka`, `-rabbitmq`, `-postgres`
  (durable outbox), or `-redis` (Streams) each implement the same `Broker` port;
  swap the constructor, keep your handlers.
- **Pick vendors.** Code against the parent-port trait (`notifications::Channel`,
  `idp::Adapter`, `ecm::ContentStore`) and pull in the concrete adapter crate at
  wiring time, so heavy SDKs stay out of services that do not use them.

## What you will build

Throughout the book the running examples revolve around an **Orders** service
and a small **banking ledger**: a REST API that records orders, a reactive
endpoint that streams them, CQRS handlers that mutate state, domain events that
fan out over a broker, a saga that coordinates a transfer, and the
observability, security, and testing that make it production-ready. Starting
from a well-structured skeleton saves you the pain of retrofitting architecture
later — and the [`firefly` CLI](./19-cli.md) scaffolds that skeleton for you.

The next chapter gets you running. Turn to the [Quickstart](./02-quickstart.md).
