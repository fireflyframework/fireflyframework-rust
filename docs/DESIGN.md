# Firefly Framework for Rust — Design

Date: 2026-06-12 · Status: founding design record (historical / unmaintained)

> **Historical / superseded.** This is the original founding design record,
> capturing the decisions made when the framework was first scoped. Firefly has
> since been substantially expanded — a full DI container, AOP, sessions, shell,
> websockets, admin dashboard, real vendor integrations, and a reactive core
> (`Mono`/`Flux`). The vendor adapters described below as "Stubs" are now **real
> integrations**, and the workspace has 72 crates, not 52 — including the
> ergonomic `firefly` facade + `firefly-macros` declarative layer and the
> hexagonal database adapters (`firefly-data-sqlx`, `firefly-data-mongodb`). This
> document is no longer maintained; for the current design and scope see
> [`ARCHITECTURE.md`](ARCHITECTURE.md), [`MODULES.md`](../MODULES.md),
> [`CHANGELOG.md`](../CHANGELOG.md), the
> [reactive-completeness spec](superpowers/specs/2026-06-12-reactive-completeness-design.md),
> and the [mdBook](book/src/SUMMARY.md).

## Goal

`fireflyframework-rust` is a cohesive, reactive, async-native framework for building
production-grade Rust services, organized as a ports-and-adapters workspace (stdlib-first where
practical, mature crates where not). It is a standalone framework built natively on Rust +
tokio + axum.

Success criteria:

1. Every tier crate exposes a complete, idiomatic public surface with strong test coverage.
2. `cargo build --workspace`, `cargo test --workspace`, and `cargo clippy --workspace` are green.
3. The wire contracts are stable and well-defined: RFC 7807 `application/problem+json`
   shape, correlation headers, HMAC webhook signatures, config-server response shape,
   `V###__name.sql` migration naming.

## Approaches considered

1. **std-only, thread-per-connection.** Rejected: Rust's std has no HTTP server, JSON, or async
   primitives; hand-rolling them adds risk and maintenance burden for no real benefit.
2. **Tokio + minimal hand-rolled HTTP.** Rejected: same JSON problem, and axum is the de-facto
   standard for async HTTP middleware with a far richer ecosystem.
3. **Tokio + axum + serde ecosystem (chosen).** A reactive, async-first core is the natural fit
   for a high-throughput service framework. Mature, widely known crates only.

## Architecture

> **Note:** the crate counts and version in this section are frozen at the original 52-member
> milestone. The current workspace has 72 crates at `26.6.4`; see
> [`ARCHITECTURE.md`](ARCHITECTURE.md) and [`MODULES.md`](../MODULES.md) for the live layout.

- **Single Cargo workspace** at the repo root; one crate per module: 50 crates under
  `crates/`, plus `tests/integration` (cross-module suite) and `samples/orders` = 52 members.
- **Crate naming**: `firefly-<module>` with hyphenation
  (`firefly-idp-aws-cognito`, `firefly-ecm-storage-aws`, `firefly-starter-core`, …).
- **Version**: `26.6.2` — CalVer (`YY.MM.Patch`) expressed as valid semver, set to the
  June 2026 release window. Edition 2021, MSRV 1.88.
- **Dependency policy**: all external deps are declared once in `[workspace.dependencies]`;
  member crates only reference `{ workspace = true }`. Core stack: tokio, axum 0.7, tower,
  serde/serde_json/serde_yaml, thiserror, async-trait, uuid, chrono, reqwest; crypto via
  RustCrypto (sha2/hmac/aes-gcm), bcrypt, jsonwebtoken; rusqlite (bundled, dev-only) backs the
  `transactional`/`migrations` tests.
- **Ports and adapters**: provider modules (IdP, ECM, notifications) define `async_trait`
  object-safe ports; in-tree reference implementations are Full (internal-db IdP, ECM
  LocalStore, memory notification channel), and vendor adapters are typed configuration +
  request-shaping integrations.
- **Context propagation**: ambient values (correlation, tenant, transaction) ride tokio
  `task_local!` scopes plus explicit handle types, whichever reads better per module; HTTP
  propagation stays header-based (`X-Correlation-Id`).
- **Error model**: `thiserror`-derived `FireflyError` hierarchy in `firefly-kernel`;
  `ProblemDetail` serializes RFC 7807 JSON with flattened extension members.
- **Generics over reflection**: `Result<T>`, the `Typed<T>` cache, `Page<T>`, and
  `Repository<T, K>` are expressed with Rust generics; CQRS/EDA buses use `TypeId`-keyed
  registries of boxed handlers.

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

Each wave is implemented by parallel agents (one per crate) and gated on `cargo build` +
`cargo test` green before the next wave.

## Testing

- Unit tests cover each crate's public surface, including Rust-specific cases (Send/Sync
  bounds, serde round-trips).
- HTTP surfaces tested in-process via `tower::ServiceExt::oneshot` — no sockets needed.
- `tests/integration` exercises the cross-crate suite: CQRS roundtrip, callbacks dispatch
  with HMAC verification by webhooks, saga compensation, starter-core boot.
- Final gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo build --workspace`, `cargo test --workspace`.

## Documentation

Documentation set: top-level `README.md`, `MODULES.md` index, per-crate `README.md` with
public-surface description and runnable quick-start, `docs/` (ARCHITECTURE/CONFIGURATION),
the mdBook, `CHANGELOG.md`, and the `Makefile` (`build`, `test`, `clippy`, `fmt-check`, `ci`).
