# Layered Microservices

The samples so far live in a single crate. Real services — like the ones in the
[firefly-oss](https://github.com/firefly-oss) core-banking platform — are split
into **layered modules**, each a separately-compiled unit with one job, so the
public contract can be published without the persistence code, the business
logic can be tested without the web stack, and an SDK consumer pulls in only the
DTOs.

The `lumen-ledger` sample (`samples/lumen-ledger/`) is a wallet/ledger
microservice built exactly this way: **five crates**, the Rust analog of a
firefly-oss Maven multi-module project, organised Java-style with **one public
type per file** under a `<domain>/v1` package path.

## The five crates

| Crate (`firefly-sample-lumen-ledger-*`) | Spring/firefly-oss analog | Holds |
|---|---|---|
| `…-interfaces` | `-interfaces` | DTOs (`#[derive(Schema, Validate)]`) + enums — the public contract |
| `…-models`     | `-models`     | the `Wallet` entity + the sqlx `WalletRepository` |
| `…-core`       | `-core`       | the `@Service`, the `@Mapper`, a `@Component` |
| `…-web`        | `-web`        | the `@RestController` + the `FireflyApplication` binary |
| `…-sdk`        | `-sdk`        | a typed outbound client over the API |

The dependency arrows run **inward**: `interfaces ← models ← core ← web`, and
`sdk ← interfaces`. A lower layer never depends on a higher one — the web crate
knows the service, the service knows the repository, but the contract crate
knows nobody. Each leaf file holds exactly one `struct`/`trait`/`enum`
(`dtos/wallet/v1/wallet_response.rs` → `WalletResponse`), matching Java's
one-class-per-file convention; the intermediate module files just re-export.

## One stereotype per layer

Each layer contributes the framework stereotypes that belong to it. Every type
is a **DI bean** discovered by `container.scan()` — there is no composition root.

```text
@RestController  (web)    →  #[rest_controller] + #[derive(Controller)]
   │ autowires
@Service         (core)   →  #[derive(Service)] + #[firefly(provides = "dyn WalletService")]
   │ autowires
@Mapper          (core)   →  #[derive(Component)] WalletMapper        (DTO ↔ entity)
@Component       (core)   →  #[derive(Component)] WalletNumberGenerator
@Repository      (models) →  #[derive(SqlxRepository)] WalletRepository  (built from the Db @Bean)
   │ over
@Entity          (models) →  #[derive(Entity)] Wallet  (generates the SqlxEntity mapping)
   │ from
@Bean (DataSource)(models) →  #[bean] async fn data_source() -> Db
```

The `@Service` programs against the repository's **`ReactiveCrudRepository`**
trait (`save`, `find_by_id`, `delete_by_id`, `count`, … returning `Mono`/`Flux`)
plus the `#[firefly::repository]` derived queries — `find_by_owner`,
`find_by_status(.., Pageable)` (paged), and `count_by_status` — the same Spring
Data surface, generated from the method names.

## The repository, declared Spring Data-style

A Spring Data repository is a *declaration*: you write the interface, the
framework supplies the implementation. `lumen-ledger` does the same with two
derives — no factory, no hand-written CRUD.

The entity declares its `@Table` / `@Id` / `@Version` / `@Column` mapping with
`#[derive(Entity)]` — just annotated fields, the JPA `@Entity` experience:

```rust,ignore
#[derive(Entity)]
#[firefly(table = "wallets")]
pub struct Wallet {
    #[firefly(id)] pub id: Uuid,
    pub account_number: String,
    pub balance: i64,
    // an enum maps via an explicit converter (the @Enumerated(STRING) boundary)
    #[firefly(with(read = "WalletStatus::from_token", write = "WalletStatus::as_str"))]
    pub status: WalletStatus,
    #[firefly(version)] pub version: i64,            // @Version
    pub created_at: DateTime<Utc>,                   // @CreatedDate (auditor-stamped)
    pub updated_at: DateTime<Utc>,                   // @LastModifiedDate
}
```

Scalar columns (`String`, `i64`/`i32`, `bool`, `f64`, `Uuid`, `DateTime<Utc>`)
map automatically; `#[firefly(column = "name")]` renames one.

The repository is then **one annotation** — `#[derive(SqlxRepository)]` over a
struct holding the entity's repository:

```rust,ignore
#[derive(SqlxRepository)]
pub struct WalletRepository {
    repo: SqlxReactiveRepository<Wallet, Uuid>,
}

#[firefly::repository]               // the derived queries, on top
impl WalletRepository {
    pub async fn find_by_owner(&self, owner: &str) -> Result<Vec<Wallet>, DataError> { unimplemented!() }
    // find_by_status(.., Pageable), count_by_status, …
}
```

That derive registers `WalletRepository` as a **`@Repository` bean** (discovered
by the scan, classified correctly in `/beans`), **builds the inner repository
from the injected `Db`** — wiring `@Version` optimistic locking (a stale write
→ `409`) and `@CreatedDate`/`@LastModifiedDate` auditing from the entity — and
implements `ReactiveCrudRepository` by delegation, so the service programs
against the canonical CRUD surface plus the derived queries.

### The datasource is the async bean

What *is* an async bean is the **datasource** — Spring Boot's auto-configured
`DataSource`. The `-models` `@Configuration` opens the pool with `await`:

```rust,ignore
#[firefly::bean]
impl WalletPersistenceConfig {
    #[bean]
    async fn data_source(&self) -> Db {     // the DataSource @Bean
        connect_and_migrate().await          // open pool + apply schema
    }
}
```

The framework parks this factory during the synchronous `container.scan()` and
`await`s it during `Container::init_async_beans()` (run by the bootstrap right
after the scan). The `#[derive(SqlxRepository)]` repository — a *synchronous*
bean — then resolves that ready `Db` and builds itself from it. This is the
Spring Boot pattern of a `@Bean` that performs I/O at context-refresh time,
except the I/O is `await`ed instead of blocking a thread. By default the
datasource opens an in-memory SQLite database (the sample runs and tests with no
external server); set `DATABASE_URL=postgres://…` for real PostgreSQL.

## Any key type — generic like Java

Spring Data's `CrudRepository<T, ID>` leaves `ID` unbounded. Firefly's sqlx
repository accepts any **`Serialize`** key through the `SqlKey` trait
(blanket-implemented), so the wallet repository keys on `Uuid` directly:

```rust,ignore
pub struct WalletRepository { repo: SqlxReactiveRepository<Wallet, Uuid> }
impl ReactiveCrudRepository<Wallet, Uuid> for WalletRepository { /* … */ }
```

`Uuid`, `i64`, `String`, an enum, or a composite-key struct all work — the key
binds as its serde-JSON form against the id column.

## Linking the crates — `firefly::link!`

Because discovery is link-time (`inventory`), the linker will **dead-strip** a
layer crate's bean/controller/schema registrations unless the binary references
that crate. A Cargo dependency is not a reference. The `-web` binary force-links
each layer with `firefly::link!`:

```rust,ignore
// crate root of the -web binary
firefly::link!(lumen_ledger_core, lumen_ledger_models, lumen_ledger_interfaces);

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen-ledger")
        .version(firefly::VERSION)
        .run()
        .await
}
```

`firefly::assert_discovered(&container, min_beans, min_controllers)` guards a
forgotten crate at startup. This one line is the only wiring a layered service
needs; everything else is discovered.

## A production-grade web surface

The `@RestController` is more than CRUD — it carries the error and validation
discipline a Spring Boot service is expected to have, all rendered as RFC 9457
`application/problem+json`:

| Concern | How |
|---|---|
| Bean validation at the edge | `Valid<CreateWalletRequest>` / `Valid<AmountRequest>` — a blank owner, a non-ISO currency (`#[validate(pattern = "[A-Z]{3}")]`), or a non-positive amount (`#[validate(range(min = 1))]`) is a **422** before the service runs. The same `Validate` runs on query/path objects via `ValidQuery<T>` / `ValidPath<T>`, and a `multipart/form-data` upload binds through the problem-rendering `Multipart` extractor |
| Malformed path / query | `firefly::web::{Path, Query}` extractors — a non-UUID id or a missing `?owner=` is a **400** problem, not axum's plain-text default |
| Atomic transfer | `POST /api/v1/wallets/:id/transfer` debits the source and credits the destination inside **one transaction** — `#[firefly::transactional(manager = "self.tx_manager()")]` on the service. The debit and credit commit together or not at all; a rejected transfer (insufficient funds, inactive party) moves no money. The `manager = …` binds the boundary to a manager the service owns rather than the process-global registry, so the per-test isolated databases stay correct |
| Optimistic-lock conflict | a stale `@Version` write → `ServiceError::Conflict` → **409** |
| Unknown wallet | `ServiceError::NotFound` → **404** |
| Status lifecycle | `PATCH /api/v1/wallets/:id/status` transitions `active → frozen → closed`; a frozen wallet rejects a debit with **422** |
| Delete | `DELETE /api/v1/wallets/:id` → **204**, delegating to `delete_by_id` |
| Pagination | `GET /api/v1/wallets/page?status=active&page=1&size=20&sort=balance,desc` returns a Spring-Data `Page<T>` (`content` + `totalElements`). The framework's `PageRequest` argument resolver binds `page`/`size`/`sort` into a `Pageable` (exactly like a Spring `Pageable` parameter), which the `@Service` passes straight to the paged `find_by_status` derived query |
| Filtering | `GET /api/v1/wallets/search?owner=&currency=&status=&minBalance=&maxBalance=` binds a `WalletFilter` query DTO (each field an OpenAPI query parameter); the `@Service` turns the present criteria into a composable `firefly::data::Specification` that `#[derive(SqlxRepository)]`'s `find_by_spec` compiles to a dialect-aware `WHERE` — the Spring Data `JpaSpecificationExecutor` analog. At least one criterion is required (no unscoped list-all) |

Because `WebError` and `ServiceError` are both foreign to the `-web` crate,
the controller maps them with a small `service_to_web` function rather than an
orphan-rule-blocked `impl From<ServiceError> for WebError` — a worth-knowing Rust
layering detail the sample documents inline.

## The SDK and the generator

The `-sdk` crate is a typed outbound client over `firefly_client::RestClient`,
reusing the `-interfaces` DTOs so a caller never re-declares the contract. You
can also **generate** an equivalent client from the running service's OpenAPI
document:

```bash
firefly openapi-client --spec wallet-openapi.json -o src/generated.rs --client-name WalletClient
```

`firefly openapi-client` walks the spec and emits a self-contained client — a
model `struct`/`enum` per `components.schemas` entry and one `async fn` per
operation, with typed path/query parameters and JSON bodies — the Rust analog of
firefly-oss's OpenAPI-generated WebClient SDK.

## Running it

```bash
cargo run -p firefly-sample-lumen-ledger-web        # boots on :8080, admin on :8081
cargo test -p firefly-sample-lumen-ledger-web       # in-process cross-crate round-trip
```

The integration test boots the whole graph in-process and drives the full
surface — create / fetch / deposit / withdraw, the paged status query, the
status transition, delete, and every problem path (404, the 422 validation
failures, the 400 malformed-path/missing-query) plus the OpenAPI document —
proving every layer wires together through DI alone. The `-models` test
separately proves the `@Version` optimistic-lock conflict (a stale write is
rejected, detected with `firefly::data_sqlx::is_optimistic_lock`).
