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

//! Spring Boot's `management.endpoints.web.exposure` model — which
//! endpoint ids [`mount`](crate::mount) actually puts on the wire, under
//! which base path, and per-endpoint enabled overrides.
//!
//! pyfly mirrors Spring Boot's secure-by-default exposure (`health,info`
//! only). The Rust port shipped its Go-parity surface with everything
//! mounted, so [`ExposureConfig::default`] keeps `include = ["*"]` for
//! backward compatibility; use [`ExposureConfig::spring_default`] for
//! Spring's `health,info` default.

use std::collections::HashMap;

/// Spring Boot's default actuator base path.
pub const DEFAULT_BASE_PATH: &str = "/actuator";

/// Web-exposure rules consulted by [`mount`](crate::mount):
/// include/exclude id sets (`"*"` wildcard supported, exclude always
/// wins), the base path, and per-endpoint enabled overrides — the
/// counterpart of pyfly's `exposure.py` + the registry's
/// `management.endpoint.{id}.enabled` config keys.
#[derive(Debug, Clone)]
pub struct ExposureConfig {
    /// Endpoint ids to expose over HTTP; `"*"` exposes everything not
    /// excluded. Defaults to `["*"]` (Go-parity backward compatibility).
    pub include: Vec<String>,
    /// Endpoint ids never exposed; wins over `include`.
    pub exclude: Vec<String>,
    /// Base path all endpoints are mounted under. Defaults to
    /// [`DEFAULT_BASE_PATH`]; `"/"` mounts at the root.
    pub base_path: String,
    /// Per-endpoint enabled overrides — Spring's
    /// `management.endpoint.{id}.enabled`. Wins over the endpoint's own
    /// default enabled state.
    pub endpoint_enabled: HashMap<String, bool>,
}

impl Default for ExposureConfig {
    fn default() -> Self {
        Self {
            include: vec!["*".to_string()],
            exclude: Vec::new(),
            base_path: DEFAULT_BASE_PATH.to_string(),
            endpoint_enabled: HashMap::new(),
        }
    }
}

impl ExposureConfig {
    /// Spring Boot's secure default: only `health` and `info` are
    /// web-exposed.
    pub fn spring_default() -> Self {
        Self {
            include: vec!["health".to_string(), "info".to_string()],
            ..Self::default()
        }
    }

    /// Builds an exposure config from comma-separated include / exclude
    /// lists — the wire format of
    /// `management.endpoints.web.exposure.include`.
    pub fn from_csv(include: &str, exclude: &str) -> Self {
        Self {
            include: split_csv(include),
            exclude: split_csv(exclude),
            ..Self::default()
        }
    }

    /// Whether `endpoint_id` should be exposed over HTTP: `exclude`
    /// always wins; `"*"` in `include` exposes everything else.
    pub fn is_exposed(&self, endpoint_id: &str) -> bool {
        if self.exclude.iter().any(|e| e == endpoint_id) {
            return false;
        }
        self.include.iter().any(|i| i == "*" || i == endpoint_id)
    }

    /// Whether `endpoint_id` is enabled: an explicit override in
    /// [`ExposureConfig::endpoint_enabled`] wins, otherwise
    /// `default_enabled` (the endpoint's own declared state).
    pub fn is_enabled(&self, endpoint_id: &str, default_enabled: bool) -> bool {
        self.endpoint_enabled
            .get(endpoint_id)
            .copied()
            .unwrap_or(default_enabled)
    }

    /// The base path normalized for routing: leading `/`, no trailing
    /// `/`, empty string when mounting at the root (pyfly's
    /// `base_path()` followed by Starlette's `rstrip("/")`).
    pub fn normalized_base_path(&self) -> String {
        let raw = self.base_path.trim();
        if raw.is_empty() {
            return DEFAULT_BASE_PATH.to_string();
        }
        let trimmed = raw.trim_matches('/');
        if trimmed.is_empty() {
            String::new() // mount at root
        } else {
            format!("/{trimmed}")
        }
    }
}

/// Parses a comma-separated id list, trimming blanks (pyfly's `_split`).
fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // pyfly: test_default_exposes_only_health_and_info
    #[test]
    fn spring_default_exposes_only_health_and_info() {
        let exposure = ExposureConfig::spring_default();
        assert!(exposure.is_exposed("health"));
        assert!(exposure.is_exposed("info"));
        assert!(!exposure.is_exposed("metrics"));
        assert!(!exposure.is_exposed("env"));
    }

    // Rust back-compat: Default keeps the Go-parity "everything" surface.
    #[test]
    fn default_exposes_everything() {
        let exposure = ExposureConfig::default();
        assert!(exposure.is_exposed("health"));
        assert!(exposure.is_exposed("metrics"));
        assert!(exposure.is_exposed("anything"));
    }

    // pyfly: test_wildcard_exposes_everything_except_excluded
    #[test]
    fn wildcard_exposes_everything_except_excluded() {
        let exposure = ExposureConfig::from_csv("*", "env");
        assert!(exposure.is_exposed("metrics"));
        assert!(exposure.is_exposed("loggers"));
        assert!(!exposure.is_exposed("env"), "excluded wins");
    }

    // pyfly: test_csv_include
    #[test]
    fn csv_include() {
        let exposure = ExposureConfig::from_csv("health, metrics ,prometheus", "");
        assert!(exposure.is_exposed("metrics"));
        assert!(exposure.is_exposed("prometheus"));
        assert!(!exposure.is_exposed("loggers"));
    }

    // pyfly: test_base_path_default_and_override
    #[test]
    fn base_path_default_and_override() {
        assert_eq!(
            ExposureConfig::default().normalized_base_path(),
            "/actuator"
        );
        let exposure = ExposureConfig {
            base_path: "/manage".into(),
            ..ExposureConfig::default()
        };
        assert_eq!(exposure.normalized_base_path(), "/manage");
        let root = ExposureConfig {
            base_path: "/".into(),
            ..ExposureConfig::default()
        };
        assert_eq!(root.normalized_base_path(), "");
        let messy = ExposureConfig {
            base_path: " manage/ ".into(),
            ..ExposureConfig::default()
        };
        assert_eq!(messy.normalized_base_path(), "/manage");
        let empty = ExposureConfig {
            base_path: String::new(),
            ..ExposureConfig::default()
        };
        assert_eq!(empty.normalized_base_path(), "/actuator");
    }

    #[test]
    fn endpoint_enabled_override_wins() {
        let mut exposure = ExposureConfig::default();
        exposure.endpoint_enabled.insert("git".into(), false);
        exposure.endpoint_enabled.insert("test-off".into(), true);
        assert!(!exposure.is_enabled("git", true));
        assert!(exposure.is_enabled("test-off", false));
        assert!(exposure.is_enabled("other", true));
        assert!(!exposure.is_enabled("other2", false));
    }
}
