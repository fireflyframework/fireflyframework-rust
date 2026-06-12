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
| `firefly actuator <endpoint> --url <base>` | Query a running app's `/actuator/*` endpoints. |

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

### `firefly actuator`

```bash
firefly actuator health --url http://localhost:8080
firefly actuator metrics requests --url http://localhost:8080 --json
```

Remote-only: a compiled binary has no offline DI context to boot, so `--url` is
required.

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
