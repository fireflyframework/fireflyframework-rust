# Design — v26.6.3 "Hexagonal + Ergonomics + Book PDF"

Date: 2026-06-13 · Status: approved (autonomous; the user's detailed request is the approval)
Repo: fireflyframework-rust (published, v26.6.2, 66 crates / 69 members, 3742 tests)

## Goal

Make fireflyframework-rust as *pleasant to build services with* as pyfly/Spring Boot, and prove
its hexagonal architecture by making "new technology = new adapter" real (databases first). Plus
ship the book as a polished PDF (and EPUB) like pyfly's.

Three deliverables, all additive (the byte-stable core and existing APIs do not change):

1. **Hexagonal data adapters** — clean, technology-agnostic repository ports in `firefly-data`,
   with real pluggable adapter crates so adding a database is writing one adapter:
   - `firefly-data-sqlx` — relational adapter over `sqlx` covering **Postgres + MySQL + SQLite**
     (feature-gated), implementing the repository ports + specification/pageable/auditing/
     soft-delete compilation (the analog of pyfly's `data/relational/sqlalchemy`).
   - `firefly-data-mongodb` — document-store adapter over the `mongodb` crate behind the SAME
     ports (the analog of pyfly's `data/document/mongodb`).
   Each is real and tested against the genuine engine (Docker MySQL/Mongo; SQLite file/in-mem;
   the existing Dockerised Postgres).

2. **Ergonomic declarative layer** — a `firefly-macros` proc-macro crate giving Rust the
   Spring/pyfly "annotations" experience, plus a `firefly` facade crate with a `prelude` so a
   service author adds one dependency and writes declarative code:
   - CQRS: `#[derive(Command)]`, `#[derive(Query)]` (generate `impl Message`), `#[command_handler]`
     / `#[query_handler]` registration helpers.
   - Web (the headline Spring `@RestController` experience): `#[rest_controller]` on an `impl`
     block + `#[get("/..")]`/`#[post]`/`#[put]`/`#[delete]`/`#[patch]` on methods, generating an
     axum `Router`.
   - Scheduling: `#[scheduled(cron = "..", fixed_rate = .., fixed_delay = .., initial_delay = ..)]`.
   - Domain: `#[derive(AggregateRoot)]`, `#[derive(DomainEvent)]`.
   - DI/container: `#[derive(Component)]` for `firefly-container` registration.
   - Where a decorator maps cleanly (`#[cacheable]`, `#[event_listener]`, `#[saga_step]`), include
     it; where a proc-macro would be unsound or low-value, keep the builder API and document it.
   Every macro generates code against the EXISTING runtime crates — no new runtime semantics.
   The `firefly` facade re-exports the prelude + macros; heavy adapters (mongodb, kafka, …) are
   optional features so a minimal service stays lean.

3. **Book PDF + EPUB** — produce `docs/book/dist/firefly-rust-by-example.pdf` (and `.epub`) from
   the existing mdBook chapters via `pandoc` + `tectonic` (self-contained LaTeX; weasyprint
   fallback). Title page, parts/chapters, TOC, Apache rights line — matching pyfly's book. Add
   `make book-pdf` / `make book-epub` and commit the artifacts (as pyfly commits `book/dist`).

Plus a **fresh pyfly re-analysis** (targeted at hexagonal cleanliness, data/persistence,
decorator/DX parity, and any remaining functional gap) that drives the above and catches anything
still missing.

## Approaches considered

- **Macros**: (a) hand-written `macro_rules!` only — rejected, can't do attribute macros on impl
  blocks / derive with field introspection; (b) full proc-macro crate (chosen) — derive +
  attribute macros, the only way to mirror `@RestController`/`@Command`; (c) no macros, keep
  builders — rejected, that's exactly the DX gap the user is calling out.
- **DB adapters**: (a) bake more DBs into `firefly-data` — rejected, bloats the port crate and
  couples it to every driver; (b) one adapter crate per technology behind shared ports (chosen) —
  the textbook hexagonal layout, mirrors pyfly's `relational/` vs `document/` split, lets a user
  depend only on the DB they use.
- **Book PDF**: (a) port pyfly's weasyprint `build.py` — faithful but heavy to port; (b)
  pandoc + tectonic from the mdBook markdown (chosen) — industry-standard, self-contained, emits
  PDF *and* EPUB, a real typeset book; (c) mdbook-pdf (chromium) — fallback if pandoc is fragile.

## Architecture

- `firefly-data` stays the **port** crate (Repository, ReactiveCrudRepository, Specification,
  Filter, Pageable, Page, Mapper, Auditor/SoftDelete, query parser, transactional contract,
  RowMapper). Its existing in-memory and tokio-postgres reactive repos remain for back-compat.
- New **adapter** crates implement those ports for a technology:
  `firefly-data-sqlx` (relational: pg/mysql/sqlite features) and `firefly-data-mongodb`
  (document). Each: build query from `Filter`/`Specification`/`Pageable`, map rows/docs via
  `RowMapper`/`Mapper`, honor auditing + soft-delete, expose a `transactional` integration. The
  pattern is documented as "implement the firefly-data ports in a `firefly-data-<tech>` crate".
- `firefly-macros` (proc-macro = true) depends only on `syn`/`quote`/`proc-macro2`. It emits code
  referencing the runtime crates by path (e.g. `::firefly_cqrs::Message`); the facade crate
  ensures those paths resolve. Each macro has a paired runtime expectation documented and tested.
- `firefly` facade crate: `pub use` re-exports of the common types into `firefly::prelude`, plus
  `pub use firefly_macros::*`; Cargo features select tiers/adapters (default = web service
  essentials; optional `data-sqlx`, `data-mongodb`, `eda-kafka`, `eda-rabbitmq`, `admin`, …).
- A `samples/macro-quickstart` (or `examples/`) service written the declarative way
  (`#[rest_controller]` + `#[derive(Command)]` + `#[scheduled]`) proves the DX end-to-end and is
  tested via `tower::oneshot`.

## Testing

- Adapters: in-memory/SQLite tests run on a bare machine; Postgres/MySQL/Mongo round-trips are
  env-gated (`FIREFLY_TEST_*_URL`) and run for real against Docker (compose gains `mysql` +
  `mongodb`). The whole CRUD + specification + pageable + auditing + soft-delete contract is
  exercised against each engine and **actually run green** in this milestone.
- Macros: `trybuild` for compile-pass/compile-fail diagnostics + behavioral tests in a consumer
  crate (a `#[rest_controller]` routes correctly via oneshot; `#[derive(Command)]` round-trips on
  the bus; `#[scheduled]` registers; `#[derive(Component)]` resolves from the container).
- Book: `make book-pdf` produces a valid PDF (and EPUB); CI-checkable that the file builds.
- Full gate: `make ci` (fmt + clippy -D warnings + build + test) green; adversarial multi-agent
  review of the macros + adapters with a fix loop.

## Execution waves (each gated on green)

- **A — re-analysis**: targeted pyfly gap workflow (hexagonal/data/DX/misc) → report.
- **B — data adapters**: scaffold `firefly-data-sqlx` + `firefly-data-mongodb`; implement against
  the ports; add mysql+mongodb to docker-compose; run real round-trips.
- **C — macros + facade**: `firefly-macros`, the `firefly` facade/prelude, the macro-quickstart
  sample; trybuild + behavioral tests.
- **D — book PDF/EPUB**: install pandoc+tectonic; build pipeline + `make` targets; generate +
  commit artifacts.
- **E — close any remaining Wave-A gaps**.
- **F — verify + publish**: full gate, adversarial review + fixes, live/real-infra runs, bump to
  **CalVer 26.6.3** (June 2026), push, tag `v26.6.3`, GitHub release; book PDF attached.

Versioning is CalVer YY.MM.Patch — June 2026 → 26.6.3 (not 26.7.x).
