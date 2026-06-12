# firefly-cli

The `firefly` developer CLI for the Firefly Framework for Rust: scaffold new
projects, generate code artifacts, introspect a running app's actuator, and
diagnose your toolchain.

```bash
cargo install --path crates/cli   # installs the `firefly` binary
firefly --help
```

## Commands

| Command | Purpose |
| --- | --- |
| `firefly new <name>` | Scaffold a new firefly-rust project. |
| `firefly generate <kind> <name>` (alias `g`) | Generate a code artifact into the current project. |
| `firefly info` | Framework + environment information. |
| `firefly doctor` | Toolchain checks (`rustc`, `cargo`, `git`, clippy, rustfmt, docker). |
| `firefly db <init\|migrate\|upgrade\|downgrade\|status>` | Migration management (firefly-migrations). |
| `firefly openapi --format json\|yaml [-o file]` | Export an OpenAPI 3.1 document. |
| `firefly actuator <endpoint> --url <base>` | Query a running app's `/actuator/*` endpoints. |
| `firefly routes\|env\|health\|metrics --url <base>` | Remote introspection of a running app. |
| `firefly beans\|conditions` | Documented no-op (no Rust runtime analog). |

### `firefly new`

```bash
firefly new my-service --archetype web-api --features web,data,cqrs --git
firefly new my-lib --archetype library --dep-path ../../crates   # local dev deps
firefly new --list                                               # archetypes + features
firefly new svc --dry-run                                        # plan without writing
```

Archetypes: `core`, `web-api`, `web`, `hexagonal`, `library`, `cli`.

Each project is a workspace-less Cargo crate with git/path/version-configurable
`firefly-*` dependencies (`--dep-path` / `--dep-version`, defaulting to the
canonical GitHub repo), a `src/` tree appropriate to the archetype,
`firefly.yaml`, `.gitignore`, `README.md`, a `Dockerfile`, and `tests/`.
`--git` initializes a repository with an initial commit; `--force` overwrites
existing files.

### `firefly generate`

```bash
firefly generate handler Order
firefly generate entity Product          # data-aware when firefly.yaml enables relational data
firefly generate command OpenWallet      # command + handler in src/cqrs/
firefly generate migration AddUsers      # V###__add_users.sql in migrations/
firefly g saga MoneyTransfer --dry-run
```

Artifact kinds: `handler`, `route`, `entity`, `repository`, `dto`, `aggregate`,
`command`, `query`, `saga`, `migration`. Names are accepted in any case and
converted as needed; `--force` overwrites and `--dry-run` plans without writing.
The current project's package, archetype, and feature flags are detected from
`Cargo.toml` + `firefly.yaml`.

### `firefly db`

```bash
firefly db init                                  # migrations/ + starter V001__init.sql
firefly db migrate -m "create users"             # writes V002__create_users.sql
firefly db upgrade --url sqlite://app.db          # apply pending migrations
firefly db status --url sqlite://app.db           # show applied + pending
```

Migration management on top of `firefly-migrations` (the runner the generated
projects already ship with). The subcommand *names* mirror pyfly's `pyfly db`
(`init`/`migrate`/`upgrade`/`downgrade`/status), but the engine differs: pyfly
drives **Alembic**, this drives the framework's own **forward-only** runner.

The database URL resolves from `--url`, then `$DATABASE_URL`, then
`firefly.datasource.url` in `firefly.yaml`, defaulting to `sqlite://firefly.db`.
The fully-wired CLI backend is **SQLite via `rusqlite`**; a `postgres://` /
`mysql://` URL returns a clear "not wired into the CLI" error (adapt the
`firefly_migrations::Database` port to your driver and call `run` directly).

### `firefly openapi`

```bash
firefly openapi                                  # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml     # YAML to a file
```

Exports an OpenAPI 3.1 document built with `firefly-openapi`. The flags
(`--format json|yaml`, `-o/--output`) and wire shape match pyfly; the document
metadata (`info.title` / `info.version` / `description`) is read from
`firefly.yaml` (`firefly.app.*`) then `Cargo.toml`.

### `firefly actuator` / remote introspection

```bash
firefly actuator health --url http://localhost:8080
firefly actuator metrics requests --url http://localhost:8080 --json
firefly routes --url http://localhost:8080         # -> /actuator/mappings
firefly env    --url http://localhost:8080         # -> /actuator/env
firefly health --url http://localhost:8080         # -> /actuator/health
firefly metrics requests --url http://localhost:8080
```

Remote-only: a compiled binary has no offline DI context to boot, so `--url` is
required. `routes` maps to `/actuator/mappings`; `env`/`health`/`metrics` map
1:1. `beans` and `conditions` have **no local Rust analog** (generated apps
have no runtime DI container to enumerate and no auto-configuration condition
report); they fail with an explanatory message unless `--url` is given to a
running app that happens to expose those endpoints.

## pyfly parity

This crate is the Rust port of pyfly's `pyfly.cli` package, adapted to a
compiled Cargo workspace:

- **`naming`** — `Names` case-conversion, a hand-rolled port of `naming.py`
  (no `heck`); the naming table tests are ported verbatim from
  `tests/cli/test_naming.py`.
- **`project`** — `detect_project` / `feature_flags`, ported from `_project.py`
  but keyed off `Cargo.toml` + `firefly.yaml` and Rust's flat `src/` layout.
- **`generate`** — `Artifact` / `write_artifacts` with the same `force`/`dry_run`
  semantics as `generate.py`, plus a per-kind dispatcher; the engine and
  dispatch tests are ported from `test_generate_engine.py` and
  `test_generate_commands.py`.
- **`templates`** — `generate_project` / archetype catalog, ported from
  `templates.py`; templates are embedded with `include_str!` and rendered with
  minijinja using the same `has_*` / `package_name` context keys.
- **`actuator`** — the remote half of `_introspect.py` only; tests use an
  in-process axum server on port 0 (no external services).
- **`db`** — `db.py`'s command group, retargeted from Alembic to
  `firefly-migrations`; tests drive an in-memory / temp-file SQLite database
  via `rusqlite` (no external server), mirroring `test_db.py`/`test_db_extra.py`.
- **`openapi`** — `openapi.py`'s export command; since a compiled binary can't
  boot an app to enumerate live routes, it emits a project-metadata-stamped
  OpenAPI 3.1 *skeleton* via `firefly-openapi`. Tests mirror `test_openapi.py`
  (3.1 version marker, `paths` present, file output).
- **`diagnostics`** — `info` / `doctor`, retargeted from Python interpreter
  probes to Rust toolchain probes.

### Deliberate divergences from pyfly

- The `fastapi-api` archetype is dropped (Rust has a single web stack, Axum).
- Generated projects are **plausible Rust** and are intentionally **not**
  compiled by the test-suite; template snapshot + tempfile tests assert the
  structural markers, exactly as the pyfly suite does.
- The interactive `questionary` wizard, Python-runtime-only commands
  (`run`/`shell`/quality wrappers/`upgrade`), and the entry-point plugin
  mechanism are out of scope per the cli brief.
- `firefly migration` numbering uses the framework's `V###__name.sql`
  convention (auto-incremented from the highest existing version) rather than
  Alembic revisions.
- `firefly db` drives the **forward-only** `firefly-migrations` runner instead
  of Alembic: `db migrate` writes a `V###__msg.sql` file (not an Alembic
  autogenerate diff), and `db downgrade` is **unsupported** (the append-only
  history has no rollback — write a corrective migration instead). pyfly's
  `current`/`history`/`heads`/`show`/`revision`/`stamp`/`merge`/`reset`
  collapse to `db status`. Only the SQLite backend is wired into the CLI.
- `firefly openapi` emits a metadata-stamped **skeleton** (empty `paths`):
  a compiled binary can't boot an arbitrary app to enumerate routes the way
  pyfly's `boot_context()` does. Wire real routes via
  `firefly_openapi::Builder` and serve them with `Builder::router()`.
- `firefly beans` / `conditions` have **no local Rust analog** (no runtime DI
  container or condition report in generated apps); they are kept as commands
  that explain the gap and can pass through to a remote app via `--url`.
