## Preface

Rust gives you fearless concurrency, zero-cost abstractions, and a compiler that refuses to ship a data race. What it does not give you is *cohesion*. Every new back-office service forces the same cascade of decisions before a single line of business logic is written: which HTTP layer, which database story, how to wire dependencies, how to handle configuration, errors, correlation IDs, metrics, and graceful shutdown. **Firefly** changes that. It brings the cohesive, convention-over-configuration experience that Spring Boot gave the JVM world, rebuilt from the ground up for Rust 1.85+ on `tokio` and `axum`.

This book teaches Firefly **by doing**. You build one real application from an empty crate to a secured, observable, event-driven service — making every concept concrete before moving to the next. The code in these pages is not illustrative pseudocode: every listing was written against the real crate APIs and compiles against the framework at version 26.6.x. What you read is what actually works.

### Who This Book Is For

This book is for intermediate Rust developers comfortable with `async`/`await`, traits, and the basics of HTTP services. You need no prior framework expertise — if you have built anything with `axum`, `actix`, or `sqlx`, you are well prepared.

Spring Boot, WebFlux, and Project Reactor developers will feel especially at home. Wherever Firefly mirrors a concept you already know — beans, stereotypes, declarative transactions, application events, `Mono`/`Flux` — a **Spring parity** or **Reactor parity** callout draws the parallel explicitly, so you map what you already know rather than learning from zero.

### What You Will Build

Every chapter advances **Lumen**, a digital-wallet and ledger service. The journey follows a deliberate arc, one part at a time:

- **Part I — Foundations.** You scaffold the first Lumen service, wire Firefly's dependency-injection container, bind typed configuration and profiles, master the `Mono`/`Flux` reactive surface, and expose your first validated REST endpoints.
- **Part II — Modeling & Persisting.** You persist wallets through reactive repositories, model the domain with a `Money` value object and a `Wallet` aggregate, and split reads from writes with CQRS command and query handlers dispatched through a bus.
- **Part III — Event-Driven.** The aggregate raises domain events; a listener projects them; an **event-sourced ledger** rebuilds every balance by replaying its event stream; and the same events flow out to Kafka or RabbitMQ for other services.
- **Part IV — Into Microservices.** Lumen reaches beyond its own process: a typed HTTP client calls an external service, a thin experience-tier BFF composes downstream calls, and an orchestrated **transfer saga** moves money across wallets and *compensates* when a step fails.
- **Part V — Secure · Observe · Ship.** You secure the endpoints, make the service observable with metrics, tracing, and health checks, add caching, scheduling, and notifications, test the whole stack against real infrastructure, and finally ship it to production behind the `firefly` CLI.

By the last page you have a working, tested, observable, secured service — and the mental model to extend it.

### How to Use This Book

**Read sequentially.** Each chapter builds on the one before, and the Lumen codebase grows incrementally; skipping ahead leaves gaps. The **Reactive Model** chapter is the keystone — the whole reactive surface builds on `Mono` and `Flux`, so read it before the service-building chapters.

**Type every listing yourself.** Reading and typing code at the same time is how the patterns stick. Resist copy-pasting until you have written each listing at least once.

**Run it.** Lumen really runs — `cargo run` boots the service and `cargo test` exercises it. Whenever a chapter adds a feature, start the app or the tests and watch it work. Seeing real JSON come back from a real endpoint is worth a hundred diagrams.

### Conventions in Brief

Typographic and structural conventions — code-listing captions, callout types, and figure numbering — are demonstrated, with live examples, in the **Conventions** section that follows.

### The Companion Code

The complete, runnable Lumen project lives in the framework's `examples/lumen` directory. It is a single, layered Firefly workspace that you grow chapter by chapter; the finished source there is the destination this book walks you to. Build it once with `cargo build`, and use it to compare your work, catch up if you fall behind, or simply run the parts you are reading about.
