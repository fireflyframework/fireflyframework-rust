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

//! High-level `firefly new` flow: name validation, project generation, and
//! optional git initialization.
//!
//! Port of the non-interactive parts of pyfly's `new.py`. The interactive
//! questionary wizard is intentionally omitted (the brief scopes the first cut
//! to non-interactive `--archetype`/`--features` flags).

use std::path::Path;
use std::process::Command;

use crate::error::CliError;
use crate::generate::Action;
use crate::templates::{generate_project, is_known_feature, Archetype, DepSource};

/// Names that collide with common Rust crates / keywords and should be rejected
/// as project names. Rust analogue of pyfly's `_RESERVED_NAMES`.
const RESERVED_NAMES: &[&str] = &[
    "test", "tests", "src", "firefly", "core", "std", "alloc", "main", "lib", "self", "crate",
    "super", "async", "await", "dyn", "match", "move", "ref", "type", "use", "mod", "fn", "impl",
    "trait", "where", "tokio", "serde", "axum", "clap",
];

/// Validate a project name. Returns a friendly error message, or `None` if valid.
///
/// Mirrors pyfly's `_validate_project_name`: must start with a letter and
/// contain only letters, digits, hyphens, underscores; must not be a reserved
/// or keyword-like name.
pub fn validate_project_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Project name cannot be empty.".to_string());
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() {
        return Some(
            "Project name must start with a letter and contain only letters, digits, hyphens, and underscores."
                .to_string(),
        );
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(
            "Project name must start with a letter and contain only letters, digits, hyphens, and underscores."
                .to_string(),
        );
    }
    let pkg = name.replace(['-', ' '], "_").to_lowercase();
    if RESERVED_NAMES.contains(&pkg.as_str()) {
        return Some(format!(
            "'{name}' conflicts with a Rust keyword or common crate. Choose a different name."
        ));
    }
    None
}

/// Options for [`scaffold_new`].
#[derive(Debug, Clone)]
pub struct NewOptions {
    /// The project name (also the default Cargo package name).
    pub name: String,
    /// The archetype to scaffold.
    pub archetype: Archetype,
    /// The selected feature set (may be empty).
    pub features: Vec<String>,
    /// Where the generated `firefly-*` deps come from.
    pub dep_source: DepSource,
    /// Overwrite an existing target directory's files when `true`.
    pub force: bool,
    /// Plan only; write nothing.
    pub dry_run: bool,
    /// Initialize a git repository with an initial commit.
    pub init_git: bool,
}

/// The outcome of a `firefly new` invocation.
#[derive(Debug)]
pub struct NewOutcome {
    /// The created project directory.
    pub project_dir: std::path::PathBuf,
    /// The planned/performed write actions.
    pub actions: Vec<(Action, std::path::PathBuf)>,
    /// Whether a git repo was initialized.
    pub git_initialized: bool,
}

/// Run the full `firefly new` flow into `parent`/`<name>`.
///
/// # Errors
/// - [`CliError::InvalidName`] when the name fails validation,
/// - [`CliError::UnknownFeatures`] when any feature is not in the catalog,
/// - [`CliError::DirectoryExists`] when the target exists and `force` is unset,
/// - [`CliError::Template`]/[`CliError::Io`] on render/write failures.
pub fn scaffold_new(parent: &Path, opts: &NewOptions) -> Result<NewOutcome, CliError> {
    if let Some(msg) = validate_project_name(&opts.name) {
        return Err(CliError::InvalidName(msg));
    }
    let unknown: Vec<String> = opts
        .features
        .iter()
        .filter(|f| !is_known_feature(f))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(CliError::UnknownFeatures(unknown.join(", ")));
    }

    let project_dir = parent.join(&opts.name);
    if project_dir.exists() && !opts.force && !opts.dry_run {
        return Err(CliError::DirectoryExists(project_dir));
    }

    let actions = generate_project(
        &opts.name,
        &project_dir,
        opts.archetype,
        &opts.features,
        None,
        &opts.dep_source,
        opts.dry_run,
    )?;

    let git_initialized = if opts.init_git && !opts.dry_run {
        init_git_repo(&project_dir)
    } else {
        false
    };

    Ok(NewOutcome {
        project_dir,
        actions,
        git_initialized,
    })
}

/// Initialize a git repository in `project_dir` with an initial commit.
///
/// Returns `true` on success, `false` if `git` is missing or any step fails.
/// Port of pyfly's `_init_git_repo` (uses `-c user.name`/`user.email` so the
/// commit succeeds even without global git config).
pub fn init_git_repo(project_dir: &Path) -> bool {
    if which_git().is_none() {
        return false;
    }
    let run = |args: &[&str]| -> bool {
        Command::new("git")
            .args(args)
            .current_dir(project_dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    run(&["init", "-q"])
        && run(&["add", "-A"])
        && run(&[
            "-c",
            "user.name=Firefly",
            "-c",
            "user.email=firefly@example.com",
            "commit",
            "-q",
            "-m",
            "Initial commit from firefly new",
        ])
}

/// Return the path to `git` on `PATH`, if any.
fn which_git() -> Option<String> {
    Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| "git".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_accepts_good_names() {
        assert!(validate_project_name("my-service").is_none());
        assert!(validate_project_name("svc").is_none());
        assert!(validate_project_name("order_api").is_none());
    }

    #[test]
    fn validate_rejects_bad_names() {
        assert!(validate_project_name("").is_some());
        assert!(validate_project_name("9lives").is_some());
        assert!(validate_project_name("has space").is_some());
        assert!(validate_project_name("bad/slash").is_some());
        // reserved
        assert!(validate_project_name("std").is_some());
        assert!(validate_project_name("tokio").is_some());
    }

    #[test]
    fn scaffold_creates_project() {
        let tmp = TempDir::new().unwrap();
        let opts = NewOptions {
            name: "svc".into(),
            archetype: Archetype::Core,
            features: vec![],
            dep_source: DepSource::default(),
            force: false,
            dry_run: false,
            init_git: false,
        };
        let outcome = scaffold_new(tmp.path(), &opts).unwrap();
        assert!(outcome.project_dir.join("Cargo.toml").is_file());
        assert!(outcome.project_dir.join("src/main.rs").is_file());
        assert!(!outcome.git_initialized);
    }

    #[test]
    fn scaffold_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let opts = NewOptions {
            name: "svc".into(),
            archetype: Archetype::WebApi,
            features: vec!["web".into()],
            dep_source: DepSource::default(),
            force: false,
            dry_run: true,
            init_git: true, // ignored under dry-run
        };
        let outcome = scaffold_new(tmp.path(), &opts).unwrap();
        assert!(!outcome.project_dir.exists());
        assert!(!outcome.git_initialized);
        assert!(!outcome.actions.is_empty());
    }

    #[test]
    fn scaffold_errors_on_existing_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("svc")).unwrap();
        let opts = NewOptions {
            name: "svc".into(),
            archetype: Archetype::Core,
            features: vec![],
            dep_source: DepSource::default(),
            force: false,
            dry_run: false,
            init_git: false,
        };
        let err = scaffold_new(tmp.path(), &opts);
        assert!(matches!(err, Err(CliError::DirectoryExists(_))));
    }

    #[test]
    fn scaffold_errors_on_unknown_feature() {
        let tmp = TempDir::new().unwrap();
        let opts = NewOptions {
            name: "svc".into(),
            archetype: Archetype::Core,
            features: vec!["telepathy".into()],
            dep_source: DepSource::default(),
            force: false,
            dry_run: false,
            init_git: false,
        };
        let err = scaffold_new(tmp.path(), &opts);
        assert!(matches!(err, Err(CliError::UnknownFeatures(_))));
    }

    #[test]
    fn scaffold_errors_on_bad_name() {
        let tmp = TempDir::new().unwrap();
        let opts = NewOptions {
            name: "9bad".into(),
            archetype: Archetype::Core,
            features: vec![],
            dep_source: DepSource::default(),
            force: false,
            dry_run: false,
            init_git: false,
        };
        assert!(matches!(
            scaffold_new(tmp.path(), &opts),
            Err(CliError::InvalidName(_))
        ));
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "git behavior verified on unix CI")]
    fn git_init_creates_repo() {
        if which_git().is_none() {
            return; // git not installed; skip silently
        }
        let tmp = TempDir::new().unwrap();
        let opts = NewOptions {
            name: "svc".into(),
            archetype: Archetype::Core,
            features: vec![],
            dep_source: DepSource::default(),
            force: false,
            dry_run: false,
            init_git: true,
        };
        let outcome = scaffold_new(tmp.path(), &opts).unwrap();
        assert!(outcome.git_initialized);
        assert!(outcome.project_dir.join(".git").is_dir());
    }
}
