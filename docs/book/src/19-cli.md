# The CLI

So far you have built Lumen by hand — a file at a time, `cargo build` after
each chapter. By the end of this chapter you will know the other way to start a
service like Lumen: the `firefly` developer CLI scaffolds a project, generates
the same artifacts the earlier chapters wrote by hand, runs the binary with
profiles and overrides, manages migrations, exports an OpenAPI document, and
introspects a running Lumen over its actuator surface — all from one binary
built for a compiled Cargo workspace.

> **One binary, the whole lifecycle.** `firefly` scaffolds a project, generates
> code artifacts, runs the binary with profiles and overrides, stamps
> build-info, manages migrations, exports OpenAPI, and introspects a running
> service — the everyday developer loop in a single command-line tool
> (`new` / `run` / `generate` / `db` / `doctor` / `sbom` / `license`).

## Installing

```bash
cargo install --path crates/cli   # installs the `firefly` binary
firefly --help                     # prints the banner + every command
firefly --version                  # 26.6.4
```

## Command overview

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
| `firefly actuator <endpoint> --url <base>`           | query a running app's `/actuator/*`           |
| `firefly routes\|env\|health\|metrics --url <base>`  | remote introspection of a running app         |
| `firefly beans\|conditions --url <base>`             | DI / auto-config report of a running app      |
| `firefly completion <shell>`                         | print a shell-completion script               |
| `firefly sbom [--json]`                              | software bill of materials from `Cargo.lock`  |
| `firefly license`                                    | framework + dependency license report         |

## Scaffolding a project

`firefly new` generates a workspace-less Cargo crate with a `src/` tree, a
`firefly.yaml`, a `.gitignore`, a `README.md`, a `Dockerfile`, and `tests/` —
roughly the shape Lumen started from in chapter 2:

```bash
firefly new lumen --archetype web-api --features web,data,cqrs --git
firefly new my-lib --archetype library --dep-path ../../crates   # local dev deps
firefly new --list                                               # archetypes + features
firefly new svc --dry-run                                        # plan without writing
```

Archetypes: `core`, `web-api`, `web`, `hexagonal`, `library`, `cli`. The
generated `firefly-*` dependencies are git / path / version configurable
(`--dep-path` / `--dep-version`, defaulting to the canonical GitHub repo).
`--git` initializes a repository with an initial commit; `--force` overwrites an
existing target directory.

> **Archetypes scaffold a working service.** `firefly new --archetype web-api`
> stamps the entry point, a controller, and the dependency set so the first
> `cargo run` boots. `--features` selects the opt-in adapters the
> [facade chapter](./21-declarative-macros.md) lists
> (`web`, `data`, `cqrs`, `eda`, `cache`, `security`, …).

## Generating artifacts

`firefly generate` writes a code artifact into the current project, detecting the
package, archetype, and feature flags from `Cargo.toml` + `firefly.yaml`. These
are the same pieces you wrote by hand for Lumen — a command + handler, an
aggregate, a saga:

```bash
firefly generate command OpenWallet      # command + handler in src/cqrs/
firefly generate query   GetWallet        # query  + handler
firefly generate aggregate Wallet         # a #[derive(AggregateRoot)] skeleton
firefly generate saga    MoneyTransfer --dry-run
firefly generate migration AddWallets     # V###__add_wallets.sql in migrations/
firefly g handler Deposit                 # `g` is the alias
```

Artifact kinds: `handler`, `route`, `entity`, `repository`, `dto`, `aggregate`,
`command`, `query`, `saga`, `migration`. Names are accepted in any case and
converted as needed; `--force` overwrites, `--dry-run` plans without writing.

## Running the app

`firefly run` is a thin wrapper over `cargo run` that maps profile and
configuration flags to the `FIREFLY_*` environment variables the framework reads
at startup, then exec's Cargo:

```bash
firefly run                                  # cargo run
firefly run -p dev -p test                   # FIREFLY_PROFILES_ACTIVE=dev,test
firefly run -D server.port=9090              # FIREFLY_SERVER_PORT=9090
firefly run --env LUMEN_ADDR=0.0.0.0:8080    # a raw env var for the process
firefly run --debug                          # FIREFLY_LOGGING_LEVEL_ROOT=DEBUG
firefly run --release --bin lumen            # cargo run --release --bin lumen
firefly run --dry-run                        # print the resolved env + cargo command
```

> **Flags become `FIREFLY_*` env vars.** `firefly run -p dev -D server.port=9090`
> maps to `FIREFLY_PROFILES_ACTIVE=dev` and `FIREFLY_SERVER_PORT=9090`, then
> exec's `cargo run`. The resolution order is CLI flag → env → config file. A
> Firefly service is a single compiled binary, so there is no live-reload or
> worker-process selection — you rebuild and rerun.

For Lumen specifically, recall that `main.rs` reads `LUMEN_ADDR` /
`LUMEN_ADMIN_ADDR` directly, so the equivalent of the two-port bind is:

```bash
firefly run --bin lumen \
  --env LUMEN_ADDR=127.0.0.1:8080 \
  --env LUMEN_ADMIN_ADDR=127.0.0.1:8081
```

## Building for release

Plain compilation is `cargo build`; the `build` group adds the two artifacts a
release pipeline needs:

```bash
firefly build info                       # write build-info.json (git SHA + UTC time)
firefly build info -o target/build-info.json
firefly build image -t lumen:1.0.0       # OCI image via Cloud Native Buildpacks (`pack`)
firefly build image --builder docker     # or a plain Dockerfile build
```

`build info` writes the `build-info.json` that `/actuator/info` surfaces — the
admin-port `info` endpoint chapter 15 wired for Lumen reads it when present, so
the git SHA and build time show up next to the `InfoContributor` block.

## Database migrations

Lumen runs over an in-memory event store, so it ships no SQL migrations — but
the moment you swap in the Postgres event store from chapter 20, `firefly db`
manages the schema. It wraps the
[`firefly-migrations`](./07-persistence.md) forward-only runner:

```bash
firefly db init                            # migrations/ + starter V001__init.sql
firefly db migrate -m "create wallets"     # writes V002__create_wallets.sql
firefly db upgrade --url sqlite://app.db   # apply pending migrations
firefly db status  --url sqlite://app.db   # show applied + pending
```

The database URL resolves from `--url`, then `$DATABASE_URL`, then
`firefly.datasource.url` in `firefly.yaml`, defaulting to `sqlite://firefly.db`.

> **Forward-only migrations.** `firefly db` drives Firefly's own forward-only
> migration runner. Because the runner is forward-only, `firefly db downgrade`
> is unsupported — write a corrective migration instead.

> **Note** — The CLI migration backend is **SQLite via `rusqlite`**; a
> `postgres://` / `mysql://` URL returns a clear "not wired into the CLI" error.
> For another driver, adapt the `firefly_migrations::Database` port and call
> `run` directly from your build, rather than through the CLI.

## Exporting OpenAPI

```bash
firefly openapi                           # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
```

The document metadata (`info.title` / `info.version` / `description`) is read
from `firefly.yaml` then `Cargo.toml`. A compiled binary cannot boot an
arbitrary app to enumerate live routes, so the CLI emits a metadata-stamped
**skeleton**; to emit Lumen's real routes, build them with
`firefly_openapi::Builder` (which reads the `#[rest_controller]` route table) and
serve them with `Builder::router()`.

## Introspecting a running app

These commands query a *running* Lumen over HTTP — a compiled binary has no
offline DI context to boot, so `--url` is required. Point them at Lumen's admin
port (the actuator surface from chapter 15):

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

> **Introspecting a running service.** `firefly beans` renders the
> [DI container's bean table](./04a-dependency-injection.md), `firefly conditions`
> the conditional-bean evaluation report, and `firefly routes` the route table —
> all read over HTTP from a running service's actuator port
> (`/actuator/beans`, `/actuator/conditions`, and `/actuator/mappings`).

## Diagnosing, completing, and auditing

```bash
firefly info                # framework version + which optional adapters are built
firefly doctor              # checks rustc, cargo, git, clippy, rustfmt, docker
firefly completion zsh      # > ~/.zfunc/_firefly  (bash | zsh | fish | powershell)
firefly sbom                # a software bill of materials from Cargo.lock
firefly sbom --json         # machine-readable, for a compliance pipeline
firefly license             # the framework + dependency license report
```

`firefly doctor` is the first thing to run on a fresh machine: it reports your
`rustc` / `cargo` versions and whether `git`, `clippy`, `rustfmt`, and `docker`
are on the `PATH`, ending with "All checks passed!" or a list of what to fix.

## Running through cargo

If you have not installed the binary, drive the CLI through cargo from a
framework checkout:

```bash
make cli ARGS="doctor"
make cli ARGS="new orders --archetype web-api"
cargo run -p firefly-cli --bin firefly -- info
```

## What changed in Lumen

Nothing in `samples/lumen` itself — this chapter is operational. But you saw the
CLI path to every artifact Lumen grew by hand: `firefly new --archetype web-api`
scaffolds the chapter-2 skeleton, `firefly generate command/query/aggregate/saga`
writes the CQRS, DDD, and orchestration pieces, `firefly run --bin lumen`
launches it with `LUMEN_ADDR` overrides, and `firefly health/routes/beans
--url :8081` introspects the actuator surface from chapter 15. The CLI never
invents APIs — every command maps to a framework crate (`firefly-migrations`,
`firefly-openapi`, the actuator endpoints) you have already met.

## Exercises

1. **Scaffold a Lumen twin.** Run `firefly new lumen2 --archetype web-api
   --features web,cqrs --dry-run`, then without `--dry-run`. Compare the
   generated `src/` tree to Lumen's, and `cargo build` it.
2. **Generate the CQRS pieces.** In the scaffolded project, run `firefly
   generate command OpenWallet` and `firefly generate query GetWallet`. Inspect
   the generated handlers and note how they match the `#[command_handler]` /
   `#[query_handler]` shape from chapter 9.
3. **Run with a profile and an override.** Start the app with `firefly run -p
   dev -D server.port=9090 --dry-run` and read the resolved `FIREFLY_*`
   environment it would export. Then drop `--dry-run` and confirm the port.
4. **Introspect the real Lumen.** `cargo run --bin lumen`, then in another shell
   run `firefly health --url http://localhost:8081`, `firefly routes --url
   http://localhost:8081`, and `firefly beans --url http://localhost:8081`.
   Match the route table against the endpoint table in `web.rs`.

With a project scaffolded, generated, run, and introspected, the next chapter
takes Lumen all the way to production. Continue to
[Production & Deployment](./20-production.md).
