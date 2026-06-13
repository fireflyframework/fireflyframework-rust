# Design — v26.6.4 "Full Parity · Best-in-class DI · Tiers · Book"

Date: 2026-06-13 · Status: approved (autonomous; the user's detailed request is the approval)
Repo: fireflyframework-rust (published, v26.6.3, 72 crates / 76 members, 3936 tests)

## Goal

Four thrusts, all additive (byte-stable core + existing APIs unchanged):

### A. Exhaustive pyfly parity — miss nothing
A symbol-level audit of **all 41 pyfly packages** against the Rust workspace, then close every remaining
gap. Explicitly verify the **admin dashboard** is complete vs pyfly's (every view, `/admin/api/*`
endpoint, SSE stream, instance/client mode). Close the remaining macro tiers and any misc deltas the
sweep surfaces.

### B. Tiered starters parity (core · domain · experience · data · web · application)
Firefly (Java) and the firefly skills define a **three-tier service model — core, domain, experience
(BFF)**. The Rust port has starter-core/application/domain/data/web/backoffice (matching pyfly's five
starters) but **no experience/BFF tier**. Add `firefly-starter-experience` — the BFF tier: signal-driven
`@Workflow`-style composition over domain SDKs (firefly-client), Redis-backed workflow state, atomic REST
endpoints — mirroring the Java/skill `generate-execution-plan-experience` contract. Document the
core/domain/experience tier model in MODULES.md + the book.

### C. Best-in-class Dependency Injection (Spring/pyfly parity)
`firefly-container` is a solid container (scopes Singleton/Prototype/Request/Session/Refresh/Custom,
`bind` trait-object, `Provider`, `resolve_all`, fuzzy `NoSuchBean`). The *ergonomics* are the gap — only
`#[derive(Component)]` exists. Bring the DI developer experience to full parity via `firefly-macros` +
`firefly-container`:
- **Stereotype derives**: `#[derive(Component)]` (have), `#[derive(Service)]`, `#[derive(Repository)]`,
  `#[derive(Configuration)]` (aliases carrying a default scope/role).
- **`#[bean]`** factory methods on a `Configuration` type → `register_factory` keyed on the return type.
- **Field autowiring**: `#[autowired]` with `#[qualifier("name")]`, `#[primary]`, `#[order(n)]`,
  `#[lazy]`, and `Provider<T>` / `Vec<T>` (resolve_all) injection — the derive generates the
  constructor-from-container.
- **Auto-bind interfaces**: `#[component(provides = "dyn SomePort")]` also `bind`s the trait object
  (pyfly's `_auto_bind_interfaces`).
- **Lifecycle**: `#[post_construct]` / `#[pre_destroy]` hooks run by the container.
- **Conditionals & profiles** (Java parity): `#[conditional(on_property = "k=v", on_missing_bean =
  "T")]`, `#[profile("prod")]` — evaluated at scan/registration time against `firefly-config`.
- **`#[value("config.key", default = "..")]`** field injection from config.
- **Component scanning — the centerpiece**: `#[derive(Component)]`/stereotypes emit an
  `inventory::submit!` registration thunk; `Container::scan()` (and `firefly::scan(&container)`) collects
  every annotated type across the crate graph at startup and registers it — the Rust analog of pyfly
  `scan_package` / Spring component-scan, eliminating the manual `register_all!` list. (`inventory`
  added to the catalog. Generics can't be inventoried; documented, with `register_all!` as the explicit
  fallback for generic components.)
A new `samples/di-showcase` (or the Lumen book sample) demonstrates scan-based wiring end to end; trybuild
+ behavioral tests cover every macro and the scan/conditional/profile/lifecycle paths.

### D. Book overhaul — "Firefly for Rust by Example"
Rework the book to match pyfly's *PyFly by Example* in pedagogy and design:
- **A single guided use case threaded across every chapter** — **Lumen**, a digital-wallet & ledger
  service, grown from an empty project to a secured, observable, event-driven, multi-service system,
  Part by Part (Foundations → Persist the Domain → Event-Driven → Microservices → Secure/Observe/Ship).
  Backed by a real, compiling, tested `samples/lumen` crate so every listing is verified against running
  code (pyfly's guarantee). A `verify-book-code` step extracts listings and checks them against the
  sample.
- **Front matter**: title, copyright, a real **Preface/Introduction** ("what you'll build", who it's for,
  how to use it), and a **Conventions** page (callouts, code captions, Spring-parity boxes).
- **Per-chapter**: an opener, a **Recap** ("what changed in Lumen"), and **Exercises**; **Spring-parity**
  callouts wherever a concept mirrors Spring/Firefly-Java; figures/diagrams.
- **Design**: a `book.yaml`-driven build with **parts**, **cover art**, **per-chapter opener art**, and a
  themed stylesheet (callouts, code captions, part dividers) — a weasyprint HTML+CSS pipeline like
  pyfly's (install weasyprint + pango/cairo; pandoc+tectonic kept as a fallback). Regenerate
  `docs/book/dist/firefly-rust-by-example.{pdf,epub}` and attach to the release.
- **More content**: chapters for the new surface (declarative macros / DI deep-dive, the experience/BFF
  tier, pluggable databases) and richer prose throughout. mdBook HTML stays in sync.

## Architecture notes
- DI macros generate code against the facade `__rt` contract (`::firefly::__rt::firefly_container::…`) so
  the one-dependency story holds; `inventory` is re-exported through the facade for the scan thunks.
- `firefly-starter-experience` depends on starter-web + client + cache(redis) + orchestration (workflow
  engine) — it composes domain SDKs, it does not own a database.
- The book sample `samples/lumen` is a normal workspace member (tested in CI/`make ci`); the book build
  preprocesses listings out of/into it so docs and code never drift.

## Testing
- DI: trybuild compile-pass/fail + behavioral tests (scan registers all stereotypes; qualifier/primary
  disambiguation; conditional/profile gating against a config; post_construct/pre_destroy ordering;
  Provider/Vec injection; auto-bind interface resolution). Real, not mocked.
- Experience tier: a BFF sample/test composing two domain SDKs via the workflow engine with a
  `@WaitForSignal`-style gate, driven via `tower::oneshot`.
- Parity-gap fixes: each closed gap ships with tests; adversarial multi-agent review + fix loop over all
  new code.
- Book: `make book-pdf`/`book-epub` produce valid artifacts; the listing-verification step passes against
  `samples/lumen`; `mdbook build` clean.
- Full gate `make ci` green (now incl. real pg/mysql/sqlite/mongo/redis); version → **CalVer 26.6.4**.

## Execution waves
- **W1 — exhaustive audit** (all 41 pkgs grouped + admin + tiers + DI) → complete gap report.
- **W2 — DI + tiers + gap closure**: DI macros & container scan/conditional/lifecycle; `starter-experience`;
  admin completeness; remaining macro tiers; misc symbol-level gaps (parallel, gated).
- **W3 — book overhaul**: design pipeline (weasyprint, cover/openers/theme), the Lumen sample, the guided
  rewrite (intro + per-chapter Recap/Exercises/parity + new chapters), regenerate PDF/EPUB.
- **W4 — verify + publish**: full gate + adversarial review + fix loop + real-infra + book build; bump
  26.6.4; push; tag `v26.6.4`; GitHub release with the book attached.
