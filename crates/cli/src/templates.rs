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

//! minijinja-based project template renderer for `firefly new`.
//!
//! Port of pyfly's `templates.py`, adapted to produce a `firefly-rust` consumer
//! project: a workspace-less `Cargo.toml` with path/git-configurable `firefly-*`
//! dependencies, a `src/` tree appropriate to the archetype, `firefly.yaml`,
//! `.gitignore`, `README.md`, a `Dockerfile`, and `tests/`.
//!
//! Templates are embedded with `include_str!` (no runtime template directory),
//! and rendered with the same `has_*`/`package_name`/`name` context keys the
//! pyfly templates used, so the conditional structure is identical.

use std::path::{Path, PathBuf};

use minijinja::{context, Environment};

use crate::error::CliError;
use crate::generate::write_artifacts;
use crate::generate::{Action, Artifact};

/// The project archetypes `firefly new` can scaffold.
///
/// Drops pyfly's `fastapi-api` (Rust has a single web stack — Axum), per the
/// porting plan in the cli brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archetype {
    /// Minimal microservice with DI container and config.
    Core,
    /// REST API with controller/service/repository layers.
    WebApi,
    /// Server-rendered web application with HTML templates.
    Web,
    /// Hexagonal architecture (ports & adapters).
    Hexagonal,
    /// Reusable library crate.
    Library,
    /// Command-line application.
    Cli,
}

impl Archetype {
    /// All archetypes, in catalog/display order.
    pub const ALL: [Archetype; 6] = [
        Archetype::Core,
        Archetype::WebApi,
        Archetype::Web,
        Archetype::Hexagonal,
        Archetype::Library,
        Archetype::Cli,
    ];

    /// The lowercase kebab name (e.g. `"web-api"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Archetype::Core => "core",
            Archetype::WebApi => "web-api",
            Archetype::Web => "web",
            Archetype::Hexagonal => "hexagonal",
            Archetype::Library => "library",
            Archetype::Cli => "cli",
        }
    }

    /// A one-line description for `--list`.
    pub fn description(self) -> &'static str {
        match self {
            Archetype::Core => "Minimal microservice with DI container and config",
            Archetype::WebApi => "REST API with Axum, controller/service/repository layers",
            Archetype::Web => "Server-rendered web application with HTML templates",
            Archetype::Hexagonal => "Hexagonal architecture (ports & adapters)",
            Archetype::Library => "Reusable library crate",
            Archetype::Cli => "Command-line application",
        }
    }

    /// Parse an archetype from its kebab name.
    pub fn parse(s: &str) -> Result<Archetype, CliError> {
        Archetype::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| CliError::UnknownArchetype(s.to_string()))
    }

    /// Default features selected for this archetype.
    pub fn default_features(self) -> Vec<&'static str> {
        match self {
            Archetype::Core | Archetype::Library | Archetype::Cli => vec![],
            Archetype::WebApi | Archetype::Web | Archetype::Hexagonal => vec!["web"],
        }
    }
}

/// The catalog of optional features, each mapping to `firefly-*` crate deps.
///
/// Port of pyfly's `AVAILABLE_FEATURES` / `FEATURE_DETAILS`, retargeted to Rust
/// crates. The `has_*` template flags are derived from the selected set.
pub const AVAILABLE_FEATURES: &[(&str, &str)] = &[
    (
        "web",
        "HTTP server, REST handlers, OpenAPI docs (firefly-web)",
    ),
    (
        "data",
        "Relational data + migrations (firefly-data, firefly-migrations)",
    ),
    ("mongodb", "Document data (firefly-data document store)"),
    ("eda", "Event-driven architecture (firefly-eda)"),
    ("cache", "Caching abstraction (firefly-cache)"),
    ("client", "Resilient HTTP client (firefly-client)"),
    (
        "security",
        "JWT auth and password hashing (firefly-security)",
    ),
    (
        "scheduling",
        "Cron-based task scheduling (firefly-scheduling)",
    ),
    (
        "observability",
        "Metrics and tracing (firefly-observability)",
    ),
    (
        "cqrs",
        "Command/Query Responsibility Segregation (firefly-cqrs)",
    ),
    ("shell", "Interactive shell commands (firefly-shell)"),
    (
        "transactional",
        "Distributed SAGA/TCC (firefly-transactional, firefly-orchestration)",
    ),
];

/// Returns `true` if `feature` is in [`AVAILABLE_FEATURES`].
pub fn is_known_feature(feature: &str) -> bool {
    AVAILABLE_FEATURES.iter().any(|(f, _)| *f == feature)
}

// ── Embedded templates ──

macro_rules! atmpl {
    ($name:literal) => {
        ($name, include_str!(concat!("templates/archetype/", $name)))
    };
}

const ARCHETYPE_TEMPLATES: &[(&str, &str)] = &[
    atmpl!("cargo.toml.j2"),
    atmpl!("firefly.yaml.j2"),
    atmpl!("gitignore.j2"),
    atmpl!("readme.md.j2"),
    atmpl!("dockerfile.j2"),
    atmpl!("core_test.rs.j2"),
    atmpl!("main_core.rs.j2"),
    atmpl!("main_web_api.rs.j2"),
    atmpl!("main_hex.rs.j2"),
    atmpl!("lib_library.rs.j2"),
    atmpl!("library_test.rs.j2"),
    atmpl!("web_api_lib.rs.j2"),
    atmpl!("web_api_controllers.rs.j2"),
    atmpl!("web_api_todo_model.rs.j2"),
    atmpl!("web_api_todo_service.rs.j2"),
    atmpl!("web_api_todo_repository.rs.j2"),
    atmpl!("web_api_models_mod.rs.j2"),
    atmpl!("web_api_services_mod.rs.j2"),
    atmpl!("web_api_repositories_mod.rs.j2"),
    atmpl!("web_api_test.rs.j2"),
    atmpl!("web/main.rs.j2"),
    atmpl!("web/lib.rs.j2"),
    atmpl!("web/home_controller.rs.j2"),
    atmpl!("web/page_service.rs.j2"),
    atmpl!("web/services_mod.rs.j2"),
    atmpl!("web/home.html.j2"),
    atmpl!("web/about.html.j2"),
    atmpl!("web/test_pages.rs.j2"),
    atmpl!("hex/lib.rs.j2"),
    atmpl!("hex/domain_mod.rs.j2"),
    atmpl!("hex/domain_models.rs.j2"),
    atmpl!("hex/ports.rs.j2"),
    atmpl!("hex/application.rs.j2"),
    atmpl!("hex/infrastructure.rs.j2"),
    atmpl!("hex/infrastructure_mod.rs.j2"),
    atmpl!("hex/adapters_mod.rs.j2"),
    atmpl!("hex/api.rs.j2"),
    atmpl!("hex/test_models.rs.j2"),
    atmpl!("cli/main.rs.j2"),
    atmpl!("cli/lib.rs.j2"),
    atmpl!("cli/hello_command.rs.j2"),
    atmpl!("cli/greeting_service.rs.j2"),
    atmpl!("cli/commands_mod.rs.j2"),
    atmpl!("cli/services_mod.rs.j2"),
    atmpl!("cli/test_hello.rs.j2"),
];

/// `(template_name, output_path)` mapping per archetype. Output paths use the
/// `{package_name}` placeholder (substituted at render time). Mirrors pyfly's
/// `_ARCHETYPE_FILES`, retargeted to a Rust crate layout.
fn archetype_files(archetype: Archetype) -> Vec<(&'static str, &'static str)> {
    let shared: &[(&str, &str)] = &[
        ("cargo.toml.j2", "Cargo.toml"),
        ("firefly.yaml.j2", "firefly.yaml"),
        ("gitignore.j2", ".gitignore"),
        ("readme.md.j2", "README.md"),
        ("dockerfile.j2", "Dockerfile"),
    ];
    let mut files: Vec<(&str, &str)> = Vec::new();
    match archetype {
        Archetype::Core => {
            files.extend_from_slice(shared);
            files.push(("main_core.rs.j2", "src/main.rs"));
            files.push(("core_test.rs.j2", "tests/smoke.rs"));
        }
        Archetype::WebApi => {
            files.extend_from_slice(shared);
            files.push(("main_web_api.rs.j2", "src/main.rs"));
            files.push(("web_api_lib.rs.j2", "src/lib.rs"));
            files.push(("web_api_controllers.rs.j2", "src/controllers.rs"));
            files.push(("web_api_models_mod.rs.j2", "src/models.rs"));
            files.push(("web_api_todo_model.rs.j2", "src/models/todo.rs"));
            files.push(("web_api_services_mod.rs.j2", "src/services.rs"));
            files.push(("web_api_todo_service.rs.j2", "src/services/todo_service.rs"));
            files.push(("web_api_repositories_mod.rs.j2", "src/repositories.rs"));
            files.push((
                "web_api_todo_repository.rs.j2",
                "src/repositories/todo_repository.rs",
            ));
            files.push(("web_api_test.rs.j2", "tests/api.rs"));
        }
        Archetype::Web => {
            files.extend_from_slice(shared);
            files.push(("web/main.rs.j2", "src/main.rs"));
            files.push(("web/lib.rs.j2", "src/lib.rs"));
            files.push(("web/home_controller.rs.j2", "src/controllers.rs"));
            files.push(("web/services_mod.rs.j2", "src/services.rs"));
            files.push(("web/page_service.rs.j2", "src/services/page_service.rs"));
            files.push(("web/home.html.j2", "src/templates/home.html"));
            files.push(("web/about.html.j2", "src/templates/about.html"));
            files.push(("web/test_pages.rs.j2", "tests/pages.rs"));
        }
        Archetype::Hexagonal => {
            files.extend_from_slice(shared);
            files.push(("main_hex.rs.j2", "src/main.rs"));
            files.push(("hex/lib.rs.j2", "src/lib.rs"));
            files.push(("hex/domain_mod.rs.j2", "src/domain.rs"));
            files.push(("hex/domain_models.rs.j2", "src/domain/models.rs"));
            files.push(("hex/ports.rs.j2", "src/domain/ports.rs"));
            files.push(("hex/application.rs.j2", "src/application.rs"));
            files.push(("hex/infrastructure_mod.rs.j2", "src/infrastructure.rs"));
            files.push(("hex/adapters_mod.rs.j2", "src/infrastructure/adapters.rs"));
            files.push((
                "hex/infrastructure.rs.j2",
                "src/infrastructure/adapters/persistence.rs",
            ));
            files.push(("hex/api.rs.j2", "src/api.rs"));
            files.push(("hex/test_models.rs.j2", "tests/domain.rs"));
        }
        Archetype::Library => {
            files.extend_from_slice(&[
                ("cargo.toml.j2", "Cargo.toml"),
                ("gitignore.j2", ".gitignore"),
                ("readme.md.j2", "README.md"),
            ]);
            files.push(("lib_library.rs.j2", "src/lib.rs"));
            files.push(("library_test.rs.j2", "tests/lib.rs"));
        }
        Archetype::Cli => {
            files.extend_from_slice(shared);
            files.push(("cli/main.rs.j2", "src/main.rs"));
            files.push(("cli/lib.rs.j2", "src/lib.rs"));
            files.push(("cli/commands_mod.rs.j2", "src/commands.rs"));
            files.push(("cli/hello_command.rs.j2", "src/commands/hello_command.rs"));
            files.push(("cli/services_mod.rs.j2", "src/services.rs"));
            files.push((
                "cli/greeting_service.rs.j2",
                "src/services/greeting_service.rs",
            ));
            files.push(("cli/test_hello.rs.j2", "tests/hello.rs"));
        }
    }
    files
}

/// Convert a project name to a valid Rust crate package name (`snake_case`-ish,
/// hyphens preserved are not valid for `mod` paths but are valid Cargo names;
/// we lowercase and replace spaces with hyphens to match Cargo conventions).
fn to_package_name(name: &str) -> String {
    name.replace(' ', "-").to_lowercase()
}

/// Maps a `firefly-<name>` crate to its workspace subdirectory under
/// `crates/`. Every crate the archetypes/generators can reference lives in a
/// directory named after the crate's suffix (e.g. `firefly-starter-core` →
/// `crates/starter-core`), so a `--dep-path <base>` must be joined with that
/// subdirectory to resolve a path dependency correctly. (A `git` source needs
/// no mapping — Cargo locates every package inside the repo automatically.)
fn crate_subdir(crate_name: &str) -> &str {
    crate_name.strip_prefix("firefly-").unwrap_or(crate_name)
}

/// Where to fetch the `firefly-*` crates from in the generated `Cargo.toml`.
///
/// The rendered dependency spec is *per crate*: a `git` source is identical for
/// every crate (Cargo resolves the package within the repo), but a `path`
/// source must append each crate's `crates/<subdir>` so the generated project
/// compiles against a local checkout.
#[derive(Debug, Clone)]
pub enum DepSource {
    /// `git = "<url>"` (default: the canonical GitHub repo).
    Git(String),
    /// `path = "<base>"` — joined with `crates/<crate-subdir>` per dependency
    /// for local workspace development (e.g. `--dep-path ../firefly`).
    Path(String),
    /// `version = "<semver>"` — once published to crates.io.
    Version(String),
}

impl DepSource {
    /// Render the inline-table body of a dependency on `crate_name`
    /// (everything between the braces in `firefly-x = { ... }`).
    fn render_for(&self, crate_name: &str) -> String {
        match self {
            DepSource::Git(url) => format!("git = \"{url}\""),
            DepSource::Path(base) => {
                let base = base.trim_end_matches('/');
                let sub = crate_subdir(crate_name);
                format!("path = \"{base}/crates/{sub}\"")
            }
            DepSource::Version(v) => format!("version = \"{v}\""),
        }
    }
}

impl Default for DepSource {
    fn default() -> Self {
        DepSource::Git("https://github.com/fireflyframework/fireflyframework-rust".to_string())
    }
}

/// Every `firefly-*` crate a generated `Cargo.toml` template can reference,
/// rendered into a `crate_name -> "<source spec>"` map so the template emits
/// the correct per-crate dependency line (path deps resolve into each crate's
/// `crates/<subdir>`; git/version deps are uniform).
const TEMPLATED_FIREFLY_CRATES: &[&str] = &[
    "firefly-kernel",
    "firefly-config",
    "firefly-lifecycle",
    "firefly-starter-core",
    "firefly-starter-web",
    "firefly-web",
    "firefly-cqrs",
    "firefly-data",
    "firefly-migrations",
    "firefly-eda",
    "firefly-cache",
    "firefly-client",
    "firefly-security",
    "firefly-scheduling",
    "firefly-observability",
    "firefly-eventsourcing",
    "firefly-orchestration",
    "firefly-transactional",
    "firefly-shell",
];

/// Build the `deps` map exposed to the `cargo.toml.j2` template: each
/// `firefly-*` crate name mapped to its rendered inline-table body for the
/// chosen [`DepSource`].
fn dep_specs(dep_source: &DepSource) -> std::collections::BTreeMap<String, String> {
    TEMPLATED_FIREFLY_CRATES
        .iter()
        .map(|name| ((*name).to_string(), dep_source.render_for(name)))
        .collect()
}

/// Build the minijinja environment with every archetype template registered.
fn archetype_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_keep_trailing_newline(true);
    env.set_lstrip_blocks(true);
    env.set_trim_blocks(true);
    for (name, src) in ARCHETYPE_TEMPLATES {
        env.add_template(name, src).expect("static template parses");
    }
    env
}

/// Plan every artifact `firefly new` would write for a project, without
/// touching the filesystem. Exposed for snapshot tests and `--dry-run`.
///
/// # Errors
/// Returns [`CliError::Template`] on a render failure.
pub fn plan_project(
    name: &str,
    project_dir: &Path,
    archetype: Archetype,
    features: &[String],
    package_name: Option<&str>,
    dep_source: &DepSource,
) -> Result<Vec<Artifact>, CliError> {
    let package_name = package_name
        .map(str::to_string)
        .unwrap_or_else(|| to_package_name(name));
    let has = |f: &str| features.iter().any(|x| x == f);
    let env = archetype_env();

    let ctx = context! {
        name => name,
        package_name => package_name.clone(),
        archetype => archetype.as_str(),
        features => features,
        deps => dep_specs(dep_source),
        has_web => has("web"),
        has_data => has("data"),
        has_mongodb => has("mongodb"),
        has_eda => has("eda"),
        has_cache => has("cache"),
        has_client => has("client"),
        has_security => has("security"),
        has_scheduling => has("scheduling"),
        has_observability => has("observability"),
        has_cqrs => has("cqrs"),
        has_shell => has("shell"),
        has_transactional => has("transactional"),
    };

    let mut artifacts = Vec::new();
    for (template_name, output_path) in archetype_files(archetype) {
        let rel = output_path.replace("{package_name}", &package_name);
        let rendered = env.get_template(template_name)?.render(&ctx)?;
        artifacts.push(Artifact::new("file", project_dir.join(&rel), rendered));
    }
    Ok(artifacts)
}

/// Generate a project from templates into `project_dir`.
///
/// Faithful port of pyfly's `generate_project`. Returns the planned actions so
/// the caller can render a creation tree.
///
/// # Errors
/// Returns [`CliError::Template`] on render failure or [`CliError::Io`] on a
/// filesystem write failure.
#[allow(clippy::too_many_arguments)]
pub fn generate_project(
    name: &str,
    project_dir: &Path,
    archetype: Archetype,
    features: &[String],
    package_name: Option<&str>,
    dep_source: &DepSource,
    dry_run: bool,
) -> Result<Vec<(Action, PathBuf)>, CliError> {
    let artifacts = plan_project(
        name,
        project_dir,
        archetype,
        features,
        package_name,
        dep_source,
    )?;
    // `firefly new` always writes fresh files (the target dir must not exist),
    // so `force` is irrelevant; pass true to overwrite-on-collision-free dirs.
    write_artifacts(&artifacts, true, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::TempDir;

    fn feats(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn archetype_parse_roundtrip() {
        for a in Archetype::ALL {
            assert_eq!(Archetype::parse(a.as_str()).unwrap(), a);
        }
        assert!(Archetype::parse("fastapi-api").is_err());
    }

    #[test]
    fn default_features_match_pyfly() {
        assert!(Archetype::Core.default_features().is_empty());
        assert_eq!(Archetype::WebApi.default_features(), vec!["web"]);
        // The CLI archetype is a plain clap binary by default (no firefly-shell
        // unless the user opts into `--features shell`).
        assert!(Archetype::Cli.default_features().is_empty());
        assert!(Archetype::Library.default_features().is_empty());
    }

    #[test]
    fn every_archetype_template_resolves() {
        // Guards against a missing include_str!/path mismatch.
        let env = archetype_env();
        for a in Archetype::ALL {
            for (tmpl, _) in archetype_files(a) {
                assert!(env.get_template(tmpl).is_ok(), "missing template {tmpl}");
            }
        }
    }

    #[test]
    fn dep_source_render_variants() {
        // Path deps resolve into each crate's `crates/<subdir>`.
        assert_eq!(
            DepSource::Path("../firefly".into()).render_for("firefly-web"),
            "path = \"../firefly/crates/web\""
        );
        assert_eq!(
            DepSource::Path("../firefly/".into()).render_for("firefly-starter-core"),
            "path = \"../firefly/crates/starter-core\""
        );
        // Version/git deps are uniform across crates.
        assert_eq!(
            DepSource::Version("26.6.22".into()).render_for("firefly-kernel"),
            "version = \"26.6.22\""
        );
        assert!(DepSource::default()
            .render_for("firefly-kernel")
            .starts_with("git ="));
    }

    #[test]
    fn web_api_project_snapshot_markers() {
        let tmp = TempDir::new().unwrap();
        let arts = plan_project(
            "shop",
            tmp.path(),
            Archetype::WebApi,
            &feats(&["web"]),
            None,
            &DepSource::default(),
        )
        .unwrap();
        let by_name = |suffix: &str| {
            arts.iter()
                .find(|a| a.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("missing {suffix}"))
        };
        // Cargo.toml is workspace-less and wires firefly-web via the dep source.
        let cargo = &by_name("Cargo.toml").content;
        assert!(cargo.contains("name = \"shop\""));
        assert!(cargo.contains("firefly-web = { git ="));
        assert!(cargo.contains("firefly-starter-web = { git ="));
        // firefly.yaml records the archetype (parity with pyfly persistence test).
        let yaml = &by_name("firefly.yaml").content;
        assert!(yaml.contains("archetype: web-api"));
        // main.rs boots the real web starter (WebStack) and runs the
        // lifecycle application; the layered tree exists.
        let main = &by_name("src/main.rs").content;
        assert!(main.contains("WebStack::new"));
        assert!(main.contains("new_application"));
        assert!(by_name("src/lib.rs")
            .content
            .contains("pub mod controllers"));
        assert!(by_name("src/controllers.rs").content.contains("fn routes"));
        assert!(by_name("src/models/todo.rs")
            .content
            .contains("struct Todo"));
    }

    #[test]
    fn library_project_has_lib_not_main() {
        let tmp = TempDir::new().unwrap();
        let arts = plan_project(
            "mylib",
            tmp.path(),
            Archetype::Library,
            &[],
            None,
            &DepSource::default(),
        )
        .unwrap();
        let paths: HashSet<String> = arts
            .iter()
            .map(|a| a.path.to_string_lossy().into_owned())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")));
        assert!(!paths.iter().any(|p| p.ends_with("src/main.rs")));
        // No Dockerfile / firefly.yaml for a library.
        assert!(!paths.iter().any(|p| p.ends_with("Dockerfile")));
        assert!(!paths.iter().any(|p| p.ends_with("firefly.yaml")));
    }

    #[test]
    fn hexagonal_layers_present() {
        let tmp = TempDir::new().unwrap();
        let arts = plan_project(
            "bank",
            tmp.path(),
            Archetype::Hexagonal,
            &feats(&["web"]),
            None,
            &DepSource::default(),
        )
        .unwrap();
        let paths: HashSet<String> = arts
            .iter()
            .map(|a| a.path.to_string_lossy().into_owned())
            .collect();
        for layer in [
            "src/domain/models.rs",
            "src/domain/ports.rs",
            "src/application.rs",
            "src/infrastructure/adapters/persistence.rs",
            "src/api.rs",
        ] {
            assert!(paths.iter().any(|p| p.ends_with(layer)), "missing {layer}");
        }
    }

    #[test]
    fn package_name_substitution_and_dep_source_path() {
        let tmp = TempDir::new().unwrap();
        let arts = plan_project(
            "My Service",
            tmp.path(),
            Archetype::Core,
            &[],
            None,
            &DepSource::Path("../firefly".into()),
        )
        .unwrap();
        let cargo = arts
            .iter()
            .find(|a| a.path.ends_with("Cargo.toml"))
            .unwrap();
        // "My Service" -> "my-service" Cargo name.
        assert!(cargo.content.contains("name = \"my-service\""));
        // Path deps resolve into each crate's own `crates/<subdir>`.
        assert!(cargo
            .content
            .contains("firefly-kernel = { path = \"../firefly/crates/kernel\" }"));
    }

    #[test]
    fn generate_writes_to_disk_and_dry_run_does_not() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("svc");
        // Dry run writes nothing.
        generate_project(
            "svc",
            &dir,
            Archetype::Core,
            &[],
            None,
            &DepSource::default(),
            true,
        )
        .unwrap();
        assert!(!dir.exists());
        // Real run writes the files.
        generate_project(
            "svc",
            &dir,
            Archetype::Core,
            &[],
            None,
            &DepSource::default(),
            false,
        )
        .unwrap();
        assert!(dir.join("Cargo.toml").is_file());
        assert!(dir.join("src/main.rs").is_file());
        assert!(dir.join("firefly.yaml").is_file());
        assert!(dir.join(".gitignore").is_file());
    }

    #[test]
    fn feature_flags_drive_cargo_deps() {
        let tmp = TempDir::new().unwrap();
        let arts = plan_project(
            "svc",
            tmp.path(),
            Archetype::Core,
            &feats(&["data", "cqrs", "security"]),
            None,
            &DepSource::default(),
        )
        .unwrap();
        let cargo = &arts
            .iter()
            .find(|a| a.path.ends_with("Cargo.toml"))
            .unwrap()
            .content;
        assert!(cargo.contains("firefly-data"));
        assert!(cargo.contains("firefly-migrations"));
        assert!(cargo.contains("firefly-cqrs"));
        assert!(cargo.contains("firefly-security"));
        assert!(!cargo.contains("firefly-eda"));
    }
}
