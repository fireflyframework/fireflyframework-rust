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

//! Error type shared across the CLI library surface.

use std::path::PathBuf;

/// Errors produced by the `firefly-cli` library functions.
///
/// Command handlers convert these into a friendly diagnostic plus a non-zero
/// process exit; the variants are public so embedders (and tests) can match on
/// them.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// No firefly-rust project (Cargo.toml) was found, or its shape could not
    /// be determined. Port of pyfly's `ProjectNotFoundError`.
    #[error("{0}")]
    ProjectNotFound(String),

    /// The user-supplied name contained no usable identifier characters.
    #[error("cannot derive a name from {0:?}")]
    InvalidName(String),

    /// The requested archetype is not one of the known archetypes.
    #[error("unknown archetype: {0}")]
    UnknownArchetype(String),

    /// One or more requested features are not in the feature catalog.
    #[error("unknown features: {0}")]
    UnknownFeatures(String),

    /// The target directory for `firefly new` already exists.
    #[error("directory '{0}' already exists")]
    DirectoryExists(PathBuf),

    /// A template failed to render.
    #[error("template error: {0}")]
    Template(String),

    /// Filesystem I/O error during scaffolding or generation.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path the operation targeted.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A remote actuator request failed.
    #[error("request to {url} failed: {message}")]
    Request {
        /// The URL that was requested.
        url: String,
        /// A human-readable failure reason.
        message: String,
    },

    /// A database / migration operation failed (the `firefly db` group).
    #[error("{0}")]
    Database(String),

    /// A requested operation is not supported by the Rust port (and the
    /// divergence is documented). Mapped to a non-zero exit with an
    /// explanatory message, mirroring pyfly's `SystemExit(1)` for an
    /// unavailable capability.
    #[error("{0}")]
    Unsupported(String),
}

impl From<minijinja::Error> for CliError {
    fn from(e: minijinja::Error) -> Self {
        CliError::Template(e.to_string())
    }
}
