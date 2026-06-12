# The CLI

The `firefly` developer CLI scaffolds projects, generates code artifacts,
manages migrations, exports OpenAPI, introspects a running app's actuator, and
diagnoses your toolchain. It is the Rust port of pyfly's `pyfly` CLI, adapted to
a compiled Cargo workspace.

## Installing

```bash
cargo install --path crates/cli   # installs the `firefly` binary
firefly --help
```

## Command overview

| Command                                              | Purpose                                     |
|------------------------------------------------------|---------------------------------------------|
| `firefly new <name>`                                 | scaffold a new firefly-rust project         |
| `firefly generate <kind> <name>` (alias `g`)         | generate a code artifact                    |
| `firefly info`                                       | framework + environment information         |
| `firefly doctor`                                     | toolchain checks (rustc, cargo, git, …)     |
| `firefly db <init\|migrate\|upgrade\|status>`        | migration management                        |
| `firefly openapi --format json\|yaml [-o file]`      | export an OpenAPI 3.1 document              |
| `firefly actuator <endpoint> --url <base>`           | query a running app's `/actuator/*`         |
| `firefly routes\|env\|health\|metrics --url <base>`  | remote introspection of a running app       |

## Scaffolding a project

`firefly new` generates a workspace-less Cargo crate with a `src/` tree, a
`firefly.yaml`, a `.gitignore`, a `README.md`, a `Dockerfile`, and `tests/`:

```bash
firefly new my-service --archetype web-api --features web,data,cqrs --git
firefly new my-lib --archetype library --dep-path ../../crates   # local dev deps
firefly new --list                                               # archetypes + features
firefly new svc --dry-run                                        # plan without writing
```

Archetypes: `core`, `web-api`, `web`, `hexagonal`, `library`, `cli`. The
`firefly-*` dependencies are git / path / version configurable
(`--dep-path` / `--dep-version`, defaulting to the canonical GitHub repo).
`--git` initializes a repository with an initial commit; `--force` overwrites.

## Generating artifacts

`firefly generate` writes a code artifact into the current project, detecting the
package, archetype, and feature flags from `Cargo.toml` + `firefly.yaml`:

```bash
firefly generate handler Order
firefly generate entity Product          # data-aware when relational data is enabled
firefly generate command OpenWallet      # command + handler in src/cqrs/
firefly generate migration AddUsers      # V###__add_users.sql in migrations/
firefly g saga MoneyTransfer --dry-run
```

Artifact kinds: `handler`, `route`, `entity`, `repository`, `dto`, `aggregate`,
`command`, `query`, `saga`, `migration`. Names are accepted in any case and
converted as needed; `--force` overwrites, `--dry-run` plans.

## Database migrations

`firefly db` wraps the [`firefly-migrations`](./07-persistence.md) forward-only
runner:

```bash
firefly db init                           # migrations/ + starter V001__init.sql
firefly db migrate -m "create users"      # writes V002__create_users.sql
firefly db upgrade --url sqlite://app.db   # apply pending migrations
firefly db status  --url sqlite://app.db   # show applied + pending
```

The database URL resolves from `--url`, then `$DATABASE_URL`, then
`firefly.datasource.url` in `firefly.yaml`, defaulting to `sqlite://firefly.db`.

> **Note** — The CLI backend is **SQLite via `rusqlite`**; a `postgres://` /
> `mysql://` URL returns a clear "not wired into the CLI" error — adapt the
> `firefly_migrations::Database` port to your driver and call `run` directly.
> The runner is forward-only, so `db downgrade` is unsupported (write a
> corrective migration instead).

## Exporting OpenAPI

```bash
firefly openapi                           # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
```

The document metadata (`info.title` / `info.version` / `description`) is read
from `firefly.yaml` then `Cargo.toml`. A compiled binary cannot boot an
arbitrary app to enumerate live routes, so the CLI emits a metadata-stamped
**skeleton**; to emit real routes, build them with `firefly_openapi::Builder`
and serve them with `Builder::router()`.

## Introspecting a running app

These commands query a running app's actuator over HTTP — a compiled binary has
no offline DI context to boot, so `--url` is required:

```bash
firefly actuator health --url http://localhost:8080
firefly actuator metrics requests --url http://localhost:8080 --json
firefly routes  --url http://localhost:8080   # -> /actuator/mappings
firefly env     --url http://localhost:8080   # -> /actuator/env
firefly health  --url http://localhost:8080   # -> /actuator/health
firefly metrics requests --url http://localhost:8080
```

## Diagnosing your environment

```bash
firefly info     # framework + environment information
firefly doctor   # checks rustc, cargo, git, clippy, rustfmt, docker
```

## Running through cargo

If you have not installed the binary, drive the CLI through cargo from a
framework checkout:

```bash
make cli ARGS="doctor"
make cli ARGS="new orders --archetype web-api"
cargo run -p firefly-cli --bin firefly -- info
```

With a project scaffolded and code generated, the last chapter takes it to
production. Continue to [Production & Deployment](./20-production.md).
