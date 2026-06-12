//! End-to-end integration tests for the firefly-cli library surface.
//!
//! These mirror the pyfly `CliRunner` integration tests: scaffold a project,
//! then run a generator against it and assert the artifacts land on disk with
//! the expected case-converted names and content markers. The generated Rust
//! is *not* compiled — only its structure is asserted.

use std::fs;
use std::path::Path;

use firefly_cli::generate::{plan_artifacts, write_artifacts, ArtifactKind};
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
