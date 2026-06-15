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
   │ autowires                #[derive(Mapper-style @Component)]  +  #[derive(Component)]
@Repository      (models) →  WalletRepository: ReactiveCrudRepository + #[firefly::repository]
   │ binds
@Entity          (models) →  Wallet
```

The `@Service` programs against the repository's **`ReactiveCrudRepository`**
trait (`save`, `find_by_id`, `count`, … returning `Mono`/`Flux`) plus the
`#[firefly::repository]` derived queries (`find_by_owner`, `count_by_status`) —
the same Spring Data surface, generated from the method names.

## The repository is an async bean

A real repository needs a connection pool, and opening one is asynchronous. The
`-models` crate declares the repository with an **`async fn` `#[bean]`**:

```rust,ignore
#[firefly::bean]
impl WalletPersistenceConfig {
    #[bean]
    async fn wallet_repository(&self) -> WalletRepository {
        WalletRepository::new(connect_and_migrate().await)
    }
}
```

The framework parks the factory during the synchronous `container.scan()` and
`await`s it during `Container::init_async_beans()` (run by the bootstrap right
after the scan), then publishes the result as a ready singleton — so the service
and controller resolve it normally. This is the Spring Boot pattern of a `@Bean`
that performs I/O at context-refresh time, except the I/O is `await`ed instead of
blocking a thread. By default the repository opens an in-memory SQLite database
(the sample runs and tests with no external server); set `DATABASE_URL=postgres://…`
to point it at real PostgreSQL.

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
    firefly::FireflyApplication::new("lumen-ledger").run().await
}
```

`firefly::assert_discovered(&container, min_beans, min_controllers)` guards a
forgotten crate at startup. This one line is the only wiring a layered service
needs; everything else is discovered.

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

The integration test boots the whole graph in-process and drives
`POST/GET /api/v1/wallets`, deposit/withdraw, list-by-owner, the RFC 9457 404,
and the OpenAPI document — proving every layer wires together through DI alone.
