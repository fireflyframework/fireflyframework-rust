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

//! `firefly run` — start the application via Cargo with profile/override env
//! mapping.
//!
//! The Rust analog of pyfly's `pyfly run`. pyfly auto-selects an ASGI server and
//! boots the interpreter; a compiled Firefly app is a normal Cargo binary, so
//! the Rust `run` maps the launch flags to the framework's environment
//! variables (mirroring pyfly's `_to_env_key` / `_build_launch_env`) and then
//! execs `cargo run`:
//!
//! - `--profile p,q` → `FIREFLY_PROFILES_ACTIVE=p,q`
//! - `-D key=value`  → `FIREFLY_<KEY>=value` (the config-key→env-var mapping)
//! - `--env KEY=VAL` → raw `KEY=VAL` passthrough
//! - `--debug`       → `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`
//!
//! `--release` selects the optimized profile and `--bin <name>` targets a
//! specific binary, both passed straight through to Cargo.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::error::CliError;

/// Map a framework config key (with or without the `firefly.` prefix) to its
/// environment-variable name.
///
/// Mirrors pyfly's `_to_env_key` / `firefly-config`'s env convention: strip a
/// leading `firefly.`, upper-case, replace `.` and `-` with `_`, then prepend
/// `FIREFLY_`. So `web.port` → `FIREFLY_WEB_PORT` and
/// `firefly.logging.level-root` → `FIREFLY_LOGGING_LEVEL_ROOT`.
#[must_use]
pub fn to_env_key(key: &str) -> String {
    let base = key.strip_prefix("firefly.").unwrap_or(key);
    let upper = base.to_uppercase().replace(['.', '-'], "_");
    format!("FIREFLY_{upper}")
}

/// Build the environment overrides to apply before launching the app.
///
/// The Rust analog of pyfly's `_build_launch_env`. `profiles` are flattened
/// (each may be comma-separated) into `FIREFLY_PROFILES_ACTIVE`; `defines`
/// (`key=value`) become `FIREFLY_<KEY>=value`; `env_vars` (`KEY=VALUE`) pass
/// through verbatim; `debug` sets `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`.
///
/// # Errors
/// Returns [`CliError::InvalidName`] when a `-D` or `--env` item is missing its
/// `=` separator (matching pyfly's `BadParameter`).
pub fn build_launch_env(
    profiles: &[String],
    defines: &[String],
    env_vars: &[String],
    debug: bool,
) -> Result<BTreeMap<String, String>, CliError> {
    let mut out = BTreeMap::new();

    let flat_profiles: Vec<String> = profiles
        .iter()
        .flat_map(|chunk| chunk.split(','))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect();
    if !flat_profiles.is_empty() {
        out.insert(
            "FIREFLY_PROFILES_ACTIVE".to_string(),
            flat_profiles.join(","),
        );
    }

    for item in defines {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| CliError::InvalidName(format!("-D expects key=value, got {item:?}")))?;
        out.insert(to_env_key(key.trim()), value.to_string());
    }

    for item in env_vars {
        let (key, value) = item.split_once('=').ok_or_else(|| {
            CliError::InvalidName(format!("--env expects KEY=VALUE, got {item:?}"))
        })?;
        out.insert(key.trim().to_string(), value.to_string());
    }

    if debug {
        out.insert(
            "FIREFLY_LOGGING_LEVEL_ROOT".to_string(),
            "DEBUG".to_string(),
        );
    }

    Ok(out)
}

/// Options for [`run`], parsed from the `firefly run` flags.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Active profile(s); repeatable or comma-separated.
    pub profiles: Vec<String>,
    /// `-D key=value` config overrides (mapped to `FIREFLY_<KEY>`).
    pub defines: Vec<String>,
    /// `--env KEY=VALUE` raw environment passthrough.
    pub env_vars: Vec<String>,
    /// `--debug`: set `FIREFLY_LOGGING_LEVEL_ROOT=DEBUG`.
    pub debug: bool,
    /// `--release`: build/run the optimized Cargo profile.
    pub release: bool,
    /// `--bin <name>`: run a specific binary target.
    pub bin: Option<String>,
    /// Print the resolved environment + Cargo command without executing
    /// (the `--dry-run` testing affordance; no pyfly equivalent flag, kept
    /// non-destructive for scripted checks).
    pub dry_run: bool,
}

/// Build the `cargo run` argument vector for the given options.
fn cargo_args(opts: &RunOptions) -> Vec<String> {
    let mut args = vec!["run".to_string()];
    if opts.release {
        args.push("--release".to_string());
    }
    if let Some(bin) = &opts.bin {
        args.push("--bin".to_string());
        args.push(bin.clone());
    }
    args
}

/// `firefly run` — map the launch flags to framework env vars and exec
/// `cargo run`.
///
/// Requires a Cargo project in `root` (a `Cargo.toml`). On `--dry-run` the
/// resolved environment and command are printed and nothing is executed.
///
/// # Errors
/// Returns [`CliError::ProjectNotFound`] when `root` has no `Cargo.toml`,
/// [`CliError::InvalidName`] for a malformed `-D`/`--env` item, and
/// [`CliError::Unsupported`] when Cargo is not on `PATH` or the run exits
/// non-zero.
pub fn run(root: &Path, opts: &RunOptions) -> Result<(), CliError> {
    if !root.join("Cargo.toml").is_file() {
        return Err(CliError::ProjectNotFound(format!(
            "no Cargo.toml in {} — run 'firefly run' inside a firefly-rust project.",
            root.display()
        )));
    }
    let env = build_launch_env(&opts.profiles, &opts.defines, &opts.env_vars, opts.debug)?;
    let args = cargo_args(opts);

    if opts.dry_run {
        println!("Would run: cargo {}", args.join(" "));
        if !env.is_empty() {
            println!("With environment:");
            for (key, value) in &env {
                println!("  {key}={value}");
            }
        }
        return Ok(());
    }

    let mut command = Command::new("cargo");
    command.current_dir(root).args(&args);
    for (key, value) in &env {
        command.env(key, value);
    }

    let status = command.status().map_err(|source| {
        CliError::Unsupported(format!(
            "could not launch 'cargo run' ({source}); is Cargo on PATH?"
        ))
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::Unsupported(format!(
            "'cargo run' exited with status {}",
            status.code().unwrap_or(-1)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_env_key_strips_prefix_uppercases_and_replaces() {
        assert_eq!(to_env_key("web.port"), "FIREFLY_WEB_PORT");
        assert_eq!(to_env_key("firefly.web.port"), "FIREFLY_WEB_PORT");
        assert_eq!(
            to_env_key("logging.level-root"),
            "FIREFLY_LOGGING_LEVEL_ROOT"
        );
    }

    #[test]
    fn build_launch_env_flattens_profiles() {
        let env = build_launch_env(
            &["dev".to_string(), "local,test".to_string()],
            &[],
            &[],
            false,
        )
        .unwrap();
        assert_eq!(
            env.get("FIREFLY_PROFILES_ACTIVE").map(String::as_str),
            Some("dev,local,test")
        );
    }

    #[test]
    fn build_launch_env_maps_defines_and_env_and_debug() {
        let env = build_launch_env(
            &[],
            &["web.port=9000".to_string()],
            &["RUST_LOG=info".to_string()],
            true,
        )
        .unwrap();
        assert_eq!(
            env.get("FIREFLY_WEB_PORT").map(String::as_str),
            Some("9000")
        );
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert_eq!(
            env.get("FIREFLY_LOGGING_LEVEL_ROOT").map(String::as_str),
            Some("DEBUG")
        );
    }

    #[test]
    fn build_launch_env_value_may_contain_equals() {
        let env = build_launch_env(&[], &[], &["DSN=k=v;x=y".to_string()], false).unwrap();
        assert_eq!(env.get("DSN").map(String::as_str), Some("k=v;x=y"));
    }

    #[test]
    fn build_launch_env_rejects_define_without_equals() {
        let err = build_launch_env(&[], &["nope".to_string()], &[], false).unwrap_err();
        assert!(matches!(err, CliError::InvalidName(_)));
    }

    #[test]
    fn build_launch_env_rejects_env_without_equals() {
        let err = build_launch_env(&[], &[], &["NOPE".to_string()], false).unwrap_err();
        assert!(matches!(err, CliError::InvalidName(_)));
    }

    #[test]
    fn cargo_args_includes_release_and_bin() {
        let opts = RunOptions {
            release: true,
            bin: Some("svc".to_string()),
            ..Default::default()
        };
        assert_eq!(cargo_args(&opts), ["run", "--release", "--bin", "svc"]);
    }

    #[test]
    fn cargo_args_plain_run_by_default() {
        assert_eq!(cargo_args(&RunOptions::default()), ["run"]);
    }

    #[test]
    fn run_requires_a_cargo_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = run(tmp.path(), &RunOptions::default()).unwrap_err();
        assert!(matches!(err, CliError::ProjectNotFound(_)));
    }

    #[test]
    fn run_dry_run_does_not_execute() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let opts = RunOptions {
            profiles: vec!["dev".to_string()],
            defines: vec!["web.port=9000".to_string()],
            dry_run: true,
            ..Default::default()
        };
        // Dry-run never spawns Cargo, so this is deterministic and fast.
        assert!(run(tmp.path(), &opts).is_ok());
    }
}
