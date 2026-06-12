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
        // `core` + `web` is a reachable public-CLI path (`firefly new x --archetype
        // core --features web`): scaffold validates only that `web` is a known
        // feature, with no archetype/feature compatibility guard. It must still
        // emit a Cargo.toml Cargo can parse (regression: duplicate dependency
        // keys when both the core block and the web block fired).
        (Archetype::Core, vec!["web"]),
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

/// Collect the `key = ...` left-hand sides of a `[<section>]` table in a
/// generated `Cargo.toml`, in order. Used to assert Cargo would not reject the
/// manifest for a duplicate key. Only top-level `key = ...` lines are read; the
/// section ends at the next `[...]` table header.
fn table_keys(manifest: &str, section: &str) -> Vec<String> {
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut keys = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            continue;
        }
        if in_section {
            if let Some((lhs, _)) = trimmed.split_once('=') {
                let key = lhs.trim();
                if !key.is_empty() {
                    keys.push(key.to_string());
                }
            }
        }
    }
    keys
}

/// Always-on regression: no generated `Cargo.toml` carries a duplicate
/// dependency key, which Cargo rejects outright (`error: duplicate key`) so the
/// project would not even parse.
///
/// Guards the `core` + `web` path specifically: before the fix, the `[% if
/// archetype == "core" %]` and `[% if has_web %]` blocks in `cargo.toml.j2`
/// both fired, emitting `firefly-starter-core`, `firefly-cqrs`,
/// `firefly-lifecycle`, and `axum` twice. The `has_web` block is now gated on
/// `archetype != "core"`.
#[test]
fn generated_cargo_toml_has_no_duplicate_dependency_keys() {
    for (archetype, features) in archetype_matrix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proj = scaffold_at(tmp.path(), "svc", archetype, &features);
        let manifest = std::fs::read_to_string(proj.join("Cargo.toml")).unwrap();

        for section in ["dependencies", "dev-dependencies"] {
            let keys = table_keys(&manifest, section);
            let mut seen = std::collections::HashSet::new();
            for key in &keys {
                assert!(
                    seen.insert(key.clone()),
                    "{archetype:?} (features {features:?}) emits duplicate key \
                     `{key}` in [{section}] — Cargo would reject this manifest:\n{manifest}"
                );
            }
        }
    }

    // Belt-and-braces for the specific reproduction: `core` + `web` emits each
    // of the previously-doubled keys exactly once in [dependencies].
    let tmp = tempfile::TempDir::new().unwrap();
    let proj = scaffold_at(tmp.path(), "svc", Archetype::Core, &["web"]);
    let manifest = std::fs::read_to_string(proj.join("Cargo.toml")).unwrap();
    let deps = table_keys(&manifest, "dependencies");
    for key in [
        "firefly-starter-core",
        "firefly-cqrs",
        "firefly-lifecycle",
        "axum",
    ] {
        let count = deps.iter().filter(|k| k.as_str() == key).count();
        assert_eq!(
            count, 1,
            "core + web must emit `{key}` exactly once, found {count}:\n{manifest}"
        );
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
