// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! clap v4 derive command definitions and the dispatch entry point for the
//! `firefly` binary.
//!
//! Port of pyfly's `main.py` Click group, adapted to clap derive subcommand
//! enums. Click decorators map to `#[derive(Subcommand)]` variants; the
//! questionary wizard is omitted (non-interactive flags only, per the brief).

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use clap_complete::Shell;

use crate::actuator::ActuatorClient;
use crate::build::{build_image, write_build_info, ImageBuilder};
use crate::completion::write_completion;
use crate::db::{self, db_downgrade, db_init, db_migrate, db_status, db_upgrade};
use crate::diagnostics::{run_doctor, run_info};
use crate::error::CliError;
use crate::generate::{plan_artifacts, write_artifacts, Action, ArtifactKind};
use crate::license::LicenseReport;
use crate::openapi::{meta_for_project, render_spec, OpenApiFormat};
use crate::project::detect_project;
use crate::run::{run as run_app, RunOptions};
use crate::sbom::Sbom;
use crate::scaffold::{scaffold_new, NewOptions};
use crate::templates::{Archetype, DepSource, AVAILABLE_FEATURES};

/// The `firefly` developer CLI.
#[derive(Debug, Parser)]
#[command(
    name = "firefly",
    version = crate::VERSION,
    about = "Firefly Framework for Rust — project scaffolding and introspection",
    propagate_version = true
)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level subcommands. Mirrors the pyfly command set scoped to the Rust port.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create a new firefly-rust project.
    New(NewArgs),
    /// Generate a code artifact into the current project.
    #[command(subcommand)]
    Generate(GenerateCommand),
    /// Alias for `generate`.
    #[command(subcommand)]
    G(GenerateCommand),
    /// Run the application via Cargo, mapping profile/override flags to env.
    Run(RunArgs),
    /// Build the application: stamp build-info, or build an OCI image.
    #[command(subcommand)]
    Build(BuildCommand),
    /// Display framework and environment information.
    Info,
    /// Check the development environment and toolchain.
    Doctor,
    /// Query a running app's `/actuator/*` endpoints (remote, requires --url).
    #[command(subcommand)]
    Actuator(ActuatorCommand),
    /// Database migration commands (powered by firefly-migrations).
    #[command(subcommand)]
    Db(DbCommand),
    /// Export an OpenAPI 3.1 document for the current project.
    Openapi(OpenapiArgs),
    /// Generate a typed Rust client from an OpenAPI document.
    #[command(name = "openapi-client")]
    OpenapiClient(OpenapiClientArgs),
    /// List HTTP route mappings of a running app (remote; --url -> /actuator/mappings).
    Routes(IntrospectArgs),
    /// List container beans (no Rust analog; see --help for details).
    Beans(IntrospectArgs),
    /// Auto-configuration condition report (no Rust analog; see --help).
    Conditions(IntrospectArgs),
    /// Show a running app's resolved configuration (remote; --url -> /actuator/env).
    Env(IntrospectArgs),
    /// Show a running app's health (remote; --url -> /actuator/health).
    Health(IntrospectArgs),
    /// Show a running app's metrics (remote; --url -> /actuator/metrics).
    Metrics(IntrospectMetricsArgs),
    /// Print a shell-completion script for the `firefly` CLI.
    Completion(CompletionArgs),
    /// Print a Software Bill of Materials (resolved deps from Cargo.lock).
    Sbom(SbomArgs),
    /// Print the framework + dependency license report.
    License,
}

/// Arguments for `firefly new`.
#[derive(Debug, clap::Args)]
pub struct NewArgs {
    /// Project name (required unless `--list`).
    pub name: Option<String>,
    /// Project archetype.
    #[arg(long, value_parser = ["core", "web-api", "web", "hexagonal", "library", "cli"])]
    pub archetype: Option<String>,
    /// Comma-separated feature list (e.g. `web,data,cache`).
    #[arg(long)]
    pub features: Option<String>,
    /// Parent directory for the new project.
    #[arg(long, default_value = ".")]
    pub directory: PathBuf,
    /// List archetypes and features, then exit.
    #[arg(long)]
    pub list: bool,
    /// Initialize a git repository with an initial commit.
    #[arg(long)]
    pub git: bool,
    /// Overwrite existing files in the target directory.
    #[arg(long)]
    pub force: bool,
    /// Show what would be created without writing.
    #[arg(long)]
    pub dry_run: bool,
    /// Point generated `firefly-*` deps at a local path instead of git.
    #[arg(long)]
    pub dep_path: Option<String>,
    /// Point generated `firefly-*` deps at a crates.io version instead of git.
    #[arg(long)]
    pub dep_version: Option<String>,
}

/// `firefly generate <kind> <name>` subcommands.
#[derive(Debug, Subcommand, Clone)]
pub enum GenerateCommand {
    /// Generate an HTTP handler.
    Handler(GenArgs),
    /// Generate route mappings.
    Route(GenArgs),
    /// Generate a model/entity.
    Entity(GenArgs),
    /// Generate a repository.
    Repository(GenArgs),
    /// Generate request/response DTOs.
    Dto(GenArgs),
    /// Generate a DDD aggregate root.
    Aggregate(GenArgs),
    /// Generate a CQRS command + handler.
    Command(GenArgs),
    /// Generate a CQRS query + handler.
    Query(GenArgs),
    /// Generate a saga orchestration.
    Saga(GenArgs),
    /// Generate a database migration file.
    Migration(GenArgs),
}

impl GenerateCommand {
    fn kind_and_args(&self) -> (ArtifactKind, &GenArgs) {
        match self {
            GenerateCommand::Handler(a) => (ArtifactKind::Handler, a),
            GenerateCommand::Route(a) => (ArtifactKind::Route, a),
            GenerateCommand::Entity(a) => (ArtifactKind::Entity, a),
            GenerateCommand::Repository(a) => (ArtifactKind::Repository, a),
            GenerateCommand::Dto(a) => (ArtifactKind::Dto, a),
            GenerateCommand::Aggregate(a) => (ArtifactKind::Aggregate, a),
            GenerateCommand::Command(a) => (ArtifactKind::Command, a),
            GenerateCommand::Query(a) => (ArtifactKind::Query, a),
            GenerateCommand::Saga(a) => (ArtifactKind::Saga, a),
            GenerateCommand::Migration(a) => (ArtifactKind::Migration, a),
        }
    }
}

/// Arguments shared by every `generate` subcommand.
#[derive(Debug, clap::Args, Clone)]
pub struct GenArgs {
    /// The artifact name (any case; converted as needed).
    pub name: String,
    /// Overwrite existing files.
    #[arg(long)]
    pub force: bool,
    /// Show what would be created without writing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `firefly run`.
///
/// Port of pyfly's `run` flags adapted to a compiled Cargo binary: the
/// profile/override flags map to `FIREFLY_*` environment variables (mirroring
/// pyfly's `_to_env_key` / `_build_launch_env`), then `cargo run` is exec'd.
/// pyfly's ASGI-server selection (`--server`/`--workers`/`--reload`/`--app`)
/// has no analog for a single compiled binary and is intentionally omitted.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Active profile(s); repeatable or comma-separated (`-p dev -p test` or
    /// `-p dev,test`). Sets `FIREFLY_PROFILES_ACTIVE`.
    #[arg(long = "profile", short = 'p')]
    pub profiles: Vec<String>,
    /// Override a config value: `-D key=value` -> `FIREFLY_<KEY>=value`.
    #[arg(short = 'D', value_name = "KEY=VALUE")]
    pub defines: Vec<String>,
    /// Set a raw environment variable for the app process (`--env KEY=VALUE`).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env_vars: Vec<String>,
    /// Enable debug logging (sets `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`).
    #[arg(long)]
    pub debug: bool,
    /// Build/run the optimized Cargo profile (`cargo run --release`).
    #[arg(long)]
    pub release: bool,
    /// Run a specific binary target (`cargo run --bin <name>`).
    #[arg(long)]
    pub bin: Option<String>,
    /// Print the resolved environment and Cargo command without executing.
    #[arg(long)]
    pub dry_run: bool,
}

/// `firefly build <subcommand>` — packaging and image helpers.
///
/// Plain compilation is `cargo build` (the wheel/sdist analog); this group
/// ports pyfly's `build info` (the `/actuator/info` build stamp) and
/// `build image` (OCI image via Buildpacks/Docker).
#[derive(Debug, Subcommand)]
pub enum BuildCommand {
    /// Write `build-info.json` (git SHA + UTC build time) for `/actuator/info`.
    Info(BuildInfoArgs),
    /// Build an OCI image via Cloud Native Buildpacks (`pack`) or Docker.
    Image(BuildImageArgs),
}

/// Arguments for `firefly build info`.
#[derive(Debug, clap::Args)]
pub struct BuildInfoArgs {
    /// Output path (relative to the project root unless absolute).
    #[arg(long, short = 'o', default_value = "build-info.json")]
    pub output: PathBuf,
}

/// Arguments for `firefly build image`.
#[derive(Debug, clap::Args)]
pub struct BuildImageArgs {
    /// Image tag (default: `firefly-app:latest`).
    #[arg(long, short = 't')]
    pub tag: Option<String>,
    /// Image builder backend.
    #[arg(long, default_value = "pack", value_parser = ["pack", "docker"])]
    pub builder: String,
}

/// `firefly actuator <endpoint> --url ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum ActuatorCommand {
    /// Application health.
    Health(ActuatorArgs),
    /// Application info.
    Info(ActuatorArgs),
    /// Application metrics (optionally a single metric name).
    Metrics(MetricsArgs),
    /// Resolved configuration and active profiles.
    Env(ActuatorArgs),
}

/// Arguments shared by every `actuator` subcommand.
#[derive(Debug, clap::Args)]
pub struct ActuatorArgs {
    /// Base URL of the running app (e.g. `http://localhost:8080`). Required.
    #[arg(long)]
    pub url: String,
    /// Emit raw JSON (pretty-printed).
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `firefly actuator metrics`.
#[derive(Debug, clap::Args)]
pub struct MetricsArgs {
    /// Optional single-metric selector.
    pub name: Option<String>,
    /// Base URL of the running app. Required.
    #[arg(long)]
    pub url: String,
    /// Emit raw JSON (pretty-printed).
    #[arg(long)]
    pub json: bool,
}

/// `firefly db <subcommand>` — migration management over firefly-migrations.
///
/// Mirrors pyfly's `pyfly db` group (`init`/`migrate`/`upgrade`/`downgrade`/
/// status). pyfly drives Alembic; this port drives the framework's own
/// forward-only migration runner against a SQLite database (other backends
/// are a documented divergence — see [`crate::db`]).
#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Create the `migrations/` directory with a starter `V001__init.sql`.
    Init,
    /// Write a new empty `V###__<message>.sql` migration file.
    Migrate(DbMigrateArgs),
    /// Apply every pending migration (default: head).
    Upgrade(DbUrlArgs),
    /// Roll back migrations — unsupported (the runner is forward-only).
    Downgrade(DbUrlArgs),
    /// Show applied + pending migrations.
    Status(DbUrlArgs),
}

/// Arguments for `firefly db migrate`.
#[derive(Debug, clap::Args)]
pub struct DbMigrateArgs {
    /// Revision message (becomes the `V###__<slug>.sql` description).
    #[arg(long, short = 'm')]
    pub message: Option<String>,
}

/// Arguments shared by the database-driving `db` subcommands.
#[derive(Debug, clap::Args)]
pub struct DbUrlArgs {
    /// Database URL (default: `DATABASE_URL`, then `firefly.datasource.url`,
    /// then `sqlite://firefly.db`). Only SQLite URLs are wired into the CLI.
    #[arg(long)]
    pub url: Option<String>,
}

/// Arguments for `firefly openapi`.
#[derive(Debug, clap::Args)]
pub struct OpenapiArgs {
    /// Output format.
    #[arg(long, default_value = "json", value_parser = ["json", "yaml"])]
    pub format: String,
    /// Write to a file instead of stdout.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
}

/// Arguments for `firefly openapi-client` — generate a typed Rust client.
#[derive(Debug, clap::Args)]
pub struct OpenapiClientArgs {
    /// Path to the OpenAPI 3.x document (JSON).
    #[arg(long)]
    pub spec: PathBuf,
    /// Write the generated client to a file instead of stdout.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
    /// The generated client struct's name.
    #[arg(long, default_value = "ApiClient")]
    pub client_name: String,
}

/// Arguments shared by the remote introspection commands
/// (`routes`/`beans`/`conditions`/`env`/`health`).
#[derive(Debug, clap::Args)]
pub struct IntrospectArgs {
    /// Base URL of the running app (e.g. `http://localhost:8080`). Required
    /// for the commands that map to an actuator endpoint.
    #[arg(long)]
    pub url: Option<String>,
    /// Emit raw JSON (pretty-printed).
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `firefly metrics` (remote introspection).
#[derive(Debug, clap::Args)]
pub struct IntrospectMetricsArgs {
    /// Optional single-metric selector.
    pub name: Option<String>,
    /// Base URL of the running app. Required.
    #[arg(long)]
    pub url: Option<String>,
    /// Emit raw JSON (pretty-printed).
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `firefly completion`.
#[derive(Debug, clap::Args)]
pub struct CompletionArgs {
    /// The target shell. clap_complete supports bash, zsh, fish, powershell,
    /// and elvish; the value is validated against that set at parse time.
    #[arg(value_enum)]
    pub shell: Shell,
}

/// Arguments for `firefly sbom`.
#[derive(Debug, clap::Args)]
pub struct SbomArgs {
    /// Emit the SBOM as JSON (pretty-printed) instead of a table.
    #[arg(long)]
    pub json: bool,
}

/// Dispatch a parsed [`Cli`] to its handler, returning the process exit code.
///
/// Errors are printed to stderr with a `✗` prefix (matching pyfly's console
/// diagnostics) and mapped to exit code `1`.
pub fn run(cli: Cli) -> i32 {
    let result = match cli.command {
        Commands::New(args) => cmd_new(args),
        Commands::Generate(cmd) | Commands::G(cmd) => cmd_generate(cmd),
        Commands::Run(args) => cmd_run(args),
        Commands::Build(cmd) => cmd_build(cmd),
        Commands::Info => {
            print_info();
            Ok(())
        }
        Commands::Doctor => {
            cmd_doctor();
            Ok(())
        }
        Commands::Actuator(cmd) => cmd_actuator(cmd),
        Commands::Db(cmd) => cmd_db(cmd),
        Commands::Openapi(args) => cmd_openapi(args),
        Commands::OpenapiClient(args) => cmd_openapi_client(args),
        Commands::Routes(args) => cmd_introspect("routes", "mappings", args),
        Commands::Beans(args) => cmd_no_analog("beans", args),
        Commands::Conditions(args) => cmd_no_analog("conditions", args),
        Commands::Env(args) => cmd_introspect("env", "env", args),
        Commands::Health(args) => cmd_introspect("health", "health", args),
        Commands::Metrics(args) => cmd_metrics(args),
        Commands::Completion(args) => {
            cmd_completion(args);
            Ok(())
        }
        Commands::Sbom(args) => {
            cmd_sbom(args);
            Ok(())
        }
        Commands::License => {
            cmd_license();
            Ok(())
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("\u{2717} {e}");
            1
        }
    }
}

fn cmd_new(args: NewArgs) -> Result<(), CliError> {
    if args.list {
        print_catalog();
        return Ok(());
    }
    let name = match args.name {
        Some(n) => n,
        None => {
            return Err(CliError::InvalidName(
                "A project name is required.".to_string(),
            ))
        }
    };
    let archetype = match args.archetype.as_deref() {
        Some(s) => Archetype::parse(s)?,
        None => Archetype::Core,
    };
    let features: Vec<String> = match args.features {
        Some(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        None => archetype
            .default_features()
            .into_iter()
            .map(String::from)
            .collect(),
    };
    let dep_source = if let Some(path) = args.dep_path {
        DepSource::Path(path)
    } else if let Some(version) = args.dep_version {
        DepSource::Version(version)
    } else {
        DepSource::default()
    };

    let opts = NewOptions {
        name,
        archetype,
        features,
        dep_source,
        force: args.force,
        dry_run: args.dry_run,
        init_git: args.git,
    };
    let outcome = scaffold_new(&args.directory, &opts)?;

    let verb = if opts.dry_run {
        "Would create"
    } else {
        "Created"
    };
    println!(
        "{} {} project at {}",
        verb,
        opts.archetype.as_str(),
        outcome.project_dir.display()
    );
    print_actions(&outcome.actions, &outcome.project_dir);
    if outcome.git_initialized {
        println!("  \u{2713} Initialized git repository.");
    }
    Ok(())
}

fn cmd_generate(cmd: GenerateCommand) -> Result<(), CliError> {
    let (kind, args) = cmd.kind_and_args();
    let info = detect_project(None)?;
    let artifacts = plan_artifacts(&info, kind, &args.name)?;
    let actions = write_artifacts(&artifacts, args.force, args.dry_run)?;
    let verb = if args.dry_run {
        "Would generate"
    } else {
        "Generated"
    };
    println!("{verb}:");
    print_actions(&actions, &info.root);
    Ok(())
}

/// `firefly run` — map the launch flags to `FIREFLY_*` env vars and exec
/// `cargo run` from the detected project root (current dir when not inside a
/// project, so a plain Cargo crate works too).
fn cmd_run(args: RunArgs) -> Result<(), CliError> {
    let root = detect_project(None)
        .map(|info| info.root)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let opts = RunOptions {
        profiles: args.profiles,
        defines: args.defines,
        env_vars: args.env_vars,
        debug: args.debug,
        release: args.release,
        bin: args.bin,
        dry_run: args.dry_run,
    };
    run_app(&root, &opts)
}

/// `firefly build <info|image>` — stamp build metadata or build an OCI image
/// from the detected project root.
fn cmd_build(cmd: BuildCommand) -> Result<(), CliError> {
    let root = detect_project(None)
        .map(|info| info.root)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    match cmd {
        BuildCommand::Info(args) => {
            let path = write_build_info(&root, &args.output)?;
            println!("\u{2713} Wrote {}", path.display());
            Ok(())
        }
        BuildCommand::Image(args) => {
            let builder = ImageBuilder::parse(&args.builder)?;
            build_image(&root, args.tag.as_deref(), builder)?;
            Ok(())
        }
    }
}

fn cmd_doctor() {
    let report = run_doctor();
    println!("\nFirefly Doctor\n");
    println!("  Required tools:");
    for c in &report.required {
        let mark = if c.ok { "\u{2713}" } else { "\u{2717}" };
        println!("    {mark} {} — {}", c.name, c.detail);
    }
    println!("\n  Optional tools:");
    for c in &report.optional {
        let mark = if c.ok { "\u{2713}" } else { "-" };
        println!("    {mark} {} — {}", c.name, c.detail);
    }
    match &report.project {
        Some(p) => {
            println!("\n  Project:");
            println!("    \u{2713} package    {}", p.package);
            println!("    \u{2713} archetype  {}", p.archetype);
            println!("    \u{2713} root       {}", p.root);
            let yaml = if p.has_firefly_yaml { "\u{2713}" } else { "-" };
            println!("    {yaml} firefly.yaml present");
            let mig = if p.has_migrations { "\u{2713}" } else { "-" };
            println!("    {mig} migrations/ present");
        }
        None => {
            println!("\n  Project:");
            println!("    - not inside a firefly-rust project (no Cargo.toml found)");
        }
    }
    println!();
    if report.all_ok {
        println!("  All required checks passed!\n");
    } else {
        println!("  Some required tools are missing. See above.\n");
    }
}

fn print_info() {
    println!();
    for row in run_info() {
        println!("  {:<14} {}", row.key, row.value);
    }
    println!();
}

fn cmd_actuator(cmd: ActuatorCommand) -> Result<(), CliError> {
    let (endpoint, url, json) = match &cmd {
        ActuatorCommand::Health(a) => ("health".to_string(), a.url.clone(), a.json),
        ActuatorCommand::Info(a) => ("info".to_string(), a.url.clone(), a.json),
        ActuatorCommand::Env(a) => ("env".to_string(), a.url.clone(), a.json),
        ActuatorCommand::Metrics(a) => {
            let ep = match &a.name {
                Some(n) => format!("metrics/{n}"),
                None => "metrics".to_string(),
            };
            (ep, a.url.clone(), a.json)
        }
    };
    let data = ActuatorClient::new(url).get(&endpoint)?;
    if json {
        // Plain stdout (not pretty console) so output is exact and pipeable.
        println!(
            "{}",
            serde_json::to_string_pretty(&data).unwrap_or_default()
        );
    } else {
        println!("actuator/{endpoint}");
        println!(
            "{}",
            serde_json::to_string_pretty(&data).unwrap_or_default()
        );
    }
    Ok(())
}

fn cmd_db(cmd: DbCommand) -> Result<(), CliError> {
    let dir = Path::new("migrations");
    match cmd {
        DbCommand::Init => {
            let outcome = db_init(dir)?;
            match outcome.created {
                Some(path) => println!(
                    "\u{2713} Initialized migrations in {} (wrote {}).",
                    outcome.dir.display(),
                    path.display()
                ),
                None => println!(
                    "\u{2713} Migrations directory {} already initialized.",
                    outcome.dir.display()
                ),
            }
            Ok(())
        }
        DbCommand::Migrate(args) => {
            let path = db_migrate(dir, args.message.as_deref())?;
            println!("\u{2713} Created migration {}.", path.display());
            Ok(())
        }
        DbCommand::Upgrade(args) => {
            let url = db::resolve_url(args.url.as_deref());
            let applied = db_upgrade(dir, &url)?;
            if applied == 0 {
                println!("\u{2713} Database already up to date ({url}).");
            } else {
                println!("\u{2713} Applied {applied} migration(s) ({url}).");
            }
            Ok(())
        }
        DbCommand::Downgrade(_) => db_downgrade(),
        DbCommand::Status(args) => {
            let url = db::resolve_url(args.url.as_deref());
            let status = db_status(dir, &url)?;
            println!("Migration status ({url}):");
            println!("  Applied ({}):", status.applied.len());
            for m in &status.applied {
                println!("    \u{2713} {}", m.filename);
            }
            println!("  Pending ({}):", status.pending.len());
            for m in &status.pending {
                println!("    - {}", m.filename);
            }
            Ok(())
        }
    }
}

fn cmd_openapi(args: OpenapiArgs) -> Result<(), CliError> {
    let format = OpenApiFormat::parse(&args.format)?;
    // Derive metadata from the current project when one is present; default
    // metadata otherwise (the command never requires a project, mirroring
    // pyfly's config-default reads).
    let root = detect_project(None)
        .map(|info| info.root)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let meta = meta_for_project(&root);
    let text = render_spec(&meta, format)?;
    match args.output {
        Some(path) => {
            std::fs::write(&path, &text).map_err(|source| CliError::Io {
                path: path.clone(),
                source,
            })?;
            println!("\u{2713} Wrote OpenAPI spec to {}", path.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

/// Generate a typed Rust client from an OpenAPI document (`firefly
/// openapi-client`). Reads the spec JSON, walks it, and emits a self-contained
/// client over `firefly_client::RestClient`.
fn cmd_openapi_client(args: OpenapiClientArgs) -> Result<(), CliError> {
    let text = std::fs::read_to_string(&args.spec).map_err(|source| CliError::Io {
        path: args.spec.clone(),
        source,
    })?;
    let spec: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CliError::Template(format!("openapi-client: invalid JSON spec: {e}")))?;
    let opts = crate::openapi_client::ClientGenOptions {
        client_name: args.client_name,
    };
    let client = crate::openapi_client::generate_client(&spec, &opts)?;
    match args.output {
        Some(path) => {
            std::fs::write(&path, &client).map_err(|source| CliError::Io {
                path: path.clone(),
                source,
            })?;
            println!("\u{2713} Wrote generated client to {}", path.display());
        }
        None => println!("{client}"),
    }
    Ok(())
}

/// Remote introspection: GET a mapped actuator endpoint via `--url`.
///
/// `routes` maps to `mappings`, `env`/`health` map 1:1. These require a
/// running app — a compiled binary has no in-process context to boot.
fn cmd_introspect(command: &str, endpoint: &str, args: IntrospectArgs) -> Result<(), CliError> {
    let url = args.url.ok_or_else(|| {
        CliError::Unsupported(format!(
            "'{command}' requires --url (it queries a running app's /actuator/{endpoint}; \
             a compiled binary has no in-process context to introspect)."
        ))
    })?;
    let data = ActuatorClient::new(url).get(endpoint)?;
    print_json_titled(&format!("actuator/{endpoint}"), &data, args.json);
    Ok(())
}

/// `firefly metrics [name] --url ...` — remote introspection of
/// `/actuator/metrics` (optionally a single metric).
fn cmd_metrics(args: IntrospectMetricsArgs) -> Result<(), CliError> {
    let url = args.url.ok_or_else(|| {
        CliError::Unsupported(
            "'metrics' requires --url (it queries a running app's /actuator/metrics).".to_string(),
        )
    })?;
    let endpoint = match &args.name {
        Some(n) => format!("metrics/{n}"),
        None => "metrics".to_string(),
    };
    let data = ActuatorClient::new(url).get(&endpoint)?;
    print_json_titled(&format!("actuator/{endpoint}"), &data, args.json);
    Ok(())
}

/// `beans` / `conditions` have no Rust analog: generated apps have no
/// runtime DI container to enumerate, and there is no auto-configuration
/// condition report. With `--url` we still pass the request through (a
/// running app *may* expose the endpoint); without it we explain the gap.
fn cmd_no_analog(endpoint: &str, args: IntrospectArgs) -> Result<(), CliError> {
    match args.url {
        Some(url) => {
            let data = ActuatorClient::new(url).get(endpoint)?;
            print_json_titled(&format!("actuator/{endpoint}"), &data, args.json);
            Ok(())
        }
        None => Err(CliError::Unsupported(format!(
            "'{endpoint}' has no local Rust analog: generated firefly-rust apps have no \
             runtime DI container / condition report to introspect. Pass --url to query a \
             running app that exposes /actuator/{endpoint}."
        ))),
    }
}

/// `firefly completion <shell>` — write the shell-completion script to stdout.
///
/// The script is generated by clap_complete from the live `firefly` CLI
/// definition, so it always matches the available subcommands and flags.
fn cmd_completion(args: CompletionArgs) {
    write_completion(args.shell, &mut std::io::stdout());
}

/// `firefly sbom [--json]` — print the resolved-dependency Software Bill of
/// Materials read from the project's `Cargo.lock`.
fn cmd_sbom(args: SbomArgs) {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sbom = Sbom::collect(&start);
    if args.json {
        println!("{}", sbom.to_json());
        return;
    }
    println!("\nFirefly Framework for Rust v{} — SBOM\n", sbom.version);
    println!("  {:<40} {:<14} Source", "Package", "Version");
    for dep in &sbom.dependencies {
        println!("  {:<40} {:<14} {}", dep.name, dep.version, dep.source);
    }
    println!("\n  Total dependencies: {}\n", sbom.dependencies.len());
}

/// `firefly license` — print the framework + dependency license report.
fn cmd_license() {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let report = LicenseReport::collect(&start);
    println!("\n{}", report.render());
}

/// Print a JSON value, optionally with a title line (mirrors the actuator
/// command's plain, pipeable output).
fn print_json_titled(title: &str, data: &serde_json::Value, json: bool) {
    if !json {
        println!("{title}");
    }
    println!("{}", serde_json::to_string_pretty(data).unwrap_or_default());
}

fn print_actions(actions: &[(Action, PathBuf)], root: &std::path::Path) {
    for (action, path) in actions {
        let rel = path.strip_prefix(root).unwrap_or(path);
        println!("  {:<9} {}", action.as_str(), rel.display());
    }
}

fn print_catalog() {
    println!("\nArchetypes:");
    for a in Archetype::ALL {
        println!("  {:<10} {}", a.as_str(), a.description());
    }
    println!("\nFeatures:");
    for (feat, desc) in AVAILABLE_FEATURES {
        println!("  {feat:<14} {desc}");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // clap asserts internal invariants here; catches duplicate args etc.
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_new_with_flags() {
        let cli = Cli::try_parse_from([
            "firefly",
            "new",
            "svc",
            "--archetype",
            "web-api",
            "--features",
            "web,data",
            "--git",
        ])
        .unwrap();
        match cli.command {
            Commands::New(a) => {
                assert_eq!(a.name.as_deref(), Some("svc"));
                assert_eq!(a.archetype.as_deref(), Some("web-api"));
                assert_eq!(a.features.as_deref(), Some("web,data"));
                assert!(a.git);
            }
            _ => panic!("expected new"),
        }
    }

    #[test]
    fn rejects_unknown_archetype_at_parse() {
        let res = Cli::try_parse_from(["firefly", "new", "svc", "--archetype", "fastapi-api"]);
        assert!(res.is_err());
    }

    #[test]
    fn g_alias_parses_like_generate() {
        let cli = Cli::try_parse_from(["firefly", "g", "service-stub", "--help"]);
        // --help short-circuits with an error kind; we only assert it routed to g.
        assert!(cli.is_err());
        let cli = Cli::try_parse_from(["firefly", "g", "entity", "Product", "--dry-run"]).unwrap();
        match cli.command {
            Commands::G(GenerateCommand::Entity(a)) => {
                assert_eq!(a.name, "Product");
                assert!(a.dry_run);
            }
            _ => panic!("expected g entity"),
        }
    }

    #[test]
    fn actuator_requires_url() {
        // --url has no default; omitting it is a parse error.
        let res = Cli::try_parse_from(["firefly", "actuator", "health"]);
        assert!(res.is_err());
        let ok = Cli::try_parse_from([
            "firefly",
            "actuator",
            "health",
            "--url",
            "http://localhost:8080",
        ]);
        assert!(ok.is_ok());
    }

    #[test]
    fn generate_subcommands_all_parse() {
        for sub in [
            "handler",
            "route",
            "entity",
            "repository",
            "dto",
            "aggregate",
            "command",
            "query",
            "saga",
            "migration",
        ] {
            let cli = Cli::try_parse_from(["firefly", "generate", sub, "Thing"]);
            assert!(cli.is_ok(), "generate {sub} failed to parse");
        }
    }

    #[test]
    fn new_list_runs_without_name() {
        let cli = Cli::try_parse_from(["firefly", "new", "--list"]).unwrap();
        // run() must succeed (exit 0) without a name when --list is set.
        assert_eq!(run(cli), 0);
    }

    // --- db command group (pyfly test_db.py / test_db_extra.py parity) ---

    #[test]
    fn db_subcommands_all_parse() {
        // pyfly TestDbHelp asserts init/migrate/upgrade/downgrade are present.
        for sub in ["init", "upgrade", "downgrade", "status"] {
            let cli = Cli::try_parse_from(["firefly", "db", sub]);
            assert!(cli.is_ok(), "db {sub} failed to parse");
        }
        let cli = Cli::try_parse_from(["firefly", "db", "migrate", "-m", "initial"]).unwrap();
        match cli.command {
            Commands::Db(DbCommand::Migrate(a)) => {
                assert_eq!(a.message.as_deref(), Some("initial"))
            }
            _ => panic!("expected db migrate"),
        }
    }

    #[test]
    fn db_upgrade_takes_optional_url() {
        let cli = Cli::try_parse_from(["firefly", "db", "upgrade", "--url", ":memory:"]).unwrap();
        match cli.command {
            Commands::Db(DbCommand::Upgrade(a)) => assert_eq!(a.url.as_deref(), Some(":memory:")),
            _ => panic!("expected db upgrade"),
        }
    }

    #[test]
    fn db_downgrade_runs_with_nonzero_exit() {
        // The runner is forward-only; downgrade is unsupported -> exit 1.
        let cli = Cli::try_parse_from(["firefly", "db", "downgrade"]).unwrap();
        assert_eq!(run(cli), 1);
    }

    // --- openapi command (pyfly test_openapi.py parity) ---

    #[test]
    fn openapi_parses_format_and_output() {
        let cli =
            Cli::try_parse_from(["firefly", "openapi", "--format", "yaml", "-o", "spec.yaml"])
                .unwrap();
        match cli.command {
            Commands::Openapi(a) => {
                assert_eq!(a.format, "yaml");
                assert_eq!(a.output.as_deref(), Some(std::path::Path::new("spec.yaml")));
            }
            _ => panic!("expected openapi"),
        }
    }

    #[test]
    fn openapi_rejects_unknown_format_at_parse() {
        let res = Cli::try_parse_from(["firefly", "openapi", "--format", "toml"]);
        assert!(res.is_err());
    }

    #[test]
    fn openapi_to_stdout_runs() {
        // No project required; defaults are used. run() must succeed.
        let cli = Cli::try_parse_from(["firefly", "openapi"]).unwrap();
        assert_eq!(run(cli), 0);
    }

    // --- remote introspection (pyfly test_introspect_remote.py parity) ---

    #[test]
    fn routes_maps_to_mappings_endpoint() {
        // routes -> /actuator/mappings (pyfly test_routes_remote).
        use std::net::SocketAddr;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let handle = rt.spawn(async move {
            use axum::{routing::get, Json, Router};
            let app = Router::new().route(
                "/actuator/mappings",
                get(|| async { Json(serde_json::json!({ "routes": [] })) }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
        let addr = addr_rx.recv().unwrap();

        let cli = Cli::try_parse_from([
            "firefly",
            "routes",
            "--url",
            &format!("http://{addr}"),
            "--json",
        ])
        .unwrap();
        assert_eq!(run(cli), 0);
        handle.abort();
    }

    #[test]
    fn routes_requires_url() {
        let cli = Cli::try_parse_from(["firefly", "routes"]).unwrap();
        // Without --url there is no in-process context to introspect -> exit 1.
        assert_eq!(run(cli), 1);
    }

    #[test]
    fn beans_without_url_has_no_analog() {
        // beans/conditions have no local Rust analog -> exit 1 without --url.
        let cli = Cli::try_parse_from(["firefly", "beans"]).unwrap();
        assert_eq!(run(cli), 1);
        let cli = Cli::try_parse_from(["firefly", "conditions"]).unwrap();
        assert_eq!(run(cli), 1);
    }

    #[test]
    fn metrics_parses_optional_name_and_url() {
        let cli = Cli::try_parse_from(["firefly", "metrics", "requests", "--url", "http://h:8080"])
            .unwrap();
        match cli.command {
            Commands::Metrics(a) => {
                assert_eq!(a.name.as_deref(), Some("requests"));
                assert_eq!(a.url.as_deref(), Some("http://h:8080"));
            }
            _ => panic!("expected metrics"),
        }
    }

    // --- run command (pyfly test run.py parity) ---

    #[test]
    fn run_parses_profile_define_env_and_flags() {
        let cli = Cli::try_parse_from([
            "firefly",
            "run",
            "-p",
            "dev,test",
            "-D",
            "web.port=9000",
            "--env",
            "RUST_LOG=info",
            "--debug",
            "--release",
            "--bin",
            "svc",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Commands::Run(a) => {
                assert_eq!(a.profiles, vec!["dev,test".to_string()]);
                assert_eq!(a.defines, vec!["web.port=9000".to_string()]);
                assert_eq!(a.env_vars, vec!["RUST_LOG=info".to_string()]);
                assert!(a.debug);
                assert!(a.release);
                assert_eq!(a.bin.as_deref(), Some("svc"));
                assert!(a.dry_run);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn run_dry_run_exits_zero() {
        // --dry-run never spawns Cargo; run from the workspace (a Cargo project).
        let cli = Cli::try_parse_from(["firefly", "run", "--dry-run", "-p", "dev"]).unwrap();
        assert_eq!(run(cli), 0);
    }

    // --- build command (pyfly test build.py parity) ---

    #[test]
    fn build_subcommands_parse() {
        let cli = Cli::try_parse_from(["firefly", "build", "info", "-o", "bi.json"]).unwrap();
        match cli.command {
            Commands::Build(BuildCommand::Info(a)) => {
                assert_eq!(a.output, std::path::Path::new("bi.json"));
            }
            _ => panic!("expected build info"),
        }
        let cli = Cli::try_parse_from([
            "firefly",
            "build",
            "image",
            "-t",
            "svc:1",
            "--builder",
            "docker",
        ])
        .unwrap();
        match cli.command {
            Commands::Build(BuildCommand::Image(a)) => {
                assert_eq!(a.tag.as_deref(), Some("svc:1"));
                assert_eq!(a.builder, "docker");
            }
            _ => panic!("expected build image"),
        }
    }

    #[test]
    fn build_image_rejects_unknown_builder_at_parse() {
        let res = Cli::try_parse_from(["firefly", "build", "image", "--builder", "podman"]);
        assert!(res.is_err());
    }

    #[test]
    fn build_info_writes_file_and_exits_zero() {
        // Write into a temp dir via an absolute --output so the run is hermetic.
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("build-info.json");
        let cli =
            Cli::try_parse_from(["firefly", "build", "info", "-o", out.to_str().unwrap()]).unwrap();
        assert_eq!(run(cli), 0);
        assert!(out.exists());
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        assert!(doc["build"]["time"].as_str().is_some());
    }

    // --- completion command (pyfly test_completion.py parity) ---

    #[test]
    fn completion_parses_every_supported_shell() {
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let cli = Cli::try_parse_from(["firefly", "completion", shell]);
            assert!(cli.is_ok(), "completion {shell} failed to parse");
        }
    }

    #[test]
    fn completion_rejects_unknown_shell_at_parse() {
        let res = Cli::try_parse_from(["firefly", "completion", "tcsh"]);
        assert!(res.is_err());
    }

    #[test]
    fn completion_requires_a_shell() {
        let res = Cli::try_parse_from(["firefly", "completion"]);
        assert!(res.is_err());
    }

    #[test]
    fn completion_runs_for_each_shell() {
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let cli = Cli::try_parse_from(["firefly", "completion", shell]).unwrap();
            assert_eq!(run(cli), 0, "completion {shell} did not exit 0");
        }
    }

    // --- sbom command (pyfly test_sbom.py parity) ---

    #[test]
    fn sbom_parses_with_and_without_json() {
        let cli = Cli::try_parse_from(["firefly", "sbom"]).unwrap();
        match cli.command {
            Commands::Sbom(a) => assert!(!a.json),
            _ => panic!("expected sbom"),
        }
        let cli = Cli::try_parse_from(["firefly", "sbom", "--json"]).unwrap();
        match cli.command {
            Commands::Sbom(a) => assert!(a.json),
            _ => panic!("expected sbom --json"),
        }
    }

    #[test]
    fn sbom_runs() {
        // No project is required; run inside the workspace must exit 0.
        assert_eq!(run(Cli::try_parse_from(["firefly", "sbom"]).unwrap()), 0);
        assert_eq!(
            run(Cli::try_parse_from(["firefly", "sbom", "--json"]).unwrap()),
            0
        );
    }

    // --- license command (pyfly test_license.py parity) ---

    #[test]
    fn license_parses_and_runs() {
        let cli = Cli::try_parse_from(["firefly", "license"]).unwrap();
        assert!(matches!(cli.command, Commands::License));
        assert_eq!(run(cli), 0);
    }
}
