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

//! Error type shared by every configuration source and the binder.

use std::fmt;

/// Errors produced while loading or binding configuration.
///
/// Mirrors the error shapes of the Go port: source failures are wrapped
/// with the offending source's name, binding failures carry the dotted
/// key that could not be parsed, and YAML problems report the malformed
/// construct.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A [`Source`](crate::Source) failed while producing its map. The
    /// `name` is the source's self-reported name (`yaml(<path>)`,
    /// `env(<PREFIX>)`, `flags`, …).
    #[error("config source {name:?}: {source}")]
    Source {
        /// Self-reported name of the failing source.
        name: String,
        /// The underlying failure.
        #[source]
        source: Box<ConfigError>,
    },

    /// Reading a required YAML file failed (missing file, permissions, …).
    /// Optional YAML sources swallow `NotFound` instead of raising this.
    #[error("firefly/config: {path:?}: {source}")]
    Io {
        /// Path of the file that could not be read.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The YAML document is malformed (a non-sequence line without a
    /// `key:` separator, or an orphan sequence item).
    #[error("firefly/config: {0}")]
    Yaml(String),

    /// A leaf value could not be parsed into the target field's type.
    #[error("firefly/config: key {key:?}: {message}")]
    Bind {
        /// Dotted path of the offending key (`web.port`).
        key: String,
        /// Human-readable parse failure.
        message: String,
    },

    /// Catch-all raised by `serde` while driving the binder (unknown
    /// fields under `deny_unknown_fields`, custom `Deserialize` errors, …).
    #[error("firefly/config: {0}")]
    Message(String),

    /// A `${...}` placeholder could not be resolved (unknown reference
    /// without a default) or resolution recursed past the depth guard
    /// (circular references such as `a: ${b}` / `b: ${a}`).
    #[error("firefly/config: placeholder {placeholder:?}: {message}")]
    Placeholder {
        /// The placeholder (or value) that failed to resolve.
        placeholder: String,
        /// Human-readable resolution failure.
        message: String,
    },

    /// Fetching remote configuration from a Spring-Cloud-Config-style
    /// server failed (connection error, malformed JSON document, …).
    #[error("firefly/config: remote config {url}: {message}")]
    Remote {
        /// The document URL that was queried (`{base}/{app}/{profile}/{label}`).
        url: String,
        /// Human-readable transport or decode failure.
        message: String,
    },
}

impl ConfigError {
    /// Builds a [`ConfigError::Bind`] for the given dotted key.
    pub(crate) fn bind(key: &str, message: impl fmt::Display) -> Self {
        ConfigError::Bind {
            key: key.to_string(),
            message: message.to_string(),
        }
    }
}

impl serde::de::Error for ConfigError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        ConfigError::Message(msg.to_string())
    }
}
