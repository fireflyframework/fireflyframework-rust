# Design — v26.7 "Reactive + Completeness" milestone

Date: 2026-06-12 · Status: approved (autonomous; the user's detailed request is the approval)
Repo: fireflyframework-rust (published, currently v26.6.1, 67 workspace members, 3355 tests)

## Goal

Make `fireflyframework-rust` the definitive Spring-Boot/Firefly port to Rust: a **genuinely
reactive** (WebFlux/Reactor-style) communication model threaded through the whole ecosystem,
**zero stubs/placeholders/ignored tests** (real implementations, real-infrastructure tests via
Docker), the **canonical Firefly banner**, and **best-in-class documentation** (an mdBook with
tutorials + quickstarts) — all proven working together end-to-end.

Success criteria:
1. A `firefly-reactive` crate provides `Mono<T>`/`Flux<T>` with a faithful Reactor operator set,
   integrated into web (streaming responses + reactive handlers), data (reactive repositories),
   client (a `WebClient`), eda (reactive subscriptions), and cqrs (reactive bus).
2. **No `not_implemented()` / `ERR_NOT_IMPLEMENTED` / stub path remains** — every vendor adapter
   operation calls the real provider API. The only permitted "unsupported" outcomes are precise,
   documented capability errors for operations a provider's real API genuinely cannot perform.
3. Every previously-`#[ignore]`d test runs for real against a Dockerized service (Postgres,
   Redis, RabbitMQ, Kafka, Keycloak, LocalStack/S3, Azurite/Blob, MailHog/SMTP) — and is actually
   executed green in this milestone.
4. An mdBook documentation site (`docs/book/`) with a reactive chapter, per-module guides,
   quickstart, tutorials, CLI guide, and Spring-Boot migration appendix.
5. A full-ecosystem end-to-end reference service wiring web + reactive + cqrs + eda +
   eventsourcing + saga + cache(redis) + data(postgres) + security + observability, with an
   integration test against real infra exercising the full HTTP→command→saga→events→projection
   →query→reactive-stream flow.
6. `make ci` green; adversarial review clean; live smoke + real-infra e2e pass; version 26.7.0
   tagged + released.

## Non-negotiable principle: additive, Go-parity core stays byte-stable

The reactive layer and all completeness work are **additive**. Existing async APIs and wire
formats (RFC 7807 bytes, headers, signatures) do not change. Reactive types wrap the existing
async primitives; they are an ergonomic, Reactor-flavored surface, not a rewrite.

## Component 1 — `firefly-reactive` (the Reactor core)

A new crate. Error type is fixed to `firefly_kernel::FireflyError` (WebFlux models everything as a
Throwable; fixing the error keeps the API ergonomic and RFC-7807-integrated).

- `Mono<T>` — 0-or-1 producer. Internally `Pin<Box<dyn Future<Output = Result<Option<T>,
  FireflyError>> + Send>>` (`Ok(None)` = empty completion, `Ok(Some)` = value, `Err` = error).
  Operators: `map`, `map_async`, `flat_map` (→`Mono`), `flat_map_many` (→`Flux`), `filter`,
  `default_if_empty`, `switch_if_empty`, `then`, `then_return`, `zip_with`, `on_error_return`,
  `on_error_resume`, `on_error_map`, `retry(n)`, `retry_backoff(policy)`, `timeout(d)`,
  `delay_element(d)`, `do_on_next/success/error/finally`, `cache`, `as_flux`, `block`,
  `subscribe`. Factories: `just`, `just_or_empty`, `empty`, `error`, `from_future`, `defer`,
  `from_callable`, `when`, `zip`.
- `Flux<T>` — 0..N producer. Internally `Pin<Box<dyn Stream<Item = Result<T, FireflyError>> +
  Send>>`; an `Err` item is terminal (operators short-circuit). Operators: `map`, `map_async`,
  `flat_map(concurrency)`, `concat_map`, `filter`, `take`, `take_while`, `skip`, `skip_while`,
  `distinct`, `distinct_until_changed`, `scan`, `reduce`→`Mono`, `collect_list`→`Mono<Vec>`,
  `collect_map`, `count`→`Mono`, `all/any`→`Mono`, `then`→`Mono`, `merge_with`, `concat_with`,
  `zip_with`, `combine_latest`, `start_with`, `switch_if_empty`, `default_if_empty`,
  `on_backpressure_buffer/drop/latest`, `buffer(n)`, `window`, `group_by`, `sample(d)`,
  `debounce(d)`, `delay_elements(d)`, `retry`, `retry_backoff`, `timeout`, `on_error_resume`,
  `on_error_continue`, `do_on_*`, `index`, `limit_rate`, `take_last`, `last`/`next`/`single`→
  `Mono`, `element_at`, `flat_map_iterable`, `subscribe`, `to_stream` (escape hatch).
  Factories: `just`, `from_iter`, `from_stream`, `range`, `interval(d)`, `generate(seed,fn)`,
  `create(sink)`, `merge`, `concat`, `zip`, `combine_latest`, `defer`, `empty`, `error`, `never`.
- `Scheduler`: `Immediate`, `Parallel` (tokio spawn), `BoundedElastic` (spawn_blocking);
  `subscribe_on`, `publish_on` (channel hop). Backpressure operators use bounded channels.
- Tests: operator-by-operator unit tests, marble-style sequences, error short-circuit,
  backpressure, scheduler hops, `Send + 'static` bounds, interop with raw `Stream`/`Future`.

## Component 2 — Reactive integration

- **web**: `IntoResponse` for `Mono<T: Serialize>` (→ JSON, empty → 404 problem) and `Flux<T>`
  (→ `application/x-ndjson` streaming **with backpressure**, plus a `Flux`→SSE adapter). Reactive
  request-body streaming. A reactive router-handler ergonomics module.
- **data**: `ReactiveCrudRepository<T, ID>` (`find_all`/`find_by_id`/`save`/`save_all`/
  `delete_by_id`/`count`/`exists_by_id` returning `Flux`/`Mono`) — the Spring Data R2DBC analog —
  with an in-memory impl and a **real Postgres impl** (tokio-postgres, streaming rows as `Flux`).
- **client**: `WebClient` — `client.get().uri(..).header(..).retrieve().body_to_mono::<T>()` /
  `.body_to_flux::<T>()` (NDJSON/SSE streaming), `exchange()` for the raw reactive response, over
  reqwest streaming. The reactive analog of Spring `WebClient`.
- **eda**: `subscribe_reactive(topic) -> Flux<Event>`.
- **cqrs**: `send_mono(cmd) -> Mono<R>`, `query_mono(q) -> Mono<R>`.

## Component 3 — Canonical banner

The banner art (the "firefly" script-figlet) is already canonical. Align styling/behavior to the
Java starter + pyfly: ANSI color (red art, green foundation/license lines, bold) when stdout is a
TTY; `BannerMode` (`Text`/`Minimal`/`Off`) bound from `firefly.banner.mode`; optional
`firefly.banner.location` custom file; metadata block matching Java (`:: Firefly Framework for
Rust ::  (v26.7.0)`, app name/version, `(c) Firefly Software Foundation`, `Licensed under Apache
2.0`, runtime, optional Swagger-UI URL). Lives in `firefly-observability` (existing banner home).

## Component 4 — Zero stubs

Audit the **25 `not_implemented()` call sites** across 12 vendor crates and implement each against
the provider's real API:
- IdP (keycloak/azure-ad/cognito): MFA enrollment/verification, user CRUD, role ops via the real
  admin REST APIs (Keycloak required-actions/credentials, Microsoft Graph, Cognito Admin*).
- ECM storage (s3/azure) + e-signature (docusign/adobe/logalty): the remaining unimplemented
  operations via the real REST APIs.
- Notifications (sendgrid/resend/twilio/firebase): remaining operations via the real APIs.
Where a provider's API genuinely cannot perform an operation, replace the vague sentinel with a
precise, documented `Error::UnsupportedByProvider { provider, operation, reason }` — a typed
capability boundary, not a stub. Remove `ERR_NOT_IMPLEMENTED`/`not_implemented()` helpers once no
real stub remains.

## Component 5 — Real-infrastructure testing (no infra mocks)

- `docker-compose.yml` (+ `Makefile` targets `infra-up`/`infra-down`/`test-integration`) bringing
  up: postgres, redis, rabbitmq, redpanda (Kafka API), keycloak, localstack (S3), azurite (Blob),
  mailhog (SMTP).
- Convert all 24 `#[ignore]` round-trip tests to **env-gated real integration tests** (run when
  the matching `*_URL`/`*_ADDR` env var is set; `cargo test` on a bare machine stays green by
  skipping, but `make test-integration` with infra up runs them for real).
- Actually run the suite against the live compose stack in this milestone and record the results.
- SaaS with no local emulator (SendGrid/Twilio/Firebase/DocuSign/Adobe/Logalty): real
  implementation, contract-tested against a high-fidelity local HTTP double; documented honestly
  as contract-tested (running against production SaaS needs secrets and is out of scope for CI).

## Component 6 — mdBook documentation + tutorials + CLI

- Install mdbook; build `docs/book/` mirroring pyfly's manuscript chapters: why-firefly,
  quickstart, configuration, DI/container, first HTTP API, persistence, DDD, CQRS, EDA, messaging,
  event sourcing, sagas/workflow/TCC, HTTP clients, **reactive programming** (new), security,
  observability, testing, scheduling/notifications, admin, CLI, production, Spring-Boot migration
  appendix, glossary. Each chapter has runnable, tested code (doctested where possible).
- A `getting-started` quickstart that goes from `cargo install firefly-cli` → `firefly new` →
  running service in minutes.
- Polish + document the `firefly` CLI (new/generate/db/openapi/actuator/doctor).

## Component 7 — Full-ecosystem end-to-end sample

A reference service `samples/reactive-banking` (or extend `samples/orders`) wiring web + reactive
+ cqrs + eda(kafka) + eventsourcing + saga + cache(redis) + data(postgres reactive repo) +
security(JWT) + observability + actuator. An integration test against the compose stack drives the
full flow: authenticated HTTP command → CQRS handler → saga with compensation → domain events to
Kafka → projection → reactive streaming query endpoint (Flux→NDJSON) → `WebClient` consuming it.

## Execution plan (waves, each gated on build+test+clippy green)

- **W1 — Reactive core + banner**: `firefly-reactive` crate; canonical banner alignment.
- **W2 — Reactive integration**: web/data/client/eda/cqrs reactive surfaces (parallel, each
  additive).
- **W3 — Zero stubs**: implement all 25 call sites for real (parallel per crate).
- **W4 — Real-infra tests**: docker-compose + convert/run the 24 ignored tests for real.
- **W5 — Docs + CLI + e2e sample**: mdBook, tutorials, quickstart, CLI polish, the full-ecosystem
  sample + e2e test against real infra.
- **W6 — Verify + publish**: full gate, adversarial review + fix loop, live smoke + real-infra
  e2e, docs build, bump to 26.7.0, push + tag + GitHub release.

Each wave runs as a Workflow of crate-scoped agents; gaps and bugs are caught by adversarial
multi-agent review with 3-lens verification, mirroring the prior milestones.
