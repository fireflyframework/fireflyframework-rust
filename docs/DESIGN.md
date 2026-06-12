# Firefly Framework for Rust — Design

Date: 2026-06-12 · Status: approved (autonomous port, mirrors prior-port precedent)

## Goal

`fireflyframework-rust` is the fourth sibling port of the Firefly Framework, joining
`fireflyframework-pyfly` (Python), `fireflyframework-go` (Go), and `fireflyframework-dotnet`
(.NET). It ports the in-scope Java framework with **full module parity against the Go port**,
which is the canonical compiled-language reference (52 workspace members, stdlib-first,
ports-and-adapters).

Success criteria:

1. Every Go module has a Rust counterpart crate with the same public surface (adapted to Rust
   idiom) and equal-or-better test coverage.
2. `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace` are green.
3. Wire-level compatibility where the other ports promise it: RFC 7807 `application/problem+json`
   shape, correlation headers, HMAC webhook signatures, Spring-Cloud-Config response shape,
   `V###__name.sql` migration naming.

## Approaches considered

1. **std-only, thread-per-connection (mirror Go's stdlib purism).** Rejected: Rust's std has no
   HTTP server, JSON, or async primitives; hand-rolling them adds risk without parity value.
2. **Tokio + minimal hand-rolled HTTP.** Rejected: same JSON problem; axum is the de-facto
   standard and maps 1:1 onto Go's `net/http` middleware idioms.
3. **Tokio + axum + serde ecosystem (chosen).** The Java framework is reactive (Reactor);
   async Rust is its natural analog. Mature, widely known crates only.

## Architecture

- **Single Cargo workspace** at the repo root; one crate per Go module: 50 crates under
  `crates/`, plus `tests/integration` (cross-module suite) and `samples/orders` = 52 members,
  matching `go.work`.
- **Crate naming**: `firefly-<module>` with hyphenation following the Java repo names
  (`firefly-idp-aws-cognito`, `firefly-ecm-storage-aws`, `firefly-starter-core`, …).
- **Version**: `26.6.1` — the Go port's CalVer scheme (`v26.05.01`) expressed as valid semver,
  bumped to the June 2026 release window. Edition 2021, MSRV 1.85.
- **Dependency policy**: all external deps are declared once in `[workspace.dependencies]`;
  member crates only reference `{ workspace = true }`. Core stack: tokio, axum 0.7, tower,
  serde/serde_json/serde_yaml, thiserror, async-trait, uuid, chrono, reqwest; crypto via
  RustCrypto (sha2/hmac/aes-gcm), bcrypt, jsonwebtoken; rusqlite (bundled, dev-only) plays the
  role Go gave `modernc.org/sqlite` in `transactional`/`migrations` tests.
- **Ports and adapters**: provider modules (IdP, ECM, notifications) define `async_trait`
  object-safe ports; in-tree reference implementations are Full (internal-db IdP, ECM
  LocalStore, memory notification channel), vendor adapters are typed configuration +
  request-shaping Stubs, exactly as in the Go port.
- **Context propagation**: Go's `context.Context` values (correlation, tenant, transaction)
  become tokio `task_local!` scopes plus explicit handle types, whichever reads better per
  module; HTTP propagation stays header-based (`X-Correlation-Id`).
- **Error model**: `thiserror`-derived `FireflyError` hierarchy in `firefly-kernel`;
  `ProblemDetail` serializes RFC 7807 JSON with flattened extension members, wire-compatible
  with the Java/.NET/Go/Python ports.
- **Generics over reflection**: Go's `Result[T]`, `Typed[T]` cache, `Page[T]`, `Repository[T,K]`
  map to Rust generics; CQRS/EDA buses use `TypeId`-keyed registries of boxed handlers.

## Module dependency waves (build order)

- **Wave 1 — zero internal deps (26)**: kernel, utils, validators, config, i18n, cache, data,
  cqrs, eventsourcing, orchestration, rule-engine, plugins, lifecycle, actuator, scheduling,
  resilience, security, migrations, openapi, sse, transactional, testkit, config-server, idp,
  ecm, notifications.
- **Wave 2 — kernel-dependent (4)**: web, observability, eda, client.
- **Wave 3 — adapters + aggregate (16)**: callbacks, webhooks (→ client); idp-internal-db,
  idp-keycloak, idp-azure-ad, idp-aws-cognito (→ idp); ecm-storage-aws, ecm-storage-azure,
  ecm-esignature-docusign, ecm-esignature-adobe-sign, ecm-esignature-logalty (→ ecm);
  notifications-sendgrid, notifications-resend, notifications-twilio, notifications-firebase
  (→ notifications); starter-core (→ wave-2 set).
- **Wave 4 — composition (6)**: starter-application, starter-domain, starter-data, backoffice,
  tests/integration, samples/orders.

Each wave is implemented by parallel agents (one per crate, each porting from its Go module's
source + tests + README) and gated on `cargo build` + `cargo test` green before the next wave.

## Testing

- Unit tests ported 1:1 from each Go module's `*_test.go`, plus Rust-specific cases
  (Send/Sync bounds, serde round-trips).
- HTTP surfaces tested in-process via `tower::ServiceExt::oneshot` — no sockets needed.
- `tests/integration` reproduces the Go cross-module suite: CQRS roundtrip, callbacks dispatch
  with HMAC verification by webhooks, saga compensation, starter-core boot.
- Final gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo build --workspace`, `cargo test --workspace`.

## Documentation

Mirrors the Go port: top-level `README.md`, `MODULES.md` index, per-crate `README.md` with
public-surface description and runnable quick-start, `docs/`
(ARCHITECTURE/CONFIGURATION/MIGRATION-GUIDE), `CHANGELOG.md`, `Makefile` (`build`, `test`,
`clippy`, `fmt-check`, `ci`).
