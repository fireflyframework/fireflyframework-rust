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

//! Simple text and HTML template rendering — the Rust port of Go's
//! `utils.RenderText` / `utils.RenderHTML`.
//!
//! Each Firefly port uses its runtime's idiomatic engine (Go uses
//! `text/template`/`html/template`, .NET uses Scriban, Java uses
//! StringTemplate); this port implements the small field-interpolation
//! subset the framework actually uses: `{{.Field}}`, nested paths
//! (`{{.User.Name}}`), and `{{.}}` for the whole data value. Any data
//! type implementing [`serde::Serialize`] can be rendered — it is
//! serialised to a JSON value and fields are looked up on the result.
//!
//! Divergence from Go, by design: referencing a missing field is a
//! hard [`TemplateError::Execute`] error rather than Go's silent
//! `<no value>`/zero-value output — typos fail fast.

use serde::Serialize;
use serde_json::Value;

/// Errors produced by [`render_text`] and [`render_html`].
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    /// The template source is malformed (unclosed `{{`, unsupported
    /// action, bad field path).
    #[error("template {name:?}: parse: {message}")]
    Parse {
        /// The template name passed by the caller.
        name: String,
        /// What went wrong.
        message: String,
    },
    /// The template referenced data that does not exist or cannot be
    /// traversed.
    #[error("template {name:?}: execute: {message}")]
    Execute {
        /// The template name passed by the caller.
        name: String,
        /// What went wrong.
        message: String,
    },
    /// The data value could not be serialised to JSON for lookup.
    #[error("template {name:?}: serialize: {source}")]
    Serialize {
        /// The template name passed by the caller.
        name: String,
        /// The underlying serde error.
        source: serde_json::Error,
    },
}

/// Renders a template against `data` and returns the resulting
/// string, with interpolated values inserted verbatim. The equivalent
/// of Go's `RenderText`. `name` is used only in error messages.
///
/// ```
/// let out = firefly_utils::render_text(
///     "greet",
///     "hello {{.Name}}",
///     &serde_json::json!({"Name": "world"}),
/// ).unwrap();
/// assert_eq!(out, "hello world");
/// ```
pub fn render_text<T>(name: &str, source: &str, data: &T) -> Result<String, TemplateError>
where
    T: Serialize + ?Sized,
{
    render(name, source, data, false)
}

/// Renders a template against `data` with every interpolated value
/// HTML-escaped (`&`, `<`, `>`, `"`, `'`), preserving the XSS-safety
/// guarantee of Go's `html/template`-based `RenderHTML`. Use this for
/// any template whose output is rendered as HTML. Template literal
/// text is emitted verbatim — escaping applies to interpolations.
pub fn render_html<T>(name: &str, source: &str, data: &T) -> Result<String, TemplateError>
where
    T: Serialize + ?Sized,
{
    render(name, source, data, true)
}

fn render<T>(name: &str, source: &str, data: &T, escape: bool) -> Result<String, TemplateError>
where
    T: Serialize + ?Sized,
{
    let data = serde_json::to_value(data).map_err(|e| TemplateError::Serialize {
        name: name.to_string(),
        source: e,
    })?;
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| TemplateError::Parse {
            name: name.to_string(),
            message: "unclosed action: missing \"}}\"".to_string(),
        })?;
        let action = after[..end].trim();
        let value = eval_field(name, action, &data)?;
        let text = format_value(value);
        if escape {
            push_html_escaped(&mut out, &text);
        } else {
            out.push_str(&text);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Resolves a `.Field.Sub` action path against the data value.
fn eval_field<'a>(name: &str, action: &str, data: &'a Value) -> Result<&'a Value, TemplateError> {
    let Some(path) = action.strip_prefix('.') else {
        return Err(TemplateError::Parse {
            name: name.to_string(),
            message: format!(
                "unsupported action {{{{{action}}}}}: only field access ({{{{.Field}}}}) is supported"
            ),
        });
    };
    if path.is_empty() {
        return Ok(data); // {{.}} — the whole data value
    }
    let mut current = data;
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(TemplateError::Parse {
                name: name.to_string(),
                message: format!("bad field path in action {{{{{action}}}}}"),
            });
        }
        let Value::Object(map) = current else {
            return Err(TemplateError::Execute {
                name: name.to_string(),
                message: format!("field {segment:?} accessed on non-object value"),
            });
        };
        current = map.get(segment).ok_or_else(|| TemplateError::Execute {
            name: name.to_string(),
            message: format!("no value for field {:?}", format!(".{path}")),
        })?;
    }
    Ok(current)
}

/// Stringifies an interpolated JSON value: strings verbatim, numbers
/// and booleans via Display, null as the empty string, arrays and
/// objects as compact JSON.
fn format_value(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Appends `text` to `out` with the same five-entity escaping Go's
/// `html/template` applies: `&amp;`, `&lt;`, `&gt;`, `&#34;`, `&#39;`.
fn push_html_escaped(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    /// Port of Go `TestRender` (text half): simple field interpolation
    /// against a string map.
    #[test]
    fn render_text_interpolates_fields() {
        let data: HashMap<&str, &str> = [("Name", "world")].into();
        let got = render_text("greet", "hello {{.Name}}", &data).unwrap();
        assert_eq!(got, "hello world");
    }

    /// Port of Go `TestRender` (HTML half): interpolated values are
    /// escaped, markup in the template itself is preserved.
    #[test]
    fn render_html_escapes_interpolations() {
        let data: HashMap<&str, &str> = [("Name", "<bob>")].into();
        let got = render_html("greet", "<p>hi {{.Name}}</p>", &data).unwrap();
        assert!(got.contains("&lt;bob&gt;"), "html escaping lost: {got:?}");
        assert_eq!(got, "<p>hi &lt;bob&gt;</p>");
    }

    /// All five entities match Go's html/template replacement table.
    #[test]
    fn render_html_escapes_go_entity_set() {
        let got = render_html("e", "{{.V}}", &json!({"V": "&<>\"'"})).unwrap();
        assert_eq!(got, "&amp;&lt;&gt;&#34;&#39;");
    }

    /// Nested paths traverse objects, like Go's `{{.User.Name}}`.
    #[test]
    fn render_text_traverses_nested_paths() {
        let data = json!({"User": {"Name": "ada", "Id": 7}});
        let got = render_text("t", "{{.User.Name}}#{{.User.Id}}", &data).unwrap();
        assert_eq!(got, "ada#7");
    }

    /// `{{.}}` interpolates the whole data value; numbers and booleans
    /// render via Display; whitespace inside actions is tolerated.
    #[test]
    fn render_text_dot_numbers_bools_whitespace() {
        assert_eq!(render_text("t", "[{{.}}]", "world").unwrap(), "[world]");
        let got = render_text("t", "{{ .N }} {{ .B }}", &json!({"N": 42, "B": true})).unwrap();
        assert_eq!(got, "42 true");
    }

    /// Any `Serialize` type works as data, including derived structs.
    #[test]
    fn render_text_accepts_derived_structs() {
        #[derive(serde::Serialize)]
        struct Greeting {
            #[serde(rename = "Name")]
            name: String,
        }
        let data = Greeting {
            name: "firefly".into(),
        };
        let got = render_text("t", "hello {{.Name}}", &data).unwrap();
        assert_eq!(got, "hello firefly");
    }

    /// Literal text — including single braces — passes through
    /// untouched; only `{{ ... }}` actions are interpreted.
    #[test]
    fn render_text_leaves_literal_text_alone() {
        let got = render_text("t", "fn main() { x } done", &json!({})).unwrap();
        assert_eq!(got, "fn main() { x } done");
    }

    /// Missing fields fail fast with an Execute error.
    #[test]
    fn render_text_errors_on_missing_field() {
        let err = render_text("t", "{{.Nope}}", &json!({"Name": "x"})).expect_err("missing");
        assert!(matches!(err, TemplateError::Execute { .. }), "{err:?}");
        assert!(err.to_string().contains(".Nope"), "{err}");
    }

    /// Unclosed actions are parse errors.
    #[test]
    fn render_text_errors_on_unclosed_action() {
        let err = render_text("t", "hello {{.Name", &json!({})).expect_err("unclosed");
        assert!(matches!(err, TemplateError::Parse { .. }), "{err:?}");
    }

    /// Anything other than field access is an unsupported action.
    #[test]
    fn render_text_errors_on_unsupported_action() {
        let err = render_text("t", "{{if .X}}y{{end}}", &json!({})).expect_err("unsupported");
        assert!(matches!(err, TemplateError::Parse { .. }), "{err:?}");
    }

    /// Null values render as the empty string.
    #[test]
    fn render_text_renders_null_as_empty() {
        let got = render_text("t", "[{{.V}}]", &json!({"V": null})).unwrap();
        assert_eq!(got, "[]");
    }

    /// Rust-specific: errors are Send + Sync.
    #[test]
    fn template_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TemplateError>();
    }
}
