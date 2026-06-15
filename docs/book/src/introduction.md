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
microservices on Rust 1.88+ (tokio + axum) — taught one running service at a
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
firefly = { version = "26.6.5" }
```

Firefly is a production-grade platform for reactive, event-driven Rust
microservices, built natively on tokio and axum. It speaks a stable,
versioned, language-neutral wire contract — RFC 9457 problem documents,
idempotency semantics, event envelopes, saga step definitions — so a Firefly
service interoperates cleanly with any other service that honors the same
contracts, regardless of the stack it runs on. The
[`fireflyframework.org`](https://fireflyframework.org) contracts are the
specification; this book is about building against them in Rust.

## What you build: Lumen

This is a book you read with a terminal open. Rather than tour the framework
feature by feature, you build **Lumen** — a digital-wallet and ledger service —
from an empty crate into a secured, observable, event-sourced microservice.
Lumen is the worked example the whole book is built around — every concept lands
as real code in one service you grow chapter by chapter.

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
saga compensation, and observability on every project. Firefly's ergonomics are
deliberately familiar: an opinionated composition root, declarative macros, and
a `Mono`/`Flux` reactive core. If you have used a batteries-included framework or
a reactive-streams library before, the concepts will land quickly — **Design
note** callouts throughout point out where an idea will feel familiar, framed as
Firefly's own design choices rather than a translation table.

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
- The **Appendices** index every crate, define the framework vocabulary, and
  offer a quick-reference for developers arriving from other ecosystems.

> **Note.** Every code block in this book is real, compiling Rust against the
> actual crate APIs at version 26.6.5. Where a snippet elides setup for brevity
> it is marked `ignore`/`no_run`, but the API names, types, and method
> signatures are exactly what the crates expose.

## Conventions

Inline code such as `Core::new` and `Mono::just` uses monospace. Reference
tables collect related Firefly APIs in one place for quick scanning. Callouts
are blockquotes that open with a bold label:

> **Note.** Supplementary context worth reading.

> **Tip.** A shortcut or idiom that saves you time.

> **Warning.** A sharp edge that causes hard-to-debug problems if ignored.

> **Design note.** Why Firefly makes a particular design choice — and where that
> choice will feel familiar if you have used a comparable framework or a
> reactive-streams library. Offered as orientation, not as a claim that Firefly
> reimplements anything else.

The full set, with live examples and the Recap/Exercises structure every
chapter follows, lives in the [Conventions](./00-front/00-conventions.md) page.

Turn the page to [Why Firefly for Rust](./01-why-firefly.md), where the Lumen
journey begins.
