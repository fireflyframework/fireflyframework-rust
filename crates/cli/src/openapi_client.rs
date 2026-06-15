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

//! OpenAPI 3.x → typed Rust client generator (`firefly openapi-client`).
//!
//! The inverse of [`openapi`](crate::openapi) (which exports the server's spec):
//! given an OpenAPI 3.x document, this emits a self-contained typed client over
//! [`firefly_client::RestClient`] — a model `struct`/`enum` per
//! `components.schemas` entry and one `async fn` per operation, with typed path
//! parameters, JSON request bodies, and JSON responses. It is the Rust analog of
//! the OpenAPI-generated WebClient that firefly-oss's `-sdk` module ships.
//!
//! The generator is deliberately small and dependency-free (it walks
//! `serde_json::Value`), covering the shapes a Firefly service produces: object
//! and string-enum schemas, `$ref` links, arrays, path/query parameters, and a
//! single JSON request/response body per operation. Unmapped shapes degrade to
//! `serde_json::Value` rather than failing.

use serde_json::Value;

use crate::error::CliError;
use crate::naming::names;

/// Options controlling the generated client.
#[derive(Debug, Clone)]
pub struct ClientGenOptions {
    /// The generated client struct's name (e.g. `WalletClient`).
    pub client_name: String,
}

impl Default for ClientGenOptions {
    fn default() -> Self {
        Self {
            client_name: "ApiClient".to_string(),
        }
    }
}

/// `snake_case` for an identifier, falling back to the raw token.
fn snake(s: &str) -> String {
    names(s).map(|n| n.snake).unwrap_or_else(|| sanitize(s))
}

/// `PascalCase` for a type/variant identifier, falling back to the raw token.
fn pascal(s: &str) -> String {
    names(s).map(|n| n.pascal).unwrap_or_else(|| sanitize(s))
}

/// Replaces non-identifier characters with `_` so a raw token is a legal Rust
/// identifier fragment.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// The last path segment of a `$ref` (`#/components/schemas/Wallet` → `Wallet`).
fn ref_name(reference: &str) -> Option<String> {
    reference.rsplit('/').next().map(|s| s.to_string())
}

/// Maps an OpenAPI schema fragment to a Rust type. `required` makes a
/// non-required field `Option<T>`.
fn rust_type(schema: &Value, required: bool) -> String {
    let base = rust_type_inner(schema);
    if required {
        base
    } else {
        format!("Option<{base}>")
    }
}

fn rust_type_inner(schema: &Value) -> String {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if let Some(name) = ref_name(reference) {
            return pascal(&name);
        }
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "String".to_string(),
        Some("integer") => "i64".to_string(),
        Some("number") => "f64".to_string(),
        Some("boolean") => "bool".to_string(),
        Some("array") => {
            let item = schema
                .get("items")
                .map(rust_type_inner)
                .unwrap_or_else(|| "serde_json::Value".to_string());
            format!("Vec<{item}>")
        }
        // Inline objects / unknown shapes degrade to an open JSON value.
        _ => "serde_json::Value".to_string(),
    }
}

/// Renders one `components.schemas` entry as a Rust `struct` (object) or `enum`
/// (string enumeration). Anything else becomes a transparent type alias.
fn render_schema(name: &str, schema: &Value) -> String {
    let type_name = pascal(name);

    // String enumeration → a Rust enum with serde renames.
    if schema.get("type").and_then(Value::as_str) == Some("string") {
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            let mut out = format!(
                "/// `{name}` — generated from the OpenAPI schema.\n\
                 #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]\n\
                 pub enum {type_name} {{\n"
            );
            for v in values {
                if let Some(token) = v.as_str() {
                    let variant = pascal(token);
                    if variant == token {
                        out.push_str(&format!("    {variant},\n"));
                    } else {
                        out.push_str(&format!(
                            "    #[serde(rename = \"{token}\")]\n    {variant},\n"
                        ));
                    }
                }
            }
            out.push_str("}\n\n");
            return out;
        }
    }

    // Object → struct.
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut out = format!(
        "/// `{name}` — generated from the OpenAPI schema.\n\
         #[derive(Debug, Clone, Serialize, Deserialize)]\n\
         pub struct {type_name} {{\n"
    );
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (prop_name, prop_schema) in props {
            let is_required = required.contains(&prop_name.as_str());
            let field = snake(prop_name);
            let ty = rust_type(prop_schema, is_required);
            // Rename when the JSON name differs from the snake_case field; make
            // optional fields skip-when-none and default on absence.
            if &field != prop_name {
                out.push_str(&format!("    #[serde(rename = \"{prop_name}\")]\n"));
            }
            if !is_required {
                out.push_str("    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n");
            }
            out.push_str(&format!("    pub {field}: {ty},\n"));
        }
    }
    out.push_str("}\n\n");
    out
}

/// A single generated operation (one verb on one path).
struct Operation {
    method: String,
    name: String,
    path_params: Vec<String>,
    query_params: Vec<String>,
    body_type: Option<String>,
    response_type: String,
    path_template: String,
    summary: Option<String>,
}

/// Extracts the operations from `spec.paths`, sorted for deterministic output.
fn collect_operations(spec: &Value) -> Vec<Operation> {
    const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
    let mut ops = Vec::new();
    let Some(paths) = spec.get("paths").and_then(Value::as_object) else {
        return ops;
    };
    let mut path_keys: Vec<&String> = paths.keys().collect();
    path_keys.sort();
    for path in path_keys {
        let item = &paths[path];
        for verb in VERBS {
            let Some(op) = item.get(verb) else { continue };
            ops.push(build_operation(verb, path, op));
        }
    }
    ops
}

fn build_operation(verb: &str, path: &str, op: &Value) -> Operation {
    // Path parameters: `{id}` (OpenAPI) or `:id` (axum-style) segments.
    let mut path_params = Vec::new();
    let mut template = String::new();
    for seg in path.split('/') {
        if seg.is_empty() {
            continue;
        }
        template.push('/');
        if let Some(p) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            path_params.push(snake(p));
            template.push_str("{}");
        } else if let Some(p) = seg.strip_prefix(':') {
            path_params.push(snake(p));
            template.push_str("{}");
        } else {
            template.push_str(seg);
        }
    }

    // Query parameters declared with `in: query`.
    let query_params: Vec<String> = op
        .get("parameters")
        .and_then(Value::as_array)
        .map(|params| {
            params
                .iter()
                .filter(|p| p.get("in").and_then(Value::as_str) == Some("query"))
                .filter_map(|p| p.get("name").and_then(Value::as_str))
                .map(snake)
                .collect()
        })
        .unwrap_or_default();

    let body_type = op
        .get("requestBody")
        .and_then(|b| b.pointer("/content/application~1json/schema"))
        .and_then(|s| s.get("$ref"))
        .and_then(Value::as_str)
        .and_then(ref_name)
        .map(|n| pascal(&n));

    let response_type = success_response_type(op);

    Operation {
        method: verb.to_uppercase(),
        name: operation_name(verb, path, op),
        path_params,
        query_params,
        body_type,
        response_type,
        path_template: template,
        summary: op
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// The Rust response type from the first 2xx JSON response (`()` when none).
fn success_response_type(op: &Value) -> String {
    let Some(responses) = op.get("responses").and_then(Value::as_object) else {
        return "()".to_string();
    };
    let mut codes: Vec<&String> = responses.keys().filter(|c| c.starts_with('2')).collect();
    codes.sort();
    for code in codes {
        if let Some(schema) = responses[code].pointer("/content/application~1json/schema") {
            if let Some(name) = schema
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(ref_name)
            {
                return pascal(&name);
            }
            if schema.get("type").and_then(Value::as_str) == Some("array") {
                return rust_type_inner(schema);
            }
            return "serde_json::Value".to_string();
        }
    }
    "()".to_string()
}

/// Derives a method name: the `operationId` if present, else
/// `{verb}_{meaningful path segments}` with params rendered as `by_{name}`.
fn operation_name(verb: &str, path: &str, op: &Value) -> String {
    if let Some(oid) = op.get("operationId").and_then(Value::as_str) {
        return snake(oid);
    }
    let mut parts = vec![verb.to_string()];
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if let Some(p) = seg
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .or_else(|| seg.strip_prefix(':'))
        {
            parts.push(format!("by_{}", snake(p)));
        } else if seg == "api"
            || (seg.starts_with('v') && seg[1..].chars().all(|c| c.is_ascii_digit()))
        {
            // Skip the `/api` prefix and version segments for a tidier name.
            continue;
        } else {
            parts.push(snake(seg));
        }
    }
    snake(&parts.join("_"))
}

/// Renders the client struct + one method per operation.
fn render_client(ops: &[Operation], opts: &ClientGenOptions) -> String {
    let client = &opts.client_name;
    let mut out = format!(
        "/// A typed client over the API, generated from its OpenAPI document.\n\
         pub struct {client} {{\n    inner: RestClient,\n}}\n\n\
         impl {client} {{\n\
         \x20   /// Builds a client against `base_url` (e.g. `http://localhost:8080`).\n\
         \x20   pub fn new(base_url: impl AsRef<str>) -> Self {{\n\
         \x20       Self {{ inner: RestBuilder::new(base_url).build() }}\n\
         \x20   }}\n\n\
         \x20   /// Wraps an already-configured [`RestClient`] (custom headers, retries, …).\n\
         \x20   pub fn with_client(inner: RestClient) -> Self {{\n        Self {{ inner }}\n    }}\n"
    );

    for op in ops {
        out.push('\n');
        if let Some(summary) = &op.summary {
            out.push_str(&format!("    /// {summary}\n"));
        }
        out.push_str(&format!("    /// `{} {}`\n", op.method, op.path_template));

        // Build the argument list: path params, then a JSON body, then queries.
        let mut args: Vec<String> = Vec::new();
        for p in &op.path_params {
            args.push(format!("{p}: impl std::fmt::Display"));
        }
        if let Some(body) = &op.body_type {
            args.push(format!("body: &{body}"));
        }
        for q in &op.query_params {
            args.push(format!("{q}: impl std::fmt::Display"));
        }
        let arg_list = if args.is_empty() {
            String::new()
        } else {
            format!(", {}", args.join(", "))
        };

        out.push_str(&format!(
            "    pub async fn {}(&self{}) -> Result<{}, ClientError> {{\n",
            op.name, arg_list, op.response_type
        ));

        // Build the path expression (format! when there are path/query params).
        let path_expr = render_path_expr(op);
        out.push_str(&path_expr);

        // The request call: `Some(body)` with a body, `NO_BODY` otherwise.
        let body_generic = op.body_type.clone().unwrap_or_else(|| "()".to_string());
        let body_arg = if op.body_type.is_some() {
            "Some(body)"
        } else {
            "firefly_client::NO_BODY"
        };
        out.push_str(&format!(
            "        self.inner\n            .request::<{}, {}>(Method::{}, {}, {})\n            .await\n    }}\n",
            body_generic,
            op.response_type,
            op.method,
            if op.path_params.is_empty() && op.query_params.is_empty() {
                format!("\"{}\"", op.path_template)
            } else {
                "&__path".to_string()
            },
            body_arg,
        ));
    }

    out.push_str("}\n");
    out
}

/// Emits the `let __path = …;` binding when an operation has path or query
/// parameters (nothing when the path is a static literal).
fn render_path_expr(op: &Operation) -> String {
    if op.path_params.is_empty() && op.query_params.is_empty() {
        return String::new();
    }
    let mut expr = if op.path_params.is_empty() {
        format!(
            "        let mut __path = \"{}\".to_string();\n",
            op.path_template
        )
    } else {
        let args = op.path_params.join(", ");
        format!(
            "        let mut __path = format!(\"{}\", {});\n",
            op.path_template, args
        )
    };
    // Append query parameters as `?k=v&k2=v2`.
    for (i, q) in op.query_params.iter().enumerate() {
        let sep = if i == 0 { '?' } else { '&' };
        expr.push_str(&format!(
            "        __path.push_str(&format!(\"{sep}{q}={{}}\", {q}));\n"
        ));
    }
    expr
}

/// Generates the full client source for `spec`.
pub fn generate_client(spec: &Value, opts: &ClientGenOptions) -> Result<String, CliError> {
    if spec.get("openapi").is_none() {
        return Err(CliError::Template(
            "openapi-client: not an OpenAPI document (missing `openapi` field)".to_string(),
        ));
    }

    let title = spec
        .pointer("/info/title")
        .and_then(Value::as_str)
        .unwrap_or("the API");

    let mut out = format!(
        "// Code generated by `firefly openapi-client`. DO NOT EDIT.\n\
         //! A typed client for {title}, generated from its OpenAPI document.\n\n\
         #![allow(clippy::all, dead_code)]\n\n\
         use firefly_client::{{ClientError, RestBuilder, RestClient}};\n\
         use http::Method;\n\
         use serde::{{Deserialize, Serialize}};\n\n\
         // ---- models ---------------------------------------------------------\n\n"
    );

    if let Some(schemas) = spec
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    {
        let mut keys: Vec<&String> = schemas.keys().collect();
        keys.sort();
        for name in keys {
            out.push_str(&render_schema(name, &schemas[name]));
        }
    }

    out.push_str("// ---- client ---------------------------------------------------------\n\n");
    let ops = collect_operations(spec);
    out.push_str(&render_client(&ops, opts));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_spec() -> Value {
        json!({
            "openapi": "3.1.0",
            "info": {"title": "Wallet API", "version": "1.0"},
            "paths": {
                "/api/v1/wallets": {
                    "post": {
                        "summary": "Open a wallet",
                        "requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateWalletRequest"}}}},
                        "responses": {"201": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/WalletResponse"}}}}}
                    },
                    "get": {
                        "summary": "List by owner",
                        "parameters": [{"name": "owner", "in": "query"}],
                        "responses": {"200": {"content": {"application/json": {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/WalletResponse"}}}}}}
                    }
                },
                "/api/v1/wallets/{id}": {
                    "get": {
                        "summary": "Fetch a wallet",
                        "responses": {"200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/WalletResponse"}}}}}
                    }
                }
            },
            "components": {"schemas": {
                "WalletStatus": {"type": "string", "enum": ["active", "frozen", "closed"]},
                "CreateWalletRequest": {
                    "type": "object",
                    "required": ["owner", "currency"],
                    "properties": {
                        "owner": {"type": "string"},
                        "currency": {"type": "string"},
                        "openingBalance": {"type": "integer"}
                    }
                },
                "WalletResponse": {
                    "type": "object",
                    "required": ["id", "balance"],
                    "properties": {
                        "id": {"type": "string"},
                        "accountNumber": {"type": "string"},
                        "balance": {"type": "integer"},
                        "status": {"$ref": "#/components/schemas/WalletStatus"}
                    }
                }
            }}
        })
    }

    #[test]
    fn generates_enum_struct_and_operations() {
        let out = generate_client(&sample_spec(), &ClientGenOptions::default()).unwrap();

        // Enum schema.
        assert!(out.contains("pub enum WalletStatus"));
        assert!(out.contains("#[serde(rename = \"active\")]"));

        // Struct schema with required + optional + renamed fields.
        assert!(out.contains("pub struct CreateWalletRequest"));
        assert!(out.contains("pub owner: String,"));
        assert!(out.contains("#[serde(rename = \"openingBalance\")]"));
        assert!(out.contains("pub opening_balance: Option<i64>,"));
        assert!(out.contains("pub account_number: Option<String>,"));

        // Operations: path param, query param, body, array response.
        assert!(out.contains("pub async fn post_wallets(&self, body: &CreateWalletRequest)"));
        assert!(out.contains("Method::POST"));
        assert!(out.contains("Some(body)"));
        assert!(out.contains("pub async fn get_wallets_by_id(&self, id: impl std::fmt::Display)"));
        assert!(out.contains("firefly_client::NO_BODY"));
        assert!(out.contains("Vec<WalletResponse>"));
        assert!(out.contains("owner: impl std::fmt::Display"));
    }

    #[test]
    fn rejects_non_openapi_input() {
        let err = generate_client(&json!({"hello": "world"}), &ClientGenOptions::default());
        assert!(err.is_err());
    }
}
