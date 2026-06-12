# `firefly-openapi`

> **Tier:** Platform · **Status:** Full · **Java original:** `springdoc-openapi` · **Go module:** `openapi`

## Overview

`firefly-openapi` generates an **OpenAPI 3.1** document from registered
`RouteDef` descriptors plus JSON sample values of the types they
consume / return. The generator walks `serde_json::Value` samples
(RFC 3339 strings mapping to `string format=date-time`, the wire shape
of Go's `time.Time` and chrono's `DateTime<Utc>`), registers schemas
under `#/components/schemas/{TypeName}`, and serves the result at
`/openapi.json` with a Swagger-UI shim at `/openapi/ui`.

The generator is deliberately small — it has no annotation framework,
no DI, no codegen step. You hand-register routes and the JSON samples
do the rest. Where the Go port inspected struct types via reflection,
this port inspects a serialized *sample* of the type (`Sample::of`
accepts any `T: Serialize`; `Sample::named` accepts a
`serde_json::json!` literal) — Rust has no runtime reflection, and the
serialized shape carries the same field names the reflection walk
produced.

## Public surface

```rust,ignore
pub struct Info { pub title: String, pub version: String, pub description: String }
pub struct Server { pub url: String, pub description: String }

pub struct Sample { pub name: Option<String>, pub value: serde_json::Value }
impl Sample {
    pub fn named(name: impl Into<String>, value: Value) -> Self;       // $ref-registered
    pub fn inline(value: Value) -> Self;                               // always inlined
    pub fn of<T: Serialize>(name: impl Into<String>, value: &T) -> Result<Self, OpenApiError>;
    pub fn of_inline<T: Serialize>(value: &T) -> Result<Self, OpenApiError>;
}

pub struct RouteDef {
    pub method: String, pub path: String,
    pub summary: String, pub description: String,
    pub tags: Vec<String>,
    pub request: Option<Sample>,   // sample of the body type, or None
    pub response: Option<Sample>,  // sample of the success response, or None
    pub status: u16,               // success status code; 0 defaults to 200/201
}

pub struct Builder { pub info: Info, pub servers: Vec<Server>, /* routes */ }
impl Builder {
    pub fn new(info: Info) -> Self;
    pub fn add_server(self, s: Server) -> Self;
    pub fn add(self, r: RouteDef) -> Self;
    pub fn build(&self) -> Document;
    pub fn json(&self) -> String;          // the exact /openapi.json bytes
    pub fn router(&self) -> axum::Router;  // serves /openapi.json + /openapi/ui
}

pub struct Document { /* serializable OAS 3.1 root */ }
pub struct Operation { /* ... */ }
pub enum OpenApiError { Serialize(serde_json::Error) }
```

## Quick start

```rust
use firefly_openapi::{Builder, Info, RouteDef, Sample, Server};
use serde::Serialize;

#[derive(Serialize)]
struct PlaceOrderRequest {
    customer: String,
    quantity: i64,
}

#[derive(Serialize)]
struct Order {
    id: String,
    customer: String,
    quantity: i64,
}

let sample_order = Order { id: "o-1".into(), customer: "acme".into(), quantity: 2 };

let builder = Builder::new(Info {
    title: "Orders API".into(),
    version: "1.0.0".into(),
    ..Info::default()
})
.add_server(Server { url: "https://api.example.com".into(), ..Server::default() })
.add(RouteDef {
    method: "POST".into(),
    path: "/api/v1/orders".into(),
    summary: "Place an order".into(),
    tags: vec!["orders".into()],
    request: Some(
        Sample::of("PlaceOrderRequest", &PlaceOrderRequest { customer: "acme".into(), quantity: 2 })
            .unwrap(),
    ),
    response: Some(Sample::of("Order", &sample_order).unwrap()),
    ..RouteDef::default()
});

let doc = builder.build();
assert_eq!(doc.openapi, "3.1.0");

let app: axum::Router = builder.router(); // serves /openapi.json + /openapi/ui
```

## Schema mapping

| JSON sample shape           | OpenAPI shape                                                    |
|-----------------------------|------------------------------------------------------------------|
| string                      | `{"type":"string"}`                                              |
| RFC 3339 string             | `{"type":"string","format":"date-time"}` (Go's `time.Time`)      |
| bool                        | `{"type":"boolean"}`                                             |
| integer                     | `{"type":"integer","format":"int64"}`                            |
| float                       | `{"type":"number"}`                                              |
| array                       | `{"type":"array","items":...}` (from the first element)          |
| named object (`Sample::named`) | `$ref: #/components/schemas/{TypeName}`                       |
| anonymous / nested object   | inline `{"type":"object","properties":...,"required":[...]}`     |
| null                        | `{"type":"object"}` (optional: excluded from `required`)         |

The default error response (`default`) uses
`#/components/schemas/ProblemDetail` so every operation surfaces
RFC 7807 errors uniformly.

### Differences from the Go reflection walk

Sample values carry less information than Go's `reflect.Type`, so three
edge cases adapt:

- **Empty arrays** fall back to `items: {"type":"object"}` (Go derived
  the element type); prefer one-element samples.
- **Nested structs** inline instead of registering their own component
  (samples carry no nested type names); name the top-level type via
  `Sample::named` / `Sample::of`.
- **Required fields** are every non-null key present in the sample
  (Go used the absence of `omitempty`); model optional fields as `null`
  or omit them from the sample.

## Wire compatibility

`Builder::json()` (and the `/openapi.json` route) emits exactly what
the Go handler emits: compact JSON, struct keys in Go declaration
order, map keys sorted, empty optionals omitted (`omitempty`), plus
the trailing newline of Go's `json.Encoder`. The Swagger-UI page at
`/openapi/ui` is byte-for-byte the Go page
(`text/html; charset=utf-8`).

## Testing

```bash
cargo test -p firefly-openapi
```

Covers operation registration, schema generation for primitives /
arrays / objects / timestamps, the `/openapi.json` and `/openapi/ui`
handlers (in-process via `tower::ServiceExt::oneshot`), the canonical
ProblemDetail error response, Go wire-format omission rules, and serde
round-trips.
