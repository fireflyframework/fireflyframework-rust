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
//! The generator registers named schemas under
//! `#/components/schemas/{TypeName}` — from `#[derive(Schema)]` component
//! descriptors ([`Builder::add_schema_descriptors`]) and/or from
//! [`serde_json::Value`] samples (where Go walked struct types via reflection)
//! — and serves the document plus a Swagger-UI and a ReDoc page.
//!
//! ## Two ways to serve it
//!
//! - [`Builder::router`] serves the Go-port paths `/openapi.json` +
//!   `/openapi/ui` + `/redoc`.
//! - [`Builder::docs_router`] serves a configurable surface — by default the
//!   springdoc/pyfly paths `/v3/api-docs` (+ `/openapi.json`), `/swagger-ui`
//!   (+ `/swagger-ui.html`) and `/redoc` — and is what `FireflyApplication`
//!   auto-mounts so every service exposes live API docs with no app code.
//!
//! [`Builder::from_inventory`] builds a complete document straight from the
//! live registries: every `#[rest_controller]` route plus every
//! `#[derive(Schema)]` component schema.
//!
//! ## Automatic generation from `#[rest_controller]`
//!
//! When the `firefly-macros` `#[rest_controller]` / `#[get]` / `#[post]`
//! markers decorate your controllers, they emit a compile-time
//! [`firefly_container::RouteDescriptor`] for every mapped method into an
//! `inventory` registry. [`Builder::from_routes`] (and
//! [`Builder::add_route_descriptors`]) consume that registry to build a
//! spec **without re-declaring a single route** — the Rust analog of
//! pyfly's `ControllerRegistrar.collect_route_metadata` driving its
//! `OpenAPIGenerator`. Each descriptor yields an operation whose:
//!
//! - **path** is the axum route with `:param` / `*rest` segments
//!   normalised to OpenAPI `{param}`,
//! - **path parameters** become required `in: path` [`Parameter`]s,
//! - **tag** is derived from the controller type name
//!   (`OrderApi` → `Order`, `CatalogController` → `Catalog`),
//! - **operationId** is the handler method name.
//!
//! ```no_run
//! use firefly_openapi::{Builder, Info};
//!
//! // `firefly_container::routes()` enumerates every `#[rest_controller]`
//! // route discovered across the crate graph.
//! let builder = Builder::new(Info {
//!     title: "Orders API".into(),
//!     version: "1.0.0".into(),
//!     ..Info::default()
//! })
//! .from_routes(firefly_container::routes());
//!
//! // Serves GET /openapi.json + GET /openapi/ui + GET /redoc.
//! let app: axum::Router = builder.router();
//! # let _ = app;
//! ```
//!
//! Enrich any auto-derived operation with request/response schemas,
//! summaries, or a `deprecated` flag by also calling [`Builder::add`]
//! with a matching `method` + `path` — same-path/same-method routes merge
//! field-by-field (an explicit value wins over the auto-derived default).
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
pub const VERSION: &str = "26.6.18";

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

/// A top-level tag declaration (`{"name": "Order"}`), the OpenAPI
/// grouping pyfly emits into the document's `tags` array.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// The tag name shown by Swagger-UI / ReDoc.
    pub name: String,
    /// Optional human-readable description; omitted from JSON when empty.
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
    /// Stable operation identifier (e.g. the handler method name);
    /// omitted from the document when empty.
    pub operation_id: String,
    /// One-line operation summary.
    pub summary: String,
    /// Longer operation description.
    pub description: String,
    /// Grouping tags shown by Swagger-UI.
    pub tags: Vec<String>,
    /// Path / query / header / cookie parameters.
    pub parameters: Vec<Parameter>,
    /// Sample of the request body type, or `None` for no body.
    pub request: Option<Sample>,
    /// Sample of the success response body, or `None` for no body.
    pub response: Option<Sample>,
    /// Request body component-schema name to `$ref` (set by a route's
    /// `request = Type` attribute, naming a `#[derive(Schema)]` type). Takes
    /// precedence over [`request`](Self::request) when both are present.
    pub request_schema: Option<String>,
    /// Success-response component-schema name to `$ref` (a route's
    /// `response = Type`). Takes precedence over [`response`](Self::response).
    pub response_schema: Option<String>,
    /// Success status code; `0` defaults to 201 for POST, 200 otherwise.
    pub status: u16,
    /// Whether the endpoint is deprecated; renders `deprecated: true` and
    /// is omitted from the document when `false`.
    pub deprecated: bool,
}

/// Where a [`Parameter`] is carried — the OpenAPI `in` field.
///
/// Mirrors pyfly's `PathVar` / `QueryParam` / `Header` / `Cookie`
/// bindings, which it maps onto `in: path|query|header|cookie`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterIn {
    /// A templated path segment (`/orders/{id}`); always required.
    Path,
    /// A `?key=value` query-string parameter.
    Query,
    /// An HTTP request header.
    Header,
    /// An HTTP cookie.
    Cookie,
}

impl ParameterIn {
    /// The lowercase OpenAPI `in` token (`"path"`, `"query"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            ParameterIn::Path => "path",
            ParameterIn::Query => "query",
            ParameterIn::Header => "header",
            ParameterIn::Cookie => "cookie",
        }
    }
}

/// One OpenAPI operation parameter (the analog of pyfly's per-binding
/// parameter dict). Path parameters are always required.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    /// Parameter name (`id`, `page`, `X-Tenant-Id`, …).
    pub name: String,
    /// Where the parameter is carried.
    pub location: ParameterIn,
    /// Whether the parameter must be supplied (always `true` for `path`).
    pub required: bool,
    /// The parameter's JSON schema (defaults to `{"type":"string"}`).
    pub schema: Value,
}

impl Parameter {
    /// Builds a required `in: path` string parameter — the shape derived
    /// from a `{name}` segment when a route declares no richer schema.
    pub fn path(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            location: ParameterIn::Path,
            required: true,
            schema: json!({"type": "string"}),
        }
    }

    /// Renders the parameter as the OpenAPI parameter object.
    fn to_value(&self) -> Value {
        json!({
            "name": self.name,
            "in": self.location.as_str(),
            "required": self.required,
            "schema": self.schema,
        })
    }
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
    /// Top-level tag declarations (one `{ "name": … }` per controller
    /// group), in discovery order; omitted from JSON when empty —
    /// matching pyfly's `spec["tags"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
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
    /// Stable operation identifier (pyfly emits the handler name);
    /// omitted from JSON when empty.
    #[serde(
        rename = "operationId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub operation_id: String,
    /// One-line summary; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// Longer description; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Grouping tags; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Whether the endpoint is deprecated; omitted from JSON when `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deprecated: bool,
    /// Path / query / header / cookie parameters; omitted from JSON when
    /// empty. Each entry is the OpenAPI parameter object produced by
    /// [`Parameter`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Value>,
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
    /// Tag names in discovery order (de-duplicated), emitted into the
    /// document's top-level `tags` array.
    tags: Vec<String>,
    /// Component schemas registered explicitly (e.g. from `#[derive(Schema)]`
    /// descriptors), merged into `components.schemas`. Keyed by component name.
    extra_schemas: BTreeMap<String, Value>,
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
        for tag in &r.tags {
            self.track_tag(tag);
        }
        self.routes.push(r);
        self
    }

    /// Records a tag name into the top-level `tags` array, preserving
    /// discovery order and de-duplicating — pyfly's `_collect_tags`.
    fn track_tag(&mut self, tag: &str) {
        if !tag.is_empty() && !self.tags.iter().any(|t| t == tag) {
            self.tags.push(tag.to_string());
        }
    }

    /// Builds a [`RouteDef`] from a single [`firefly_container::RouteDescriptor`]
    /// (a `#[rest_controller]` route) and registers it.
    ///
    /// The descriptor's axum path (`/orders/:id`, `/files/*rest`) is
    /// normalised to the OpenAPI template form (`/orders/{id}`,
    /// `/files/{rest}`); every templated segment becomes a required
    /// `in: path` [`Parameter`]; the operation's tag is derived from the
    /// controller type name and its `operationId` is the handler name.
    #[must_use]
    pub fn add_route(self, descriptor: &firefly_container::RouteDescriptor) -> Self {
        let (path, params) = openapi_path_and_params(descriptor.path);
        // Tags from the macro (`#[get(tags = [..])]` / `#[rest_controller(tag)]`)
        // win; otherwise derive a single tag from the controller type name.
        let tags = if descriptor.tags.is_empty() {
            let tag = derive_tag(descriptor.controller);
            if tag.is_empty() {
                Vec::new()
            } else {
                vec![tag]
            }
        } else {
            descriptor.tags.iter().map(|t| (*t).to_string()).collect()
        };
        let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
        self.add(RouteDef {
            method: descriptor.method.to_string(),
            path,
            operation_id: descriptor.handler.to_string(),
            summary: descriptor.summary.to_string(),
            description: descriptor.description.to_string(),
            tags,
            deprecated: descriptor.deprecated,
            parameters: params,
            request_schema: opt(descriptor.request_schema),
            response_schema: opt(descriptor.response_schema),
            status: descriptor.status,
            ..RouteDef::default()
        })
    }

    /// Registers a component schema under `name` from a JSON Schema value,
    /// merged into `components.schemas`. The Rust analog of springdoc adding a
    /// `@Schema` model to the document.
    #[must_use]
    pub fn add_schema(mut self, name: impl Into<String>, schema: Value) -> Self {
        self.extra_schemas.insert(name.into(), schema);
        self
    }

    /// Registers every [`firefly_container::SchemaDescriptor`] in `descriptors`
    /// (the JSON Schema strings emitted by `#[derive(Schema)]`) into
    /// `components.schemas`. A descriptor whose JSON fails to parse is skipped.
    #[must_use]
    pub fn add_schema_descriptors<'a, I>(mut self, descriptors: I) -> Self
    where
        I: IntoIterator<Item = &'a firefly_container::SchemaDescriptor>,
    {
        for descriptor in descriptors {
            if let Ok(schema) = serde_json::from_str::<Value>(descriptor.schema) {
                self.extra_schemas
                    .insert(descriptor.name.to_string(), schema);
            }
        }
        self
    }

    /// Builds a complete document from the **live inventory**: every
    /// `#[rest_controller]` route ([`firefly_container::routes`]) plus every
    /// `#[derive(Schema)]` component schema ([`firefly_container::schemas`]).
    /// This is what `FireflyApplication` serves at `/v3/api-docs` — the Rust
    /// analog of springdoc scanning the classpath.
    #[must_use]
    pub fn from_inventory(self) -> Self {
        self.from_routes(firefly_container::routes())
            .add_schema_descriptors(firefly_container::schemas())
    }

    /// Registers every descriptor in `descriptors` via [`Builder::add_route`].
    ///
    /// Pass [`firefly_container::routes()`] to build a spec from every
    /// `#[rest_controller]` route discovered across the crate graph — the
    /// Rust analog of pyfly's `collect_route_metadata` feeding the
    /// `OpenAPIGenerator`.
    #[must_use]
    pub fn add_route_descriptors<'a, I>(mut self, descriptors: I) -> Self
    where
        I: IntoIterator<Item = &'a firefly_container::RouteDescriptor>,
    {
        for descriptor in descriptors {
            self = self.add_route(descriptor);
        }
        self
    }

    /// Alias of [`Builder::add_route_descriptors`] with a name that reads
    /// at the call site as "build from the live route table".
    #[must_use]
    pub fn from_routes<'a, I>(self, descriptors: I) -> Self
    where
        I: IntoIterator<Item = &'a firefly_container::RouteDescriptor>,
    {
        self.add_route_descriptors(descriptors)
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

        for r in &self.merged_routes() {
            let mut op = Operation {
                operation_id: r.operation_id.clone(),
                summary: r.summary.clone(),
                description: r.description.clone(),
                tags: r.tags.clone(),
                deprecated: r.deprecated,
                parameters: r.parameters.iter().map(Parameter::to_value).collect(),
                request_body: None,
                responses: BTreeMap::new(),
            };
            // A request schema name `$ref`s a component — but ONLY when that
            // component is actually registered (a `#[derive(Schema)]` type), so
            // an inferred-but-unregistered body type (e.g. `serde_json::Value`)
            // never produces a dangling `$ref`. Otherwise a JSON sample is used.
            let request_ref = r
                .request_schema
                .as_ref()
                .filter(|name| self.extra_schemas.contains_key(*name));
            if let Some(name) = request_ref {
                op.request_body = Some(RequestBody {
                    required: true,
                    content: BTreeMap::from([(
                        "application/json".to_string(),
                        MediaType {
                            schema: json!({"$ref": format!("#/components/schemas/{name}")}),
                        },
                    )]),
                });
            } else if let Some(req) = &r.request {
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
            let response_ref = r
                .response_schema
                .as_ref()
                .filter(|name| self.extra_schemas.contains_key(*name));
            if let Some(name) = response_ref {
                resp.content = Some(BTreeMap::from([(
                    "application/json".to_string(),
                    MediaType {
                        schema: json!({"$ref": format!("#/components/schemas/{name}")}),
                    },
                )]));
            } else if let Some(res) = &r.response {
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

        // Merge the explicitly-registered component schemas (`#[derive(Schema)]`
        // DTOs). A sample-derived schema of the same name keeps the sample
        // version; otherwise the registered schema is added.
        for (name, schema) in &self.extra_schemas {
            schemas
                .entry(name.clone())
                .or_insert_with(|| schema.clone());
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
            tags: self
                .tags
                .iter()
                .map(|name| Tag {
                    name: name.clone(),
                    ..Tag::default()
                })
                .collect(),
            paths,
            components: Components { schemas },
        }
    }

    /// Folds every registered [`RouteDef`] sharing the same `(method,
    /// path)` into one, merging field-by-field so an auto-derived route
    /// (from a `#[rest_controller]` descriptor) and an explicit
    /// [`Builder::add`] of the same endpoint compose: the later route's
    /// non-default fields win, the earlier route's fill the gaps. This is
    /// what lets a user enrich an auto-generated operation with a request
    /// /response schema or a `deprecated` flag without re-declaring its
    /// path, method, tag, and parameters.
    ///
    /// Insertion order is preserved across distinct endpoints.
    fn merged_routes(&self) -> Vec<RouteDef> {
        let mut order: Vec<(String, String)> = Vec::new();
        let mut merged: BTreeMap<(String, String), RouteDef> = BTreeMap::new();
        for r in &self.routes {
            let key = (r.method.to_lowercase(), r.path.clone());
            match merged.get_mut(&key) {
                Some(existing) => merge_route(existing, r),
                None => {
                    order.push(key.clone());
                    merged.insert(key, r.clone());
                }
            }
        }
        order
            .into_iter()
            .map(|key| merged.remove(&key).expect("merged route present"))
            .collect()
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

    /// Returns an [`axum::Router`] serving `GET /openapi.json`,
    /// `GET /openapi/ui` (a minimal Swagger-UI HTML page), and
    /// `GET /redoc` (a ReDoc HTML page) — the Go port's `Handler()`
    /// extended with pyfly's `/redoc` documentation endpoint.
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
            .route(
                "/redoc",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                        REDOC_PAGE,
                    )
                }),
            )
    }

    /// Returns an [`axum::Router`] serving the documentation surface at the
    /// configured paths — the spec JSON (with aliases), a Swagger-UI page, and
    /// a ReDoc page, with the UI shells pointed at the configured spec path.
    ///
    /// Unlike [`router`](Self::router) (which hard-codes the Go-port paths),
    /// this honours a [`DocsConfig`]. The default surface is the spec at
    /// `GET /v3/api-docs` (with `/openapi.json` as an alias), Swagger-UI at
    /// `GET /swagger-ui` and `/swagger-ui.html` (Spring Boot's springdoc paths),
    /// and ReDoc at `GET /redoc` (pyfly's path). This is what
    /// `FireflyApplication` auto-mounts so every service exposes live API docs
    /// with no application code. The document is rendered once, here.
    pub fn docs_router(&self, cfg: &DocsConfig) -> Router {
        let body = self.json();
        let swagger = swagger_ui_html(&cfg.spec_path, &self.info.title);
        let redoc = redoc_html(&cfg.spec_path, &self.info.title);

        // The spec is served at its primary path plus any aliases, de-duplicated
        // (axum panics on a duplicate route).
        let mut spec_paths = vec![cfg.spec_path.clone()];
        for alias in &cfg.spec_aliases {
            if !spec_paths.contains(alias) {
                spec_paths.push(alias.clone());
            }
        }

        let mut router = Router::new();
        for path in spec_paths {
            let body = body.clone();
            router = router.route(
                &path,
                get(move || {
                    let body = body.clone();
                    async move { ([(header::CONTENT_TYPE, "application/json")], body) }
                }),
            );
        }

        // Swagger UI at its path and a `.html` alias (Spring serves both).
        let swagger_html_path = format!("{}.html", cfg.swagger_ui_path);
        for path in [cfg.swagger_ui_path.clone(), swagger_html_path] {
            let swagger = swagger.clone();
            router = router.route(
                &path,
                get(move || {
                    let swagger = swagger.clone();
                    async move {
                        (
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            swagger,
                        )
                    }
                }),
            );
        }

        router.route(
            &cfg.redoc_path,
            get(move || {
                let redoc = redoc.clone();
                async move { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], redoc) }
            }),
        )
    }
}

/// Where the live documentation endpoints are mounted by
/// [`Builder::docs_router`]. The defaults mirror Spring Boot's springdoc
/// (`/v3/api-docs`, `/swagger-ui`) and pyfly's ReDoc (`/redoc`), keeping
/// `/openapi.json` as a spec alias for the framework's own tooling.
#[derive(Debug, Clone)]
pub struct DocsConfig {
    /// The primary spec path (Swagger/ReDoc point here). Default `/v3/api-docs`.
    pub spec_path: String,
    /// Additional paths the spec JSON is also served at. Default `/openapi.json`.
    pub spec_aliases: Vec<String>,
    /// The Swagger-UI mount (also served at `{path}.html`). Default `/swagger-ui`.
    pub swagger_ui_path: String,
    /// The ReDoc mount. Default `/redoc`.
    pub redoc_path: String,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            spec_path: "/v3/api-docs".to_string(),
            spec_aliases: vec!["/openapi.json".to_string()],
            swagger_ui_path: "/swagger-ui".to_string(),
            redoc_path: "/redoc".to_string(),
        }
    }
}

/// The Swagger-UI HTML page, pointed at `spec_url` and titled `title`
/// (CDN-loaded `swagger-ui-dist@5` from jsdelivr).
fn swagger_ui_html(spec_url: &str, title: &str) -> String {
    let title = html_escape(title);
    format!(
        r##"<!doctype html>
<html><head><meta charset="utf-8"><title>{title} · Swagger UI</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css"></head>
<body><div id="swagger-ui"></div>
<script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
<script>
window.onload = () => SwaggerUIBundle({{ url: "{spec_url}", dom_id: "#swagger-ui", deepLinking: true, persistAuthorization: true }});
</script></body></html>"##
    )
}

/// The ReDoc HTML page, pointed at `spec_url` and titled `title`
/// (CDN-loaded `redoc@2` from jsdelivr).
fn redoc_html(spec_url: &str, title: &str) -> String {
    let title = html_escape(title);
    format!(
        r##"<!doctype html>
<html><head><meta charset="utf-8"><title>{title} · ReDoc</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>body {{ margin: 0; padding: 0; }}</style></head>
<body><div id="redoc-container"></div>
<script src="https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js"></script>
<script>
Redoc.init("{spec_url}", {{ expandResponses: "200,201" }}, document.getElementById("redoc-container"));
</script>
<noscript>ReDoc requires JavaScript to render the API documentation.</noscript></body></html>"##
    )
}

/// Minimal HTML-escaping for the document title interpolated into the UI shells.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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

/// The ReDoc HTML page served at `/redoc` — the Rust counterpart of
/// pyfly's `make_redoc_endpoint`, pointing ReDoc at `/openapi.json`.
const REDOC_PAGE: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><title>API · Firefly · ReDoc</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>body { margin: 0; padding: 0; }</style></head>
<body><div id="redoc-container"></div>
<script src="https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js"></script>
<script>
Redoc.init("/openapi.json", { expandResponses: "200,201" }, document.getElementById("redoc-container"));
</script>
<noscript>ReDoc requires JavaScript to render the API documentation.</noscript></body></html>"##;

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
///
/// Keys are emitted in **sorted order** to match Go's `encoding/json`
/// map encoding byte-for-byte. We iterate via a `BTreeMap` rather than
/// the input `Map` (and rather than relying on `serde_json::Map`'s
/// iteration order) so the output is deterministic regardless of whether
/// the `serde_json/preserve_order` feature is active anywhere in the
/// workspace dependency graph.
fn object_schema(map: &Map<String, Value>) -> Value {
    let sorted: BTreeMap<&String, &Value> = map.iter().collect();
    let mut props = Map::new();
    let mut required: Vec<String> = Vec::new();
    for (key, val) in sorted {
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

/// Merges `incoming` onto `base` field-by-field: every field set on
/// `incoming` (i.e. not its [`RouteDef::default`] value) overwrites
/// `base`; unset fields leave `base` untouched. `method` and `path` are
/// the merge key and are never changed.
fn merge_route(base: &mut RouteDef, incoming: &RouteDef) {
    if !incoming.operation_id.is_empty() {
        base.operation_id = incoming.operation_id.clone();
    }
    if !incoming.summary.is_empty() {
        base.summary = incoming.summary.clone();
    }
    if !incoming.description.is_empty() {
        base.description = incoming.description.clone();
    }
    if !incoming.tags.is_empty() {
        base.tags = incoming.tags.clone();
    }
    if !incoming.parameters.is_empty() {
        base.parameters = incoming.parameters.clone();
    }
    if incoming.request.is_some() {
        base.request = incoming.request.clone();
    }
    if incoming.response.is_some() {
        base.response = incoming.response.clone();
    }
    if incoming.request_schema.is_some() {
        base.request_schema = incoming.request_schema.clone();
    }
    if incoming.response_schema.is_some() {
        base.response_schema = incoming.response_schema.clone();
    }
    if incoming.status != 0 {
        base.status = incoming.status;
    }
    if incoming.deprecated {
        base.deprecated = true;
    }
}

/// Derives an OpenAPI tag from a controller type name, mirroring pyfly's
/// `_derive_tag`: a trailing `Controller` or `Api` suffix is stripped
/// (`OrderApi` → `Order`, `CatalogController` → `Catalog`), and a leading
/// path qualifier (`crate :: OrderApi`, as a Rust type path renders) is
/// dropped so only the final segment remains.
fn derive_tag(controller: &str) -> String {
    // The macro stringifies `Self`, which may render a path or carry
    // spaces around `::`; take the final identifier segment.
    let last = controller.rsplit("::").next().unwrap_or(controller).trim();
    for suffix in ["Controller", "Api"] {
        if let Some(stripped) = last.strip_suffix(suffix) {
            if !stripped.is_empty() {
                return stripped.to_string();
            }
        }
    }
    last.to_string()
}

/// Converts an axum route path to the OpenAPI template form and extracts
/// its path parameters.
///
/// axum captures are `:name` (single segment) and `*name` (catch-all);
/// both become OpenAPI `{name}` segments and a required `in: path`
/// [`Parameter`]. Plain segments pass through unchanged.
fn openapi_path_and_params(path: &str) -> (String, Vec<Parameter>) {
    let mut params = Vec::new();
    let mut segments: Vec<String> = Vec::new();
    for segment in path.split('/') {
        if let Some(name) = segment
            .strip_prefix(':')
            .or_else(|| segment.strip_prefix('*'))
        {
            if !name.is_empty() {
                params.push(Parameter::path(name));
                segments.push(format!("{{{name}}}"));
                continue;
            }
        }
        segments.push(segment.to_string());
    }
    (segments.join("/"), params)
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

    // ----- docs_router: springdoc/pyfly serving + component schemas -----

    #[tokio::test]
    async fn docs_router_serves_springdoc_and_pyfly_paths() {
        let router = || orders_builder().docs_router(&DocsConfig::default());

        let (status, ct, body) = get_response(router(), "/v3/api-docs").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "application/json");
        assert!(body.contains("\"openapi\":\"3.1.0\""));

        // `/openapi.json` is a back-compat alias for the same spec.
        let (status, ct, _) = get_response(router(), "/openapi.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "application/json");

        // Swagger UI at the springdoc path + its `.html` alias, pointed at the spec.
        for path in ["/swagger-ui", "/swagger-ui.html"] {
            let (status, ct, body) = get_response(router(), path).await;
            assert_eq!(status, StatusCode::OK, "path {path}");
            assert!(ct.starts_with("text/html"), "path {path} ct {ct}");
            assert!(body.contains("swagger-ui"), "path {path}");
            assert!(body.contains("/v3/api-docs"), "swagger points at spec");
        }

        // ReDoc at pyfly's path.
        let (status, ct, body) = get_response(router(), "/redoc").await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with("text/html"));
        assert!(body.contains("redoc"));
        assert!(body.contains("/v3/api-docs"), "redoc points at spec");
    }

    #[test]
    fn add_schema_registers_component_and_route_refs_it() {
        let doc = Builder::new(Info {
            title: "T".into(),
            version: "1".into(),
            ..Info::default()
        })
        .add_schema(
            "Foo",
            json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}),
        )
        .add(RouteDef {
            method: "POST".to_string(),
            path: "/foo".to_string(),
            request_schema: Some("Foo".to_string()),
            response_schema: Some("Foo".to_string()),
            ..RouteDef::default()
        })
        .build();

        assert!(doc.components.schemas.contains_key("Foo"));
        let op = doc.paths.get("/foo").unwrap().get("post").unwrap();
        let req = &op.request_body.as_ref().unwrap().content["application/json"].schema;
        assert_eq!(req, &json!({"$ref": "#/components/schemas/Foo"}));
        let resp = op.responses["201"].content.as_ref().unwrap()["application/json"]
            .schema
            .clone();
        assert_eq!(resp, json!({"$ref": "#/components/schemas/Foo"}));
    }

    #[test]
    fn unregistered_schema_name_is_not_reffed() {
        // An inferred body type that is not a `#[derive(Schema)]` component must
        // NOT produce a dangling `$ref`.
        let doc = Builder::new(Info {
            title: "T".into(),
            version: "1".into(),
            ..Info::default()
        })
        .add(RouteDef {
            method: "POST".to_string(),
            path: "/foo".to_string(),
            request_schema: Some("Missing".to_string()),
            response_schema: Some("Missing".to_string()),
            ..RouteDef::default()
        })
        .build();

        assert!(!doc.components.schemas.contains_key("Missing"));
        let op = doc.paths.get("/foo").unwrap().get("post").unwrap();
        assert!(op.request_body.is_none(), "no dangling request $ref");
        // The success response carries no JSON content (no dangling $ref).
        assert!(op.responses["201"].content.is_none());
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
        assert_send_sync::<Parameter>();
        assert_send_sync::<Tag>();
    }

    // ----- Automatic generation from #[rest_controller] route metadata -----

    use firefly_container::RouteDescriptor;

    const ORDER_ROUTES: &[RouteDescriptor] = &[
        RouteDescriptor {
            controller: "OrderApi",
            method: "POST",
            path: "/api/v1/orders",
            handler: "create",
            summary: "",
            description: "",
            tags: &[],
            deprecated: false,
            request_schema: "",
            response_schema: "",
            status: 0,
        },
        RouteDescriptor {
            controller: "OrderApi",
            method: "GET",
            path: "/api/v1/orders/:id",
            handler: "fetch",
            summary: "",
            description: "",
            tags: &[],
            deprecated: false,
            request_schema: "",
            response_schema: "",
            status: 0,
        },
    ];

    #[test]
    fn derive_tag_strips_controller_and_api_suffixes() {
        assert_eq!(derive_tag("OrderApi"), "Order");
        assert_eq!(derive_tag("CatalogController"), "Catalog");
        assert_eq!(derive_tag("Health"), "Health");
        // A Rust type path keeps only the final segment.
        assert_eq!(derive_tag("crate :: api :: OrderApi"), "Order");
        // A bare `Api`/`Controller` (nothing left after stripping) is kept.
        assert_eq!(derive_tag("Api"), "Api");
    }

    #[test]
    fn openapi_path_converts_axum_captures_and_extracts_params() {
        let (path, params) = openapi_path_and_params("/api/v1/orders/:id");
        assert_eq!(path, "/api/v1/orders/{id}");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "id");
        assert_eq!(params[0].location, ParameterIn::Path);
        assert!(params[0].required);
        assert_eq!(params[0].schema, json!({"type": "string"}));

        // Catch-all `*rest` and multiple captures.
        let (path, params) = openapi_path_and_params("/files/:bucket/*rest");
        assert_eq!(path, "/files/{bucket}/{rest}");
        assert_eq!(
            params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            vec!["bucket", "rest"]
        );

        // A path with no captures is unchanged and has no parameters.
        let (path, params) = openapi_path_and_params("/health");
        assert_eq!(path, "/health");
        assert!(params.is_empty());
    }

    #[test]
    fn from_routes_builds_operations_tags_and_parameters() {
        let doc = Builder::new(Info {
            title: "Orders API".to_string(),
            version: "1.0.0".to_string(),
            ..Info::default()
        })
        .from_routes(ORDER_ROUTES)
        .build();

        // POST /api/v1/orders → operationId + tag, no path params.
        let post = &doc.paths["/api/v1/orders"]["post"];
        assert_eq!(post.operation_id, "create");
        assert_eq!(post.tags, vec!["Order".to_string()]);
        assert!(post.parameters.is_empty());
        // POST default success status is 201.
        assert!(post.responses.contains_key("201"));

        // GET /api/v1/orders/{id} → axum :id converted, required path param.
        let item = doc
            .paths
            .get("/api/v1/orders/{id}")
            .expect("templated path");
        let get = &item["get"];
        assert_eq!(get.operation_id, "fetch");
        assert_eq!(get.tags, vec!["Order".to_string()]);
        assert_eq!(get.parameters.len(), 1);
        assert_eq!(get.parameters[0]["name"], "id");
        assert_eq!(get.parameters[0]["in"], "path");
        assert_eq!(get.parameters[0]["required"], json!(true));
        assert!(get.responses.contains_key("200"));

        // Top-level tags array (de-duplicated, discovery order).
        assert_eq!(
            doc.tags,
            vec![Tag {
                name: "Order".to_string(),
                ..Tag::default()
            }]
        );
        // Every operation still carries the RFC 7807 default response.
        assert!(post.responses.contains_key("default"));
        assert!(get.responses.contains_key("default"));
    }

    #[test]
    fn explicit_add_enriches_auto_derived_route_by_merge() {
        let doc = Builder::new(Info::default())
            .from_routes(ORDER_ROUTES)
            // Same method+path as the auto-derived GET: enrich it with a
            // response schema, a summary, and a deprecated flag without
            // re-declaring tag / operationId / parameters.
            .add(RouteDef {
                method: "GET".to_string(),
                path: "/api/v1/orders/{id}".to_string(),
                summary: "Fetch one order".to_string(),
                response: Some(Sample::named("Order", json!({"id": "o-1"}))),
                deprecated: true,
                ..RouteDef::default()
            })
            .build();

        let get = &doc.paths["/api/v1/orders/{id}"]["get"];
        // Auto-derived fields survive the merge.
        assert_eq!(get.operation_id, "fetch");
        assert_eq!(get.tags, vec!["Order".to_string()]);
        assert_eq!(get.parameters.len(), 1);
        // Explicit fields win.
        assert_eq!(get.summary, "Fetch one order");
        assert!(get.deprecated);
        assert_eq!(
            get.responses["200"].content.as_ref().unwrap()["application/json"].schema,
            json!({"$ref": "#/components/schemas/Order"})
        );
        assert!(doc.components.schemas.contains_key("Order"));
        // Exactly one GET operation on the path (merged, not duplicated).
        assert_eq!(doc.paths["/api/v1/orders/{id}"].len(), 1);
    }

    #[test]
    fn deprecated_flag_serializes_and_is_omitted_when_false() {
        let doc = Builder::new(Info::default())
            .add(RouteDef {
                method: "GET".to_string(),
                path: "/old".to_string(),
                deprecated: true,
                ..RouteDef::default()
            })
            .add(RouteDef {
                method: "GET".to_string(),
                path: "/new".to_string(),
                ..RouteDef::default()
            })
            .build();
        let value = serde_json::to_value(&doc).unwrap();
        assert_eq!(value["paths"]["/old"]["get"]["deprecated"], json!(true));
        // A non-deprecated operation omits the key (Go omitempty parity).
        assert!(value["paths"]["/new"]["get"].get("deprecated").is_none());
    }

    #[test]
    fn auto_generated_wire_shape_matches_pyfly_layout() {
        let value = serde_json::to_value(
            Builder::new(Info {
                title: "Orders API".to_string(),
                version: "1.0.0".to_string(),
                ..Info::default()
            })
            .from_routes(ORDER_ROUTES)
            .build(),
        )
        .unwrap();
        // pyfly emits operationId + tags + parameters in the operation.
        let get = &value["paths"]["/api/v1/orders/{id}"]["get"];
        assert_eq!(get["operationId"], "fetch");
        assert_eq!(get["tags"], json!(["Order"]));
        assert_eq!(
            get["parameters"],
            json!([{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}])
        );
        // Top-level tags array, like pyfly's spec["tags"].
        assert_eq!(value["tags"], json!([{"name": "Order"}]));
    }

    #[tokio::test]
    async fn handler_serves_redoc() {
        let (status, ct, body) = get_response(orders_builder().router(), "/redoc").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "text/html; charset=utf-8");
        assert!(body.starts_with("<!doctype html>"));
        assert!(body.contains("redoc.standalone.js"));
        assert!(body.contains(r#"Redoc.init("/openapi.json""#));
        assert!(body.contains("ReDoc requires JavaScript"));
    }

    #[test]
    fn empty_builder_omits_top_level_tags() {
        // No routes → no tags array (omitempty parity); existing wire
        // format unchanged.
        let body = Builder::new(Info {
            title: "T".to_string(),
            version: "1".to_string(),
            ..Info::default()
        })
        .json();
        assert!(!body.contains("\"tags\""));
    }
}
