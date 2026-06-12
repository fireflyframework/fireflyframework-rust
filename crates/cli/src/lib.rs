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
//! - [`scaffold`] — the high-level `firefly new` flow (validation, git init).
//! - [`actuator`] — the remote `/actuator/*` introspection client.
//! - [`db`] — the `firefly db` migration command group (firefly-migrations).
//! - [`openapi`] — the `firefly openapi` skeleton-spec export.
//! - [`diagnostics`] — `firefly info` / `firefly doctor` environment checks.
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
pub mod cli;
pub mod db;
pub mod diagnostics;
pub mod error;
pub mod generate;
pub mod naming;
pub mod openapi;
pub mod project;
pub mod scaffold;
pub mod templates;

pub use error::CliError;

/// Framework version stamp (kept for backward compatibility with the prior
/// placeholder crate; equals the workspace version).
pub const VERSION: &str = "26.6.1";
