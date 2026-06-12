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

//! End-to-end integration tests for the firefly-cli library surface.
//!
//! These mirror the pyfly `CliRunner` integration tests: scaffold a project,
//! then run a generator against it and assert the artifacts land on disk with
//! the expected case-converted names and content markers. The generated Rust
//! is *not* compiled — only its structure is asserted.

use std::fs;
use std::path::Path;

use firefly_cli::db::{db_downgrade, db_init, db_migrate, db_status, db_upgrade};
use firefly_cli::error::CliError;
use firefly_cli::generate::{plan_artifacts, write_artifacts, ArtifactKind};
use firefly_cli::openapi::{meta_for_project, render_spec, OpenApiFormat};
use firefly_cli::project::detect_project;
use firefly_cli::scaffold::{scaffold_new, NewOptions};
use firefly_cli::templates::{Archetype, DepSource};

fn opts(name: &str, archetype: Archetype, features: &[&str]) -> NewOptions {
    NewOptions {
        name: name.to_string(),
        archetype,
        features: features.iter().map(|s| s.to_string()).collect(),
        dep_source: DepSource::default(),
        force: false,
        dry_run: false,
        init_git: false,
    }
}

fn generate(root: &Path, kind: ArtifactKind, name: &str) {
    let info = detect_project(Some(root)).unwrap();
    let arts = plan_artifacts(&info, kind, name).unwrap();
    write_artifacts(&arts, false, false).unwrap();
}

#[test]
fn new_then_generate_handler_lands_on_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let outcome = scaffold_new(tmp.path(), &opts("shop", Archetype::WebApi, &["web"])).unwrap();
    let root = &outcome.project_dir;

    // The scaffolded project is detectable as web-api.
    let info = detect_project(Some(root)).unwrap();
    assert_eq!(info.package, "shop");
    assert_eq!(info.archetype, "web-api");

    generate(root, ArtifactKind::Handler, "OrderLine");
    let handler = root.join("src/handlers/order_line_handler.rs");
    assert!(handler.is_file());
    let text = fs::read_to_string(&handler).unwrap();
    assert!(text.contains("struct OrderLineHandler"));
}

#[test]
fn generate_command_pair_into_cqrs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let outcome = scaffold_new(tmp.path(), &opts("bank", Archetype::Core, &[])).unwrap();
    let root = &outcome.project_dir;

    generate(root, ArtifactKind::Command, "OpenWallet");
    assert!(root.join("src/cqrs/open_wallet_command.rs").is_file());
    assert!(root
        .join("src/cqrs/open_wallet_command_handler.rs")
        .is_file());
}

#[test]
fn generate_migration_increments_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let outcome = scaffold_new(tmp.path(), &opts("svc", Archetype::Core, &["data"])).unwrap();
    let root = &outcome.project_dir;

    generate(root, ArtifactKind::Migration, "CreateUsers");
    assert!(root.join("migrations/V001__create_users.sql").is_file());

    generate(root, ArtifactKind::Migration, "AddIndexes");
    assert!(root.join("migrations/V002__add_indexes.sql").is_file());
}

#[test]
fn entity_is_data_aware_when_yaml_enables_data() {
    let tmp = tempfile::TempDir::new().unwrap();
    // `data` feature writes a firefly.yaml with relational.enabled: true.
    let outcome = scaffold_new(tmp.path(), &opts("shop", Archetype::Core, &["data"])).unwrap();
    let root = &outcome.project_dir;

    generate(root, ArtifactKind::Entity, "Product");
    let text = fs::read_to_string(root.join("src/models/product.rs")).unwrap();
    assert!(text.contains("Entity"));
    assert!(text.contains("table = \"products\""));
}

#[test]
fn skip_then_force_overwrite() {
    let tmp = tempfile::TempDir::new().unwrap();
    let outcome = scaffold_new(tmp.path(), &opts("shop", Archetype::Core, &[])).unwrap();
    let root = &outcome.project_dir;
    let info = detect_project(Some(root)).unwrap();

    let arts = plan_artifacts(&info, ArtifactKind::Entity, "Order").unwrap();
    write_artifacts(&arts, false, false).unwrap();
    let path = root.join("src/models/order.rs");
    fs::write(&path, "// hand-edited\n").unwrap();

    // Without force, the existing file is preserved.
    write_artifacts(&arts, false, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "// hand-edited\n");

    // With force, it is overwritten by the generated content.
    write_artifacts(&arts, true, false).unwrap();
    assert!(fs::read_to_string(&path).unwrap().contains("struct Order"));
}

// --- `firefly db` migration group (pyfly tests/cli/test_db.py parity) ---

#[test]
fn db_init_then_migrate_then_upgrade_status() {
    // End-to-end mirror of pyfly's TestDbInit/TestDbUpgrade against a file-backed
    // SQLite database (no external server).
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("migrations");

    // init creates migrations/ + a starter V001__init.sql.
    let outcome = db_init(&dir).unwrap();
    assert!(dir.is_dir());
    assert!(outcome.created.is_some());
    assert!(dir.join("V001__init.sql").is_file());

    // Replace the starter with real DDL, then add a second migration.
    fs::write(
        dir.join("V001__init.sql"),
        "CREATE TABLE account (id INTEGER PRIMARY KEY)",
    )
    .unwrap();
    let second = db_migrate(&dir, Some("add balance")).unwrap();
    assert!(second.ends_with("V002__add_balance.sql"));
    fs::write(&second, "ALTER TABLE account ADD COLUMN balance INTEGER").unwrap();

    // upgrade against a shared file-backed db applies both.
    let db_file = tmp.path().join("app.db");
    let url = format!("sqlite://{}", db_file.display());
    assert_eq!(db_upgrade(&dir, &url).unwrap(), 2);

    // status reflects both applied, nothing pending.
    let status = db_status(&dir, &url).unwrap();
    assert_eq!(status.applied.len(), 2);
    assert_eq!(status.pending.len(), 0);

    // re-upgrade is idempotent.
    assert_eq!(db_upgrade(&dir, &url).unwrap(), 0);
}

#[test]
fn db_downgrade_is_unsupported_divergence() {
    // pyfly supports downgrade via Alembic; the Rust forward-only runner does not.
    assert!(matches!(db_downgrade(), Err(CliError::Unsupported(_))));
}

#[test]
fn db_upgrade_rejects_non_sqlite_backend() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("migrations");
    db_init(&dir).unwrap();
    let err = db_upgrade(&dir, "postgres://localhost/db");
    assert!(matches!(err, Err(CliError::Unsupported(_))));
}

// --- `firefly openapi` export (pyfly tests/cli/test_openapi.py parity) ---

#[test]
fn openapi_json_spec_is_openapi_31_with_project_meta() {
    let tmp = tempfile::TempDir::new().unwrap();
    let outcome = scaffold_new(tmp.path(), &opts("widgets", Archetype::WebApi, &["web"])).unwrap();
    let root = &outcome.project_dir;

    let meta = meta_for_project(root);
    assert_eq!(meta.title, "widgets");
    let json = render_spec(&meta, OpenApiFormat::Json).unwrap();
    let spec: serde_json::Value = serde_json::from_str(&json).unwrap();
    // pyfly test_openapi_json_to_stdout: openapi startswith "3.", "paths" present.
    assert!(spec["openapi"].as_str().unwrap().starts_with("3."));
    assert!(spec.get("paths").is_some());
    assert_eq!(spec["info"]["title"], "widgets");
}

#[test]
fn openapi_yaml_round_trips() {
    let meta = meta_for_project(Path::new("/nonexistent-project-root"));
    let yaml = render_spec(&meta, OpenApiFormat::Yaml).unwrap();
    let spec: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(spec["openapi"], "3.1.0");
}
