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

//! firefly-cli — the `firefly` project scaffolding and introspection CLI.
//!
//! This crate provides both the `firefly` binary (`src/main.rs`) and a library
//! surface used by the binary and integration tests. It is the Rust port of
//! pyfly's `pyfly.cli` package, adapted to a compiled Cargo workspace:
//!
//! - [`naming`] — case-conversion (`Names`), a hand-rolled port of `naming.py`.
//! - [`project`] — current-project detection from `Cargo.toml` + `firefly.yaml`.
//! - [`templates`] — `firefly new` archetype scaffolding (minijinja + `include_str!`).
//! - [`generate`] — `firefly generate` per-artifact code generators.
//! - [`run`] — `firefly run` Cargo-native launch with profile/override env mapping.
//! - [`build`] — `firefly build info`/`image` (build-info stamp + OCI image).
//! - [`scaffold`] — the high-level `firefly new` flow (validation, git init).
//! - [`actuator`] — the remote `/actuator/*` introspection client.
//! - [`db`] — the `firefly db` migration command group (firefly-migrations).
//! - [`openapi`] — the `firefly openapi` skeleton-spec export.
//! - [`diagnostics`] — `firefly info` / `firefly doctor` environment checks.
//! - [`completion`] — `firefly completion <shell>` shell-completion scripts.
//! - [`sbom`] — `firefly sbom` Software Bill of Materials from `Cargo.lock`.
//! - [`license`] — `firefly license` framework + dependency license report.
//! - [`cli`] — the clap v4 command definitions and dispatcher.
//!
//! # pyfly parity
//! The naming table, project-detection inference rules, `write_artifacts`
//! force/dry-run semantics, and generator dispatch are ported test-case for
//! test-case from `tests/cli/`.
//!
//! # Generated projects compile
//! Unlike the first cut, the archetypes target the **real** `firefly-*` APIs
//! (`firefly_starter_core::Core`, `firefly_starter_web::WebStack`,
//! `firefly_lifecycle::Application`, the closure-based `firefly_cqrs::Bus`) and
//! produce projects that build out of the box. `tests/compile_generated.rs`
//! verifies this: it scaffolds every archetype with `firefly-*` deps pointed at
//! the local workspace and runs `cargo check --tests` over each one (under the
//! `FIREFLY_CLI_COMPILE_TEST=1` gate, since the full framework check is heavy),
//! with always-on assertions that each scaffold carries the real API markers.

#![forbid(unsafe_code)]

pub mod actuator;
pub mod build;
pub mod cli;
pub mod completion;
pub mod db;
pub mod diagnostics;
pub mod error;
pub mod generate;
pub mod license;
pub mod naming;
pub mod openapi;
pub mod project;
pub mod run;
pub mod sbom;
pub mod scaffold;
pub mod templates;

pub use error::CliError;

/// Framework version stamp (kept for backward compatibility with the prior
/// placeholder crate; equals the workspace version).
pub const VERSION: &str = "26.7.0";
