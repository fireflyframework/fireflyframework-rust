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
microservices on Rust 1.85+ (tokio + axum).**

The Firefly Framework provides the cross-cutting machinery that every
non-trivial business service needs — RFC 7807 error envelopes, idempotency,
correlation propagation, CQRS, event-driven messaging, event sourcing, sagas,
configuration servers, identity adapters, document management, notifications,
callbacks, webhooks — behind a single, opinionated composition pattern. You
write commands, queries, handlers, and routes; the framework wires the rest.

This is the official Rust port of the Java/Spring Boot
[`org.fireflyframework`](https://fireflyframework.org) platform — the fourth
sibling port, joining the .NET, Go, and Python (PyFly) ports. A service running
version *X* on Java, .NET, Go, Python, or Rust consumes the same contracts and
emits the same wire format.

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

- **[Why Firefly for Rust](./01-why-firefly.md)** and
  **[Quickstart](./02-quickstart.md)** get you from zero to a running reactive
  endpoint in minutes.
- **[The Reactive Model](./05-reactive-model.md)** is the keystone chapter. The
  whole reactive surface — reactive endpoints, repositories, the `WebClient`,
  reactive EDA and CQRS — builds on `Mono` and `Flux`, so read it before the
  service-building chapters.
- The **Building Services** chapters each take one concern (HTTP, persistence,
  DDD, CQRS, EDA, event sourcing, sagas, clients, security, observability,
  scheduling, caching) and show the real crate APIs end-to-end.
- The **Shipping** chapters cover testing against real infrastructure, the
  `firefly` CLI, and production deployment.
- The **Appendices** map Spring Boot concepts to Firefly, index every crate, and
  define the vocabulary.

> **Note** — Every code block in this book is real, compiling Rust against the
> actual crate APIs at version 26.6.3. Where a snippet elides setup for brevity
> it is marked, but the API names, types, and method signatures are exactly what
> the crates expose.

## Conventions

Inline code such as `Core::new` and `Mono::just` uses monospace. Mapping tables
line up a familiar concept (Reactor, Spring, Spring Data) against its Firefly
spelling. Callouts come in four flavours:

> **Note** — supplementary context worth reading.

> **Tip** — a shortcut or idiom that saves you time.

> **Warning** — a sharp edge that causes hard-to-debug problems if ignored.

> **Spring parity** — where a Firefly concept maps directly onto something you
> already know from Spring Boot / WebFlux.

Turn the page to [Why Firefly for Rust](./01-why-firefly.md).
