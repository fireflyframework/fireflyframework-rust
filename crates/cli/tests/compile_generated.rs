//! Compile-verification for `firefly new` scaffolds.
//!
//! Every archetype `firefly new` emits must produce a project that **actually
//! compiles** against the real `firefly-*` crates — not plausible-looking Rust.
//! These tests generate each archetype into a tempdir, pointing the generated
//! `firefly-*` dependencies at the local workspace checkout (`DepSource::Path`),
//! and run `cargo check --tests` on the result.
//!
//! `cargo check` against the whole framework is expensive, so the heavy check is
//! gated behind the `FIREFLY_CLI_COMPILE_TEST=1` environment variable. It is
//! enabled in the project's verification gate; an ordinary `cargo test` skips it
//! (printing a one-line note) but still runs the always-on structural assertions
//! below, which confirm the generated `Cargo.toml`/`main.rs` carry the real API
//! markers (`WebStack`/`Core::new`/`new_application`, never the removed
//! `FireflyServer`).

use std::path::{Path, PathBuf};
use std::process::Command;

use firefly_cli::scaffold::{scaffold_new, NewOptions};
use firefly_cli::templates::{Archetype, DepSource};

/// The workspace root (two levels up from `crates/cli`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/cli has a workspace root")
        .to_path_buf()
}

/// Every archetype paired with a representative feature set.
fn archetype_matrix() -> Vec<(Archetype, Vec<&'static str>)> {
    vec![
        (Archetype::Core, vec![]),
        (Archetype::Core, vec!["data", "cqrs"]),
        (Archetype::WebApi, vec!["web"]),
        (Archetype::Web, vec!["web"]),
        (Archetype::Hexagonal, vec!["web"]),
        (Archetype::Library, vec![]),
        (Archetype::Cli, vec![]),
    ]
}

fn scaffold_at(parent: &Path, name: &str, archetype: Archetype, features: &[&str]) -> PathBuf {
    let opts = NewOptions {
        name: name.to_string(),
        archetype,
        features: features.iter().map(|s| s.to_string()).collect(),
        // Point firefly-* deps at the local workspace so `cargo check` needs no
        // network and resolves the in-tree crates.
        dep_source: DepSource::Path(workspace_root().to_string_lossy().into_owned()),
        force: false,
        dry_run: false,
        init_git: false,
    };
    scaffold_new(parent, &opts)
        .expect("scaffold succeeds")
        .project_dir
}

/// Always-on: the generated tree carries the real-API markers, never the old
/// fictional `FireflyServer` / `Application::builder()` placeholders.
#[test]
fn generated_projects_use_real_api_markers() {
    for (archetype, features) in archetype_matrix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proj = scaffold_at(tmp.path(), "svc", archetype, &features);

        let cargo = std::fs::read_to_string(proj.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("[dependencies]"), "{archetype:?} Cargo.toml");
        assert!(
            !cargo.contains("FireflyServer"),
            "{archetype:?} must not reference the removed FireflyServer"
        );

        let main_or_lib = if archetype == Archetype::Library {
            proj.join("src/lib.rs")
        } else {
            proj.join("src/main.rs")
        };
        let src = std::fs::read_to_string(&main_or_lib).unwrap();
        assert!(
            !src.contains("FireflyServer") && !src.contains("Application::builder"),
            "{archetype:?} entry point references a removed placeholder API"
        );
        match archetype {
            Archetype::Core => {
                assert!(src.contains("Core::new"), "core main wires Core::new");
                assert!(src.contains("new_application"));
            }
            Archetype::WebApi | Archetype::Web | Archetype::Hexagonal => {
                assert!(
                    src.contains("WebStack::new"),
                    "{archetype:?} wires WebStack"
                );
            }
            _ => {}
        }
    }
}

/// Gated heavy check: every archetype compiles against the real framework.
///
/// Enable with `FIREFLY_CLI_COMPILE_TEST=1 cargo test -p firefly-cli`.
#[test]
fn generated_projects_compile() {
    if std::env::var("FIREFLY_CLI_COMPILE_TEST").as_deref() != Ok("1") {
        eprintln!(
            "skipping generated_projects_compile (set FIREFLY_CLI_COMPILE_TEST=1 to run the \
             real `cargo check` over every scaffold)"
        );
        return;
    }
    let ws = workspace_root();
    // Share a target dir so the framework crates are built once across archetypes.
    let target_dir = ws.join("target").join("cli-gen-compile-test");

    for (archetype, features) in archetype_matrix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proj = scaffold_at(tmp.path(), "svc", archetype, &features);

        let status = Command::new(env!("CARGO"))
            .current_dir(&proj)
            .env("CARGO_TARGET_DIR", &target_dir)
            .args(["check", "--tests"])
            .status()
            .expect("cargo check runs");
        assert!(
            status.success(),
            "generated {archetype:?} project (features {features:?}) failed `cargo check`"
        );
    }
}
