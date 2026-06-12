//! clap v4 derive command definitions and the dispatch entry point for the
//! `firefly` binary.
//!
//! Port of pyfly's `main.py` Click group, adapted to clap derive subcommand
//! enums. Click decorators map to `#[derive(Subcommand)]` variants; the
//! questionary wizard is omitted (non-interactive flags only, per the brief).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::actuator::ActuatorClient;
use crate::diagnostics::{run_doctor, run_info};
use crate::error::CliError;
use crate::generate::{plan_artifacts, write_artifacts, Action, ArtifactKind};
use crate::project::detect_project;
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
    /// Display framework and environment information.
    Info,
    /// Check the development environment and toolchain.
    Doctor,
    /// Query a running app's `/actuator/*` endpoints (remote, requires --url).
    #[command(subcommand)]
    Actuator(ActuatorCommand),
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

/// Dispatch a parsed [`Cli`] to its handler, returning the process exit code.
///
/// Errors are printed to stderr with a `✗` prefix (matching pyfly's console
/// diagnostics) and mapped to exit code `1`.
pub fn run(cli: Cli) -> i32 {
    let result = match cli.command {
        Commands::New(args) => cmd_new(args),
        Commands::Generate(cmd) | Commands::G(cmd) => cmd_generate(cmd),
        Commands::Info => {
            print_info();
            Ok(())
        }
        Commands::Doctor => {
            cmd_doctor();
            Ok(())
        }
        Commands::Actuator(cmd) => cmd_actuator(cmd),
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
}
