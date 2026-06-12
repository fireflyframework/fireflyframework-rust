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

//! `firefly generate` — scaffold individual artifacts into an existing project.
//!
//! Port of pyfly's `generate.py` core: the [`Artifact`] record, the
//! [`write_artifacts`] engine (with `force`/`dry_run` semantics), and a
//! per-kind dispatcher that mirrors the pyfly subcommands. Every template
//! renders real Rust against the live `firefly-*` APIs (axum handlers, the
//! closure-based `firefly_cqrs::Bus`, `firefly_data::MemoryRepository`,
//! `firefly_eventsourcing::AggregateRoot`, the `firefly_orchestration::Saga`
//! builder) — no `todo!()` / placeholder bodies. Dispatch and marker tests are
//! ported from `test_generate_engine.py` / `test_generate_commands.py`; the
//! workspace's own suite compiles each artifact kind once wired into a crate.

use std::path::{Path, PathBuf};

use minijinja::{context, Environment};

use crate::error::CliError;
use crate::naming::{names, Names};
use crate::project::{feature_flags, FeatureFlags, ProjectInfo};

/// The kind of artifact a generator produces. Each maps to a pyfly `generate`
/// subcommand (the Python `service`/`controller`/`event`/`shell-command`
/// surfaces are folded into the Rust-idiomatic `handler`/`route` here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// An HTTP request handler (`src/handlers/`).
    Handler,
    /// Route mappings for a resource (`src/routes/`).
    Route,
    /// A model/entity (`src/models/`).
    Entity,
    /// A repository (`src/repositories/`).
    Repository,
    /// Request/response DTOs (`src/dto/`).
    Dto,
    /// A DDD aggregate root (`src/domain/`).
    Aggregate,
    /// A CQRS command + handler (`src/cqrs/`).
    Command,
    /// A CQRS query + handler (`src/cqrs/`).
    Query,
    /// A saga orchestration (`src/sagas/`).
    Saga,
    /// A database migration file (`migrations/`).
    Migration,
}

impl ArtifactKind {
    /// The lowercase CLI name of this kind (e.g. `"handler"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Handler => "handler",
            ArtifactKind::Route => "route",
            ArtifactKind::Entity => "entity",
            ArtifactKind::Repository => "repository",
            ArtifactKind::Dto => "dto",
            ArtifactKind::Aggregate => "aggregate",
            ArtifactKind::Command => "command",
            ArtifactKind::Query => "query",
            ArtifactKind::Saga => "saga",
            ArtifactKind::Migration => "migration",
        }
    }
}

/// A single file the generator intends to write. Port of pyfly's `Artifact`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// A short label for the kind of file (used in the report).
    pub kind: String,
    /// The absolute path the file will be written to.
    pub path: PathBuf,
    /// The rendered file contents.
    pub content: String,
}

impl Artifact {
    /// Construct an artifact.
    pub fn new(
        kind: impl Into<String>,
        path: impl Into<PathBuf>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            path: path.into(),
            content: content.into(),
        }
    }
}

/// The action [`write_artifacts`] planned (or performed) for one artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The file did not exist and was created.
    Create,
    /// The file existed and was overwritten (because `force`).
    Overwrite,
    /// The file existed and was left untouched (no `force`).
    Skip,
}

impl Action {
    /// The lowercase label used in the textual report.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Overwrite => "overwrite",
            Action::Skip => "skip",
        }
    }
}

/// Write artifacts, skipping existing files unless `force`.
///
/// Returns the planned `(action, path)` list in input order. With `dry_run`,
/// nothing is written but the same plan is returned. Faithful port of pyfly's
/// `write_artifacts`.
///
/// # Errors
/// Returns [`CliError::Io`] if a directory or file write fails.
pub fn write_artifacts(
    artifacts: &[Artifact],
    force: bool,
    dry_run: bool,
) -> Result<Vec<(Action, PathBuf)>, CliError> {
    let mut actions = Vec::with_capacity(artifacts.len());
    for art in artifacts {
        let exists = art.path.exists();
        if exists && !force {
            actions.push((Action::Skip, art.path.clone()));
            continue;
        }
        let action = if exists {
            Action::Overwrite
        } else {
            Action::Create
        };
        actions.push((action, art.path.clone()));
        if !dry_run {
            if let Some(parent) = art.path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(&art.path, &art.content).map_err(|source| CliError::Io {
                path: art.path.clone(),
                source,
            })?;
        }
    }
    Ok(actions)
}

// ── Embedded templates ──

macro_rules! tmpl {
    ($name:literal) => {
        ($name, include_str!(concat!("templates/generators/", $name)))
    };
}

const GENERATOR_TEMPLATES: &[(&str, &str)] = &[
    tmpl!("handler.rs.j2"),
    tmpl!("route.rs.j2"),
    tmpl!("entity.rs.j2"),
    tmpl!("repository.rs.j2"),
    tmpl!("dto.rs.j2"),
    tmpl!("aggregate.rs.j2"),
    tmpl!("command.rs.j2"),
    tmpl!("command_handler.rs.j2"),
    tmpl!("query.rs.j2"),
    tmpl!("query_handler.rs.j2"),
    tmpl!("saga.rs.j2"),
    tmpl!("migration.sql.j2"),
];

/// Build a minijinja environment with every generator template registered.
pub(crate) fn generator_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_keep_trailing_newline(true);
    for (name, src) in GENERATOR_TEMPLATES {
        // include_str! sources are 'static; registration cannot fail.
        env.add_template(name, src).expect("static template parses");
    }
    env
}

fn render(template: &str, n: &Names, flags: &FeatureFlags) -> Result<String, CliError> {
    render_with_version(template, n, flags, "")
}

fn render_with_version(
    template: &str,
    n: &Names,
    flags: &FeatureFlags,
    version: &str,
) -> Result<String, CliError> {
    let env = generator_env();
    let tmpl = env.get_template(template)?;
    let out = tmpl.render(context! {
        names => n,
        has_data => flags.has_data,
        has_mongodb => flags.has_mongodb,
        has_web => flags.has_web,
        version => version,
    })?;
    Ok(out)
}

/// Plan the artifacts a single `generate <kind> <name>` invocation produces,
/// without writing them. Useful for snapshot tests and `--dry-run`.
///
/// # Errors
/// Returns [`CliError::InvalidName`] when no identifier can be derived from
/// `raw_name`, or [`CliError::Template`] on a render failure.
pub fn plan_artifacts(
    info: &ProjectInfo,
    kind: ArtifactKind,
    raw_name: &str,
) -> Result<Vec<Artifact>, CliError> {
    let n = names(raw_name).ok_or_else(|| CliError::InvalidName(raw_name.to_string()))?;
    let flags = feature_flags(info);
    let pkg = &info.src_dir;

    let single = |subdir: &str, suffix: &str, template: &str| -> Result<Artifact, CliError> {
        let stem = stem(&n.snake, suffix);
        let path = pkg.join(subdir).join(format!("{stem}.rs"));
        Ok(Artifact::new(subdir, path, render(template, &n, &flags)?))
    };

    let artifacts = match kind {
        ArtifactKind::Handler => vec![single("handlers", "_handler", "handler.rs.j2")?],
        ArtifactKind::Route => vec![single("routes", "_route", "route.rs.j2")?],
        ArtifactKind::Entity => vec![single("models", "", "entity.rs.j2")?],
        ArtifactKind::Repository => {
            vec![single("repositories", "_repository", "repository.rs.j2")?]
        }
        ArtifactKind::Dto => vec![single("dto", "_dto", "dto.rs.j2")?],
        ArtifactKind::Aggregate => vec![single("domain", "", "aggregate.rs.j2")?],
        ArtifactKind::Command => {
            let dir = pkg.join("cqrs");
            vec![
                Artifact::new(
                    "command",
                    dir.join(format!("{}_command.rs", n.snake)),
                    render("command.rs.j2", &n, &flags)?,
                ),
                Artifact::new(
                    "handler",
                    dir.join(format!("{}_command_handler.rs", n.snake)),
                    render("command_handler.rs.j2", &n, &flags)?,
                ),
            ]
        }
        ArtifactKind::Query => {
            let dir = pkg.join("cqrs");
            vec![
                Artifact::new(
                    "query",
                    dir.join(format!("{}_query.rs", n.snake)),
                    render("query.rs.j2", &n, &flags)?,
                ),
                Artifact::new(
                    "handler",
                    dir.join(format!("{}_query_handler.rs", n.snake)),
                    render("query_handler.rs.j2", &n, &flags)?,
                ),
            ]
        }
        ArtifactKind::Saga => vec![single("sagas", "_saga", "saga.rs.j2")?],
        ArtifactKind::Migration => {
            let version = next_migration_version(&info.root);
            let path = info
                .root
                .join("migrations")
                .join(format!("V{version}__{}.sql", n.snake));
            vec![Artifact::new(
                "migration",
                path,
                render_with_version("migration.sql.j2", &n, &flags, &version)?,
            )]
        }
    };
    Ok(artifacts)
}

/// Append `suffix` unless `snake` already ends with it (avoids e.g.
/// `report_job_job`). Port of pyfly's `_stem`.
fn stem(snake: &str, suffix: &str) -> String {
    if !suffix.is_empty() && snake.ends_with(suffix) {
        snake.to_string()
    } else {
        format!("{snake}{suffix}")
    }
}

/// Compute the next `V###` migration version by scanning `migrations/` for the
/// highest existing `V{n}__*.sql` and adding one (3-digit, zero-padded). Returns
/// `"001"` when the directory is empty or absent.
fn next_migration_version(root: &Path) -> String {
    let dir = root.join("migrations");
    let mut max = 0u32;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(version) = parse_version(&name) {
                max = max.max(version);
            }
        }
    }
    format!("{:03}", max + 1)
}

/// Parse the numeric version from a `V{version}__{description}.sql` filename.
fn parse_version(filename: &str) -> Option<u32> {
    let rest = filename
        .strip_prefix('V')
        .or_else(|| filename.strip_prefix('v'))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let after = &rest[digits.len()..];
    if !after.starts_with("__") {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── write_artifacts engine (ported from test_generate_engine.py) ──

    #[test]
    fn write_creates_file() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a.rs");
        let actions =
            write_artifacts(&[Artifact::new("x", &target, "hello\n")], false, false).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello\n");
        assert_eq!(actions, vec![(Action::Create, target)]);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a.rs");
        let actions = write_artifacts(&[Artifact::new("x", &target, "hi\n")], false, true).unwrap();
        assert!(!target.exists());
        assert_eq!(actions, vec![(Action::Create, target)]);
    }

    #[test]
    fn skip_existing_without_force() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a.rs");
        fs::write(&target, "old\n").unwrap();
        let actions =
            write_artifacts(&[Artifact::new("x", &target, "new\n")], false, false).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "old\n");
        assert_eq!(actions, vec![(Action::Skip, target)]);
    }

    #[test]
    fn overwrite_with_force() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a.rs");
        fs::write(&target, "old\n").unwrap();
        write_artifacts(&[Artifact::new("x", &target, "new\n")], true, false).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
    }

    #[test]
    fn stem_avoids_double_suffix() {
        assert_eq!(stem("report_job", "_job"), "report_job");
        assert_eq!(stem("report", "_job"), "report_job");
        assert_eq!(stem("order", ""), "order");
    }

    #[test]
    fn parses_migration_version() {
        assert_eq!(parse_version("V001__init.sql"), Some(1));
        assert_eq!(parse_version("V42__add_users.sql"), Some(42));
        assert_eq!(parse_version("R__repeatable.sql"), None);
        assert_eq!(parse_version("V1_no_double_underscore.sql"), None);
    }

    fn project(root: &Path, archetype: &str, data: bool) -> ProjectInfo {
        let mut yaml = format!("firefly:\n  app:\n    name: shop\n    archetype: {archetype}\n");
        if data {
            yaml.push_str("  data:\n    relational:\n      enabled: true\n");
        }
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"shop\"\n").unwrap();
        fs::write(root.join("firefly.yaml"), yaml).unwrap();
        crate::project::detect_project(Some(root)).unwrap()
    }

    #[test]
    fn next_migration_increments() {
        let tmp = TempDir::new().unwrap();
        let mig = tmp.path().join("migrations");
        fs::create_dir_all(&mig).unwrap();
        assert_eq!(next_migration_version(tmp.path()), "001"); // none yet? created empty -> 001
        fs::write(mig.join("V001__init.sql"), "").unwrap();
        fs::write(mig.join("V007__later.sql"), "").unwrap();
        assert_eq!(next_migration_version(tmp.path()), "008");
    }

    // ── generator dispatch (ported from test_generate_commands.py) ──

    #[test]
    fn handler_plan() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "web-api", false);
        let arts = plan_artifacts(&info, ArtifactKind::Handler, "Order").unwrap();
        assert_eq!(arts.len(), 1);
        assert!(arts[0].path.ends_with("src/handlers/order_handler.rs"));
        assert!(arts[0].content.contains("struct OrderHandler"));
    }

    #[test]
    fn entity_plain_when_no_data() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "web-api", false);
        let arts = plan_artifacts(&info, ArtifactKind::Entity, "Product").unwrap();
        assert!(arts[0].path.ends_with("src/models/product.rs"));
        let text = &arts[0].content;
        assert!(text.contains("pub struct Product"));
        assert!(!text.contains("firefly(table"));
    }

    #[test]
    fn entity_data_aware_when_data() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "web-api", true);
        let arts = plan_artifacts(&info, ArtifactKind::Entity, "Product").unwrap();
        let text = &arts[0].content;
        assert!(text.contains("Entity"));
        assert!(text.contains("table = \"products\""));
    }

    #[test]
    fn repository_plan() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "web-api", true);
        let arts = plan_artifacts(&info, ArtifactKind::Repository, "Product").unwrap();
        assert!(arts[0]
            .path
            .ends_with("src/repositories/product_repository.rs"));
        assert!(arts[0].content.contains("struct ProductRepository"));
    }

    #[test]
    fn dto_plan() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "web-api", false);
        let arts = plan_artifacts(&info, ArtifactKind::Dto, "Order").unwrap();
        let text = &arts[0].content;
        assert!(text.contains("struct OrderCreateRequest"));
        assert!(text.contains("struct OrderResponse"));
    }

    #[test]
    fn aggregate_plan() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "hexagonal", false);
        let arts = plan_artifacts(&info, ArtifactKind::Aggregate, "Wallet").unwrap();
        assert!(arts[0].path.ends_with("src/domain/wallet.rs"));
        let text = &arts[0].content;
        assert!(text.contains("struct Wallet"));
        assert!(text.contains("_events"));
    }

    #[test]
    fn command_and_handler() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "core", false);
        let arts = plan_artifacts(&info, ArtifactKind::Command, "OpenWallet").unwrap();
        assert_eq!(arts.len(), 2);
        assert!(arts[0].path.ends_with("src/cqrs/open_wallet_command.rs"));
        assert!(arts[1]
            .path
            .ends_with("src/cqrs/open_wallet_command_handler.rs"));
        assert!(arts[0].content.contains("struct OpenWallet"));
        assert!(arts[0].content.contains("impl Message for OpenWallet"));
        // The handler is a `bus.register(...)` registrar function.
        assert!(arts[1]
            .content
            .contains("pub fn register_open_wallet_handler(bus: &Bus)"));
        assert!(arts[1].content.contains("bus.register("));
    }

    #[test]
    fn query_and_handler() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "core", false);
        let arts = plan_artifacts(&info, ArtifactKind::Query, "GetWallet").unwrap();
        assert!(arts[0].path.ends_with("src/cqrs/get_wallet_query.rs"));
        assert!(arts[1]
            .path
            .ends_with("src/cqrs/get_wallet_query_handler.rs"));
        assert!(arts[1]
            .content
            .contains("pub fn register_get_wallet_handler(bus: &Bus)"));
        assert!(arts[0].content.contains("impl Message for GetWallet"));
    }

    #[test]
    fn saga_plan() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "core", false);
        let arts = plan_artifacts(&info, ArtifactKind::Saga, "MoneyTransfer").unwrap();
        assert!(arts[0].path.ends_with("src/sagas/money_transfer_saga.rs"));
        let text = &arts[0].content;
        assert!(text.contains("Saga::new(\"money-transfer\")"));
        assert!(text.contains("Step::new("));
        assert!(text.contains(".with_compensation("));
        assert!(text.contains("pub fn build_money_transfer_saga()"));
    }

    #[test]
    fn migration_plan_versioned() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "core", true);
        let arts = plan_artifacts(&info, ArtifactKind::Migration, "AddUsers").unwrap();
        assert_eq!(arts.len(), 1);
        assert!(arts[0].path.ends_with("migrations/V001__add_users.sql"));
        assert!(arts[0].content.contains("V001__add_users.sql"));
    }

    #[test]
    fn invalid_name_errors() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "core", false);
        let err = plan_artifacts(&info, ArtifactKind::Entity, "---");
        assert!(matches!(err, Err(CliError::InvalidName(_))));
    }

    #[test]
    fn dry_run_then_real_write() {
        let tmp = TempDir::new().unwrap();
        let info = project(tmp.path(), "web-api", false);
        let arts = plan_artifacts(&info, ArtifactKind::Handler, "Pricing").unwrap();
        write_artifacts(&arts, false, true).unwrap();
        assert!(!arts[0].path.exists());
        write_artifacts(&arts, false, false).unwrap();
        assert!(arts[0].path.exists());
        assert!(info.src_dir.join("handlers/pricing_handler.rs").exists());
    }
}
