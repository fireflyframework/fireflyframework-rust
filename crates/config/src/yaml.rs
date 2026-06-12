//! YAML file source: parses `application.yaml`-style documents into the
//! flat dot-keyed map shared by every source.
//!
//! The Go port embeds a tiny YAML-subset scanner to stay dependency-free;
//! here `serde_yaml` does the parsing and the document is flattened to the
//! same shape: nested mappings become dot-joined keys, sequences of
//! scalars are comma-joined, empty values render as `""`, and keys are
//! lower-cased. Sequences containing mappings or nested sequences are
//! rejected, matching the Go scanner's "scalars only" contract.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use serde_yaml::Value;

use crate::error::ConfigError;
use crate::source::Source;

/// Loads a YAML file (or returns nothing when absent and `optional` is
/// true). Supports the configuration shape Firefly uses: mappings,
/// scalars, nested mappings, sequences of scalars (rendered comma-joined).
#[derive(Debug, Clone)]
pub struct YamlSource {
    /// Path of the YAML file to read.
    pub path: PathBuf,
    /// When true, a missing file yields an empty map instead of an error.
    pub optional: bool,
}

/// Returns a [`YamlSource`] that reads `path`. The file is required by
/// default; use [`from_optional_yaml`] to tolerate absence.
pub fn from_yaml(path: impl Into<PathBuf>) -> YamlSource {
    YamlSource {
        path: path.into(),
        optional: false,
    }
}

/// Returns a [`YamlSource`] that tolerates absence.
pub fn from_optional_yaml(path: impl Into<PathBuf>) -> YamlSource {
    YamlSource {
        path: path.into(),
        optional: true,
    }
}

impl Source for YamlSource {
    fn name(&self) -> String {
        format!("yaml({})", self.path.display())
    }

    fn load(&self) -> Result<HashMap<String, String>, ConfigError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound && self.optional => {
                return Ok(HashMap::new());
            }
            Err(err) => {
                return Err(ConfigError::Io {
                    path: self.path.display().to_string(),
                    source: err,
                });
            }
        };
        parse_yaml(&text)
    }
}

/// Parses a YAML document into a flat dot-keyed map.
pub(crate) fn parse_yaml(text: &str) -> Result<HashMap<String, String>, ConfigError> {
    if text.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let value: Value = serde_yaml::from_str(text)
        .map_err(|err| ConfigError::Yaml(format!("malformed YAML: {err}")))?;
    let mut out = HashMap::new();
    flatten(&value, "", &mut out)?;
    Ok(out)
}

fn flatten(
    value: &Value,
    prefix: &str,
    out: &mut HashMap<String, String>,
) -> Result<(), ConfigError> {
    match value {
        // A comments-only / empty document parses to Null: no entries.
        Value::Null => Ok(()),
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                let key = scalar_to_string(key).ok_or_else(|| {
                    ConfigError::Yaml(format!(
                        "malformed YAML: non-scalar mapping key under {prefix:?}"
                    ))
                })?;
                let full = if prefix.is_empty() {
                    key.to_lowercase()
                } else {
                    format!("{}.{}", prefix, key.to_lowercase())
                };
                match child {
                    Value::Mapping(_) => flatten(child, &full, out)?,
                    Value::Sequence(items) => {
                        let mut parts = Vec::with_capacity(items.len());
                        for item in items {
                            parts.push(scalar_to_string(item).ok_or_else(|| {
                                ConfigError::Yaml(format!(
                                    "unsupported sequence item under key {full:?}"
                                ))
                            })?);
                        }
                        out.insert(full, parts.join(","));
                    }
                    other => {
                        let rendered = scalar_to_string(other).ok_or_else(|| {
                            ConfigError::Yaml(format!("unsupported value under key {full:?}"))
                        })?;
                        out.insert(full, rendered);
                    }
                }
            }
            Ok(())
        }
        _ => Err(ConfigError::Yaml(
            "malformed YAML: top-level document must be a mapping".to_string(),
        )),
    }
}

/// Renders a scalar node as its string form: `Null` → `""`, booleans and
/// numbers via `Display`, strings verbatim. Returns `None` for non-scalars.
fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some(String::new()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_mappings_scalars_and_sequences() {
        let flat = parse_yaml(
            "
web:
  port: 8080
  host: 0.0.0.0
cache:
  adapter: redis
  ttl: 60
on: true
tags:
  - one
  - two
",
        )
        .unwrap();
        assert_eq!(flat["web.port"], "8080");
        assert_eq!(flat["web.host"], "0.0.0.0");
        assert_eq!(flat["cache.adapter"], "redis");
        assert_eq!(flat["cache.ttl"], "60");
        assert_eq!(flat["on"], "true");
        assert_eq!(flat["tags"], "one,two");
    }

    #[test]
    fn lowercases_keys() {
        let flat = parse_yaml("Web:\n  PORT: 1\n").unwrap();
        assert_eq!(flat["web.port"], "1");
    }

    #[test]
    fn strips_comments_and_handles_quoted_scalars() {
        let flat = parse_yaml(
            "# leading comment
host: \"0.0.0.0\"   # trailing comment
label: 'quoted'
",
        )
        .unwrap();
        assert_eq!(flat["host"], "0.0.0.0");
        assert_eq!(flat["label"], "quoted");
    }

    #[test]
    fn null_values_render_empty() {
        let flat = parse_yaml("key:\nother: x\n").unwrap();
        assert_eq!(flat["key"], "");
        assert_eq!(flat["other"], "x");
    }

    #[test]
    fn empty_and_comment_only_documents_yield_empty_maps() {
        assert!(parse_yaml("").unwrap().is_empty());
        assert!(parse_yaml("   \n\t\n").unwrap().is_empty());
        assert!(parse_yaml("# just a comment\n").unwrap().is_empty());
    }

    #[test]
    fn non_mapping_root_is_rejected() {
        let err = parse_yaml("just a scalar").unwrap_err();
        assert!(err.to_string().contains("malformed YAML"), "got: {err}");
    }

    #[test]
    fn sequences_of_mappings_are_rejected() {
        let err = parse_yaml("items:\n  - name: a\n  - name: b\n").unwrap_err();
        assert!(
            err.to_string().contains("unsupported sequence item"),
            "got: {err}"
        );
    }

    #[test]
    fn booleans_and_floats_render_via_display() {
        let flat = parse_yaml("flag: false\nratio: 1.5\n").unwrap();
        assert_eq!(flat["flag"], "false");
        assert_eq!(flat["ratio"], "1.5");
    }

    #[test]
    fn required_missing_file_is_an_io_error() {
        let err = from_yaml("/nonexistent/firefly.yaml").load().unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "got: {err:?}");
    }

    #[test]
    fn optional_missing_file_is_empty() {
        let flat = from_optional_yaml("/nonexistent/firefly.yaml")
            .load()
            .unwrap();
        assert!(flat.is_empty());
    }

    #[test]
    fn source_name_includes_path() {
        assert_eq!(from_yaml("/etc/app.yaml").name(), "yaml(/etc/app.yaml)");
    }
}
