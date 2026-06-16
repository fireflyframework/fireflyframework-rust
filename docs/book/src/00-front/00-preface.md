## Preface

Rust gives you fearless concurrency, zero-cost abstractions, and a compiler that refuses to ship a data race. What it does not give you is *cohesion*. Every new back-office service forces the same cascade of decisions before a single line of business logic is written: which HTTP layer, which database story, how to wire dependencies, how to handle configuration, errors, correlation IDs, metrics, and graceful shutdown. **Firefly** changes that. It is an opinionated, convention-over-configuration framework that makes those cross-cutting decisions once, so every service shares one idiom — built from the ground up for Rust 1.88+ on `tokio` and `axum`.

This book teaches Firefly **by example**. You build one real application from an empty crate to a secured, observable, event-sourced service — making every concept concrete before moving to the next. The code in these pages is not illustrative pseudocode: every listing is a slice of a **real project that compiles, boots, and passes its tests** against the framework at version 26.6.x. Each snippet was lifted from the running sample and checked against the crate APIs, so what you read is what actually works. When a listing drifts from the source, the sample's build breaks and a test fails — that is the guarantee behind every listing in this book.

### Who This Book Is For

This book is for intermediate Rust developers comfortable with `async`/`await`, traits, and the basics of HTTP services. You need no prior framework expertise — if you have built anything with `axum`, `actix`, or `sqlx`, you are well prepared.

If you have used an opinionated, batteries-included framework or a reactive-streams library before, Firefly's concepts — beans and stereotypes, declarative messaging, application events, `Mono`/`Flux` — will land quickly. A **Design note** callout appears wherever an idea will feel familiar, so you can lean on what you already know; each one is framed as Firefly's own design choice, not a translation from another framework.

### What You Will Build: Lumen

Every chapter advances **Lumen**, a digital-wallet and ledger service — the worked example this book is built around. Lumen lets a customer open a wallet, deposit and withdraw money, transfer funds between wallets, and read a live balance. Behind that small surface sits the full spread of patterns a real back-office service needs: a value object that does exact money arithmetic, an aggregate that enforces invariants, CQRS with a read-side cache, domain events, an event-sourced ledger, a compensating transfer saga, JWT-secured endpoints, an actuator surface, a scheduled task, and an end-to-end test suite.

The single most important property of Lumen is its dependency list:

```toml
[dependencies]
firefly = { version = "26.6.21" }   # the whole framework — and every macro
axum   = { version = "0.7" }       # you author the handler functions
serde  = { version = "1", features = ["derive"] }
```

**One Firefly dependency.** The entire framework — CQRS, dependency injection, the reactive web stack, event-driven messaging, event sourcing, saga orchestration, scheduling, resilience, security, observability — and every `#[derive(...)]` / `#[...]` macro arrives through `use firefly::prelude::*;`. The chapters make a deliberate point of this: even Lumen's typed error enums hand-write `Display` + `std::error::Error` instead of pulling in `thiserror`, so the one-dependency promise holds end to end.

The journey follows a deliberate arc, one slice of Lumen at a time:

- **Part I — Foundations.** You scaffold the first Lumen service, bind typed configuration and profiles, learn how the composition root wires collaborators, master the `Mono`/`Flux` reactive surface, and expose your first validated REST endpoints.
- **Part II — Modeling & Persisting.** You stand up a read model behind a repository, model the domain with a `Money` value object and a `Wallet` aggregate, and split reads from writes with CQRS command and query handlers dispatched through a bus.
- **Part III — Event-Driven.** The aggregate raises domain events; a `#[event_listener]` projection keeps the read model current; and an **event-sourced ledger** rebuilds every balance by folding its event stream — with the same events ready to flow out to Kafka or RabbitMQ.
- **Part IV — Into Microservices.** Lumen reaches beyond its own process: a typed HTTP client sketch shows how a wallet would call an external payments provider, and an orchestrated **transfer saga** moves money across wallets and *compensates* when the credit leg fails.
- **Part V — Secure · Observe · Ship.** You secure the endpoints with JWT bearer auth and path-based RBAC, make the service observable with metrics, tracing, and an actuator admin surface, add a read-side cache and a scheduled housekeeping task, test the whole stack in-process, and finally ship it behind the `firefly` CLI with graceful shutdown and a reactive streaming endpoint.

By the last page you have a working, tested, observable, secured, event-sourced service — and the mental model to extend it.

### How to Use This Book

**Read sequentially.** Each chapter builds on the one before, and the Lumen codebase grows incrementally; skipping ahead leaves gaps. The **Reactive Model** chapter is the keystone — the whole reactive surface builds on `Mono` and `Flux`, so read it before the service-building chapters. The early chapters (1–5) introduce the framework with small standalone snippets; **Lumen proper begins in Chapter 6** and grows from there. Each chapter is *additive* — it never rewrites what an earlier chapter shipped, only extends it — so the final state is exactly the companion crate.

**Type every listing yourself.** Reading and typing code at the same time is how the patterns stick. Resist copy-pasting until you have written each listing at least once.

**Run it.** Lumen really runs. From the workspace root:

```sh
cargo run   -p firefly-sample-lumen          # boot the service (API + admin)
cargo test  -p firefly-sample-lumen          # run the unit + HTTP test suite
```

Whenever a chapter adds a feature, start the app or the tests and watch it work. Seeing real JSON come back from a real endpoint — and watching a saga *compensate* a failed transfer — is worth a hundred diagrams.

Each chapter closes with a **Recap** of what changed in the Lumen codebase and a set of **Exercises** that push one step further. The exercises are optional but recommended for anything you intend to apply immediately.

### Conventions in Brief

Typographic and structural conventions — code-listing captions, callout types, and design notes — are demonstrated, with live examples, in the **Conventions** section that follows.

### The Companion Code

The complete, runnable Lumen project lives in the framework's `samples/lumen` directory. It is a single, clean Firefly crate — one module per concern (`money`, `domain`, `ledger`, `commands`, `transfer`, `security`, `web`, `housekeeping`) — that you grow chapter by chapter; the finished source there is the destination this book walks you to. Build it once with `cargo build -p firefly-sample-lumen`, and use it to compare your work, catch up if you fall behind, or simply run the parts you are reading about. The chapter-by-chapter map of *what code lands where* lives alongside it in `docs/book/LUMEN-ARC.md`.
