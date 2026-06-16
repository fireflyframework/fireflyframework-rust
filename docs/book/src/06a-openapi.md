# OpenAPI, Swagger UI & ReDoc

> By the end of this chapter you will know how Lumen serves a complete OpenAPI
> 3.1 document, Swagger UI, and ReDoc with **zero application code**: the spec is
> generated from the live inventory — every `#[rest_controller]` route plus
> every `#[derive(Schema)]` DTO — and `FireflyApplication` mounts the doc
> endpoints during boot. You will learn `#[derive(Schema)]` (and how it honours
> serde renaming), how request/response models are *inferred* from your handler
> signatures, the per-operation metadata attributes, and how to override the
> inference when you need to.

A `#[rest_controller]` already emits a route descriptor per endpoint (you met
this in [chapter 6](./06-first-http-api.md), and it feeds
`/admin/api/mappings`). The OpenAPI generator reuses that same descriptor table.
The Rust analog of springdoc-openapi, it builds the spec from what is already
compiled in — there is no annotation framework to learn beyond the attributes
you write for routing, and no codegen step.

## Served, with no app code

During step 10 of the [boot pipeline](./04b-bootstrap.md), `FireflyApplication`
builds the document from the inventory and mounts it on the **management**
router — beside the actuator and the admin dashboard, on the management port —
**not** the public API. Swagger UI, ReDoc, and the spec expose the entire API
surface and every schema, so they belong on the control-plane surface (where
operators already reach `/actuator/*` and `/admin/`), keeping the public
data-plane port free of API-introspection endpoints. The served paths (on the
management port) default to:

| Path | Serves |
|------|--------|
| `/v3/api-docs` | the OpenAPI 3.1 JSON spec (Spring Boot's springdoc path) |
| `/openapi.json` | the same spec (a back-compat alias) |
| `/swagger-ui` and `/swagger-ui.html` | Swagger UI, pointed at the spec |
| `/redoc` | ReDoc, pointed at the spec |

`serve` even prints the URLs on boot:

```text
:: api docs (management) :: swagger-ui http://0.0.0.0:8081/swagger-ui | redoc http://0.0.0.0:8081/redoc | spec http://0.0.0.0:8081/v3/api-docs
```

Internally this is
`firefly_openapi::Builder::new(info).add_server(api_url).from_inventory()` —
`from_inventory()` reads `firefly_container::routes()` (every controller route)
and `firefly_container::schemas()` (every `#[derive(Schema)]` DTO) — rendered by
`docs_router(&DocsConfig::default())`. The defaults above come from
`DocsConfig`; you would only touch them to relocate the doc endpoints. Lumen
touches nothing: the document just appears.

Because the docs are served on the management port but the API answers on the
public port, the document declares the **API base URL** as its OpenAPI `server`
— so Swagger UI's *Try it out* and ReDoc's samples target the API (`:8080`),
not the management origin they were loaded from. `FireflyApplication` derives it
from the API bind address (a wildcard host falls back to `localhost`);
`FIREFLY_OPENAPI_SERVER_URL` overrides it for a public URL behind a reverse
proxy. An unknown path on **either** listener answers the same RFC 9457
`application/problem+json` 404.

## `#[derive(Schema)]` — component schemas

A DTO becomes a reusable `#/components/schemas/{Type}` by deriving `Schema`.
Because Rust has no runtime reflection, the JSON Schema is computed **at macro-
expansion time** by walking the struct's fields. Lumen's wallet view:

```rust,ignore
// src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    pub id: String,
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

expands to a registered schema (roughly):

```json
{ "type": "object",
  "properties": {
    "id":      {"type": "string"},
    "owner":   {"type": "string"},
    "balance": {"type": "integer"},
    "version": {"type": "integer"}
  },
  "required": ["id", "owner", "balance", "version"] }
```

The field-type mapping mirrors what a Java/Spring `@Schema` model produces:

- `String`/`str`/`char` → `string`; `bool` → `boolean`; every integer type →
  `integer`; `f32`/`f64` → `number`.
- `Uuid` → `string`/`format: uuid`; chrono / time date-times → `string`/`format:
  date-time`; dates → `format: date`; times → `format: time`.
- `Option<T>` is a transparent wrapper: it describes `T` but makes the property
  **non-required** (so optionals drop out of the `required` list).
- `Box`/`Arc`/`Rc<T>` are transparent too; `Vec`/`HashSet`/`BTreeSet`/… → an
  `array` of the element schema; `HashMap`/`BTreeMap` → an open `object` with
  `additionalProperties`.
- Any *other* named type is assumed to be a sibling DTO that also derives
  `Schema`, and is emitted as a `$ref` — so a nested DTO is **linked**, not
  inlined, and the two component schemas compose.

### Serde renaming is honoured

`#[derive(Schema)]` reads the struct's serde directives so the property names in
the schema match the JSON wire shape — `rename`, `rename_all`, and `skip`.
Lumen's `TransferResult` carries field renames, and the schema follows them:

```rust,ignore
// src/transfer.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
pub struct TransferResult {
    pub status: String,
    pub from: String,
    pub to: String,
    pub amount: i64,
    #[serde(rename = "stepsExecuted")]
    pub steps_executed: Vec<String>,
    #[serde(rename = "stepsRolledBack")]
    pub steps_rolled_back: Vec<String>,
}
```

The schema names the array properties `stepsExecuted` / `stepsRolledBack` — the
exact JSON the handler serializes — not the snake_case Rust idents. A
struct-level `#[serde(rename_all = "camelCase")]` is applied to every field the
same way, and a `#[serde(skip)]` field is omitted from the schema entirely.

### Field-less enums → string enumerations

A field-less (unit-variant) enum that derives `Schema` emits a JSON Schema
`string` enumeration, the springdoc treatment of a Java `enum`. Serde renaming
is honoured, so the allowed values match the wire shape:

```rust,ignore
#[derive(Serialize, Deserialize, Schema)]
#[serde(rename_all = "lowercase")]
pub enum WalletStatus { Active, Frozen, Closed }
```

```json
"WalletStatus": { "type": "string", "enum": ["active", "frozen", "closed"] }
```

A DTO field of this type then `$ref`s the registered enum component rather than
becoming an untyped string — the layered `lumen-ledger` sample uses exactly this
for `WalletResponse.status`.

## Request/response inference from the handler signature

You do **not** name request and response models on the attribute — the macro
infers them from the handler's own signature:

- the **request body** is the inner type of the first `Json<T>` *or* `Valid<T>`
  parameter (so the validating extractor documents its body too), and
- the **response** is the `Json<T>` found inside the return type, after
  unwrapping `WebResult<…>` / `Result<…>` and looking through a
  `(StatusCode, Json<T>)` tuple.

### Parameters: path, query, header

The same signature-driven inference covers operation **parameters**, so Swagger
UI / ReDoc render an input for each — no hand-written parameter list:

- **Path** parameters come from the route template: every `:id` / `{id}` segment
  becomes a required `in: path` parameter.
- **Query** parameters come from a `Query<T>` / `ValidQuery<T>` extractor — the
  generator expands `T`'s `#[derive(Schema)]` fields into one `in: query`
  parameter each (required iff the field is non-optional). A `PageRequest`
  argument adds the standard `page` / `size` / `sort` query parameters.
- **Header** parameters are declared on the verb attribute:
  `#[post("/wallets", header("Idempotency-Key", required, description = "…"))]`
  emits an `in: header` parameter (and the handler reads it like any axum
  header). `query("…")` declares an extra query parameter the same way.

So Lumen's `GET /wallets/page` shows `status` (from its `Query<StatusQuery>`)
plus `page`/`size`/`sort` (from `PageRequest`), and `POST /wallets` shows its
`CreateWalletRequest` body plus the `Idempotency-Key` header — all in Swagger UI,
with zero parameter boilerplate.

Take Lumen's `open` handler:

```rust,ignore
// src/web.rs
#[post(
    "/wallets",
    summary = "Open a wallet",
    description = "Opens a new wallet for an owner with an optional opening balance.",
    status = 201
)]
async fn open(
    State(api): State<WalletApi>,
    Json(body): Json<OpenWallet>,
) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
    let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
    Ok((axum::http::StatusCode::CREATED, Json(view)))
}
```

From the signature alone the macro records `OpenWallet` as the request schema
(the `Json<OpenWallet>` parameter) and `WalletView` as the response schema (the
`Json<WalletView>` inside the `(StatusCode, …)` tuple inside `WebResult<…>`).
Both are `#[derive(Schema)]` types, so the operation `$ref`s
`#/components/schemas/OpenWallet` and `#/components/schemas/WalletView`.

> **No dangling `$ref`s.** A `$ref` is emitted **only** when the inferred type
> is actually a registered `#[derive(Schema)]` component. Lumen's
> `transfer_compliance` returns `Json<serde_json::Value>`; `serde_json::Value`
> is not a registered schema, so the generator emits no request/response `$ref`
> for it rather than referencing a component that does not exist. The document
> stays valid no matter what your handlers return.

## Per-operation metadata

Beyond the path, each verb attribute takes optional metadata that lands on the
OpenAPI operation. The full form is
`#[get("/x", summary = "…", description = "…", tags = ["A", "B"], status = 200, deprecated, request = T, response = T)]`:

| Argument | Effect on the operation |
|----------|-------------------------|
| `summary = "…"` | the one-line summary |
| `description = "…"` | the longer description |
| `tags = ["A", "B"]` | grouping tags (override the controller tag, below) |
| `status = 201` | the success status code (defaults to 201 for `POST`, else 200) |
| `deprecated` | marks the operation `deprecated: true` (bare flag; `deprecated = false` to unset) |
| `request = T` | the request body schema name — overrides inference |
| `response = T` | the success response schema name — overrides inference |

Lumen's `transfer` operation uses summary, description, an explicit `tags`, and
a `status`:

```rust,ignore
// src/web.rs
#[post(
    "/transfers",
    summary = "Transfer funds (saga)",
    description = "Moves funds between two wallets as a compensating saga (debit then credit).",
    tags = ["Transfers"],
    status = 200
)]
async fn transfer(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<TransferResult>> { /* … */ }
```

### Controller-level tags

`#[rest_controller(tag = "…")]` sets a default tag for **every** operation on
the controller — Spring's `@Tag(name = …)`. Lumen tags its whole wallet surface:

```rust,ignore
// src/web.rs
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi { /* … */ }
```

Tag resolution per operation is: an explicit per-method `tags = [...]` wins;
otherwise the `#[rest_controller(tag)]` applies; otherwise the generator derives
a tag from the controller type name by stripping a trailing `Api` / `Controller`
suffix (`WalletApi` → `Wallet`, `CatalogController` → `Catalog`). Lumen sets the
controller tag explicitly to `"Wallets"`, so in its spec `open`, `get`,
`deposit`, and `withdraw` carry the **Wallets** tag (the controller default),
while `transfer`, `transfer_compliance`, and `transfer_2pc` carry **Transfers**
(their per-method `tags = ["Transfers"]` override).

### Overriding inference with `request = ` / `response = `

When a body type cannot be read from the signature — a handler that takes a raw
`axum::body::Bytes`, returns an `impl IntoResponse`, or otherwise hides its DTO
— name it explicitly. `request = T` / `response = T` take the schema name (the
type's last path segment, matching what `#[derive(Schema)]` registers it under)
and **take precedence** over inference:

```rust,ignore
#[post("/import", summary = "Bulk import", request = ImportBatch, response = ImportReport)]
async fn import(/* a non-Json body */) -> impl axum::response::IntoResponse { /* … */ }
```

Lumen never needs this — every handler's body is a `Json<T>` of a
`#[derive(Schema)]` type, so the inference covers it — but the escape hatch is
there for the cases the signature cannot express.

## The worked example, end to end

Putting it together for Lumen's `WalletApi`:

- The controller is `#[rest_controller(path = "/api/v1", tag = "Wallets")]`.
- The DTOs — `OpenWallet`, `WalletView`, `AmountBody`, `TransferRequest`,
  `TransferResult`, `TccTransferResult` — each `#[derive(Schema)]`, so they
  become `#/components/schemas/*` and are `$ref`ed by the operations.
- Each operation's request/response is inferred from its `Json<T>`
  parameter/return, its summary/description/tags/status come from the verb
  attribute, and the `transfers/*` operations group under **Transfers**.
- `transfer_compliance` takes `Json<TransferRequest>` (a registered schema, so
  the request `$ref`s `TransferRequest`) but returns `Json<serde_json::Value>`,
  so its response carries **no** `$ref` — and that is correct, not a gap.

Every operation also gets a `default` RFC 9457 response referencing
`#/components/schemas/ProblemDetail` (always added to the document), so the
uniform error shape from [chapter 6](./06-first-http-api.md) is documented for
every endpoint automatically.

## Consistency: one source of truth

The `#[rest_controller]` descriptor table is read by **three** surfaces, so they
can never drift:

- the **OpenAPI document** at `/v3/api-docs`,
- the admin dashboard's **`/admin/api/mappings`** route table
  ([chapter 15](./15-observability.md)), and
- the **startup report**'s `:: routes (N) ::` block
  ([the bootstrap chapter](./04b-bootstrap.md)).

Add a route, and all three update from the same registry on the next build. The
startup report even prints the operation + component-schema counts
(`:: openapi :: N operations | K component schemas (served at /v3/api-docs) ::`)
so you can confirm the spec is live.

## Exporting the spec with the CLI

The `firefly` CLI can write an OpenAPI document for tooling / CI:

```bash
firefly openapi                           # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
```

A note on scope (covered in [chapter 19](./19-cli.md)): a *compiled* binary
cannot boot an arbitrary app to enumerate its live routes, so the CLI emits a
metadata-stamped **skeleton** (the `info` block, the `ProblemDetail` component,
empty `paths`) read from `firefly.yaml` / `Cargo.toml`. To capture Lumen's
**real** routes, run the service and fetch `/v3/api-docs` — that document, built
by `from_inventory()`, *is* the live spec:

```bash
cargo run --bin lumen &
curl -s http://localhost:8081/v3/api-docs | jq .
```

## Generating a client from the spec — `firefly openapi-client`

The inverse direction: given an OpenAPI document, generate a typed Rust client
over [`firefly_client::RestClient`] — the Rust analog of springdoc's
OpenAPI-generated WebClient SDK.

```bash
# capture the live spec, then generate a client from it
curl -s http://localhost:8081/v3/api-docs -o wallet-openapi.json
firefly openapi-client --spec wallet-openapi.json -o src/generated.rs --client-name WalletClient
```

It emits a model `struct` per object schema (and an `enum` per string
enumeration), with serde renames and optional fields preserved, plus one
`async fn` per operation — typed path/query parameters, a JSON request body, and
the success-response type — calling `RestClient::request`. The generated client
is the same shape you would hand-write (see the `lumen-ledger` `-sdk` crate and
[chapter 22](./22-layered-microservices.md)).

## What changed in Lumen

Nothing in `samples/lumen` — the controller already carried its tags, summaries,
and `#[derive(Schema)]` DTOs for routing's sake, and `FireflyApplication`
already serves the docs. This chapter is where you see that those same
declarations *are* the API documentation: write the controller, get Swagger UI,
ReDoc, and a valid OpenAPI 3.1 spec for free. Next,
[Persistence & Reactive Repositories](./07-persistence.md) builds the read model
behind the `WalletView` these docs describe.
