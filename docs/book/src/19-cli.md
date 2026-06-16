# The CLI

So far you have built **Lumen** — the digital-wallet and ledger service from
[Quickstart](./02-quickstart.md) onward — by hand: a file at a time, a
`cargo build` after each chapter. That was deliberate, so every line is
something you typed and understand. This chapter teaches the *other* way to do
the same work: the `firefly` developer CLI. It is a single compiled binary that
scaffolds a project, generates the same artifacts the earlier chapters wrote by
hand, runs the binary with profiles and config overrides, stamps build metadata,
manages migrations, exports an OpenAPI document, and introspects a *running*
Lumen over its actuator surface — the everyday developer loop in one tool.

Nothing in this chapter changes `samples/lumen` itself; it is purely
operational. But by the end you will be able to drive the whole lifecycle from
the command line, and — just as importantly — you will know exactly which
framework crate each command talks to, because the CLI never invents an API. It
calls the same `firefly-migrations`, `firefly-openapi`, and actuator endpoints
you have already met.

By the end of this chapter you will:

- Install the `firefly` binary and read its command catalogue.
- Scaffold a new service two ways — picking an *archetype* and turning on
  *features* — and preview the exact file plan with `--dry-run`.
- Generate individual code artifacts (a CQRS command, a query, an aggregate, a
  saga, a migration) into an existing project, and read what the generators
  actually emit.
- Run a Firefly app through the CLI, mapping profile and override flags to the
  `FIREFLY_*` environment variables the framework reads at startup.
- Introspect a *running* Lumen — its health, routes, beans, and metrics — over
  the actuator port, and understand why a compiled binary requires `--url`.

## Concepts you will meet

Before the first command, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — archetype.** An *archetype* is a project template that
> decides the starting shape of your crate: which modules exist, which Firefly
> features are switched on, and what the example code looks like. The CLI ships
> six (`core`, `web-api`, `web`, `hexagonal`, `library`, `cli`). The Spring
> analog is a Spring Initializr "project type" plus its preselected
> dependencies.

> **Note** **Key term — feature.** A *feature* is an opt-in subsystem the
> scaffold wires in — `web`, `data`, `cqrs`, `eda`, `cache`, `security`, and so
> on. Each maps to one or more `firefly-*` crates added to the generated
> `Cargo.toml`. In Spring terms, choosing a feature is like ticking a starter on
> the Initializr.

> **Note** **Key term — actuator surface.** The *actuator surface* is the set of
> operational HTTP endpoints — `/actuator/health`, `/actuator/info`,
> `/actuator/metrics`, `/actuator/mappings`, `/actuator/beans`,
> `/actuator/conditions`, `/actuator/env` — that a running Firefly app serves on
> its **management** port (`8081` by default), separate from the public API on
> `8080`. This mirrors Spring Boot Actuator. The CLI's introspection commands
> are thin clients over these endpoints.

> **Note** **Key term — Command/Query Responsibility Segregation (CQRS).** A
> pattern that routes state-changing **commands** and read-only **queries**
> through separate handlers on a shared *bus*. You built Lumen's command and
> query handlers in [CQRS](./09-cqrs.md); the CLI can scaffold the same pieces
> for a new project with `firefly generate command` / `firefly generate query`.

## Step 1 — Install the binary

The CLI lives in the framework's `crates/cli`. Install it from a checkout, then
ask it to describe itself.

```bash
cargo install --path crates/cli   # installs the `firefly` binary
firefly --help                     # prints the banner + every command
firefly --version                  # 26.6.28
```

What just happened: `cargo install` compiled the `firefly` binary and put it on
your `PATH`. `--version` prints the framework calendar version — the same
`26.6.28` Lumen depends on, because the CLI is versioned with the rest of the
workspace.

> **Tip** **Checkpoint.** `firefly --version` prints `26.6.28` and
> `firefly --help` lists subcommands including `new`, `generate`, `run`, `db`,
> `openapi`, `doctor`, and `health`. If `firefly` is "command not found", make
> sure `~/.cargo/bin` is on your `PATH`.

If you would rather not install the binary, you can drive the CLI through Cargo
from a framework checkout — see [Step 9](#step-9--run-the-cli-through-cargo).

## Step 2 — Read the command catalogue

The whole developer loop fits in one table. Skim it now; the rest of the chapter
walks the commands you will use most.

| Command                                              | Purpose                                       |
|------------------------------------------------------|-----------------------------------------------|
| `firefly new <name>`                                 | scaffold a new firefly-rust project           |
| `firefly generate <kind> <name>` (alias `g`)         | generate a code artifact                      |
| `firefly run`                                        | `cargo run` with profile / override flags     |
| `firefly build <info\|image>`                        | stamp `build-info.json` / build an OCI image  |
| `firefly info`                                       | framework + environment information           |
| `firefly doctor`                                     | toolchain checks (rustc, cargo, git, …)       |
| `firefly db <init\|migrate\|upgrade\|status>`        | migration management                          |
| `firefly openapi --format json\|yaml [-o file]`      | export an OpenAPI 3.1 document                |
| `firefly openapi-client --spec <file>`               | generate a typed Rust client from a spec      |
| `firefly actuator <endpoint> --url <base>`           | query a running app's `/actuator/*`           |
| `firefly routes\|env\|health\|metrics --url <base>`  | remote introspection of a running app         |
| `firefly beans\|conditions --url <base>`             | DI / auto-config report of a running app      |
| `firefly completion <shell>`                         | print a shell-completion script               |
| `firefly sbom [--json]`                              | software bill of materials from `Cargo.lock`  |
| `firefly license`                                    | framework + dependency license report         |

What just happened: that is the full surface. Notice the shape of the loop —
*scaffold* (`new`), *grow* (`generate`), *run* (`run`), *package* (`build`),
*operate* (`db`, `openapi`), *introspect* (`actuator`, `routes`, `health`, …),
and *audit* (`doctor`, `sbom`, `license`). Every command maps to a framework
crate or an actuator endpoint you have already met.

## Step 3 — Scaffold a project

`firefly new` generates a workspace-less Cargo crate: a `src/` tree shaped by the
archetype, a `firefly.yaml`, a `.gitignore`, a `README.md`, a `Dockerfile`, and
a `tests/` directory. It is the same starting shape Lumen had after
[Quickstart](./02-quickstart.md).

```bash
firefly new lumen2 --archetype web-api --features web,data,cqrs --git
firefly new my-lib --archetype library --dep-path ../../             # local dev deps
firefly new --list                                                   # archetypes + features
firefly new svc --dry-run                                            # plan without writing
```

What just happened, command by command:

- The first line scaffolds a `web-api` project named `lumen2` with the `web`,
  `data`, and `cqrs` features switched on, and (because of `--git`) initializes a
  Git repository with an initial commit.
- `--dep-path ../../` points the generated `firefly-*` dependencies at a local
  workspace checkout instead of the canonical GitHub repo. Each crate resolves
  into its own `crates/<subdir>` automatically.
- `--list` prints the archetype and feature catalogues, then exits without
  creating anything.
- `--dry-run` prints the exact file plan — every path that *would* be written —
  without touching the filesystem.

The six archetypes are `core`, `web-api`, `web`, `hexagonal`, `library`, and
`cli`. The `web-api` archetype stamps an entry point, a controller, and the
layered `models/services/repositories` tree wired against the real web starter,
so the very first `cargo run` boots. The generated `firefly-*` dependency source
is configurable: `--dep-path <base>` for a local checkout, `--dep-version <ver>`
for a published crates.io release, otherwise the canonical Git repo. `--force`
overwrites an existing target directory.

> **Note** A feature you do not select is simply absent from the generated
> `Cargo.toml` — picking `web,data,cqrs` adds `firefly-web`, `firefly-data` +
> `firefly-migrations`, and `firefly-cqrs`, and nothing else. The full feature
> list (`web`, `data`, `mongodb`, `eda`, `cache`, `client`, `security`,
> `scheduling`, `observability`, `cqrs`, `shell`, `transactional`) is printed by
> `firefly new --list`, and the underlying crates are the ones the
> [macros chapter](./21-declarative-macros.md) catalogues.

> **Tip** **Checkpoint.** Run `firefly new lumen2 --archetype web-api --dry-run`.
> You should see a plan listing `Cargo.toml`, `firefly.yaml`, `.gitignore`,
> `README.md`, `Dockerfile`, `src/main.rs`, `src/lib.rs`, `src/controllers.rs`,
> the `models/services/repositories` tree, and `tests/api.rs` — with nothing
> written to disk. Drop `--dry-run` and the same files appear under `lumen2/`.

## Step 4 — Generate individual artifacts

Once a project exists, `firefly generate` (alias `g`) writes one artifact at a
time into it, detecting the package, archetype, and feature flags from
`Cargo.toml` + `firefly.yaml`. These are exactly the pieces you wrote by hand for
Lumen — a command and its handler, a query, an aggregate, a saga, a migration.

```bash
firefly generate command OpenWallet      # src/cqrs/open_wallet_command{,_handler}.rs
firefly generate query   GetWallet       # src/cqrs/get_wallet_query{,_handler}.rs
firefly generate aggregate Wallet        # src/domain/wallet.rs (embeds AggregateRoot)
firefly generate saga    MoneyTransfer --dry-run
firefly generate migration AddWallets    # migrations/V###__add_wallets.sql
firefly g handler Deposit                # `g` is the alias
```

The artifact kinds are `handler`, `route`, `entity`, `repository`, `dto`,
`aggregate`, `command`, `query`, `saga`, and `migration`. Names are accepted in
any case and converted as needed (`OpenWallet`, `open-wallet`, and `open_wallet`
all produce the same files). `--force` overwrites an existing file; `--dry-run`
plans without writing.

What just happened, with the two CQRS generators as the worked example. A
`generate command OpenWallet` writes **two** files into `src/cqrs/`:

```rust,ignore
// src/cqrs/open_wallet_command.rs
use firefly_cqrs::{CqrsError, Message};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenWallet {
    /// Target aggregate identifier.
    pub id: String,
}

impl Message for OpenWallet {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.id.trim().is_empty() {
            return Err(CqrsError::validation("id is required"));
        }
        Ok(())
    }
}
```

```rust,ignore
// src/cqrs/open_wallet_command_handler.rs
use firefly_cqrs::{Bus, CqrsError};

use super::open_wallet_command::OpenWallet;

/// Register the `OpenWallet` command handler on `bus`. Call once at startup.
pub fn register_open_wallet_handler(bus: &Bus) {
    bus.register(|command: OpenWallet| async move {
        // Implement the OpenWallet command behaviour here.
        Ok::<_, CqrsError>(command.id)
    });
}
```

The command is a plain message struct implementing `firefly_cqrs::Message`
(its `validate` runs in the bus's validation middleware before the handler).
The handler is a `register_<name>_handler(bus: &Bus)` *registrar function* that
calls the closure-based `bus.register(...)` — the same registration shape you
used in [CQRS](./09-cqrs.md). `generate query GetWallet` mirrors this with a
`GetWallet` query struct and a `register_get_wallet_handler(bus: &Bus)`.

> **Note** The generators target the real `firefly-*` APIs, not placeholder
> bodies. `generate aggregate Wallet` writes `src/domain/wallet.rs` with a struct
> that embeds `firefly_eventsourcing::AggregateRoot` (the uncommitted-events
> buffer), exposing `raise(...)` and `take_events(...)`. `generate saga
> MoneyTransfer` writes `src/sagas/money_transfer_saga.rs` with a
> `build_money_transfer_saga()` function over the `firefly_orchestration::Saga`
> builder — `Saga::new("money-transfer")`, `Step::new(...)`,
> `.with_compensation(...)`. These are the same constructs you met in
> [Event Sourcing](./11-event-sourcing.md) and [Sagas](./12-sagas.md).

> **Tip** **Checkpoint.** Inside a scaffolded project, run
> `firefly generate command OpenWallet --dry-run`. You should see a plan naming
> `src/cqrs/open_wallet_command.rs` and `src/cqrs/open_wallet_command_handler.rs`
> as `create` actions, with nothing written.

## Step 5 — Run the app

`firefly run` is a thin wrapper over `cargo run`. It maps profile and config
override flags to the `FIREFLY_*` environment variables the framework reads at
startup, then exec's Cargo from the detected project root.

> **Note** **Key term — config-override flag.** A `-D key=value` flag overrides
> one configuration value. The CLI maps it to an environment variable by
> stripping a leading `firefly.`, upper-casing, and replacing `.`/`-` with `_`,
> then prepending `FIREFLY_`. So `-D logging.level-root=DEBUG` becomes
> `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`. This is the same env convention
> [Configuration](./03-configuration.md) describes.

```bash
firefly run                                  # cargo run
firefly run -p dev -p test                   # FIREFLY_PROFILES_ACTIVE=dev,test
firefly run -D logging.level-root=DEBUG      # FIREFLY_LOGGING_LEVEL_ROOT=DEBUG
firefly run --env FIREFLY_SERVER_ADDR=0.0.0.0:8080  # a raw env var for the process
firefly run --debug                          # FIREFLY_LOGGING_LEVEL_ROOT=DEBUG
firefly run --release --bin lumen            # cargo run --release --bin lumen
firefly run --dry-run                        # print the resolved env + cargo command
```

What just happened: the flags resolve into an environment that is applied before
`cargo run`. `-p`/`--profile` is repeatable or comma-separated and flattens into
a single `FIREFLY_PROFILES_ACTIVE`; `-D key=value` maps to `FIREFLY_<KEY>`;
`--env KEY=VALUE` passes a raw variable straight through; `--debug` is shorthand
for `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`; `--release` and `--bin <name>` pass
through to Cargo. A Firefly service is a single compiled binary, so there is no
live-reload or worker-process selection — you rebuild and rerun. `--dry-run`
prints the resolved environment and the exact `cargo run` command without
executing, which is the safest way to learn the mapping.

> **Warning** A `-D` override only takes effect if the framework actually reads
> that key. Lumen binds its two ports from `FIREFLY_SERVER_ADDR` /
> `FIREFLY_MANAGEMENT_ADDR` (a full `host:port`), not from a `server.port` key —
> so to move Lumen's ports, set the address env vars directly. The equivalent of
> the two-port bind is:

```bash
firefly run --bin lumen \
  --env FIREFLY_SERVER_ADDR=127.0.0.1:8080 \
  --env FIREFLY_MANAGEMENT_ADDR=127.0.0.1:8081
```

This is the same seam [Quickstart](./02-quickstart.md) used with raw
`FIREFLY_*` variables — `firefly run --env` just sets them for you.

> **Tip** **Checkpoint.** Run `firefly run -p dev -D logging.level-root=DEBUG
> --dry-run` from inside a project. The output prints `Would run: cargo run` and
> an environment block listing `FIREFLY_PROFILES_ACTIVE=dev` and
> `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`. Nothing is launched.

## Step 6 — Build for release

Plain compilation is `cargo build`. The `build` group adds the two artifacts a
release pipeline needs on top of the compiled binary.

```bash
firefly build info                       # write build-info.json (git SHA + UTC time)
firefly build info -o target/build-info.json
firefly build image -t lumen:1.0.0       # OCI image via Cloud Native Buildpacks (`pack`)
firefly build image --builder docker     # or a plain Dockerfile build
```

What just happened: `build info` writes a `build-info.json` of the shape
`{"git": {"sha": …}, "build": {"time": …}}` (an empty SHA when git is
unavailable). That file is the data source the `/actuator/info` build
contributor reads when present, so the git SHA and build time appear next to the
`InfoContributor` block you wired in [Observability](./15-observability.md).
`build image` builds an OCI image — by default via Cloud Native Buildpacks (the
`pack` tool), or with `--builder docker` against the scaffolded `Dockerfile`.

> **Tip** **Checkpoint.** Run `firefly build info -o /tmp/build-info.json` and
> open the file. It is valid JSON with a top-level `git` and `build` object, and
> `build.time` is an RFC 3339 UTC timestamp ending in `Z`.

## Step 7 — Manage database migrations

Lumen runs over an in-process event store, so it ships **no** SQL migrations —
its `samples/lumen` tree has no `migrations/` directory at all. But the moment
you swap in the Postgres event store from
[Production & Deployment](./20-production.md), `firefly db` manages the schema.
It drives the framework's own forward-only migration runner, the same
[`firefly-migrations`](./07-persistence.md) library the generated projects ship
with.

```bash
firefly db init                            # migrations/ + starter V001__init.sql
firefly db migrate -m "create wallets"     # writes V002__create_wallets.sql
firefly db upgrade --url sqlite://app.db   # apply pending migrations
firefly db status  --url sqlite://app.db   # show applied + pending
```

What just happened: `db init` creates the `migrations/` directory with a starter
`V001__init.sql`; `db migrate -m <msg>` writes a new empty
`V###__<slug>.sql` with the version auto-incremented from the highest existing
migration; `db upgrade` applies every pending migration (idempotently — a
re-run applies zero); `db status` reports applied and pending migrations. The
database URL resolves from `--url`, then `$DATABASE_URL`, then
`firefly.datasource.url` in `firefly.yaml`, defaulting to `sqlite://firefly.db`.

> **Note** The migration runner is **forward-only** (an append-only history,
> Flyway-style). Because of that there is no `firefly db downgrade` — running it
> fails loudly rather than silently no-op'ing. To undo a change, write a new
> corrective migration with `firefly db migrate` instead.

> **Warning** The CLI's migration backend is **SQLite via `rusqlite`**. A
> `postgres://` or `mysql://` URL returns a clear "not wired into the CLI" error.
> For another driver in production, adapt the `firefly_migrations::Database` port
> and call `firefly_migrations::run` directly from your build, rather than
> through the convenience CLI.

> **Tip** **Checkpoint.** In a scratch directory run `firefly db init`, then
> `firefly db status --url ":memory:"`. You should see one *pending* migration
> (`V001__init.sql`) and zero applied, because each `:memory:` connection starts
> empty.

## Step 8 — Export OpenAPI and generate clients

The CLI can emit an OpenAPI document for the current project and, going the other
direction, generate a typed Rust client from any spec.

```bash
firefly openapi                           # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
firefly openapi-client --spec openapi.json -o client.rs --client-name WalletClient
```

What just happened: `firefly openapi` reads the document metadata
(`info.title` / `info.version` / `info.description`) from `firefly.yaml`, then
`Cargo.toml`, and emits an OpenAPI 3.1 document. Because a compiled binary cannot
boot an arbitrary app to enumerate live routes, the exported document is a
metadata-stamped **skeleton** — correct `info` block and the standard
`ProblemDetail` component (Firefly renders errors as
`application/problem+json` per RFC 9457), but empty `paths`. To emit Lumen's
*real* routes, build them with `firefly_openapi::Builder` (which reads the
`#[rest_controller]` route table) and serve them with `Builder::router()` — the
live spec your app already publishes at `/v3/api-docs` on the management port.

`firefly openapi-client` is the inverse: given an OpenAPI 3.x document, it emits
a self-contained typed client over `firefly_client::RestClient` — a model
struct/enum per `components.schemas` entry and one `async fn` per operation, with
typed path parameters and JSON bodies. `--client-name` names the generated
struct (default `ApiClient`).

> **Tip** **Checkpoint.** Run `firefly openapi --format yaml | head`. The first
> line is `openapi: 3.1.0`, followed by an `info:` block carrying your project's
> title and version.

## Step 9 — Introspect a running app

These commands query a *running* Lumen over HTTP. A compiled binary has no
offline DI context to boot — there is nothing to introspect without a live
process — so `--url` is required, pointed at Lumen's **management** port (the
actuator surface from [Observability](./15-observability.md)).

```bash
firefly health  --url http://localhost:8081   # -> /actuator/health
firefly env     --url http://localhost:8081   # -> /actuator/env
firefly routes  --url http://localhost:8081   # -> /actuator/mappings
firefly metrics requests --url http://localhost:8081
firefly actuator info    --url http://localhost:8081 --json
firefly actuator metrics requests --url http://localhost:8081 --json
firefly beans      --url http://localhost:8081   # the DI container's bean table
firefly conditions --url http://localhost:8081   # the auto-configuration report
```

What just happened: each command GETs a mapped actuator endpoint and pretty-prints
the JSON. `routes` maps to `/actuator/mappings` (every `#[rest_controller]`
route), `health`/`env`/`metrics`/`info` map to their like-named endpoints, and
`beans`/`conditions` render the DI bean table and the conditional-bean
evaluation report — Spring Boot Actuator's DI introspection. `firefly actuator
<endpoint>` is the general form; `firefly health|env|routes|metrics|beans|
conditions` are convenience shortcuts. `--json` emits the raw body for piping.

> **Note** **Key term — bean.** A *bean* is an object the framework constructs
> and manages for you. `/actuator/beans` lists every one (type, scope,
> stereotype), and `/actuator/conditions` reports the `@Profile` /
> `@ConditionalOn…` guards each conditional bean declared. These are read over
> HTTP from a running service, the same way you would query Spring's `/beans` and
> `/conditions`. See [Dependency Injection](./04a-dependency-injection.md) for
> the bean container itself.

> **Tip** **Checkpoint.** In one terminal run `cargo run --bin lumen`; in
> another, run `firefly health --url http://localhost:8081`. You should see a
> JSON body with `"status":"UP"`. If `firefly routes --url …` returns an error
> about a missing in-process context, you omitted `--url` — these commands
> always require it.

## Step 10 — Diagnose, complete, and audit

The remaining commands report on your environment and dependencies.

```bash
firefly info                # framework version + which optional adapters are built
firefly doctor              # checks rustc, cargo, git, clippy, rustfmt, docker
firefly completion zsh      # > ~/.zfunc/_firefly  (bash | zsh | fish | powershell)
firefly sbom                # a software bill of materials from Cargo.lock
firefly sbom --json         # machine-readable, for a compliance pipeline
firefly license             # the framework + dependency license report
```

What just happened: `firefly doctor` is the first thing to run on a fresh
machine. It reports your `rustc` and `cargo` versions (the two *required* tools)
and whether `git`, `clippy`, `rustfmt`, and `docker` are on the `PATH` (the
*optional* ones), plus the detected project's package, archetype, and whether a
`firefly.yaml` and `migrations/` are present — ending with "All required checks
passed!" or a list of what to fix. `firefly completion <shell>` prints a
shell-completion script generated from the live CLI definition, so it always
matches the available subcommands and flags. `firefly sbom` and `firefly license`
read `Cargo.lock` to produce a Software Bill of Materials and a dependency
license report for a compliance pipeline.

> **Tip** **Checkpoint.** Run `firefly doctor`. Inside the framework workspace it
> reports `rustc` and `cargo` as passing required checks and prints a `Project`
> block. The final line is "All required checks passed!".

## Step 11 — Run the CLI through Cargo

If you have not installed the binary, drive the CLI through Cargo from a
framework checkout — handy in CI, or while iterating on the CLI itself.

```bash
make cli ARGS="doctor"
make cli ARGS="new orders --archetype web-api"
cargo run -p firefly-cli --bin firefly -- info
```

What just happened: each form runs the very same `firefly` binary, just without
installing it first. The `--` separates Cargo's own arguments from the ones
passed through to `firefly`.

## Recap — the CLI maps to crates you already know

You did not change `samples/lumen` in this chapter; it is operational. But you
saw the CLI path to every artifact Lumen grew by hand:

- `firefly new --archetype web-api` scaffolds the
  [Quickstart](./02-quickstart.md) skeleton — entry point, controller, layered
  tree, `Cargo.toml`, `firefly.yaml`, `Dockerfile`, `tests/`.
- `firefly generate command/query/aggregate/saga/migration` writes the CQRS,
  DDD, orchestration, and schema pieces — as registrar functions and real
  `firefly-*` constructs, not placeholders.
- `firefly run --bin lumen` launches it, mapping `-p`/`-D`/`--env` flags to the
  `FIREFLY_*` environment, and `--env FIREFLY_SERVER_ADDR/MANAGEMENT_ADDR` moves
  the two ports.
- `firefly build info` stamps the `build-info.json` the `/actuator/info` build
  contributor surfaces; `firefly db` drives the forward-only
  `firefly-migrations` runner once you adopt a SQL store.
- `firefly health/routes/beans/conditions --url http://localhost:8081`
  introspects the actuator surface over HTTP, which is why `--url` is mandatory:
  a compiled binary has no offline context to boot.

The throughline: the CLI never invents an API. Every command calls a framework
crate (`firefly-migrations`, `firefly-openapi`, `firefly-client`) or an actuator
endpoint you have already met, so the command line is just a faster door to the
same building.

## Exercises

1. **Scaffold a Lumen twin.** Run `firefly new lumen2 --archetype web-api
   --features web,cqrs --dry-run`, then again without `--dry-run`. Compare the
   generated `src/` tree to Lumen's, and `cargo build` it.
2. **Generate the CQRS pieces.** In the scaffolded project, run `firefly generate
   command OpenWallet` and `firefly generate query GetWallet`. Open the four
   generated files and confirm the handlers are `register_<name>_handler(bus:
   &Bus)` registrar functions calling `bus.register(...)` — the registration
   shape from [CQRS](./09-cqrs.md), not a macro.
3. **Learn the env mapping.** Start the app with `firefly run -p dev -D
   logging.level-root=DEBUG --dry-run` and read the resolved `FIREFLY_*`
   environment it would export. Then move the ports for real with `firefly run
   --bin lumen --env FIREFLY_SERVER_ADDR=127.0.0.1:9090 --env
   FIREFLY_MANAGEMENT_ADDR=127.0.0.1:9091` and `curl localhost:9091/actuator/health`.
4. **Introspect the real Lumen.** `cargo run --bin lumen`, then in another shell
   run `firefly health --url http://localhost:8081`, `firefly routes --url
   http://localhost:8081`, and `firefly beans --url http://localhost:8081`. Match
   the route table against the endpoint constants in `src/web.rs`.
5. **Audit the toolchain.** Run `firefly doctor` on your machine and note which
   optional tools (`git`, `clippy`, `rustfmt`, `docker`) are present, then run
   `firefly sbom --json | head` to see the resolved-dependency manifest the CLI
   reads from `Cargo.lock`.

## Where to go next

With a project scaffolded, generated, run, and introspected, the next chapter
takes Lumen all the way to production — swapping the in-process event store for
Postgres and Kafka, where `firefly db` and `firefly build` finally earn their
keep. Continue to **[Production & Deployment](./20-production.md)**.
