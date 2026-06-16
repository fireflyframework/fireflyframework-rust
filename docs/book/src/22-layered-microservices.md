# Layered Microservices

Every Lumen sample so far has lived in a *single* crate. That is the right shape
while you are learning one subsystem at a time, but it is not how a production
core-banking service is actually built. Real services — like the ones in the
[firefly-oss](https://github.com/firefly-oss) platform — are split into
**layered modules**, each a separately-compiled unit with exactly one job: the
public contract can be published without dragging in the persistence code, the
business logic can be unit-tested without the web stack, and an external SDK
consumer pulls in only the DTOs and nothing else.

In this chapter you build that shape. `lumen-ledger` (under
[`samples/lumen-ledger/`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen-ledger))
is a wallet/ledger microservice organised as **five crates** — the Rust analog
of a multi-module Maven project — laid out Java-style with **one public type per
file** under a `<domain>/v1` package path. It reuses every framework idea you
already met (DI beans, the sqlx repository, transactions, validation, RFC 9457
problems, OpenAPI) and shows how they compose *across a crate boundary* through
discovery alone.

By the end of this chapter you will:

- Lay out a service as five layered crates with the dependency arrows running
  strictly inward, and know which framework stereotype belongs to each layer.
- Declare a Spring Data-style `@Repository` over a real `@Entity` with two
  derives — no factory, no hand-written CRUD — built from an **async datasource
  bean**.
- Write a `@Service` that programs against the repository's
  `ReactiveCrudRepository` trait, runs an **atomic transfer** under
  `#[transactional]`, and translates a filter into a runtime `Specification`.
- Wire the whole graph with a single `firefly::link!` line and guard it with
  `assert_discovered`, then run and test the service in-process.
- Hand a typed SDK to downstream callers — written by hand against the shared
  DTOs, or generated from the live OpenAPI document.

## Concepts you will meet

Before the first crate, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — layered module.** A *layered module* is a separately
> compiled crate that owns exactly one architectural concern — the public
> contract, the persistence model, the business logic, the web surface, or an
> outbound client. Splitting a service this way is the Rust equivalent of a
> Maven multi-module project: each module compiles, tests, and versions on its
> own, and lower layers never import higher ones.

> **Note** **Key term — stereotype.** A *stereotype* is the role a bean plays in
> the application — controller, service, repository, component, configuration.
> Firefly marks each with its own derive (`#[derive(Controller)]`,
> `#[derive(Service)]`, …) exactly as Spring marks them with `@RestController`,
> `@Service`, `@Repository`, `@Component`, `@Configuration`. The framework
> classifies every discovered bean by its stereotype in the `/actuator/beans`
> report.

> **Note** **Key term — link-time discovery.** Firefly discovers beans,
> controllers, and schemas at *link time* using the `inventory` crate: each
> macro registers an entry the linker collects into the final binary. The catch
> is that a Rust linker **dead-strips** any crate the binary never references —
> a `Cargo.toml` dependency alone is not a reference. The `firefly::link!` macro
> supplies that reference so a layer crate's registrations survive into the
> binary.

## Step 1 — Lay out the five crates

The first decision is the module boundaries. `lumen-ledger` uses five, one per
concern, named after the firefly-oss convention:

| Crate | Holds | Stereotype it contributes |
|---|---|---|
| `firefly-sample-lumen-ledger-interfaces` | DTOs (`#[derive(Schema, Validate)]`) + the `WalletStatus` enum — the public contract | — (pure data) |
| `firefly-sample-lumen-ledger-models` | the `Wallet` `@Entity` + the sqlx `WalletRepository` + the datasource `@Configuration` | `@Entity`, `@Repository`, `@Bean` |
| `firefly-sample-lumen-ledger-core` | the `@Service`, the `@Mapper`, a `@Component` | `@Service`, `@Component` |
| `firefly-sample-lumen-ledger-web` | the `@RestController` + the `FireflyApplication` binary | `@RestController` |
| `firefly-sample-lumen-ledger-sdk` | a typed outbound client over the API | — (a client library) |

Each crate sets a short library name so code reads cleanly across the boundary —
`lumen_ledger_interfaces`, `lumen_ledger_models`, `lumen_ledger_core`,
`lumen_ledger_sdk` — while the package name stays fully qualified for publishing.
For example the `-interfaces` `Cargo.toml`:

```toml
[package]
name = "firefly-sample-lumen-ledger-interfaces"

[lib]
name = "lumen_ledger_interfaces"
path = "src/lib.rs"

[dependencies]
firefly = { workspace = true }
serde = { workspace = true }
```

The dependency arrows run strictly **inward**:

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 320" role="img"
     aria-label="Layered crate stack: interfaces, models, core and web crates with dependencies pointing strictly inward toward the interfaces contract, and an sdk crate that depends only on interfaces"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="140.0" y="32.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="30.0" width="260.0" height="50.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="270.0" y="52.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-interfaces</text><text x="270.0" y="66.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">DTOs · the public contract</text>
<line x1="270.0" y1="106.0" x2="270.0" y2="88.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="270.0,80.0 274.5,88.0 265.5,88.0" fill="#b5531f"/>
<text x="334.0" y="97.0" text-anchor="start" font-size="9.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends on</text>
<rect x="140.0" y="108.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="106.0" width="260.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="270.0" y="128.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-models</text><text x="270.0" y="142.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">@Entity · @Repository · @Bean</text>
<line x1="270.0" y1="182.0" x2="270.0" y2="164.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="270.0,156.0 274.5,164.0 265.5,164.0" fill="#b5531f"/>
<text x="334.0" y="173.0" text-anchor="start" font-size="9.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends on</text>
<rect x="140.0" y="184.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="182.0" width="260.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="270.0" y="204.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-core</text><text x="270.0" y="218.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">@Service · @Mapper · @Component</text>
<line x1="270.0" y1="258.0" x2="270.0" y2="240.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="270.0,232.0 274.5,240.0 265.5,240.0" fill="#b5531f"/>
<text x="334.0" y="249.0" text-anchor="start" font-size="9.5" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends on</text>
<rect x="140.0" y="260.5" width="260.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="140.0" y="258.0" width="260.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="270.0" y="280.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-web</text><text x="270.0" y="294.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">@RestController · the binary</text>
<rect x="444.0" y="184.5" width="112.0" height="50.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="444.0" y="182.0" width="112.0" height="50.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="500.0" y="204.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">-sdk</text><text x="500.0" y="218.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">typed client</text>
<path d="M500.0,182.0 Q415.4,145.7 401.3,62.9" fill="none" stroke="#d4793a" stroke-width="3.0" stroke-dasharray="6 5" stroke-linecap="round"/><polygon points="400.0,55.0 405.8,62.1 396.9,63.6" fill="#b5531f"/>
<text x="500.0" y="252.0" text-anchor="middle" font-size="9.5" font-weight="600" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">→ -interfaces</text>
</svg>
<figcaption>Five separately-compiled crates. Dependencies run strictly <strong>inward</strong>: <code>-web</code> knows <code>-core</code>, which knows <code>-models</code>, which knows <code>-interfaces</code> — and the contract crate knows nobody. <code>-sdk</code> depends only on <code>-interfaces</code>, so a caller links the DTOs without the persistence or web code.</figcaption>
</figure>

A lower layer never depends on a higher one. The `-web` crate knows the
`-core` service; the service knows the `-models` repository; the repository
knows the `-interfaces` contract — and the contract crate knows nobody. The
`-sdk` depends only on `-interfaces`, so a caller links the DTOs without ever
pulling in the persistence or web code. Concretely, `-models` depends on
`-interfaces`, `-core` depends on both, and `-web` depends on all three:

```toml
# firefly-sample-lumen-ledger-web/Cargo.toml
[dependencies]
firefly = { workspace = true, features = ["admin", "data-sqlx"] }
firefly-sample-lumen-ledger-interfaces = { path = "../interfaces" }
firefly-sample-lumen-ledger-models = { path = "../models" }
firefly-sample-lumen-ledger-core = { path = "../core" }
```

> **Note** **Key term — one type per file.** Each leaf file holds exactly one
> `struct` / `trait` / `enum`
> (`dtos/wallet/v1/wallet_response.rs` → `WalletResponse`), matching Java's
> one-class-per-file convention. The intermediate `mod` files
> (`dtos/wallet/v1.rs`) just re-export their leaves, and each crate's `lib.rs`
> adds flat convenience re-exports (`pub use services::wallet::v1::WalletService;`)
> so a consumer writes `lumen_ledger_core::WalletService`, not the full path.

> **Tip** **Checkpoint.** You can picture the tree before writing a line: five
> directories under `samples/lumen-ledger/`, each with its own `Cargo.toml`, and
> a `src/<domain>/v1/` package path inside. The arrows above tell you which
> `Cargo.toml` may list which — if you ever find a lower crate importing a higher
> one, the layering is wrong.

## Step 2 — Map the stereotypes to the layers

Before any code, fix in your head which framework stereotype each layer
contributes. Every type below is a **DI bean** the framework discovers during
`container.scan()` — there is no composition root assembling them by hand, just
as there was none in [Quickstart](./02-quickstart.md).

```text
@RestController  (web)    →  #[rest_controller] + #[derive(Controller)]   WalletController
   │ autowires
@Service         (core)   →  #[derive(Service)] + #[firefly(provides = "dyn WalletService")]
   │ autowires
@Mapper          (core)   →  #[derive(Component)]  WalletMapper          (DTO ↔ entity)
@Component       (core)   →  #[derive(Component)]  WalletNumberGenerator
@Repository      (models) →  #[derive(SqlxRepository)]  WalletRepository  (built from the Db @Bean)
   │ over
@Entity          (models) →  #[derive(Entity)]  Wallet                   (generates the SqlxEntity mapping)
   │ from
@Bean (DataSource)(models) →  #[bean] async fn data_source() -> Db
```

> **Note** **Key term — autowiring across crates.** *Autowiring* asks the
> container for a collaborator by type instead of constructing it yourself
> (Spring's `@Autowired`). Discovery is link-time, not per-crate, so an
> `#[autowired]` field in the `-web` controller is satisfied by a `@Service`
> bean declared in `-core`, which in turn autowires a `@Repository` from
> `-models`. The wiring crosses crate boundaries with no extra ceremony — once
> the crates are linked (Step 6), the graph is one container.

The `@Service` programs against the repository's **`ReactiveCrudRepository`**
trait (`save`, `find_by_id`, `delete_by_id`, `count`, … returning `Mono` / `Flux`)
plus the `#[firefly::repository]` derived queries — `find_by_owner`,
`find_by_status(.., Pageable)` (paged), and `count_by_status` — the same Spring
Data surface, generated from the method names. You met all of these in
[Persistence](./07-persistence.md); here they simply live one crate down.

## Step 3 — Declare the entity, Spring Data-style

Start at the bottom — the `-models` crate. A Spring Data repository is a
*declaration*: you write the interface, the framework supplies the
implementation. `lumen-ledger` does the same with two derives, and the first is
on the entity.

> **Note** **Key term — entity.** An *entity* is the persisted shape of a domain
> object — one row of one table. `#[derive(Entity)]` generates the
> `@Table` / `@Id` / `@Version` / `@Column` mapping from the struct's fields, the
> JPA `@Entity` experience: scalar columns map automatically, and annotated
> fields opt into the special roles (primary key, version, audit timestamps).

Create `models/src/entities/wallet/v1/wallet.rs`:

```rust,ignore
use chrono::{DateTime, Utc};
use lumen_ledger_interfaces::WalletStatus;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, firefly::Entity)]
#[firefly(table = "wallets")]
pub struct Wallet {
    #[firefly(id)]
    pub id: Uuid,
    pub account_number: String,
    pub owner: String,
    pub balance: i64,            // minor units (cents)
    pub currency: String,       // ISO-4217 code
    // The typed enum maps via an explicit converter — the @Enumerated(STRING) boundary.
    #[firefly(with(read = "WalletStatus::from_token", write = "WalletStatus::as_str"))]
    pub status: WalletStatus,
    #[firefly(version)]
    pub version: i64,           // @Version — bumped by the store on update
    pub created_at: DateTime<Utc>,  // @CreatedDate, stamped on insert
    pub updated_at: DateTime<Utc>,  // @LastModifiedDate, stamped on every write
}
```

What just happened: the derive read the struct and produced the table mapping.
Scalar columns (`String`, `i64` / `i32`, `bool`, `f64`, `Uuid`, `DateTime<Utc>`)
map automatically, with `Uuid` and `DateTime<Utc>` persisted as text;
`#[firefly(column = "name")]` would rename one. The `WalletStatus` enum is *not*
scalar, so it carries an explicit `with(read = …, write = …)` converter — the
read direction (`from_token`) and the write direction (`as_str`) — which is the
JPA `@Enumerated(STRING)` boundary made explicit. The `#[firefly(id)]`,
`#[firefly(version)]`, and the two timestamp fields opt into the special roles:
the store stamps the version and timestamps for you, so the service never touches
them.

> **Note** **Key term — optimistic locking.** *Optimistic locking* lets two
> readers load the same row, then makes the *second* writer fail if the first
> already changed it — detected by comparing the `@Version` column. No row is
> ever locked for reading; the conflict is caught at write time. A stale write
> surfaces as Spring's `OptimisticLockingFailureException`, which this service
> turns into a `409`.

## Step 4 — Declare the repository with one derive

The repository is **one annotation** — `#[derive(SqlxRepository)]` over a struct
whose only field is the framework's reactive repository — plus an optional block
of *derived queries*.

> **Note** **Key term — derived query.** A *derived query* is a finder whose SQL
> the framework generates from the method's *name* — `find_by_owner`,
> `count_by_status`, `find_by_status(.., Pageable)` — exactly like Spring Data's
> `findByOwner`. You write the signature and leave the body unimplemented; the
> `#[firefly::repository]` macro replaces it.

Create `models/src/repositories/wallet/v1/wallet_repository.rs`:

```rust,ignore
use firefly::data::{DataError, Pageable};
use firefly::data_sqlx::SqlxReactiveRepository;
use uuid::Uuid;

use crate::entities::wallet::v1::Wallet;

#[derive(firefly::SqlxRepository)]
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}

#[firefly::repository] // the derived queries, on top
impl WalletRepository {
    /// `SELECT … WHERE owner = ?`
    pub async fn find_by_owner(&self, owner: &str) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }

    /// `SELECT COUNT(*) WHERE status = ?`
    pub async fn count_by_status(&self, status: &str) -> Result<i64, DataError> {
        unimplemented!()
    }

    /// Paged `SELECT … WHERE status = ?` — ORDER BY / LIMIT / OFFSET come from
    /// the trailing `Pageable`.
    pub async fn find_by_status(
        &self,
        status: &str,
        page: Pageable,
    ) -> Result<Vec<Wallet>, DataError> {
        unimplemented!()
    }
}
```

What just happened, and why it matters: that one derive does three things at
once. It **registers `WalletRepository` as a `@Repository` bean** (discovered by
the scan, classified correctly in `/actuator/beans`); it **builds the inner
`SqlxReactiveRepository` from the autowired `Db`** datasource — wiring the entity's
`@Version` optimistic locking and `@CreatedDate` / `@LastModifiedDate` auditing
from the `SqlxEntity` mapping the entity derive emitted; and it **implements
`ReactiveCrudRepository` *and* `ReactiveSpecificationRepository` by delegation**.
That last point is what lets the service in Step 5 call `save`, `find_by_id`,
`delete_by_id`, and `find_by_spec` without you writing any of them.

The `#[firefly::repository]` block adds the derived queries on top: each method
body is `unimplemented!()` in your source, and the macro replaces it with SQL
generated from the method name. No `#[bean]` factory, no hand-written CRUD — the
struct's only state is the inner repository, and the derive builds it.

> **Tip** **Checkpoint.** Two derives, zero CRUD bodies, and the only field is
> `repo: SqlxReactiveRepository<Wallet, Uuid>`. If you find yourself writing a
> `save` or a `SELECT` by hand here, step back — the derive already supplies the
> canonical surface.

### The key type is generic, like Java

Spring Data's `CrudRepository<T, ID>` leaves `ID` unbounded. Firefly's sqlx
repository accepts **any `Serialize` key** through the `SqlKey` trait
(blanket-implemented), so the wallet repository keys on `Uuid` directly:

```rust,ignore
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}
```

`Uuid`, `i64`, `String`, an enum, or a composite-key struct all work — the key
binds as its serde-JSON form against the id column. Nothing about the repository
is hard-coded to UUIDs.

## Step 5 — Open the datasource as an async bean

The repository is a *synchronous* bean: building it is just wrapping the `Db`
handle. What actually performs I/O at startup is the **datasource** — Spring
Boot's auto-configured `DataSource`. In `lumen-ledger` it is an **async `@Bean`**
on the `-models` `@Configuration`.

> **Note** **Key term — async bean.** An *async bean* is a bean whose factory is
> an `async fn` — it must `await` work (open a pool, dial a broker) before the
> bean exists. The framework parks such a factory during the synchronous
> `container.scan()` and `await`s it during `Container::init_async_beans()`, run
> by the bootstrap right after the scan. This is Spring Boot's pattern of a
> `@Bean` that performs I/O at context-refresh time, except the I/O is awaited
> instead of blocking a thread.

Create `models/src/config/wallet_persistence_config.rs`:

```rust,ignore
use firefly::data_sqlx::Db;
use firefly::prelude::*;

#[derive(Configuration, Default)]
pub struct WalletPersistenceConfig;

#[firefly::bean]
impl WalletPersistenceConfig {
    /// The `Db` datasource bean — an async factory that opens the pool and
    /// applies the schema with `await`.
    #[bean]
    async fn data_source(&self) -> Db {
        connect_and_migrate().await // open pool + apply schema
    }
}
```

What just happened: the `#[bean] async fn data_source` is parked during the scan,
then `await`ed during `init_async_beans()`. Because the datasource is *ready*
before any synchronous bean resolves, the `#[derive(SqlxRepository)]` repository
(a synchronous bean that autowires `Db`) finds a live pool when the framework
builds it. A construction error here aborts startup — fail-fast, surfaced through
`Container::init_async_beans` as a `BeanCreation` error.

By default `connect_and_migrate()` opens an **in-memory SQLite database**, so the
sample runs and tests with no external server. Set `DATABASE_URL=postgres://…`
and it targets real PostgreSQL instead — the only environment dependency in the
whole sample, and it is optional.

> **Design note.** Why does the service own its transaction manager (Step 7) but
> the datasource live here? Because the registry of process-global transaction
> managers is *first-wins*, and this sample's test suite boots one isolated
> in-memory database **per test**. A single global manager would cross-contaminate
> them. The datasource bean is fine to share — every consumer resolves the same
> `Db` — but the transaction boundary is bound to a per-instance manager so each
> test stays hermetic. A single-datasource production service may equally register
> one manager at startup and use a bare `#[firefly::transactional]`.

> **Tip** **Checkpoint.** The `-models` crate now has three files of substance:
> the entity, the repository, and the config. `cargo test -p
> firefly-sample-lumen-ledger-models` exercises the repository directly against
> an isolated in-memory database — including the derived queries and a real
> `@Version` optimistic-lock conflict (a stale write detected with
> `firefly::data_sqlx::is_optimistic_lock`).

## Step 6 — Write the service against the repository trait

Move up to `-core`. The `@Service` is the business layer: it autowires the
repository, the mapper, and the number generator, and programs against the
repository's *trait* surface — never its concrete SQL.

The service is published as a **port** so the controller depends on an interface,
not a struct:

```rust,ignore
use std::sync::Arc;

use firefly::prelude::*;
use firefly::data_sqlx::Db;

#[derive(Service)]
#[firefly(provides = "dyn WalletService")]
pub struct WalletServiceImpl {
    #[autowired] repository: Arc<WalletRepository>,
    #[autowired] mapper: Arc<WalletMapper>,
    #[autowired] numbers: Arc<WalletNumberGenerator>,
    #[autowired] db: Arc<Db>,   // for the service's own transaction manager
}
```

> **Note** **Key term — provided port.** `#[firefly(provides = "dyn WalletService")]`
> registers the impl under the *trait object* type, so anyone who autowires
> `Arc<dyn WalletService>` (the controller, a test) receives this bean. The
> trait is the published port; the struct is a hidden adapter — Spring's
> "program to an interface, inject the implementation".

The simple read paths just delegate to the repository's `ReactiveCrudRepository`
trait and map the result through the `@Mapper`:

```rust,ignore
async fn get(&self, id: Uuid) -> Result<WalletResponse, ServiceError> {
    let wallet = self
        .repository
        .find_by_id(id)
        .await
        .map_err(|e| ServiceError::Backend(e.to_string()))?
        .ok_or(ServiceError::NotFound)?;
    Ok(self.mapper.to_response(&wallet))
}

async fn list_by_owner(&self, owner: &str) -> Result<Vec<WalletResponse>, ServiceError> {
    let wallets = self
        .repository
        .find_by_owner(owner)            // the derived query
        .await
        .map_err(|e| ServiceError::Backend(e.to_string()))?;
    Ok(wallets.iter().map(|w| self.mapper.to_response(w)).collect())
}
```

What just happened: `find_by_id` comes from the `ReactiveCrudRepository` trait the
derive implemented; `find_by_owner` is the derived query from the
`#[firefly::repository]` block. The service never sees SQL — it sees a repository
that already speaks its domain.

> **Note** **Key term — mapper.** A *mapper* translates between layers — here the
> `-models` `Wallet` entity and the `-interfaces` `WalletResponse` DTO. Because
> the two types live in *different* crates, Rust's orphan rule forbids
> `impl From<Wallet> for WalletResponse` in `-core`. So `WalletMapper` is a
> hand-written `#[derive(Component)]` bean with a `to_response(&self, &Wallet) ->
> WalletResponse` method — exactly the shape MapStruct's `@Mapper` generates.

### Filtering with a runtime Specification

The `search` use case shows the framework's `Specification` — the Spring Data
`JpaSpecificationExecutor` analog. The service turns each *present* filter field
into an AND-combined predicate, then runs the composed specification:

```rust,ignore
use firefly::data::{Op, Predicate, ReactiveSpecificationRepository, Specification};

async fn search(&self, filter: WalletFilter) -> Result<Vec<WalletResponse>, ServiceError> {
    // At least one criterion is required — a no-filter search would be an
    // unscoped list-every-wallet enumeration.
    if filter.owner.is_none()
        && filter.currency.is_none()
        && filter.status.is_none()
        && filter.min_balance.is_none()
        && filter.max_balance.is_none()
    {
        return Err(ServiceError::Validation("provide at least one filter criterion".into()));
    }

    let mut spec = Specification::all();
    if let Some(owner) = filter.owner {
        spec = spec.and(Specification::eq("owner", owner));
    }
    if let Some(min) = filter.min_balance {
        spec = spec.and(Specification::pred(Predicate::new("balance", Op::Gte, min)));
    }
    // …currency, status, max_balance the same way…

    let wallets = self
        .repository
        .find_by_spec(spec)         // from ReactiveSpecificationRepository
        .collect_list()
        .block()
        .await
        .map_err(|e| ServiceError::Backend(e.to_string()))?
        .unwrap_or_default();
    Ok(wallets.iter().map(|w| self.mapper.to_response(w)).collect())
}
```

What just happened: `find_by_spec` comes from `ReactiveSpecificationRepository`
(the *other* trait the `SqlxRepository` derive implemented). It returns a `Flux`,
so `.collect_list().block().await` gathers it. `block()` returns
`Result<Option<Vec<Wallet>>, _>`, so `.unwrap_or_default()` turns the "no rows"
`None` into an empty `Vec`. The framework compiles the `Specification` to a
dialect-aware `WHERE`, so the same service code runs unchanged on SQLite or
PostgreSQL.

### The atomic transfer, under one transaction

The transfer is the heart of a ledger: debit the source and credit the
destination, both-or-neither. That demands a transaction.

> **Note** **Key term — transactional boundary.** `#[firefly::transactional]`
> wraps a method so every write inside it commits together or rolls back
> together — Spring's `@Transactional`. The `manager = "self.tx_manager()"`
> argument binds the boundary to a manager the *service* owns (evaluated per
> call) rather than the process-global registry. The attribute lives on an
> inherent method (`transfer_tx`), because an `async-trait` method cannot carry
> it cleanly; the trait method just delegates.

```rust,ignore
use firefly::data_sqlx::SqlxTransactionManager;
use firefly::transactional::TransactionManager;

impl WalletServiceImpl {
    fn tx_manager(&self) -> Arc<dyn TransactionManager> {
        Arc::new(SqlxTransactionManager::new((*self.db).clone()))
    }

    #[firefly::transactional(manager = "self.tx_manager()")]
    async fn transfer_tx(&self, from: Uuid, to: Uuid, amount: i64)
        -> Result<WalletResponse, ServiceError>
    {
        if amount <= 0 { return Err(ServiceError::Validation("transfer amount must be positive".into())); }
        if from == to { return Err(ServiceError::Validation("cannot transfer to the same wallet".into())); }

        let mut source = self.load_active(from).await?;  // 404 if absent, 422 if not active
        let mut dest = self.load_active(to).await?;
        if source.currency != dest.currency {
            return Err(ServiceError::Validation("currency mismatch".into()));
        }
        if source.balance < amount {
            return Err(ServiceError::Validation("insufficient funds".into()));
        }

        // Every precondition is checked BEFORE the source is debited, so a
        // rejected transfer moves no money. If the credit fails after the debit,
        // the transaction rolls the debit back.
        source.balance = source.balance.checked_sub(amount)
            .ok_or_else(|| ServiceError::Validation("balance underflow".into()))?;
        let saved_source = self.persist(source).await?;
        dest.balance = dest.balance.checked_add(amount)
            .ok_or_else(|| ServiceError::Validation("balance overflow".into()))?;
        self.persist(dest).await?;
        Ok(saved_source)  // the updated source
    }
}
```

What just happened, and why it matters: `transfer_tx` runs inside a single
transaction bound to `self.tx_manager()`. Every guard (positive amount, distinct
active wallets, matching currency, sufficient funds) fires *before* the first
write, so a rejected transfer never touches a balance. The arithmetic is
`checked_*`, so a ledger overflow is a domain error, not a silent wrap. And if
the credit ever failed after the debit, the boundary rolls the debit back — the
both-or-neither guarantee a ledger lives on.

> **Note** Because `#[transactional]` requires the error type to be
> `From<firefly::transactional::TxError>`, `ServiceError` implements that
> conversion — a transaction-infrastructure failure (begin / commit / rollback)
> surfaces as `ServiceError::Backend`. The `no_rollback_for` /
> `rollback_only_for` arguments to `#[transactional]` (not shown here) let you
> tune which error variants trigger a rollback; the default rolls back on any
> `Err`.

The `persist` helper centralises the save-and-map, and maps a stale `@Version`
write to a `Conflict`:

```rust,ignore
async fn persist(&self, wallet: Wallet) -> Result<WalletResponse, ServiceError> {
    let saved = self
        .repository
        .save(wallet)
        .await
        .map_err(|e| {
            if is_optimistic_lock(&e) {
                ServiceError::Conflict("wallet was modified concurrently; retry".into())
            } else {
                ServiceError::Backend(e.to_string())
            }
        })?
        .ok_or_else(|| ServiceError::Backend("save returned no row".into()))?;
    Ok(self.mapper.to_response(&saved))
}
```

`deposit` and `withdraw` are plain read-modify-writes that lean on the same
`load_active` + `persist` pair; their concurrency safety comes from the
repository's `@Version` optimistic locking (a stale write → `409`), not a
transaction.

> **Tip** **Checkpoint.** The `-core` crate now holds the service (with its
> trait, impl, and `ServiceError`), the mapper, and the number generator — every
> one a DI bean, none constructed by hand. The service compiles against
> `-models` and `-interfaces` but knows nothing of `-web`.

## Step 7 — Wire the crates with `firefly::link!`

Now the binary. The `-web` crate holds the `@RestController` (Step 8) and the
one-line `FireflyApplication` boot — but a Cargo dependency on the layer crates
is **not enough**. Because discovery is link-time, the linker will dead-strip a
layer crate's bean / controller / schema registrations unless the binary actually
*references* that crate. The `firefly::link!` macro is that reference.

Create `web/src/main.rs`:

```rust,ignore
// LINK-TIME WIRING — DO NOT REMOVE. Force-links each layer crate so its beans,
// controllers, and schemas survive dead-code elimination into the binary.
firefly::link!(
    lumen_ledger_core,
    lumen_ledger_models,
    lumen_ledger_interfaces
);

mod controllers;

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen-ledger")
        .version(firefly::VERSION)
        .run()
        .await
}
```

What just happened: `firefly::link!(a, b, c)` expands to `extern crate a as _;`
for each crate, which is exactly the reference the linker needs to keep that
crate's `inventory` registrations. Without it you get the classic "6 of 16 beans"
symptom — the binary compiles, links, runs, and silently drops half its beans.
The `-web` crate itself is referenced (it *is* the binary, and it declares
`mod controllers`), so it does not appear in the `link!` list; the three library
layers do.

Note that `main` itself is the same one line you wrote in
[Quickstart](./02-quickstart.md) — `FireflyApplication::new(name).run().await` —
just with `.version(firefly::VERSION)` set so `/actuator/info` reports the
framework release. A layered service needs exactly **one** extra line of wiring
(`link!`); everything else is discovered.

To turn a forgotten `link!` crate from a silent bug into a loud failure, guard
the boot with `assert_discovered`. You call it right after `bootstrap()` returns
(the test seam from Quickstart), using the returned `Bootstrapped::container`:

```rust,ignore
let app = firefly::FireflyApplication::new("lumen-ledger")
    .bootstrap()
    .await
    .expect("bootstrap");

// At least 8 beans (repository, service, mapper, component, config, …) and at
// least 1 controller were discovered — across all three layer crates.
firefly::assert_discovered(&app.container, 8, 1);
```

`assert_discovered(&container, min_beans, min_controllers)` panics at startup if
the discovered bean or controller count falls below the floor you assert — the
single most useful check in a layered service.

> **Tip** **Checkpoint.** `cargo run -p firefly-sample-lumen-ledger-web` boots on
> `:8080` (public) with the management surface on `:8081`, and the startup report
> lists beans drawn from all four code crates. If the bean count looks too small,
> a crate is missing from `firefly::link!`.

## Step 8 — The production-grade web surface

The `@RestController` is the last layer, and it is more than CRUD — it carries
the error and validation discipline a Spring Boot service is expected to have,
every failure rendered as RFC 9457 `application/problem+json`. You met each of
these tools in [Your First HTTP API](./06-first-http-api.md) and
[OpenAPI](./06a-openapi.md); here they compose over the layered service.

The controller is a `#[derive(Controller)]` bean that autowires the `dyn
WalletService` port from `-core` and is auto-mounted by `#[rest_controller]`:

```rust,ignore
use std::sync::Arc;
use firefly::prelude::*;
use firefly::web::{PageRequest, Path, Query, Valid, WebError, WebResult};
use lumen_ledger_core::{ServiceError, WalletService};

#[derive(Clone, Controller)]
pub struct WalletController {
    #[autowired]
    service: Arc<dyn WalletService>,
}

#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletController {
    #[post("/wallets", summary = "Open a wallet", status = 201,
        header("Idempotency-Key", description = "optional client-supplied key to make retries safe"))]
    async fn open(
        State(api): State<WalletController>,
        headers: axum::http::HeaderMap,
        Valid(body): Valid<CreateWalletRequest>,   // 422 on a blank owner / bad currency
    ) -> WebResult<(StatusCode, Json<WalletResponse>)> {
        let view = api.service.create(body).await.map_err(service_to_web)?;
        Ok((StatusCode::CREATED, Json(view)))
    }

    #[get("/wallets/:id", summary = "Fetch a wallet")]
    async fn get(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,                       // 400 on a non-UUID id
    ) -> WebResult<Json<WalletResponse>> {
        let view = api.service.get(id).await.map_err(service_to_web)?;
        Ok(Json(view))
    }

    #[get("/wallets/page", summary = "List wallets by status (paged)")]
    async fn list_paged(
        State(api): State<WalletController>,
        Query(query): Query<StatusQuery>,
        PageRequest(pageable): PageRequest,         // binds page/size/sort
    ) -> WebResult<Json<Page<WalletResponse>>> {
        let page = api.service.list_by_status(query.status, pageable).await.map_err(service_to_web)?;
        Ok(Json(page))
    }
    // …deposit, withdraw, transfer, search, set_status, delete…
}
```

What just happened, concern by concern:

| Concern | How it is handled |
|---|---|
| Bean validation at the edge | `Valid<CreateWalletRequest>` / `Valid<AmountRequest>` / `Valid<TransferRequest>` — a blank owner, a non-ISO currency (`#[validate(pattern = "[A-Z]{3}")]`), or a non-positive amount (`#[validate(range(min = 1))]`) is a **422** before the service runs |
| Malformed path / query | the framework's `firefly::web::{Path, Query}` extractors — a non-UUID id or a missing `?owner=` is a **400** problem, not axum's plain-text default |
| Atomic transfer | `POST /api/v1/wallets/:id/transfer` debits the source and credits the destination inside **one transaction** (Step 6). A rejected transfer moves no money |
| Optimistic-lock conflict | a stale `@Version` write → `ServiceError::Conflict` → **409** |
| Unknown wallet | `ServiceError::NotFound` → **404** |
| Status lifecycle | `PATCH /api/v1/wallets/:id/status` transitions `active → frozen → closed`; a frozen wallet rejects a debit with **422** |
| Delete | `DELETE /api/v1/wallets/:id` → **204**, delegating to `delete_by_id` |
| Pagination | `GET /api/v1/wallets/page?status=active&page=1&size=20&sort=balance,desc` returns a Spring-Data `Page<T>` (`content` + `totalElements`). The `PageRequest` resolver binds `page` / `size` / `sort` into a `Pageable` (exactly like a Spring `Pageable` parameter), which the service passes to the paged `find_by_status` derived query |
| Filtering | `GET /api/v1/wallets/search?owner=&currency=&status=&minBalance=&maxBalance=` binds a `WalletFilter` query DTO (each field an OpenAPI query parameter); the service turns the present criteria into a `Specification` the repository compiles to a dialect-aware `WHERE`. At least one criterion is required |

> **Note** **Key term — orphan rule.** Rust's *orphan rule* forbids implementing
> a trait for a type when *both* are foreign to the current crate. `WebError`
> (from `firefly`) and `ServiceError` (from `-core`) are both foreign to `-web`,
> so `impl From<ServiceError> for WebError` is illegal here. The controller maps
> them with a small free function instead — the same constraint that made the
> `@Mapper` a bean rather than a `From` impl:

```rust,ignore
fn service_to_web(err: ServiceError) -> WebError {
    match err {
        ServiceError::NotFound => WebError::from(FireflyError::not_found("wallet not found")),
        ServiceError::Validation(d) => WebError::from(FireflyError::validation(d)),
        ServiceError::Conflict(d) => WebError::from(FireflyError::conflict(d)),
        ServiceError::Backend(d) => WebError::from(FireflyError::internal(d)),
    }
}
```

> **Tip** **Checkpoint.** Every controller handler returns `WebResult<T>`, and
> every domain failure flows through `service_to_web` into a precise problem
> status. Open `http://localhost:8081/swagger-ui` after `cargo run` to see the
> whole wallet surface — bodies, query params, and the declared `Idempotency-Key`
> header — rendered from the inventory. The OpenAPI docs are on the **management**
> port, beside actuator and admin, never on the public API.

## Step 9 — Hand callers a typed SDK

The fifth crate, `-sdk`, is a typed outbound client over
`firefly_client::RestClient`, reusing the `-interfaces` DTOs so a caller never
re-declares the contract. Because `-sdk` depends only on `-interfaces`, importing
it pulls in the DTOs and nothing else — no persistence, no web stack.

```rust,ignore
use firefly_client::{ClientError, RestBuilder, RestClient, NO_BODY};
use http::Method;
use lumen_ledger_interfaces::{AmountRequest, CreateWalletRequest, WalletResponse};

pub struct WalletClient {
    inner: RestClient,
}

impl WalletClient {
    pub fn new(base_url: impl AsRef<str>) -> Self {
        Self { inner: RestBuilder::new(base_url).build() }
    }

    /// `POST /api/v1/wallets` — open a wallet.
    pub async fn create_wallet(
        &self,
        request: &CreateWalletRequest,
    ) -> Result<WalletResponse, ClientError> {
        self.inner
            .request::<_, WalletResponse>(Method::POST, "/api/v1/wallets", Some(request))
            .await
    }

    /// `GET /api/v1/wallets/{id}` — fetch one wallet.
    pub async fn get_wallet(
        &self,
        id: impl std::fmt::Display,
    ) -> Result<WalletResponse, ClientError> {
        let path = format!("/api/v1/wallets/{id}");
        self.inner
            .request::<(), WalletResponse>(Method::GET, &path, NO_BODY)
            .await
    }
    // …list_wallets, deposit, withdraw…
}
```

What just happened: each method maps to one endpoint and (de)serialises the
shared DTOs, so the caller programs against *the same types* the server enforces
— a contract drift fails to compile. Every method returns `Result<T, ClientError>`;
a non-2xx RFC 9457 body decodes into a typed `FireflyError` reachable via
`ClientError::as_firefly`. The `with_client` constructor wraps an
already-configured `RestClient` (custom headers, retries, timeouts, a bearer
token), the bring-your-own-client path. The full `RestClient` surface is covered
in [HTTP Clients](./13-http-clients.md).

### Generating the SDK instead

You can also **generate** an equivalent client from the running service's OpenAPI
document, so you never hand-write a method again:

```bash
firefly openapi-client --spec wallet-openapi.json -o src/generated.rs --client-name WalletClient
```

`firefly openapi-client` walks the spec and emits a self-contained client — a
model `struct` / `enum` per `components.schemas` entry and one `async fn` per
operation, with typed path / query parameters and JSON bodies. The generated file
is headed `// Code generated by \`firefly openapi-client\`. DO NOT EDIT.`. The
full generator catalogue is in [The CLI](./19-cli.md).

> **Tip** **Checkpoint.** `cargo test -p firefly-sample-lumen-ledger-sdk`
> compiles the client and runs its contract checks — every method's typed result
> lines up with a shared `-interfaces` DTO. (The network round-trip itself is
> exercised by the `-web` integration test in Step 10.)

## Step 10 — Run and test the whole graph

With all five crates in place, run and test the service:

```bash
cargo run  -p firefly-sample-lumen-ledger-web   # boots on :8080, management on :8081
cargo test -p firefly-sample-lumen-ledger-web   # in-process cross-crate round-trip
```

The integration test boots the whole graph in-process with `bootstrap()` (no
socket bound), asserts discovery with `assert_discovered(&app.container, 8, 1)`,
and drives the full public surface through the returned `api_router` — create /
fetch / deposit / withdraw, the paged status query, the search specification, the
status transition, delete, the atomic transfer (including every rejection path),
and every problem path (404, the 422 validation failures, the 400
malformed-path / missing-query). It also checks the **management** router: that
the OpenAPI document is served there (and *absent* from the public API), and that
an unknown management path answers an RFC 9457 problem 404 — the same contract as
the public API.

The point of the test is the architecture, not just the assertions: it proves
every layer wires together through DI alone. The `@RestController` in `-web`
reaches the `@Service` in `-core`, which reaches the `@Repository` in `-models`,
which reaches the `@Bean` datasource — all discovered, none hand-assembled, across
four crate boundaries.

> **Tip** **Checkpoint.** Both commands succeed. `cargo run` prints a startup
> report whose `:: beans ::` line is drawn from every code crate, and the test
> suite is green — the layered service behaves as one application.

## Recap — what you built

You turned a single-crate sample into a five-crate layered microservice without
adding a composition root:

| Layer | Crate | What it contributes |
|---|---|---|
| contract | `…-interfaces` | DTOs + the `WalletStatus` enum — `#[derive(Schema, Validate)]`, depends on nobody |
| persistence | `…-models` | the `Wallet` `@Entity`, the two-derive `@Repository`, the async datasource `@Bean` |
| business | `…-core` | the `@Service` port, the `@Mapper`, a `@Component`; the atomic `#[transactional]` transfer |
| web | `…-web` | the `@RestController`, the `firefly::link!` wiring, the one-line `FireflyApplication` |
| client | `…-sdk` | a typed `RestClient` over the shared DTOs (or generated from OpenAPI) |

You also now know:

- Why the dependency arrows must run strictly inward, and how each layer
  contributes exactly the stereotypes that belong to it.
- That a Spring Data-style repository is two derives — `#[derive(Entity)]` and
  `#[derive(SqlxRepository)]` — built from an async datasource bean, giving you
  `ReactiveCrudRepository` + `ReactiveSpecificationRepository` + derived queries
  for free.
- That `firefly::link!` is the *one* line of wiring a layered service needs, that
  it exists because discovery is link-time, and that `assert_discovered` turns a
  forgotten crate into a loud startup failure.
- How an atomic transfer composes `#[transactional]`, optimistic locking, and a
  precondition-first design so a rejected transfer moves no money.
- How to hand callers a typed SDK that reuses the contract crate — or generate
  one from the live OpenAPI document.

## Exercises

1. **Provoke the dead-strip.** Comment out one crate in the `firefly::link!`
   line (say `lumen_ledger_models`), then `cargo run -p
   firefly-sample-lumen-ledger-web`. Watch `assert_discovered` fail at startup
   with the "discovered N beans but expected at least 8" panic — that is exactly
   the bug `link!` prevents. Restore the line.
2. **Trace a request across four crates.** With the service running, `curl -X
   POST localhost:8080/api/v1/wallets -H 'content-type: application/json' -d
   '{"owner":"ada","currency":"EUR","openingBalance":1000}'`. Name, in order,
   which crate handles each hop: the controller (`-web`), the service (`-core`),
   the mapper (`-core`), the repository (`-models`), the datasource (`-models`).
3. **Break the transfer atomicity claim.** Read `transfer_tx` and confirm the
   currency / funds / active checks all run *before* the first `persist`. Then
   `curl` a transfer with `amount` larger than the source balance and verify the
   source balance is unchanged afterward (`GET` it) — a rejected transfer moves no
   money.
4. **Add a derived query.** Add `find_by_currency(&self, currency: &str) ->
   Result<Vec<Wallet>, DataError>` to the `#[firefly::repository]` block (body
   `unimplemented!()`), expose it through the service and a controller route, and
   confirm it works — without writing any SQL.
5. **Generate the SDK.** Run the service, fetch the spec with `curl
   localhost:8081/v3/api-docs > wallet-openapi.json`, then `firefly
   openapi-client --spec wallet-openapi.json -o /tmp/generated.rs --client-name
   WalletClient`. Compare the generated methods to the hand-written `-sdk`
   client.

## Where to go next

- Drive a fully wired application in-process — the `bootstrap()` seam, the
  `api_router` / `management_router`, and cross-crate round-trip tests like the
  one in Step 10 — in **[Testing](./18-testing.md)**.
- Revisit the persistence machinery this chapter layered (entities, derived
  queries, specifications, optimistic locking) in
  **[Persistence & Reactive Repositories](./07-persistence.md)**.
- Take the layered service to production — real PostgreSQL via `DATABASE_URL`,
  containers, and the management surface — in
  **[Production & Deployment](./20-production.md)**.
