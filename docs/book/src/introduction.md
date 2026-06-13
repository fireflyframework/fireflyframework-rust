# Firefly Framework for Rust

```text
  _____.__                _____.__
_/ ____\__|______   _____/ ____\  | ___.__.
\   __\|  \_  __ \_/ __ \   __\|  |<   |  |
 |  |  |  ||  | \/\  ___/|  |  |  |_\___  |
 |__|  |__||__|    \___  >__|  |____/ ____|
                       \/           \/   rs
```

**A production-grade platform for building reactive, event-driven, resilient
microservices on Rust 1.85+ (tokio + axum) — taught one running service at a
time.**

The Firefly Framework provides the cross-cutting machinery that every
non-trivial business service needs — RFC 9457 error envelopes, idempotency,
correlation propagation, CQRS, event-driven messaging, event sourcing, sagas,
configuration, identity adapters, notifications, scheduling, observability —
behind a single, opinionated composition pattern. You write commands, queries,
handlers, and routes; the framework wires the rest. And you depend on exactly
**one** crate to get all of it:

```toml
[dependencies]
firefly = { version = "26.6.3" }
```

This is the official Rust port of the Java/Spring Boot
[`org.fireflyframework`](https://fireflyframework.org) platform — the fourth
sibling port, joining the .NET, Go, and Python (PyFly) ports. A service running
version *X* on Java, .NET, Go, Python, or Rust consumes the same contracts and
emits the same wire format.

## What you build: Lumen

This is a book you read with a terminal open. Rather than tour the framework
feature by feature, you build **Lumen** — a digital-wallet and ledger service —
from an empty crate into a secured, observable, event-sourced microservice. The
same Lumen powers the PyFly, Go, and .NET books, so the four ports stay in step.

Lumen's customer-facing surface is small and concrete:

| Method & path | What it does |
|---|---|
| `POST /api/v1/wallets` | open a wallet |
| `GET  /api/v1/wallets/:id` | read a balance (cached 30s) |
| `POST /api/v1/wallets/:id/deposit` | credit a wallet |
| `POST /api/v1/wallets/:id/withdraw` | debit a wallet |
| `POST /api/v1/transfers` | move money between wallets (a saga) |
| `GET  /actuator/*` | health, info, metrics, loggers |

Behind that surface sits the full spread of patterns a real service needs: a
`Money` value object that does exact integer-cent arithmetic, a `Wallet`
aggregate that enforces invariants, CQRS with a read-side cache, domain events,
an event-sourced ledger that rebuilds balances by folding its stream, a
compensating transfer saga, JWT bearer auth with path-based RBAC, an actuator
admin surface, a scheduled housekeeping task, and an end-to-end test suite.

Every code block in the book is a slice of the real, compiling, tested
[`samples/lumen`](https://github.com/fireflyframework) crate. When the prose
drifts from the source, the sample stops building and a test fails — so what you
read is what runs.

## Who this book is for

You are comfortable with Rust and `async`/`await`, and you want to ship
back-office services without re-litigating error envelopes, correlation IDs,
saga compensation, and observability on every project. If you come from Spring
Boot, WebFlux, or Project Reactor, you will feel at home immediately — there are
**Spring parity** and **Reactor parity** notes throughout that save you the
mental translation.

## How to read this book

The book is built to be read front-to-back the first time and used as a
reference afterward.

- **[Why Firefly for Rust](./01-why-firefly.md)** frames the problem and the
  one-dependency facade that Lumen embodies; **[Quickstart](./02-quickstart.md)**
  gets the Lumen scaffold booting in minutes.
- **[The Reactive Model](./05-reactive-model.md)** is the keystone chapter. The
  whole reactive surface — reactive endpoints, repositories, the HTTP client,
  reactive EDA and CQRS — builds on `Mono` and `Flux`, so read it before the
  service-building chapters.
- The early chapters (1–5) introduce the framework with small standalone
  snippets; **Lumen proper begins in [Chapter 6](./06-first-http-api.md)** and
  grows additively from there. Each later chapter takes one concern — HTTP,
  persistence, DDD, CQRS, EDA, event sourcing, sagas, clients, security,
  observability, scheduling, caching — and lands the real Lumen code for it.
- The **Shipping** chapters cover testing, the `firefly` CLI, and production
  deployment with graceful shutdown and a reactive streaming endpoint.
- The **Appendices** map Spring Boot concepts to Firefly, index every crate, and
  define the vocabulary.

> **Note.** Every code block in this book is real, compiling Rust against the
> actual crate APIs at version 26.6.3. Where a snippet elides setup for brevity
> it is marked `ignore`/`no_run`, but the API names, types, and method
> signatures are exactly what the crates expose.

## Conventions

Inline code such as `Core::new` and `Mono::just` uses monospace. Mapping tables
line up a familiar concept (Reactor, Spring) against its Firefly spelling.
Callouts are blockquotes that open with a bold label:

> **Note.** Supplementary context worth reading.

> **Tip.** A shortcut or idiom that saves you time.

> **Warning.** A sharp edge that causes hard-to-debug problems if ignored.

> **Spring parity.** Where a Firefly concept maps directly onto something you
> already know from Spring Boot / WebFlux / Firefly-Java.

> **Reactor parity.** Where Firefly's `Mono`/`Flux` maps onto Project Reactor.

The full set, with live examples and the Recap/Exercises structure every
chapter follows, lives in the [Conventions](./00-front/00-conventions.md) page.

Turn the page to [Why Firefly for Rust](./01-why-firefly.md), where the Lumen
journey begins.
