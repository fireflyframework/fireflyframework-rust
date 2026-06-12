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

//! YAML file source: parses `application.yaml`-style documents into the
//! flat dot-keyed map shared by every source.
//!
//! The parser is a line-by-line port of the Go module's embedded
//! YAML-subset scanner (`config/yaml.go`), so the flattened output is
//! identical to the Go port for any given file: nested mappings become
//! dot-joined lower-cased keys (each parent mapping key also yields an
//! empty-string entry, exactly like Go), sequences of scalars are
//! comma-joined, empty values render as `""`, and — crucially — scalar
//! lexemes are preserved verbatim (`1.10` stays `"1.10"`, `0x1A` stays
//! `"0x1A"`). Duplicate keys follow last-write-wins. Aliases / anchors /
//! multi-doc / tags / flow sequences are not interpreted (deliberate —
//! bring your own parser if you need them).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

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
///
/// Direct port of the Go scanner (`config/yaml.go` `parseYAML`): a tiny
/// YAML subset parser sufficient for the configuration shape Firefly
/// uses. Values are stored exactly as written in the source (modulo one
/// pair of surrounding quotes), duplicate keys follow last-write-wins,
/// and out-of-range numeric literals are perfectly fine — everything is
/// a string until the binder parses it against the target field's type.
pub(crate) fn parse_yaml(text: &str) -> Result<HashMap<String, String>, ConfigError> {
    struct Frame {
        indent: usize,
        key: String,
    }

    fn flush_seq(out: &mut HashMap<String, String>, key: &mut String, values: &mut Vec<String>) {
        if !key.is_empty() {
            out.insert(std::mem::take(key), values.join(","));
            values.clear();
        }
    }

    let mut out = HashMap::new();
    let mut stack: Vec<Frame> = Vec::new();

    let mut seq_key = String::new();
    let mut seq_indent = 0usize;
    let mut seq_values: Vec<String> = Vec::new();

    let normalized = text.replace("\r\n", "\n");
    for raw in normalized.split('\n') {
        // Strip comments: `#` opens a comment at line start or after a
        // space / tab (byte-wise, matching the Go scanner).
        let mut line = raw;
        let bytes = raw.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'#' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
                line = &raw[..i];
                break;
            }
        }
        // Trim trailing whitespace only — leading whitespace is the indent.
        let line = line.trim_end_matches([' ', '\t']);
        if line.is_empty() {
            continue;
        }
        let indent = line.bytes().take_while(|&b| b == b' ').count();
        let body = &line[indent..];

        // Sequence item.
        if let Some(item) = body.strip_prefix("- ") {
            if seq_key.is_empty() || indent < seq_indent {
                return Err(ConfigError::Yaml(format!("orphan sequence item: {line:?}")));
            }
            seq_values.push(scalar(item.trim()));
            continue;
        }
        // Anything else flushes a pending sequence.
        flush_seq(&mut out, &mut seq_key, &mut seq_values);
        seq_indent = 0;

        // Pop frames whose indent is >= this one.
        while stack.last().is_some_and(|frame| frame.indent >= indent) {
            stack.pop();
        }

        let Some(colon) = body.find(':') else {
            return Err(ConfigError::Yaml(format!("malformed YAML: {line:?}")));
        };
        let key = body[..colon].trim();
        let value = body[colon + 1..].trim();
        let full = match stack.last() {
            Some(parent) => format!("{}.{}", parent.key, key),
            None => key.to_string(),
        }
        .to_lowercase();

        if value.is_empty() {
            // Mapping start. Speculatively start a sequence; if no "- "
            // items follow, the flush renders the key as "".
            seq_key.clone_from(&full);
            seq_indent = indent + 1;
            seq_values.clear();
            stack.push(Frame { indent, key: full });
            continue;
        }

        out.insert(full, scalar(value));
        seq_key.clear();
    }
    flush_seq(&mut out, &mut seq_key, &mut seq_values);
    Ok(out)
}

/// Mirrors the Go scanner's `scalar`: strips one matching pair of
/// surrounding quotes, otherwise returns the lexeme verbatim. Values are
/// never parsed and re-rendered, so `1.10`, `0x1A`, `1e3`, and `2.50`
/// survive exactly as written in the source file.
fn scalar(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
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
        // Parent mapping keys yield empty entries, exactly like Go.
        assert_eq!(flat["web"], "");
        assert_eq!(flat["cache"], "");
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
    fn orphan_sequence_items_are_rejected() {
        let err = parse_yaml("- one\n").unwrap_err();
        assert!(
            err.to_string().contains("orphan sequence item"),
            "got: {err}"
        );
    }

    #[test]
    fn sequence_items_are_taken_verbatim_like_go() {
        // The Go scanner does not interpret structure inside sequence
        // items: "- name: a" is the scalar item "name: a".
        let flat = parse_yaml("items:\n  - name: a\n  - name: b\n").unwrap();
        assert_eq!(flat["items"], "name: a,name: b");
    }

    #[test]
    fn booleans_and_floats_are_kept_verbatim() {
        let flat = parse_yaml("flag: false\nratio: 1.5\n").unwrap();
        assert_eq!(flat["flag"], "false");
        assert_eq!(flat["ratio"], "1.5");
    }

    // Regression (bug): scalars used to be parsed into typed YAML numbers
    // and re-rendered, corrupting numeric-looking strings ("1.10" -> "1.1",
    // "0x1A" -> "26", "1e3" -> "1000.0", "2.50" -> "2.5"). The Go scanner
    // returns the raw lexeme verbatim in every branch — so must we.
    #[test]
    fn numeric_looking_scalars_keep_their_source_lexeme() {
        let flat = parse_yaml("version: 1.10\nbuild: 0x1A\nnum: 1e3\nratio: 2.50\n").unwrap();
        assert_eq!(flat["version"], "1.10");
        assert_eq!(flat["build"], "0x1A");
        assert_eq!(flat["num"], "1e3");
        assert_eq!(flat["ratio"], "2.50");
    }

    // Regression (bug): documents the Go scanner accepts used to hard-fail
    // here — duplicate keys errored instead of last-write-wins, and
    // out-of-range integer literals errored instead of staying strings.
    #[test]
    fn duplicate_keys_follow_last_write_wins_like_go() {
        let flat = parse_yaml("web:\n  port: 1\nweb:\n  port: 2\n").unwrap();
        assert_eq!(flat["web.port"], "2");
    }

    #[test]
    fn out_of_range_integer_literals_are_kept_verbatim() {
        let flat = parse_yaml("big: 12345678901234567890123\n").unwrap();
        assert_eq!(flat["big"], "12345678901234567890123");
    }

    #[test]
    fn flow_sequences_are_not_interpreted() {
        // Matches Go: the value is the verbatim text, not a parsed list.
        let flat = parse_yaml("tags: [a, b]\n").unwrap();
        assert_eq!(flat["tags"], "[a, b]");
    }

    #[test]
    fn crlf_documents_parse_like_lf() {
        let flat = parse_yaml("web:\r\n  port: 8080\r\n").unwrap();
        assert_eq!(flat["web.port"], "8080");
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
