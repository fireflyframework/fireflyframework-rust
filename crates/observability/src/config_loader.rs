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

//! External logging-config-file loading — pyfly's
//! `logging.config_loader.apply_external_config` analog.
//!
//! pyfly loads an external logging definition at startup from a dictConfig
//! YAML/JSON file or a fileConfig INI (`pyfly.logging.config`), letting an
//! operator reconfigure logging — root level, format, per-logger levels —
//! without editing code. dictConfig/fileConfig are Python-stdlib `logging`
//! constructs with no Rust equivalent, so this module reproduces the *intent*
//! (file-driven reconfiguration at startup) over a small, language-neutral
//! schema that maps onto the [`LogConfig`] builder.
//!
//! Two file shapes are accepted, chosen by extension:
//!
//! - **`.json`** — a JSON object:
//!   ```json
//!   {
//!     "level": "DEBUG",
//!     "format": "console",
//!     "service": "orders",
//!     "levels": { "firefly_web": "WARN", "app::orders": "TRACE" }
//!   }
//!   ```
//! - **`.properties` / `.conf` / `.ini`** — flat `key=value` lines
//!   (`#`/`;` comments, blank lines ignored), the fileConfig analog:
//!   ```text
//!   level = DEBUG
//!   format = console
//!   service = orders
//!   # per-target overrides under the `level.` prefix (pyfly's level map)
//!   level.firefly_web = WARN
//!   level.app::orders = TRACE
//!   ```
//!
//! All keys are optional and merged over a starting [`LogConfig`] (typically
//! [`LogConfig::default`] or one already built from environment config). An
//! unparseable level/format value is ignored (it leaves the corresponding
//! field untouched) so a slightly malformed file never silently drops the
//! whole configuration. As in pyfly, a missing or empty path, or a hard load
//! failure, returns the base config unchanged via the `Result`/`Option` API
//! rather than crashing startup.

use std::collections::BTreeMap;
use std::path::Path;

use tracing::Level;

use crate::logging::{LogConfig, LogFormat, ROOT_TARGET};

/// An error loading or parsing an external logging config file. The caller
/// decides whether to surface it or fall back to the base config (pyfly logs
/// a warning and falls back); [`apply_external_config`] does the latter for
/// you.
#[derive(Debug)]
pub enum ConfigLoadError {
    /// The path was empty or the file does not exist.
    NotFound(String),
    /// The file could not be read.
    Io(std::io::Error),
    /// The file's contents could not be parsed for its extension.
    Parse(String),
    /// The extension is not one of the supported shapes.
    UnsupportedFormat(String),
}

impl std::fmt::Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigLoadError::NotFound(p) => write!(f, "logging config file not found: {p}"),
            ConfigLoadError::Io(e) => write!(f, "failed to read logging config: {e}"),
            ConfigLoadError::Parse(m) => write!(f, "failed to parse logging config: {m}"),
            ConfigLoadError::UnsupportedFormat(ext) => {
                write!(f, "unsupported logging config format: {ext}")
            }
        }
    }
}

impl std::error::Error for ConfigLoadError {}

/// Parses a level name (case-insensitive, `TRACE`/`DEBUG`/`INFO`/`WARN`/
/// `WARNING`/`ERROR`) into a [`Level`]; `None` for an unknown name.
fn parse_level(name: &str) -> Option<Level> {
    match name.trim().to_ascii_uppercase().as_str() {
        "TRACE" => Some(Level::TRACE),
        "DEBUG" => Some(Level::DEBUG),
        "INFO" => Some(Level::INFO),
        // pyfly/stdlib spell the warn level "WARNING"; accept both.
        "WARN" | "WARNING" => Some(Level::WARN),
        "ERROR" => Some(Level::ERROR),
        _ => None,
    }
}

/// The flattened key/value form an external config file boils down to before
/// it is folded onto a [`LogConfig`]. Keys: `level`, `format`, `service`, and
/// `level.<target>` per-target overrides.
#[derive(Debug, Default, PartialEq, Eq)]
struct RawLoggingConfig {
    level: Option<String>,
    format: Option<String>,
    service: Option<String>,
    levels: BTreeMap<String, String>,
}

impl RawLoggingConfig {
    /// Folds these values over `base`, returning the merged config. Unknown
    /// level/format strings leave the corresponding field untouched (pyfly:
    /// a bad value doesn't drop the rest of the config).
    fn apply_to(self, mut base: LogConfig) -> LogConfig {
        if let Some(level) = self.level.as_deref().and_then(parse_level) {
            base.level = level;
        }
        if let Some(format) = &self.format {
            base.format = LogFormat::from_name(&format.trim().to_ascii_lowercase());
        }
        if let Some(service) = self.service {
            base.service = service;
        }
        for (target, level_name) in self.levels {
            if let Some(level) = parse_level(&level_name) {
                if target.is_empty() || target == ROOT_TARGET {
                    base.level = level;
                } else {
                    base.levels.insert(target, level);
                }
            }
        }
        base
    }
}

/// Parses the flat `key=value` (properties / INI) form.
fn parse_properties(text: &str) -> RawLoggingConfig {
    let mut raw = RawLoggingConfig::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || line.starts_with('[')
        {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().to_string();
        match key {
            "level" | "root.level" => raw.level = Some(value),
            "format" => raw.format = Some(value),
            "service" => raw.service = Some(value),
            _ => {
                if let Some(target) = key.strip_prefix("level.") {
                    raw.levels.insert(target.to_string(), value);
                }
            }
        }
    }
    raw
}

/// Parses the JSON (dictConfig analog) form.
fn parse_json(text: &str) -> Result<RawLoggingConfig, ConfigLoadError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ConfigLoadError::Parse(e.to_string()))?;
    let mut raw = RawLoggingConfig::default();
    let as_string = |v: &serde_json::Value| v.as_str().map(str::to_string);
    raw.level = value.get("level").and_then(as_string);
    raw.format = value.get("format").and_then(as_string);
    raw.service = value.get("service").and_then(as_string);
    if let Some(levels) = value.get("levels").and_then(|v| v.as_object()) {
        for (target, level) in levels {
            if let Some(level) = level.as_str() {
                raw.levels.insert(target.clone(), level.to_string());
            }
        }
    }
    Ok(raw)
}

/// Loads an external logging config file and folds it over `base`, returning
/// the merged [`LogConfig`]. The format is chosen by extension: `.json` for
/// the JSON (dictConfig analog) shape, `.properties` / `.conf` / `.ini` for
/// the flat `key=value` (fileConfig analog) shape.
///
/// This is the strict variant — it returns the underlying
/// [`ConfigLoadError`] so a caller that wants startup to fail on a bad config
/// path can. For pyfly's lenient "log-and-fall-back" behaviour use
/// [`apply_external_config`].
///
/// ```
/// use std::io::Write;
/// use firefly_observability::{load_log_config, LogConfig, LogFormat};
/// use tracing::Level;
///
/// let mut file = tempfile::Builder::new().suffix(".properties").tempfile().unwrap();
/// writeln!(file, "level = debug").unwrap();
/// writeln!(file, "format = console").unwrap();
/// writeln!(file, "level.firefly_web = warn").unwrap();
/// let cfg = load_log_config(file.path(), LogConfig::default()).unwrap();
/// assert_eq!(cfg.level, Level::DEBUG);
/// assert_eq!(cfg.format, LogFormat::Console);
/// assert_eq!(cfg.levels.get("firefly_web"), Some(&Level::WARN));
/// ```
pub fn load_log_config(
    path: impl AsRef<Path>,
    base: LogConfig,
) -> Result<LogConfig, ConfigLoadError> {
    let path = path.as_ref();
    if path.as_os_str().is_empty() {
        return Err(ConfigLoadError::NotFound(String::new()));
    }
    if !path.is_file() {
        return Err(ConfigLoadError::NotFound(path.display().to_string()));
    }
    let text = std::fs::read_to_string(path).map_err(ConfigLoadError::Io)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let raw = match ext.as_str() {
        "json" => parse_json(&text)?,
        "properties" | "conf" | "ini" | "cfg" => parse_properties(&text),
        other => return Err(ConfigLoadError::UnsupportedFormat(other.to_string())),
    };
    Ok(raw.apply_to(base))
}

/// pyfly's `apply_external_config` analog: loads the file at `path` and folds
/// it over `base`, returning `(merged_config, applied)`. On any failure
/// (empty/missing path, read error, parse error, unsupported extension) it
/// returns `(base, false)` — the config is left unchanged and startup is
/// never crashed, exactly like pyfly logging a warning and falling back to
/// its inline configuration.
///
/// ```
/// use firefly_observability::{apply_external_config, LogConfig};
///
/// // A missing path falls back to the base config (applied == false).
/// let (cfg, applied) = apply_external_config("/nonexistent/logging.json", LogConfig::default());
/// assert!(!applied);
/// assert_eq!(cfg, LogConfig::default());
///
/// // An empty path is a no-op, like pyfly's early `if not path: return False`.
/// let (_, applied) = apply_external_config("", LogConfig::default());
/// assert!(!applied);
/// ```
pub fn apply_external_config(path: impl AsRef<Path>, base: LogConfig) -> (LogConfig, bool) {
    match load_log_config(path.as_ref(), base.clone()) {
        Ok(cfg) => (cfg, true),
        Err(err) => {
            // Mirror pyfly: warn and fall back, never crash.
            tracing::warn!(
                target: "firefly_observability::config_loader",
                error = %err,
                "failed to apply external logging config; using base config",
            );
            (base, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn temp(suffix: &str, contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    // pyfly: test_apply_dictconfig (JSON dictConfig analog).
    #[test]
    fn apply_json_sets_level_format_and_targets() {
        let f = temp(
            ".json",
            r#"{"level":"DEBUG","format":"console","service":"orders",
                "levels":{"firefly_web":"WARN","app::orders":"TRACE"}}"#,
        );
        let cfg = load_log_config(f.path(), LogConfig::default()).unwrap();
        assert_eq!(cfg.level, Level::DEBUG);
        assert_eq!(cfg.format, LogFormat::Console);
        assert_eq!(cfg.service, "orders");
        assert_eq!(cfg.levels.get("firefly_web"), Some(&Level::WARN));
        assert_eq!(cfg.levels.get("app::orders"), Some(&Level::TRACE));
    }

    // pyfly: test_apply_fileconfig_ini (flat key=value fileConfig analog).
    #[test]
    fn apply_properties_sets_level_and_targets() {
        let f = temp(
            ".properties",
            "# comment\n; also a comment\nlevel = debug\nformat = logfmt\nlevel.firefly_web = warning\n\n",
        );
        let cfg = load_log_config(f.path(), LogConfig::default()).unwrap();
        assert_eq!(cfg.level, Level::DEBUG);
        assert_eq!(cfg.format, LogFormat::Text);
        assert_eq!(cfg.levels.get("firefly_web"), Some(&Level::WARN));
    }

    // pyfly: test_apply_missing_returns_false.
    #[test]
    fn missing_file_falls_back_and_reports_false() {
        let base = LogConfig::default().with_service("base");
        let (cfg, applied) = apply_external_config("/nonexistent/logging.json", base.clone());
        assert!(!applied);
        assert_eq!(cfg, base);
    }

    // pyfly: test_apply_empty_path_returns_false.
    #[test]
    fn empty_path_falls_back_and_reports_false() {
        let (_, applied) = apply_external_config("", LogConfig::default());
        assert!(!applied);
    }

    #[test]
    fn unsupported_extension_falls_back() {
        let f = temp(".xml", "<logging/>");
        let (_, applied) = apply_external_config(f.path(), LogConfig::default());
        assert!(!applied);
    }

    #[test]
    fn malformed_json_falls_back_without_crashing() {
        let f = temp(".json", "{not valid json");
        let (cfg, applied) = apply_external_config(f.path(), LogConfig::default());
        assert!(!applied);
        assert_eq!(cfg, LogConfig::default());
    }

    #[test]
    fn unknown_level_value_leaves_field_untouched() {
        // A bad level must not drop the format that parsed fine.
        let f = temp(".properties", "level = LOUD\nformat = console\n");
        let cfg = load_log_config(f.path(), LogConfig::default()).unwrap();
        assert_eq!(cfg.level, Level::INFO); // unchanged
        assert_eq!(cfg.format, LogFormat::Console); // applied
    }

    #[test]
    fn root_target_routes_to_level_field() {
        let raw = RawLoggingConfig {
            levels: BTreeMap::from([("root".to_string(), "ERROR".to_string())]),
            ..RawLoggingConfig::default()
        };
        let cfg = raw.apply_to(LogConfig::default());
        assert_eq!(cfg.level, Level::ERROR);
        assert!(!cfg.levels.contains_key("root"));
    }

    #[test]
    fn json_applied_over_existing_base_merges() {
        let base = LogConfig::default()
            .with_service("keep")
            .with_target_level("existing", Level::WARN);
        let f = temp(".json", r#"{"level":"DEBUG"}"#);
        let cfg = load_log_config(f.path(), base).unwrap();
        // level overridden, service + existing target preserved.
        assert_eq!(cfg.level, Level::DEBUG);
        assert_eq!(cfg.service, "keep");
        assert_eq!(cfg.levels.get("existing"), Some(&Level::WARN));
    }
}
