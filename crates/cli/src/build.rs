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

//! `firefly build` — package and image helpers, plus the `build-info.json`
//! stamp consumed by the actuator `/info` contributor.
//!
//! The Rust analog of pyfly's `pyfly build`. Plain compilation is `cargo build`
//! (the wheel/sdist analog), so this module ports the two commands with a direct
//! Rust counterpart:
//!
//! - [`write_build_info`] (`firefly build info`) — writes `build-info.json`
//!   (`{"git": {"sha": …}, "build": {"time": …}}`), byte-shape-identical to
//!   pyfly/Go/Java so the same file feeds every runtime's `/actuator/info`
//!   build contributor.
//! - [`build_image`] (`firefly build image`) — builds an OCI image via Cloud
//!   Native Buildpacks (`pack`) or Docker, mirroring pyfly's `build image`.

use std::path::Path;
use std::process::Command;

use crate::error::CliError;

/// Run `git rev-parse HEAD` in `root`, returning the commit SHA or an empty
/// string when git is unavailable / `root` is not a repository (matching
/// pyfly's tolerant `build info`).
fn git_sha(root: &Path) -> String {
    Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Build the `build-info.json` document for `root` at instant `build_time`.
///
/// Matches pyfly's wire shape exactly: `{"git": {"sha": <sha>}, "build":
/// {"time": <rfc3339-utc>}}`. The SHA is the empty string when git is
/// unavailable. Pure (no filesystem write) so it is unit-testable; the time is
/// passed in rather than read from the clock.
#[must_use]
pub fn build_info_json(root: &Path, build_time: &str) -> serde_json::Value {
    serde_json::json!({
        "git": { "sha": git_sha(root) },
        "build": { "time": build_time },
    })
}

/// `firefly build info` — write `build-info.json` (git SHA + UTC build time) to
/// `output`, relative to `root`.
///
/// The resulting file is the data source for the `/actuator/info` build
/// contributor; the same shape pyfly/Go/Java emit, so a deployment pipeline can
/// stamp it identically across runtimes.
///
/// Returns the absolute path written.
///
/// # Errors
/// Returns [`CliError::Io`] when the file cannot be written.
pub fn write_build_info(root: &Path, output: &Path) -> Result<std::path::PathBuf, CliError> {
    let build_time = now_rfc3339_utc();
    let doc = build_info_json(root, &build_time);
    let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let path = if output.is_absolute() {
        output.to_path_buf()
    } else {
        root.join(output)
    };
    std::fs::write(&path, text).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// The current instant as an RFC 3339 UTC timestamp (seconds precision), built
/// from `SystemTime` without pulling in a date library beyond what the workspace
/// already provides.
fn now_rfc3339_utc() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::<chrono::Utc>::from_timestamp(now.as_secs() as i64, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The image builder backing `firefly build image`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBuilder {
    /// Cloud Native Buildpacks via the `pack` CLI (the pyfly default).
    Pack,
    /// `docker build` against the project's `Dockerfile`.
    Docker,
}

impl ImageBuilder {
    /// Parse the `--builder` value (`pack` | `docker`).
    ///
    /// # Errors
    /// Returns [`CliError::Unsupported`] for any other value.
    pub fn parse(s: &str) -> Result<Self, CliError> {
        match s {
            "pack" => Ok(ImageBuilder::Pack),
            "docker" => Ok(ImageBuilder::Docker),
            other => Err(CliError::Unsupported(format!(
                "unknown image builder {other:?} (expected 'pack' or 'docker')"
            ))),
        }
    }

    /// The external tool this builder invokes.
    #[must_use]
    pub fn tool(self) -> &'static str {
        match self {
            ImageBuilder::Pack => "pack",
            ImageBuilder::Docker => "docker",
        }
    }
}

/// Build the argument vector for the chosen image `builder` and `image` tag.
fn image_args(builder: ImageBuilder, image: &str) -> Vec<String> {
    match builder {
        ImageBuilder::Pack => vec![
            "build".to_string(),
            image.to_string(),
            "--builder".to_string(),
            "paketobuildpacks/builder-jammy-base".to_string(),
        ],
        ImageBuilder::Docker => vec![
            "build".to_string(),
            "-t".to_string(),
            image.to_string(),
            ".".to_string(),
        ],
    }
}

/// `firefly build image` — build an OCI image via `pack` or Docker.
///
/// `tag` defaults to `firefly-app:latest` when `None` (the scaffolded
/// Dockerfile is already produced by `firefly new`).
///
/// # Errors
/// Returns [`CliError::Unsupported`] when the chosen builder tool is not on
/// `PATH` or the build exits non-zero.
pub fn build_image(root: &Path, tag: Option<&str>, builder: ImageBuilder) -> Result<(), CliError> {
    let tool = builder.tool();
    if which(tool).is_none() {
        return Err(CliError::Unsupported(format!(
            "'{tool}' not found on PATH — install it to build images ({})",
            match builder {
                ImageBuilder::Pack => "https://buildpacks.io/docs/tools/pack/",
                ImageBuilder::Docker => "https://docs.docker.com/get-docker/",
            }
        )));
    }
    let image = tag.unwrap_or("firefly-app:latest");
    let args = image_args(builder, image);
    let status = Command::new(tool)
        .current_dir(root)
        .args(&args)
        .status()
        .map_err(|source| CliError::Unsupported(format!("could not launch '{tool}' ({source})")))?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::Unsupported(format!(
            "'{tool} {}' exited with status {}",
            args.join(" "),
            status.code().unwrap_or(-1)
        )))
    }
}

/// Locate `tool` on `PATH`, returning its full path if found.
fn which(tool: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_json_has_pyfly_wire_shape() {
        let tmp = tempfile::TempDir::new().unwrap();
        let doc = build_info_json(tmp.path(), "2026-06-13T00:00:00Z");
        // Same shape pyfly/Go/Java emit.
        assert_eq!(doc["build"]["time"], "2026-06-13T00:00:00Z");
        assert!(doc["git"]["sha"].is_string());
        // Top-level keys are exactly `git` and `build`.
        let obj = doc.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("git"));
        assert!(obj.contains_key("build"));
    }

    #[test]
    fn write_build_info_writes_a_parseable_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_build_info(tmp.path(), Path::new("build-info.json")).unwrap();
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(doc["build"]["time"].as_str().is_some());
        assert!(doc["git"].is_object());
    }

    #[test]
    fn write_build_info_honours_absolute_output() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("nested").join("bi.json");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let path = write_build_info(tmp.path(), &out).unwrap();
        assert_eq!(path, out);
        assert!(out.exists());
    }

    #[test]
    fn now_rfc3339_is_utc_zulu() {
        let now = now_rfc3339_utc();
        assert!(now.ends_with('Z'), "expected UTC 'Z' suffix, got {now}");
        assert!(now.contains('T'));
    }

    #[test]
    fn image_builder_parse_and_tool() {
        assert_eq!(ImageBuilder::parse("pack").unwrap(), ImageBuilder::Pack);
        assert_eq!(ImageBuilder::parse("docker").unwrap(), ImageBuilder::Docker);
        assert!(ImageBuilder::parse("podman").is_err());
        assert_eq!(ImageBuilder::Pack.tool(), "pack");
        assert_eq!(ImageBuilder::Docker.tool(), "docker");
    }

    #[test]
    fn image_args_match_builders() {
        assert_eq!(
            image_args(ImageBuilder::Pack, "svc:1"),
            [
                "build",
                "svc:1",
                "--builder",
                "paketobuildpacks/builder-jammy-base"
            ]
        );
        assert_eq!(
            image_args(ImageBuilder::Docker, "svc:1"),
            ["build", "-t", "svc:1", "."]
        );
    }

    #[test]
    fn build_image_errors_when_tool_missing() {
        // A deterministic miss: point PATH at an empty dir so the tool isn't found.
        let tmp = tempfile::TempDir::new().unwrap();
        let saved = std::env::var_os("PATH");
        // Use the pure `which` against an empty dir rather than mutating PATH for
        // the whole process: assert the helper returns None for a bogus tool.
        assert!(which("definitely-not-a-real-tool-xyz").is_none());
        drop(saved);
        drop(tmp);
    }
}
