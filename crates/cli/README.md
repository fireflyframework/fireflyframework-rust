# firefly-cli

The `firefly` developer CLI for the **Firefly Framework for Rust**: scaffold new
projects that *actually compile*, generate code artifacts into them, run the app
with profile/override env mapping, stamp build metadata and build OCI images,
export an OpenAPI document, manage SQLite migrations, introspect a running app's
actuator, generate shell completions, report a Software Bill of Materials and
licenses, and diagnose your toolchain and project.

```bash
cargo install --path crates/cli   # installs the `firefly` binary
firefly --help
```

## Command reference

| Command | Purpose |
| --- | --- |
| `firefly new <name>` | Scaffold a new firefly-rust project (compiles out of the box). |
| `firefly generate <kind> <name>` (alias `g`) | Generate a code artifact into the current project. |
| `firefly run [flags]` | Run the app via Cargo, mapping `--profile`/`-D`/`--env`/`--debug` to `FIREFLY_*`. |
| `firefly build <info\|image>` | Stamp `build-info.json` for `/actuator/info`, or build an OCI image. |
| `firefly info` | Framework + environment + project information. |
| `firefly doctor` | Toolchain + project health checks. |
| `firefly db <init\|migrate\|upgrade\|downgrade\|status>` | SQLite migration management (firefly-migrations). |
| `firefly openapi --format json\|yaml [-o file]` | Export an OpenAPI 3.1 document for the project. |
| `firefly actuator <health\|info\|metrics\|env> --url <base>` | Query a running app's `/actuator/*`. |
| `firefly routes\|env\|health\|metrics --url <base>` | Remote introspection of a running app. |
| `firefly beans\|conditions [--url <base>]` | No local Rust analog (documented; pass-through with `--url`). |
| `firefly completion <bash\|zsh\|fish\|powershell\|elvish>` | Print a shell-completion script for the `firefly` CLI. |
| `firefly sbom [--json]` | Software Bill of Materials (resolved deps from `Cargo.lock`). |
| `firefly license` | Framework + dependency license report. |

Run `firefly <command> --help` for the full flag list of any command.

---

### `firefly new`

```bash
firefly new my-service --archetype web-api --features web,data,cqrs --git
firefly new my-lib     --archetype library --dep-path ../fireflyframework-rust  # local dev deps
firefly new --list                                                             # archetypes + features
firefly new svc --dry-run                                                       # plan without writing
```

**Flags**

| Flag | Effect |
| --- | --- |
| `--archetype <core\|web-api\|web\|hexagonal\|library\|cli>` | Project shape (default `core`). |
| `--features <a,b,c>` | Comma-separated feature set (default: the archetype's defaults). |
| `--directory <dir>` | Parent directory for the new project (default `.`). |
| `--git` | Initialize a git repo with an initial commit. |
| `--force` | Overwrite existing files in the target directory. |
| `--dry-run` | Show what would be created without writing. |
| `--dep-path <base>` | Point generated `firefly-*` deps at a local checkout (resolved per crate to `<base>/crates/<name>`). |
| `--dep-version <semver>` | Point generated `firefly-*` deps at a crates.io version. |
| `--list` | Print the archetype + feature catalog and exit. |

**Archetypes** — each generates a workspace-less Cargo crate with
git/path/version-configurable `firefly-*` dependencies, `firefly.yaml`,
`.gitignore`, `README.md`, a `Dockerfile`, real source, and a passing test:

| Archetype | What you get |
| --- | --- |
| `core` | A `firefly_starter_core::Core` service: CQRS bus + validation, cache, health, metrics, scheduler; lifecycle app with graceful shutdown; actuator admin server. |
| `web-api` | A `firefly_starter_web::WebStack` REST service: a `Todo` resource with CQRS `CreateTodo`/`ListTodos` handlers dispatched through the bus, an in-memory repository, public API + actuator admin servers, and a `tower::oneshot` integration test. |
| `web` | A server-rendered `WebStack` app: HTML page controllers + `PageService`, public + admin servers, page-render tests. |
| `hexagonal` | Ports & adapters: framework-free `domain` (models + ports), an `application` service, an in-memory outbound adapter, a driving HTTP `api`, and a real domain/adapter test. |
| `library` | A reusable library crate with a documented public API and a unit + integration test. |
| `cli` | A `clap` binary with a command/service split and a command test. |

**Compiles out of the box.** Generated projects are validated by the crate's own
test suite: `tests/compile_generated.rs` scaffolds every archetype (pointing
`firefly-*` deps at the local workspace) and runs `cargo check --tests` over the
result under `FIREFLY_CLI_COMPILE_TEST=1`. The always-on portion of that test
asserts each scaffold carries the real API markers (`Core::new` /
`WebStack::new` / `new_application`).

**Features** (`--features a,b,c`) toggle `firefly-*` dependencies and the
generated `firefly.yaml`:

`web`, `data`, `mongodb`, `eda`, `cache`, `client`, `security`, `scheduling`,
`observability`, `cqrs`, `shell`, `transactional`. See `firefly new --list`.

---

### `firefly generate` (alias `g`)

```bash
firefly generate handler Order            # src/handlers/order_handler.rs (axum)
firefly generate route Catalog            # src/routes/catalog_route.rs (axum Router)
firefly generate entity Product           # data-aware when firefly.yaml enables relational data
firefly generate repository Product       # firefly_data::MemoryRepository when data is enabled
firefly generate dto Order                # request/response DTOs
firefly generate aggregate Wallet         # firefly_eventsourcing::AggregateRoot-backed aggregate
firefly generate command OpenWallet       # CQRS command + a `register_*_handler(bus)` registrar
firefly generate query GetWallet          # CQRS query + registrar
firefly generate saga MoneyTransfer       # firefly_orchestration::Saga builder with compensation
firefly generate migration AddUsers       # migrations/V###__add_users.sql
firefly g saga MoneyTransfer --dry-run    # plan without writing
```

**Flags** (shared by every subcommand): `--force` overwrites existing files,
`--dry-run` plans without writing. The artifact name is accepted in any case
(`kebab`, `snake`, `camel`, `Pascal`, or spaced) and converted as needed.

**Artifact kinds**: `handler`, `route`, `entity`, `repository`, `dto`,
`aggregate`, `command`, `query`, `saga`, `migration`. Every template renders
real Rust against the live `firefly-*` APIs — no `todo!()` / placeholder bodies.
The CQRS handlers are `bus.register(|msg| async { ... })` closures (the real
closure-based bus model), not a fictional `#[command_handler]` macro.

The current project's package, archetype, and feature flags are detected from
`Cargo.toml` + `firefly.yaml`, so an `entity` becomes a data-aware persistent
model when `firefly.data.relational.enabled: true`, and a plain serializable
struct otherwise. `migration` auto-increments the `V###` version from the
highest existing file in `migrations/`.

> Generated artifacts are written as files; wire them into your module tree
> (`mod handlers;` etc.) to compile them. The crate's tests verify that, once
> wired, every artifact kind compiles against the real framework.

---

### `firefly run`

Runs the application via Cargo from the detected project root, mapping pyfly's
launch flags to the framework's environment variables (the Rust analog of
pyfly's `_to_env_key` / `_build_launch_env`):

```bash
firefly run -p dev,test                 # FIREFLY_PROFILES_ACTIVE=dev,test
firefly run -D web.port=9000            # FIREFLY_WEB_PORT=9000
firefly run --env RUST_LOG=info         # raw passthrough
firefly run --debug                     # FIREFLY_LOGGING_LEVEL_ROOT=DEBUG
firefly run --release --bin svc         # cargo run --release --bin svc
firefly run --dry-run -p dev            # print the env + command, run nothing
```

| Flag | Effect |
| --- | --- |
| `--profile`, `-p <p[,q]>` | Active profile(s); repeatable or comma-separated → `FIREFLY_PROFILES_ACTIVE`. |
| `-D <key=value>` | Config override → `FIREFLY_<KEY>=value` (`.`/`-` → `_`, `firefly.` stripped). |
| `--env <KEY=VALUE>` | Raw environment variable for the app process (verbatim). |
| `--debug` | Sets `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`. |
| `--release` | `cargo run --release` (optimized profile). |
| `--bin <name>` | `cargo run --bin <name>` (specific binary target). |
| `--dry-run` | Print the resolved environment + Cargo command without executing. |

pyfly's ASGI-server selection (`--server`/`--workers`/`--reload`/`--app`) has no
analog for a single compiled binary and is intentionally omitted — a Firefly
app is a normal Cargo binary.

---

### `firefly build`

Packaging helpers. Plain compilation is `cargo build` (the wheel/sdist analog);
this group ports the two pyfly `build` commands with a direct Rust counterpart:

```bash
firefly build info                      # write build-info.json (git SHA + UTC time)
firefly build info -o target/bi.json    # custom output path
firefly build image -t svc:1            # OCI image via Cloud Native Buildpacks (pack)
firefly build image --builder docker    # OCI image via `docker build .`
```

`build info` writes `{"git": {"sha": …}, "build": {"time": …}}`,
**byte-shape-identical** to pyfly/Go/Java so the same file feeds every runtime's
`/actuator/info` build contributor (register it as an info contributor in your
app). The SHA is the empty string when git is unavailable; the time is RFC 3339
UTC. `build image` requires `pack` (default) or `docker` on `PATH` and uses the
`Dockerfile` already scaffolded by `firefly new`.

---

### `firefly info`

Prints the framework version, host OS/architecture, the `rustc`/`cargo`
versions, and — when run inside a firefly-rust project — the detected package
name and archetype.

### `firefly doctor`

Reports **real** toolchain and project facts:

- **Required tools** — `rustc`, `cargo` (a missing one fails the run).
- **Optional tools** — `git`, `clippy-driver`, `rustfmt`, `docker`.
- **Project** — when run inside a firefly-rust project: the package name,
  archetype, root, and whether `firefly.yaml` / `migrations/` are present.

```text
$ firefly doctor
Firefly Doctor
  Required tools:
    ✓ rustc — rustc 1.96.0 ...
    ✓ cargo — cargo 1.96.0 ...
  Optional tools:
    ✓ git — git version 2.50.1 ...
    ✓ clippy-driver — clippy 0.1.96
    ✓ rustfmt — rustfmt 1.9.0
    ✓ docker — Docker version 29.4.1 ...
  Project:
    ✓ package    my-service
    ✓ archetype  web-api
    ✓ root       /path/to/my-service
    ✓ firefly.yaml present
    ✓ migrations/ present
  All required checks passed!
```

---

### `firefly db`

```bash
firefly db init                                   # migrations/ + starter V001__init.sql
firefly db migrate -m "create users"              # writes V002__create_users.sql
firefly db upgrade --url sqlite://app.db          # apply pending migrations
firefly db status  --url sqlite://app.db          # show applied + pending
firefly db downgrade                              # unsupported (forward-only) — fails loudly
```

Migration management on top of `firefly-migrations` (the runner generated
projects ship with). Subcommand *names* mirror pyfly's `pyfly db`, but the
engine differs: pyfly drives **Alembic**; this drives the framework's own
**forward-only** runner.

The database URL resolves from `--url`, then `$DATABASE_URL`, then
`firefly.datasource.url` in `firefly.yaml`, defaulting to `sqlite://firefly.db`.
The fully-wired CLI backend is **SQLite via `rusqlite`**; a `postgres://` /
`mysql://` URL returns a clear "not wired into the CLI" error (adapt the
`firefly_migrations::Database` port to your driver and call `run` directly).
`db downgrade` is unsupported by design — write a corrective migration instead.

---

### `firefly openapi`

```bash
firefly openapi                                   # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml     # YAML to a file
```

Exports an OpenAPI 3.1 document built with `firefly-openapi`. The flags
(`--format json|yaml`, `-o/--output`) and the wire shape (3.1, an
always-present `ProblemDetail` component) match pyfly; the document metadata
(`info.title` / `info.version` / `description`) is read from `firefly.yaml`
(`firefly.app.*`) then `Cargo.toml`. A compiled binary cannot boot an arbitrary
app to enumerate live routes, so the document has empty `paths` — wire real
routes with `firefly_openapi::Builder` and serve them via `Builder::router()`.

---

### `firefly actuator` / remote introspection

```bash
firefly actuator health  --url http://localhost:8081
firefly actuator info    --url http://localhost:8081
firefly actuator metrics requests --url http://localhost:8081 --json
firefly actuator env     --url http://localhost:8081
firefly routes  --url http://localhost:8081        # -> /actuator/mappings
firefly env     --url http://localhost:8081        # -> /actuator/env
firefly health  --url http://localhost:8081        # -> /actuator/health
firefly metrics requests --url http://localhost:8081
```

Remote-only: a compiled binary has no offline DI context to boot, so `--url` is
required. The client GETs `<base>/actuator/<endpoint>` with a 10-second timeout
and pretty-prints the JSON (`--json` emits the raw body only). `routes` maps to
`/actuator/mappings`; `env`/`health`/`metrics` map 1:1. `beans` and
`conditions` have **no local Rust analog** (generated apps have no runtime DI
container to enumerate and no auto-configuration condition report); they fail
with an explanatory message unless `--url` is given to a running app that
happens to expose those endpoints.

> The generated `core` / `web-api` / `web` / `hexagonal` projects bind their
> actuator admin surface on `127.0.0.1:8081` by default (override with the
> `ADMIN_ADDR` env var), so `firefly actuator health --url http://localhost:8081`
> works against a project you just scaffolded and ran.

---

### `firefly completion`

```bash
firefly completion bash                       # print the bash completion script
eval "$(firefly completion bash)"             # enable for the current shell
firefly completion zsh   > ~/.zfunc/_firefly  # zsh: drop into an fpath dir
firefly completion fish | source              # fish: enable for the session
firefly completion powershell | Out-String | Invoke-Expression   # PowerShell
```

Generates a shell-completion script for `bash`, `zsh`, `fish`, `powershell`, or
`elvish`. The script is produced by `clap_complete` from the live `firefly` clap
definition, so it always covers every subcommand, flag, and value-parser choice
(e.g. the `--archetype` and `completion <shell>` enums) and never drifts from
the CLI. This is the Rust spelling of pyfly's `pyfly completion`, which leans on
Click's completion machinery.

---

### `firefly sbom`

```bash
firefly sbom                                  # human-readable table
firefly sbom --json                           # machine-readable JSON
```

Prints a **Software Bill of Materials**: the framework name/version/license plus
every resolved dependency read from the project's `Cargo.lock` (the source of
truth Cargo uses for reproducible builds). Each row carries the crate name, the
exact locked version, and its origin (`crates.io`, a `git+<url>` source, or
`local` for workspace/path crates). `--json` emits a stable
`{ name, version, license, dependencies: [{ name, version, source }] }`
document. The lockfile is found by walking up from the current directory; run
outside a project, the command reports an empty dependency list rather than
failing. This is the Rust port of pyfly's `pyfly sbom` (which walks
`importlib.metadata`); here the resolved Cargo graph plays the same role.

---

### `firefly license`

```bash
firefly license                               # license header + full text + deps
```

Prints the framework license report: the **Apache-2.0** header and copyright
line, the full `LICENSE` text when one is found by walking up the project tree
(falling back to the canonical Apache-2.0 pointer otherwise), and a third-party
**dependency inventory** (the resolved crates from `Cargo.lock` with versions
and origins). Cargo lockfiles do not record per-crate SPDX identifiers, so the
report lists the dependency *inventory* rather than a per-crate license string —
a deliberate divergence from a `cargo-license`-style scan that would require a
heavier dependency. This extends pyfly's `pyfly license` (which prints only the
framework license) with the dependency report the gap analysis asked for.

---

## pyfly parity & deliberate divergences

This crate is the Rust port of pyfly's `pyfly.cli` package, adapted to a
compiled Cargo workspace. The naming table, project-detection rules,
`write_artifacts` force/dry-run semantics, and generator dispatch are ported
test-case for test-case from `tests/cli/`.

Deliberate divergences (a compiled tool cannot do everything an interpreter can):

- The `fastapi-api` archetype is dropped (Rust has a single web stack, Axum).
- The interactive `questionary` wizard and the truly Python-runtime-only
  commands (`shell`'s booted-context REPL, quality wrappers, `upgrade`) and the
  entry-point plugin mechanism are out of scope. (`shell`'s introspection story
  is covered by `firefly actuator`/`routes`/`env`/`health` against a running
  app.)
- `firefly run` maps pyfly's launch flags to `FIREFLY_*` env vars and execs
  `cargo run`; pyfly's ASGI-server selection has no single-binary analog.
- `firefly build info`/`image` port pyfly's build stamp + OCI image; plain
  `cargo build` is the wheel/sdist analog.
- `firefly db` drives the **forward-only** `firefly-migrations` runner instead
  of Alembic; `db downgrade` is unsupported; pyfly's
  `current`/`history`/`heads`/… collapse to `db status`; only SQLite is wired.
- `firefly openapi` emits a metadata-stamped **skeleton** (empty `paths`).
- `firefly actuator` and the remote introspection commands are **remote-only**
  (`--url`); `beans`/`conditions` have no local analog.
- The CQRS code generators target the real **closure-based bus**
  (`bus.register(|msg| async { ... })`), not a macro-based handler model.
