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

//! # firefly-openapi
//!
//! **OpenAPI 3.1** document generation from registered [`RouteDef`]
//! descriptors plus JSON sample values of the types they consume /
//! return — the Rust counterpart of `springdoc-openapi` in the Java
//! framework and the `openapi` module in the Go port.
//!
//! The generator walks [`serde_json::Value`] samples (where Go walked
//! struct types via reflection), registers named schemas under
//! `#/components/schemas/{TypeName}`, and serves the result at
//! `/openapi.json` with a Swagger-UI shim at `/openapi/ui`.
//!
//! The generator is deliberately small — it has no annotation
//! framework, no DI, no codegen step. You hand-register routes and the
//! JSON samples do the rest.
//!
//! ## Why sample values instead of reflection?
//!
//! Rust has no runtime reflection, so where the Go port inspects a
//! struct type's fields and `json` tags, this port inspects a JSON
//! *sample* of the type: any `T: Serialize` (or a literal built with
//! [`serde_json::json!`]) becomes a [`Sample`], and the schema is
//! derived from the serialized shape. RFC 3339 strings map to
//! `string format=date-time` exactly as Go's `time.Time` did, and the
//! schema mapping table in the crate README mirrors the Go one.
//!
//! Every operation carries a `default` error response referencing
//! `#/components/schemas/ProblemDetail`, so RFC 7807 errors surface
//! uniformly — wire-compatible with the Java, .NET, Go, and Python
//! ports.
//!
//! ## Quick start
//!
//! ```
//! use firefly_openapi::{Builder, Info, RouteDef, Sample, Server};
//! use serde_json::json;
//!
//! let builder = Builder::new(Info {
//!     title: "Orders API".into(),
//!     version: "1.0.0".into(),
//!     ..Info::default()
//! })
//! .add_server(Server { url: "https://api.example.com".into(), ..Server::default() })
//! .add(RouteDef {
//!     method: "POST".into(),
//!     path: "/api/v1/orders".into(),
//!     summary: "Place an order".into(),
//!     tags: vec!["orders".into()],
//!     request: Some(Sample::named("PlaceOrderRequest", json!({"customer": "acme", "quantity": 2}))),
//!     response: Some(Sample::named("Order", json!({"id": "o-1", "customer": "acme", "quantity": 2}))),
//!     ..RouteDef::default()
//! });
//!
//! let doc = builder.build();
//! assert_eq!(doc.openapi, "3.1.0");
//!
//! // Serves GET /openapi.json + GET /openapi/ui.
//! let app: axum::Router = builder.router();
//! # let _ = app;
//! ```

use std::collections::BTreeMap;

use axum::routing::get;
use axum::Router;
use http::header;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// Errors produced by the OpenAPI generator.
#[derive(Debug, thiserror::Error)]
pub enum OpenApiError {
    /// A sample value could not be serialized to JSON
    /// (e.g. a map with non-string keys).
    #[error("openapi: failed to serialize sample value: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Info is the document metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// API title (always serialized, like the Go port).
    pub title: String,
    /// API version (always serialized, like the Go port).
    pub version: String,
    /// Optional free-text description; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Server is a base URL for the API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    /// Base URL, e.g. `https://api.example.com`.
    pub url: String,
    /// Optional human-readable label; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A JSON sample of a request or response body type, optionally carrying
/// the component-schema name it should be registered under.
///
/// This is the Rust stand-in for the Go port's `Request any` /
/// `Response any` reflection inputs: named samples whose value is a JSON
/// object are registered under `#/components/schemas/{name}` and
/// referenced via `$ref`; everything else is inlined.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Component schema name, or `None` to inline the schema
    /// (the analogue of an anonymous Go struct).
    pub name: Option<String>,
    /// The sample JSON value the schema is derived from.
    pub value: Value,
}

impl Sample {
    /// Builds a named sample from a JSON value; object values are
    /// registered under `#/components/schemas/{name}`.
    pub fn named(name: impl Into<String>, value: Value) -> Self {
        Self {
            name: Some(name.into()),
            value,
        }
    }

    /// Builds an anonymous sample whose schema is always inlined.
    pub fn inline(value: Value) -> Self {
        Self { name: None, value }
    }

    /// Serializes any `T: Serialize` into a named sample.
    ///
    /// # Errors
    ///
    /// Returns [`OpenApiError::Serialize`] if `value` cannot be
    /// represented as JSON.
    pub fn of<T: Serialize>(name: impl Into<String>, value: &T) -> Result<Self, OpenApiError> {
        Ok(Self::named(name, serde_json::to_value(value)?))
    }

    /// Serializes any `T: Serialize` into an anonymous (inlined) sample.
    ///
    /// # Errors
    ///
    /// Returns [`OpenApiError::Serialize`] if `value` cannot be
    /// represented as JSON.
    pub fn of_inline<T: Serialize>(value: &T) -> Result<Self, OpenApiError> {
        Ok(Self::inline(serde_json::to_value(value)?))
    }
}

/// RouteDef describes a single REST endpoint for the generator.
///
/// Construct with struct-update syntax, mirroring the Go struct literal:
///
/// ```
/// # use firefly_openapi::RouteDef;
/// let r = RouteDef { method: "GET".into(), path: "/ping".into(), ..RouteDef::default() };
/// # let _ = r;
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouteDef {
    /// HTTP method, e.g. `"POST"` (any case; lower-cased in the document).
    pub method: String,
    /// Route path, e.g. `/api/v1/orders/{id}`.
    pub path: String,
    /// One-line operation summary.
    pub summary: String,
    /// Longer operation description.
    pub description: String,
    /// Grouping tags shown by Swagger-UI.
    pub tags: Vec<String>,
    /// Sample of the request body type, or `None` for no body.
    pub request: Option<Sample>,
    /// Sample of the success response body, or `None` for no body.
    pub response: Option<Sample>,
    /// Success status code; `0` defaults to 201 for POST, 200 otherwise.
    pub status: u16,
}

/// Document is the OpenAPI 3.1 root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Spec version, always `"3.1.0"` when produced by [`Builder::build`].
    pub openapi: String,
    /// Document metadata.
    pub info: Info,
    /// Server list; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    /// Path → method → operation map (always serialized, even when empty).
    #[serde(default)]
    pub paths: BTreeMap<String, PathItem>,
    /// Reusable component schemas (always serialized, matching the Go
    /// port, whose `omitempty` is inert on struct values).
    #[serde(default)]
    pub components: Components,
}

/// PathItem maps each HTTP method (lower-case) to the corresponding
/// [`Operation`].
pub type PathItem = BTreeMap<String, Operation>;

/// Operation is a single endpoint description.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    /// One-line summary; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Longer description; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Grouping tags; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Request body wrapper, when the route declares a request sample.
    #[serde(
        rename = "requestBody",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub request_body: Option<RequestBody>,
    /// Status-code (or `"default"`) → response map; always serialized.
    pub responses: BTreeMap<String, Response>,
}

/// RequestBody is the OAS request body wrapper.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestBody {
    /// Whether the body is required; omitted from JSON when `false`
    /// (Go's `omitempty` bool semantics).
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// Media-type → schema map; always serialized.
    pub content: BTreeMap<String, MediaType>,
}

/// Response is the OAS response wrapper.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Human-readable description (the canonical status text for
    /// status responses).
    pub description: String,
    /// Media-type → schema map; omitted from JSON when absent
    /// (Go's nil map).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, MediaType>>,
}

/// MediaType wraps a Schema value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaType {
    /// The JSON schema (an arbitrary JSON object, as in the Go port's
    /// `map[string]any`).
    pub schema: Value,
}

/// Components holds reusable schemas referenced from operations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Components {
    /// Name → schema map; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub schemas: BTreeMap<String, Value>,
}

/// Builder accumulates routes, then renders the document via
/// [`Builder::build`].
#[derive(Debug, Clone, Default)]
pub struct Builder {
    /// Document metadata emitted into `info`.
    pub info: Info,
    /// Server entries emitted into `servers`.
    pub servers: Vec<Server>,
    routes: Vec<RouteDef>,
}

impl Builder {
    /// Returns a Builder with the given info (the Go port's `New`).
    pub fn new(info: Info) -> Self {
        Self {
            info,
            ..Self::default()
        }
    }

    /// Appends an OpenAPI server entry.
    #[must_use]
    pub fn add_server(mut self, s: Server) -> Self {
        self.servers.push(s);
        self
    }

    /// Registers a route descriptor.
    ///
    /// Named `add` for parity with the Go port's `Builder.Add`; it is
    /// route registration, not arithmetic, so `std::ops::Add` does not
    /// apply.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, r: RouteDef) -> Self {
        self.routes.push(r);
        self
    }

    /// Assembles the [`Document`].
    ///
    /// Routes sharing a path merge into one [`PathItem`]; request
    /// bodies are `application/json` and `required: true`; the success
    /// response is keyed by the status code (defaulting to 201 for
    /// POST, 200 otherwise) and described by the canonical status text;
    /// every operation also carries a `default` RFC 7807 response
    /// referencing `#/components/schemas/ProblemDetail`.
    pub fn build(&self) -> Document {
        let mut paths: BTreeMap<String, PathItem> = BTreeMap::new();
        let mut schemas: BTreeMap<String, Value> = BTreeMap::new();

        for r in &self.routes {
            let mut op = Operation {
                summary: r.summary.clone(),
                description: r.description.clone(),
                tags: r.tags.clone(),
                request_body: None,
                responses: BTreeMap::new(),
            };
            if let Some(req) = &r.request {
                let schema = schema_for(req, &mut schemas);
                op.request_body = Some(RequestBody {
                    required: true,
                    content: BTreeMap::from([(
                        "application/json".to_string(),
                        MediaType { schema },
                    )]),
                });
            }
            let status = match r.status {
                0 if r.method.eq_ignore_ascii_case("POST") => 201,
                0 => 200,
                s => s,
            };
            let mut resp = Response {
                description: status_text(status).to_string(),
                content: None,
            };
            if let Some(res) = &r.response {
                let schema = schema_for(res, &mut schemas);
                resp.content = Some(BTreeMap::from([(
                    "application/json".to_string(),
                    MediaType { schema },
                )]));
            }
            op.responses.insert(status.to_string(), resp);
            op.responses.insert(
                "default".to_string(),
                Response {
                    description: "Problem details".to_string(),
                    content: Some(BTreeMap::from([(
                        "application/problem+json".to_string(),
                        MediaType {
                            schema: json!({"$ref": "#/components/schemas/ProblemDetail"}),
                        },
                    )])),
                },
            );
            paths
                .entry(r.path.clone())
                .or_default()
                .insert(r.method.to_lowercase(), op);
        }

        schemas.insert(
            "ProblemDetail".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "type":     {"type": "string"},
                    "title":    {"type": "string"},
                    "status":   {"type": "integer"},
                    "detail":   {"type": "string"},
                    "instance": {"type": "string"},
                },
            }),
        );

        Document {
            openapi: "3.1.0".to_string(),
            info: self.info.clone(),
            servers: self.servers.clone(),
            paths,
            components: Components { schemas },
        }
    }

    /// Renders the built document as the exact bytes the JSON endpoint
    /// serves: compact JSON with sorted map keys plus a trailing
    /// newline, matching Go's `json.Encoder` output.
    pub fn json(&self) -> String {
        let mut body = serde_json::to_string(&self.build())
            .expect("OpenAPI document always serializes to JSON");
        body.push('\n');
        body
    }

    /// Returns an [`axum::Router`] serving `GET /openapi.json` (and
    /// `GET /openapi/ui` — a minimal Swagger-UI HTML page), the Go
    /// port's `Handler()`.
    ///
    /// The document is rendered once, when the router is created.
    pub fn router(&self) -> Router {
        let body = self.json();
        Router::new()
            .route(
                "/openapi.json",
                get(move || {
                    let body = body.clone();
                    async move { ([(header::CONTENT_TYPE, "application/json")], body) }
                }),
            )
            .route(
                "/openapi/ui",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        SWAGGER_UI_PAGE,
                    )
                }),
            )
    }
}

/// The Swagger-UI HTML shim served at `/openapi/ui` — byte-for-byte the
/// page the Go port serves.
const SWAGGER_UI_PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><title>API · Firefly</title>
<link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css"></head>
<body><div id="swagger-ui"></div>
<script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
<script>
window.onload = () => SwaggerUIBundle({ url: "/openapi.json", dom_id: "#swagger-ui" });
</script></body></html>"##;

// ----- Sample-based schema derivation -----

/// Derives the schema for a sample. Named object samples are registered
/// under `#/components/schemas/{name}` (first registration wins, as in
/// the Go port) and referenced via `$ref`; everything else is inlined.
fn schema_for(sample: &Sample, components: &mut BTreeMap<String, Value>) -> Value {
    match (&sample.name, &sample.value) {
        (Some(name), Value::Object(map)) => {
            if !components.contains_key(name) {
                // Placeholder first, mirroring the Go recursion guard.
                components.insert(name.clone(), json!({}));
                let schema = object_schema(map);
                components.insert(name.clone(), schema);
            }
            json!({"$ref": format!("#/components/schemas/{name}")})
        }
        _ => value_schema(&sample.value),
    }
}

/// Maps a JSON sample value onto its OpenAPI schema, mirroring the Go
/// port's reflection table: strings → `string` (RFC 3339 strings →
/// `format: date-time`, Go's `time.Time`), booleans → `boolean`,
/// integers → `integer format=int64`, floats → `number`, arrays →
/// `array` with `items` from the first element, objects → inline
/// object schemas, `null` → `{"type":"object"}` (Go's default branch).
fn value_schema(v: &Value) -> Value {
    match v {
        Value::Null => json!({"type": "object"}),
        Value::Bool(_) => json!({"type": "boolean"}),
        Value::Number(n) if n.is_f64() => json!({"type": "number"}),
        Value::Number(_) => json!({"type": "integer", "format": "int64"}),
        Value::String(s) if is_rfc3339(s) => json!({"type": "string", "format": "date-time"}),
        Value::String(_) => json!({"type": "string"}),
        Value::Array(items) => {
            let item_schema = items
                .first()
                .map(value_schema)
                .unwrap_or_else(|| json!({"type": "object"}));
            json!({"type": "array", "items": item_schema})
        }
        Value::Object(map) => object_schema(map),
    }
}

/// Builds an inline object schema: every present key becomes a property
/// and every non-null key is required (the sample-value analogue of
/// Go's "no `omitempty`" rule).
fn object_schema(map: &Map<String, Value>) -> Value {
    let mut props = Map::new();
    let mut required: Vec<String> = Vec::new();
    for (key, val) in map {
        props.insert(key.clone(), value_schema(val));
        if !val.is_null() {
            required.push(key.clone());
        }
    }
    let mut out = json!({"type": "object", "properties": props});
    if !required.is_empty() {
        out["required"] = json!(required);
    }
    out
}

/// Returns the canonical reason phrase for a status code, or `""` for
/// unknown codes — the behavior of Go's `http.StatusText`.
fn status_text(status: u16) -> &'static str {
    http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("")
}

/// True when the string parses as an RFC 3339 timestamp (the wire shape
/// of Go's `time.Time` and chrono's `DateTime<Utc>`).
fn is_rfc3339(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use http::header::CONTENT_TYPE;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[derive(Serialize)]
    struct Order {
        id: String,
        customer: String,
        quantity: i64,
        #[serde(rename = "createdAt")]
        created_at: DateTime<Utc>,
    }

    #[derive(Serialize)]
    struct PlaceOrderRequest {
        customer: String,
        quantity: i64,
    }

    fn order_sample() -> Order {
        Order {
            id: "o-1".to_string(),
            customer: "acme".to_string(),
            quantity: 2,
            created_at: DateTime::parse_from_rfc3339("2026-06-12T10:00:00Z")
                .unwrap()
                .to_utc(),
        }
    }

    /// The Go test's builder: POST + GET over the orders API.
    fn orders_builder() -> Builder {
        Builder::new(Info {
            title: "Orders API".to_string(),
            version: "1.0.0".to_string(),
            ..Info::default()
        })
        .add_server(Server {
            url: "http://localhost:8080".to_string(),
            ..Server::default()
        })
        .add(RouteDef {
            method: "POST".to_string(),
            path: "/api/v1/orders".to_string(),
            summary: "Place an order".to_string(),
            tags: vec!["orders".to_string()],
            request: Some(
                Sample::of(
                    "PlaceOrderRequest",
                    &PlaceOrderRequest {
                        customer: "acme".to_string(),
                        quantity: 2,
                    },
                )
                .unwrap(),
            ),
            response: Some(Sample::of("Order", &order_sample()).unwrap()),
            ..RouteDef::default()
        })
        .add(RouteDef {
            method: "GET".to_string(),
            path: "/api/v1/orders/{id}".to_string(),
            summary: "Get one order".to_string(),
            tags: vec!["orders".to_string()],
            response: Some(Sample::of("Order", &order_sample()).unwrap()),
            ..RouteDef::default()
        })
    }

    async fn get_response(router: Router, uri: &str) -> (StatusCode, String, String) {
        let resp = router
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get(CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, ct, String::from_utf8(body.to_vec()).unwrap())
    }

    // ----- Ported from Go: TestBuildAndServe -----

    #[test]
    fn build_registers_paths_operations_and_schemas() {
        let doc = orders_builder().build();
        assert_eq!(doc.openapi, "3.1.0");
        let path = doc.paths.get("/api/v1/orders").expect("path missing");
        let post = path.get("post").expect("post operation missing");
        assert_eq!(post.summary, "Place an order");
        assert!(
            doc.components.schemas.contains_key("Order"),
            "Order schema not registered"
        );
        assert!(
            doc.components.schemas.contains_key("ProblemDetail"),
            "ProblemDetail schema not registered"
        );
    }

    #[tokio::test]
    async fn handler_serves_openapi_json() {
        let (status, ct, body) = get_response(orders_builder().router(), "/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "application/json");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["openapi"], "3.1.0", "served json malformed");
        // Parity with Go's json.Encoder: compact body + trailing newline.
        assert!(body.ends_with('\n'));
        assert!(!body.trim_end().contains('\n'));
    }

    #[tokio::test]
    async fn handler_serves_swagger_ui() {
        let (status, ct, body) = get_response(orders_builder().router(), "/openapi/ui").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.contains("text/html"), "ui not html: {ct}");
        assert_eq!(ct, "text/html; charset=utf-8");
        // Byte-for-byte the Go page.
        assert_eq!(body, SWAGGER_UI_PAGE);
        assert!(body.starts_with("<!doctype html>"));
        assert!(body.contains("API · Firefly"));
        assert!(body.contains("swagger-ui-dist@5"));
        assert!(
            body.contains(r##"SwaggerUIBundle({ url: "/openapi.json", dom_id: "#swagger-ui" })"##)
        );
    }

    // ----- Ported from Go: TestSchemaPrimitives -----

    #[test]
    fn schema_primitives() {
        assert_eq!(value_schema(&json!("x"))["type"], "string");
        assert_eq!(value_schema(&json!(42))["type"], "integer");
        assert_eq!(value_schema(&json!(42))["format"], "int64");
        assert_eq!(value_schema(&json!([0]))["type"], "array");
        let t = serde_json::to_value(order_sample().created_at).unwrap();
        assert_eq!(value_schema(&t)["format"], "date-time");
        assert_eq!(value_schema(&t)["type"], "string");
    }

    // ----- Rust-specific coverage -----

    #[test]
    fn schema_more_primitives() {
        assert_eq!(value_schema(&json!(true))["type"], "boolean");
        assert_eq!(value_schema(&json!(1.5))["type"], "number");
        assert_eq!(
            value_schema(&json!(1.0))["type"],
            "number",
            "floats stay numbers"
        );
        assert_eq!(value_schema(&Value::Null)["type"], "object");
        // Non-timestamp strings stay plain strings.
        assert_eq!(
            value_schema(&json!("2026-06-12")),
            json!({"type": "string"})
        );
        // Empty arrays fall back to object items (element type unknowable
        // from a sample, unlike Go reflection).
        assert_eq!(
            value_schema(&json!([])),
            json!({"type": "array", "items": {"type": "object"}})
        );
        // Array items derive from the first element.
        assert_eq!(
            value_schema(&json!(["a"])),
            json!({"type": "array", "items": {"type": "string"}})
        );
    }

    #[test]
    fn named_object_schema_is_registered_and_referenced() {
        let mut components = BTreeMap::new();
        let sample = Sample::of("Order", &order_sample()).unwrap();
        let schema = schema_for(&sample, &mut components);
        assert_eq!(schema, json!({"$ref": "#/components/schemas/Order"}));
        let order = components.get("Order").unwrap();
        assert_eq!(order["type"], "object");
        assert_eq!(order["properties"]["id"], json!({"type": "string"}));
        assert_eq!(
            order["properties"]["quantity"],
            json!({"type": "integer", "format": "int64"})
        );
        assert_eq!(
            order["properties"]["createdAt"],
            json!({"type": "string", "format": "date-time"})
        );
        assert_eq!(
            order["required"],
            json!(["createdAt", "customer", "id", "quantity"])
        );
    }

    #[test]
    fn named_schema_first_registration_wins() {
        let mut components = BTreeMap::new();
        let first = Sample::named("Order", json!({"id": "o-1"}));
        let second = Sample::named("Order", json!({"different": true}));
        schema_for(&first, &mut components);
        schema_for(&second, &mut components);
        assert_eq!(components.len(), 1);
        assert!(components["Order"]["properties"]["id"].is_object());
    }

    #[test]
    fn anonymous_and_non_object_samples_are_inlined() {
        let mut components = BTreeMap::new();
        let inline = schema_for(&Sample::inline(json!({"n": 1})), &mut components);
        assert_eq!(inline["type"], "object");
        assert_eq!(inline["properties"]["n"]["type"], "integer");
        // A named sample whose value is not an object inlines too (Go only
        // names structs).
        let prim = schema_for(&Sample::named("Count", json!(3)), &mut components);
        assert_eq!(prim, json!({"type": "integer", "format": "int64"}));
        assert!(components.is_empty());
    }

    #[test]
    fn nested_objects_inline_and_null_fields_are_optional() {
        let schema = value_schema(&json!({"inner": {"a": 1}, "maybe": null}));
        assert_eq!(schema["properties"]["inner"]["type"], "object");
        assert_eq!(
            schema["properties"]["inner"]["properties"]["a"]["type"],
            "integer"
        );
        // Null-valued keys get Go's default schema and are not required.
        assert_eq!(schema["properties"]["maybe"], json!({"type": "object"}));
        assert_eq!(schema["required"], json!(["inner"]));
    }

    #[test]
    fn default_status_codes_and_texts() {
        let doc = Builder::new(Info::default())
            .add(RouteDef {
                method: "POST".to_string(),
                path: "/a".to_string(),
                ..RouteDef::default()
            })
            .add(RouteDef {
                method: "get".to_string(),
                path: "/b".to_string(),
                ..RouteDef::default()
            })
            .add(RouteDef {
                method: "DELETE".to_string(),
                path: "/c".to_string(),
                status: 204,
                ..RouteDef::default()
            })
            .add(RouteDef {
                method: "GET".to_string(),
                path: "/d".to_string(),
                status: 599,
                ..RouteDef::default()
            })
            .build();
        let post = &doc.paths["/a"]["post"].responses["201"];
        assert_eq!(post.description, "Created");
        let get = &doc.paths["/b"]["get"].responses["200"];
        assert_eq!(get.description, "OK");
        let del = &doc.paths["/c"]["delete"].responses["204"];
        assert_eq!(del.description, "No Content");
        // Unknown codes get Go's http.StatusText("") behavior.
        let odd = &doc.paths["/d"]["get"].responses["599"];
        assert_eq!(odd.description, "");
        // No response sample → no content key.
        assert!(post.content.is_none());
    }

    #[test]
    fn every_operation_gets_the_problem_detail_default_response() {
        let doc = orders_builder().build();
        for (path, item) in &doc.paths {
            for (method, op) in item {
                let default = op
                    .responses
                    .get("default")
                    .unwrap_or_else(|| panic!("{method} {path} missing default response"));
                assert_eq!(default.description, "Problem details");
                let content = default.content.as_ref().unwrap();
                let media = content.get("application/problem+json").unwrap();
                assert_eq!(
                    media.schema,
                    json!({"$ref": "#/components/schemas/ProblemDetail"})
                );
            }
        }
        let pd = &doc.components.schemas["ProblemDetail"];
        assert_eq!(pd["type"], "object");
        for field in ["type", "title", "status", "detail", "instance"] {
            assert!(
                pd["properties"][field].is_object(),
                "ProblemDetail missing {field}"
            );
        }
    }

    #[test]
    fn request_body_is_required_json() {
        let doc = orders_builder().build();
        let body = doc.paths["/api/v1/orders"]["post"]
            .request_body
            .as_ref()
            .unwrap();
        assert!(body.required);
        let media = body.content.get("application/json").unwrap();
        assert_eq!(
            media.schema,
            json!({"$ref": "#/components/schemas/PlaceOrderRequest"})
        );
        // GET route declared no request body.
        assert!(doc.paths["/api/v1/orders/{id}"]["get"]
            .request_body
            .is_none());
    }

    #[test]
    fn routes_on_the_same_path_merge_into_one_path_item() {
        let doc = Builder::new(Info::default())
            .add(RouteDef {
                method: "GET".to_string(),
                path: "/x".to_string(),
                ..RouteDef::default()
            })
            .add(RouteDef {
                method: "PUT".to_string(),
                path: "/x".to_string(),
                ..RouteDef::default()
            })
            .build();
        assert_eq!(doc.paths.len(), 1);
        let item = &doc.paths["/x"];
        assert_eq!(item.len(), 2);
        assert!(item.contains_key("get") && item.contains_key("put"));
    }

    #[test]
    fn wire_format_matches_go_omission_rules() {
        // Minimal builder: no servers, no routes, empty description.
        let body = Builder::new(Info {
            title: "T".to_string(),
            version: "1".to_string(),
            ..Info::default()
        })
        .json();
        assert!(body.ends_with('\n'));
        let value: Value = serde_json::from_str(&body).unwrap();
        let expected = json!({
            "openapi": "3.1.0",
            "info": {"title": "T", "version": "1"},
            "paths": {},
            "components": {"schemas": {"ProblemDetail": {
                "type": "object",
                "properties": {
                    "type":     {"type": "string"},
                    "title":    {"type": "string"},
                    "status":   {"type": "integer"},
                    "detail":   {"type": "string"},
                    "instance": {"type": "string"},
                },
            }}},
        });
        assert_eq!(value, expected);
        // Empty optionals are omitted, exactly like Go's omitempty.
        assert!(!body.contains("\"servers\""));
        assert!(!body.contains("\"description\""));
        // Top-level key order matches the Go struct declaration order.
        let order = ["\"openapi\"", "\"info\"", "\"paths\"", "\"components\""]
            .map(|k| body.find(k).unwrap());
        assert!(order.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn full_document_wire_shape() {
        let doc = orders_builder().build();
        let value = serde_json::to_value(&doc).unwrap();
        assert_eq!(value["servers"], json!([{"url": "http://localhost:8080"}]));
        let post = &value["paths"]["/api/v1/orders"]["post"];
        assert_eq!(post["summary"], "Place an order");
        assert_eq!(post["tags"], json!(["orders"]));
        assert_eq!(post["requestBody"]["required"], json!(true));
        assert_eq!(
            post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/PlaceOrderRequest"
        );
        assert_eq!(post["responses"]["201"]["description"], "Created");
        assert_eq!(
            post["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Order"
        );
        // Operation with empty description omits the key.
        assert!(post.get("description").is_none());
    }

    #[test]
    fn request_body_required_false_is_omitted() {
        let body = RequestBody {
            required: false,
            content: BTreeMap::new(),
        };
        let s = serde_json::to_string(&body).unwrap();
        assert_eq!(s, r#"{"content":{}}"#);
        let back: RequestBody = serde_json::from_str(&s).unwrap();
        assert!(!back.required);
    }

    #[test]
    fn document_serde_round_trip() {
        let doc = orders_builder().build();
        let s = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&s).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn sample_of_reports_serialization_errors() {
        let mut bad = std::collections::HashMap::new();
        bad.insert((1u8, 2u8), "x");
        let err = Sample::of("Bad", &bad).unwrap_err();
        assert!(matches!(err, OpenApiError::Serialize(_)));
        assert!(err
            .to_string()
            .starts_with("openapi: failed to serialize sample value"));
        assert!(Sample::of_inline(&bad).is_err());
    }

    #[tokio::test]
    async fn json_route_rejects_other_methods() {
        let resp = orders_builder()
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/openapi.json")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn builder_and_document_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Builder>();
        assert_send_sync::<Document>();
        assert_send_sync::<RouteDef>();
        assert_send_sync::<Sample>();
        assert_send_sync::<OpenApiError>();
    }
}
