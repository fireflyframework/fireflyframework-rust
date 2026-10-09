# OpenAPI, Swagger UI & ReDoc

In [Your First HTTP API](./06-first-http-api.md) you gave Lumen its first real
endpoints — a `#[rest_controller]` whose `#[post]` / `#[get]` methods mount
themselves at boot. This chapter shows what those same declarations *also* bought
you, for free: a complete, live **OpenAPI 3.1** document, a **Swagger UI** page,
and a **ReDoc** page, all served without one extra line of application code. The
spec is generated from the live inventory the framework already discovered — every
controller route plus every `#[derive(Schema)]` DTO — and `FireflyApplication`
mounts the doc endpoints during boot.

Nothing in this chapter changes a single line of `samples/lumen`. The controller
you wrote already carries the summaries, tags, and `#[derive(Schema)]` DTOs the
generator reads. The point here is to *see* that those routing declarations **are**
the API documentation, and to learn how to enrich, override, and export the spec
when you need to.

By the end of this chapter you will:

- Reach Lumen's three documentation surfaces — the OpenAPI spec, Swagger UI, and
  ReDoc — and explain why they live on the management port, not the public one.
- Derive a reusable component schema from a DTO with `#[derive(Schema)]`, and read
  how it honours serde renaming, optionals, enums, and nested types.
- Trace how the request body, the response body, and the path/query/header
  parameters of an operation are *inferred* from a handler's own signature.
- Attach per-operation metadata (summary, description, tags, status, `deprecated`)
  and override the inference with `request = ` / `response = ` when a signature
  can't express the DTO.
- Export the spec with the `firefly` CLI and generate a typed Rust client from it.

## Concepts you will meet

Before the first endpoint, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — OpenAPI.** *OpenAPI* (formerly Swagger) is a
> language-neutral, machine-readable description of a REST API — every path,
> operation, parameter, request body, response, and reusable schema, as one JSON
> (or YAML) document. Tooling reads it to render docs, generate clients, and run
> contract tests. Firefly emits **OpenAPI 3.1**.

> **Note** **Key term — component schema.** A *component schema* is a named,
> reusable JSON Schema for one data type, registered under
> `#/components/schemas/{Type}` and referenced from operations by a `$ref`. The
> Java/Spring analog is a model annotated `@Schema`; in Firefly you opt a type in
> with `#[derive(Schema)]`.

> **Note** **Key term — Swagger UI / ReDoc.** Both are browser apps that render an
> OpenAPI document into human-readable, interactive documentation. *Swagger UI* has
> a "Try it out" panel that fires live requests; *ReDoc* is a clean three-pane
> reference. Firefly serves both, each pointed at the same spec.

> **Note** **Key term — the inventory.** Firefly's macros emit compile-time
> descriptors into an `inventory` registry — one `RouteDescriptor` per
> `#[rest_controller]` method and one `SchemaDescriptor` per `#[derive(Schema)]`
> type. The OpenAPI generator reads that registry rather than re-parsing your
> source. This is how a Rust framework gets springdoc-style "scan the application"
> behaviour without runtime reflection.

## Step 1 — Reach the three documentation surfaces

You do not write or register anything to get API docs. Boot Lumen exactly as in
the [Quickstart](./02-quickstart.md):

```bash
cargo run
```

Among the startup lines, the framework prints the documentation URLs:

```text
:: api docs (management) :: swagger-ui http://0.0.0.0:8081/swagger-ui | redoc http://0.0.0.0:8081/redoc | spec http://0.0.0.0:8081/v3/api-docs
```

Open each in a browser (or `curl` the spec). The endpoints, on the **management**
port, default to:

| Path | Serves |
|------|--------|
| `/v3/api-docs` | the OpenAPI 3.1 JSON spec (Spring Boot's springdoc path) |
| `/openapi.json` | the same spec (a back-compat alias) |
| `/swagger-ui` and `/swagger-ui.html` | Swagger UI, pointed at the spec |
| `/redoc` | ReDoc, pointed at the spec |

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 310" role="img"
     aria-label="OpenAPI generation: rest_controller routes and derive Schema DTOs are harvested into one openapi.json spec served at /v3/api-docs, which Swagger UI and ReDoc render"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="24.0" y="42.5" width="200.0" height="50.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="24.0" y="40.0" width="200.0" height="50.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="124.0" y="62.0" text-anchor="middle" font-size="13" font-weight="700" fill="#10110f" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[rest_controller]</text><text x="124.0" y="76.0" text-anchor="middle" font-size="10.0" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">routes + status codes</text>
<rect x="24.0" y="152.5" width="200.0" height="50.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="24.0" y="150.0" width="200.0" height="50.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="124.0" y="172.0" text-anchor="middle" font-size="13" font-weight="700" fill="#10110f" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[derive(Schema)]</text><text x="124.0" y="186.0" text-anchor="middle" font-size="10.0" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">DTO component schemas</text>
<line x1="224.0" y1="65.0" x2="310.8" y2="106.5" stroke="#ffb34a" stroke-width="3.0" stroke-linecap="round"/><polygon points="318.0,110.0 308.8,110.6 312.7,102.5" fill="#855414"/>
<line x1="224.0" y1="175.0" x2="310.8" y2="133.5" stroke="#ffb34a" stroke-width="3.0" stroke-linecap="round"/><polygon points="318.0,130.0 312.7,137.5 308.8,129.4" fill="#855414"/>
<rect x="320" y="80" width="120" height="92" rx="9" fill="#bfb8ab" opacity="0.22"/>
<rect x="320" y="78" width="120" height="92" rx="9" fill="#fff1dd" stroke="#855414" stroke-width="1.5"/>
<rect x="338" y="94" width="84" height="9" rx="4.5" fill="#ffb34a"/>
<rect x="338" y="114" width="84" height="6" rx="3" fill="#62645b" opacity="0.6"/>
<rect x="338" y="127" width="72" height="6" rx="3" fill="#62645b" opacity="0.6"/>
<rect x="338" y="140" width="60" height="6" rx="3" fill="#62645b" opacity="0.6"/>
<rect x="338" y="153" width="48" height="6" rx="3" fill="#62645b" opacity="0.6"/>
<text x="380.0" y="190.0" text-anchor="middle" font-size="11" font-weight="700" fill="#855414" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">openapi.json</text>
<text x="380.0" y="206.0" text-anchor="middle" font-size="10" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">(/v3/api-docs)</text>
<line x1="440.0" y1="110.0" x2="476.6" y2="95.0" stroke="#ffb34a" stroke-width="3.0" stroke-linecap="round"/><polygon points="484.0,92.0 478.3,99.2 474.9,90.9" fill="#855414"/>
<line x1="440.0" y1="140.0" x2="476.6" y2="155.0" stroke="#ffb34a" stroke-width="3.0" stroke-linecap="round"/><polygon points="484.0,158.0 474.9,159.1 478.3,150.8" fill="#855414"/>
<g transform="translate(486.0,80.0)"><rect x="0" y="0" width="88.0" height="26.0" rx="13.0" fill="#ffb34a" opacity="0.95"/><text x="44.0" y="17.2" text-anchor="middle" font-size="12" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Swagger UI</text></g>
<g transform="translate(486.0,146.0)"><rect x="0" y="0" width="53.0" height="26.0" rx="13.0" fill="#ffb34a" opacity="0.95"/><text x="26.5" y="17.2" text-anchor="middle" font-size="12" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ReDoc</text></g>
<svg x="520.0" y="254.0" width="72" height="52" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 335.52 242"><title>rsfly - Rust framework by Firefly</title><g fill="#10110f" transform="translate(20 130) scale(0.07 -0.07)"><g transform="translate(0 0)"><path d="M140 0L140 1080L299 1080L299 908Q310 927 323 945Q353 987 391 1014Q433 1048 486 1066Q539 1084 594 1087.5Q649 1091 697 1080L697 913Q641 928 574 921Q507 914 450 872Q398 835 369.5 782Q341 729 330 667.5Q319 606 319 542L319 0Z"/></g><g transform="translate(707 0)"><path d="M548 -29Q355 -29 231.5 55.5Q108 140 80 292L262 321Q284 233 363 181.5Q442 130 559 130Q670 130 733 176Q796 222 796 301Q796 347 775 375.5Q754 404 688.5 429Q623 454 491 489Q348 527 266 568Q184 609 149 663Q114 717 114 794Q114 889 166 960.5Q218 1032 311.5 1071Q405 1110 529 1110Q652 1110 749 1071Q846 1032 905.5 960.5Q965 889 976 794L794 761Q781 848 710.5 898Q640 948 529 951Q424 955 358.5 914Q293 873 293 803Q293 764 317.5 736Q342 708 408.5 683Q475 658 602 626Q745 590 827.5 547.5Q910 505 945 447.5Q980 390 980 306Q980 150 865 60.5Q750 -29 548 -29Z"/></g><g transform="translate(1768 0)"><path d="M243 0L243 933L60 933L60 1080L243 1080L243 1139Q243 1181 247 1224.5Q251 1268 265 1309.5Q279 1351 311 1386Q348 1427 392 1444.5Q436 1462 481.5 1466Q527 1470 568 1470L705 1470L705 1322L578 1322Q499 1322 460 1283.5Q421 1245 421 1167L421 1080L954 1080L954 1470L1132 1470L1132 0L954 0L954 933L421 933L421 0Z"/></g><g transform="translate(3060 0)"><g data-family-terminal="trail-y"><defs><linearGradient id="06a-openapi-rsfly-family-rsfly-20-130-140" gradientUnits="userSpaceOnUse" x1="30" y1="1080" x2="554" y2="230"><stop offset="0" stop-color="#ffb34a" stop-opacity="0.2"/><stop offset="0.8" stop-color="#ffb34a" stop-opacity="1"/></linearGradient></defs><path d="M290 -480L468 4L30 1080L217 1080L554 230L870 1080L1051 1080L463 -480Z" fill="url(#06a-openapi-rsfly-family-rsfly-20-130-140)"/></g></g></g><g fill="#62645b" transform="translate(25 210) scale(0.0155 -0.0155)"><path d="M625 -30Q476 -30 373 46Q314 89 271 150L271 0L139 0L139 1440L287 1440L287 953Q325 1001 375 1036Q479 1110 631 1110Q779 1110 885 1035Q991 960 1047.5 831Q1104 702 1104 541Q1104 379 1047 250Q990 121 883 45.5Q776 -30 625 -30ZM610 107Q723 107 798 164Q873 221 910.5 319.5Q948 418 948 541Q948 663 911 761Q874 859 798.5 916Q723 973 607 973Q494 973 419.5 918Q345 863 308 765.5Q271 668 271 541Q271 416 308 317.5Q345 219 420 163Q495 107 610 107Z" transform="translate(0 0)"/><path d="M294 -480L472 3L33 1080L189 1080L544 192L871 1080L1021 1080L441 -480Z" transform="translate(1131 0)"/></g><svg x="76" y="183" width="143" height="45.48" xmlns="http://www.w3.org/2000/svg" viewBox="5 -148 619.4 197"><defs><linearGradient id="06a-openapi-rsfly-firefly-trail" gradientUnits="userSpaceOnUse" x1="567.4" y1="-108" x2="623.4" y2="-54"><stop offset="0" stop-color="#ffb34a" stop-opacity="0.2"/><stop offset="0.8" stop-color="#ffb34a" stop-opacity="1"/></linearGradient></defs><path d="M24.3 0L24.3 -93.3L6 -93.3L6 -108L24.3 -108L24.3 -113.9Q24.3 -118.1 24.7 -122.45Q25.1 -126.8 26.5 -130.95Q27.9 -135.1 31.1 -138.6Q34.8 -142.7 39.2 -144.45Q43.6 -146.2 48.15 -146.6Q52.7 -147 56.8 -147L70.5 -147L70.5 -132.2L57.8 -132.2Q49.9 -132.2 46 -128.35Q42.1 -124.5 42.1 -116.7L42.1 -108L70.5 -108L70.5 -93.3L42.1 -93.3L42.1 0ZM91.5 0L91.5 -108L109.3 -108L109.3 0ZM134.8 0L134.8 -108L150.7 -108L150.7 -90.8Q151.8 -92.7 153.1 -94.5Q156.1 -98.7 159.9 -101.4Q164.1 -104.8 169.4 -106.6Q174.7 -108.4 180.2 -108.75Q185.7 -109.1 190.5 -108L190.5 -91.3Q184.9 -92.8 178.2 -92.1Q171.5 -91.4 165.8 -87.2Q160.6 -83.5 157.75 -78.2Q154.9 -72.9 153.8 -66.75Q152.7 -60.6 152.7 -54.2L152.7 0ZM247.3 3Q231.4 3 219.55 -4.05Q207.7 -11.1 201.1 -23.75Q194.5 -36.4 194.5 -53.3Q194.5 -71 201 -83.95Q207.5 -96.9 219.15 -103.95Q230.8 -111 246.5 -111Q262.8 -111 274.25 -103.5Q285.7 -96 291.45 -82.1Q297.2 -68.2 296.4 -49L213.6 -49Q214.6 -33.5 222 -24.4Q230.6 -13.9 246.5 -13.9Q257.2 -13.9 265.1 -18.8Q273 -23.7 277.5 -32.9L294.6 -27Q288.3 -12.7 275.65 -4.85Q263 3 247.3 3ZM277.9 -63.3Q276.5 -77.4 270.3 -85.2Q262.5 -95 247.1 -95Q230.6 -95 222 -84.4Q215.5 -76.4 213.9 -63.3ZM324.4 0L324.4 -93.3L306.1 -93.3L306.1 -108L324.4 -108L324.4 -113.9Q324.4 -118.1 324.8 -122.45Q325.2 -126.8 326.6 -130.95Q328 -135.1 331.2 -138.6Q334.9 -142.7 339.3 -144.45Q343.7 -146.2 348.25 -146.6Q352.8 -147 356.9 -147L370.6 -147L370.6 -132.2L357.9 -132.2Q350 -132.2 346.1 -128.35Q342.2 -124.5 342.2 -116.7L342.2 -108L395.5 -108L395.5 -147L413.3 -147L413.3 0L395.5 0L395.5 -93.3L342.2 -93.3L342.2 0ZM452.8 48L470.6 -0.4L426.8 -108L445.5 -108L479.2 -23L510.8 -108L528.9 -108L470.1 48Z" fill="#10110f"/><circle cx="100.4" cy="-136.3" r="9.97" fill="#ffb34a"/><path d="M567.4 -108L623.4 -54L603.4 -54L547.4 -108Z" fill="url(#06a-openapi-rsfly-firefly-trail)"/><path d="M623.4 -54L567.4 0L547.4 0L603.4 -54Z" fill="#ffb34a"/></svg></svg></svg>
<figcaption>No codegen step, no annotation framework. At boot <code>FireflyApplication</code> harvests the routing attributes and every <code>#[derive(Schema)]</code> type into one OpenAPI&nbsp;3.1 document (served at <code>/v3/api-docs</code> on the management port) and points Swagger&nbsp;UI and ReDoc at it.</figcaption>
</figure>

What just happened: during the boot pipeline (the docs-mounting stage you met in
[Bootstrap](./04b-bootstrap.md)), `FireflyApplication` built one OpenAPI document
from the live inventory and merged a small router serving these paths onto the
management surface. There is no annotation framework to learn beyond the routing
attributes from chapter 6, and no codegen step. This is the Rust counterpart of
springdoc-openapi.

> **Tip** **Checkpoint.** With `cargo run` running, `curl
> localhost:8081/v3/api-docs` returns a JSON body beginning with
> `{"openapi":"3.1.0",...}`, and `http://localhost:8081/swagger-ui` renders the
> wallet API in a browser. If `curl` connects but 404s, confirm you are hitting
> `8081` (management), not `8080` (public).

## Step 2 — Understand why the docs live on the management port

Notice the URLs above are all on `:8081`, the management port — beside the actuator
and the admin dashboard — **not** on the public API at `:8080`.

> **Note** **Key term — management surface.** The *management surface* is the set
> of operational HTTP endpoints — health, info, metrics, admin, and now the API
> docs — served on a separate port from your business API, for operators and
> tooling rather than end users. This mirrors Spring Boot Actuator's dedicated
> management port.

Why split them: Swagger UI, ReDoc, and the raw spec expose your **entire** API
surface and every schema — a control-plane concern. They belong where operators
already reach `/actuator/*` and `/admin/`, keeping the public data-plane port free
of API-introspection endpoints.

That split creates one wrinkle the framework solves for you. Because the docs are
*loaded* from the management origin (`:8081`) but the API *answers* on the public
port (`:8080`), the document declares the **public API base URL** as its OpenAPI
`server`. So Swagger UI's "Try it out" and ReDoc's samples target the API
(`:8080`), not the management origin they were loaded from. `FireflyApplication`
derives that URL from the API bind address — a wildcard host like `0.0.0.0` is not
client-usable, so it falls back to `localhost`:

```text
http://localhost:8080
```

Behind a reverse proxy you want a real public URL instead. Set
`FIREFLY_OPENAPI_SERVER_URL` and it overrides the derived value:

```bash
FIREFLY_OPENAPI_SERVER_URL=https://api.lumen.example cargo run
```

What just happened: the spec's `servers[0].url` becomes the value you supplied, so
every "Try it out" call goes to your public hostname. (An unknown path on **either**
listener still answers the same RFC 9457 `application/problem+json` 404 you met in
[chapter 6](./06-first-http-api.md), so the docs surface degrades cleanly too.)

> **Tip** **Checkpoint.** `curl -s localhost:8081/v3/api-docs | jq '.servers'`
> shows one entry whose `url` is `http://localhost:8080` by default — the public
> API, not the `:8081` origin you fetched from.

## Step 3 — Turn a DTO into a component schema with `#[derive(Schema)]`

A data type becomes a reusable `#/components/schemas/{Type}` by deriving `Schema`.
Because Rust has no runtime reflection, the JSON Schema is computed **at
macro-expansion time** by walking the struct's fields — so what ends up in the spec
is decided when you compile, not at boot.

> **Note** **Key term — `#[derive(Schema)]`.** This derive is the Rust analog of a
> Spring `@Schema` model. It reads the struct (or field-less enum) at compile time,
> emits a JSON Schema fragment, and submits it to the inventory so the generator
> can register it as a named component and `$ref` it from operations.

Here is Lumen's read-model view, exactly as you wrote it in `src/domain.rs`:

```rust,ignore
// src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WalletView {
    /// The wallet id.
    pub id: String,
    /// The owner's display name.
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

The derive walks the four fields and registers a schema equivalent to:

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

What just happened: each `String` field became `{"type":"string"}`, each `i64`
became `{"type":"integer"}`, and — because none of them is wrapped in `Option` —
all four landed in `required`. The mapping mirrors what a Java/Spring `@Schema`
model produces:

- `String` / `str` / `char` → `string`; `bool` → `boolean`; every integer type
  (`i8`…`u128`, `usize`, …) → `integer`; `f32` / `f64` → `number`.
- `Uuid` → `string` with `format: uuid`; chrono / time date-times → `string` with
  `format: date-time`; dates → `format: date`; times → `format: time`.
- `Option<T>` is a transparent wrapper: it describes `T` but makes the property
  **non-required** (so optionals drop out of the `required` list).
- `Box<T>` / `Arc<T>` / `Rc<T>` are transparent too; `Vec` / `HashSet` /
  `BTreeSet` / … → an `array` of the element schema; `HashMap` / `BTreeMap` → an
  open `object` with `additionalProperties`.
- Any *other* named type is assumed to be a sibling DTO that also derives `Schema`,
  and is emitted as a `$ref` — so a nested DTO is **linked**, not inlined, and the
  two component schemas compose.

> **Tip** **Checkpoint.** `curl -s localhost:8081/v3/api-docs | jq
> '.components.schemas.WalletView'` prints the object schema above. Every DTO that
> derives `Schema` shows up under `.components.schemas`.

### Serde renaming is honoured

`#[derive(Schema)]` reads the struct's serde directives so the property names in
the schema match the JSON **wire** shape — `rename`, `rename_all`, and `skip` — not
the Rust idents. Lumen's `TransferResult` carries field renames, and the schema
follows them:

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
exact JSON the handler serialises — not the snake_case Rust idents. A struct-level
`#[serde(rename_all = "camelCase")]` is applied to every field the same way, and a
`#[serde(skip)]` field is omitted from the schema entirely. The rule of thumb: the
schema describes what goes on the wire, so it always matches your serialised JSON.

> **Design note.** This is why the schema is wire-accurate without you maintaining a
> second copy of the field names: the one set of serde attributes that controls
> serialisation also controls the schema. There is no separate annotation to keep in
> sync, and no way for the docs to drift from the bytes.

### Field-less enums become string enumerations

A field-less (unit-variant) enum that derives `Schema` emits a JSON Schema `string`
enumeration — springdoc's treatment of a Java `enum`. Serde renaming is honoured
here too, so the allowed values match the wire shape. The layered `lumen-ledger`
sample models a wallet's lifecycle this way:

```rust,ignore
// lumen-ledger: interfaces/.../wallet_status.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[serde(rename_all = "lowercase")]
pub enum WalletStatus {
    #[default]
    Active,
    Frozen,
    Closed,
}
```

registers:

```json
"WalletStatus": { "type": "string", "enum": ["active", "frozen", "closed"] }
```

What just happened: each variant became one allowed string, lowercased by the
struct-level `rename_all`. A DTO field of this type then `$ref`s the registered enum
component rather than becoming an untyped string — `lumen-ledger`'s `WalletResponse`
uses exactly this for its `status: WalletStatus` field, so the two component schemas
compose. (`#[derive(Schema)]` supports only field-less enums; an enum with data in a
variant is rejected at compile time.)

## Step 4 — Let the macro infer request and response models

You do **not** name request and response models on the verb attribute. The macro
infers them from the handler's own signature, at compile time:

- the **request body** is the inner type of the first `Json<T>` *or* `Valid<T>`
  parameter (so the validating extractor documents its body too), and
- the **response** is the `Json<T>` found inside the return type, after unwrapping
  `WebResult<…>` / `Result<…>` and looking through a `(StatusCode, Json<T>)` tuple.

Take Lumen's `open` handler, unchanged from chapter 6:

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

What just happened: from the signature alone the macro recorded `OpenWallet` as the
request schema (the `Json<OpenWallet>` parameter) and `WalletView` as the response
schema (the `Json<WalletView>` inside the `(StatusCode, …)` tuple inside
`WebResult<…>`). Both derive `Schema`, so the operation `$ref`s
`#/components/schemas/OpenWallet` and `#/components/schemas/WalletView` — no
request/response declaration on the attribute.

> **Note** A `$ref` is emitted **only** when the inferred type is actually a
> registered `#[derive(Schema)]` component. Lumen's `transfer_compliance` returns
> `Json<serde_json::Value>`; `serde_json::Value` is not a registered schema, so the
> generator emits no request/response `$ref` for it rather than referencing a
> component that does not exist. The document stays valid no matter what your
> handlers return — there are never dangling `$ref`s.

> **Tip** **Checkpoint.** `curl -s localhost:8081/v3/api-docs | jq
> '.paths."/api/v1/wallets".post.requestBody'` shows a `$ref` to
> `#/components/schemas/OpenWallet`, and the `201` response `$ref`s `WalletView`.

### Path, query, and header parameters are inferred too

The same signature-driven inference covers operation **parameters**, so Swagger UI
and ReDoc render an input for each — with no hand-written parameter list:

- **Path** parameters come from the route template: every `:id` (axum) / `{id}`
  segment becomes a required `in: path` parameter. Lumen's `GET /wallets/:id` gets
  a required `id` path parameter automatically.
- **Query** parameters come from a `Query<T>` / `ValidQuery<T>` extractor — the
  generator expands `T`'s `#[derive(Schema)]` fields into one `in: query` parameter
  each (required iff the field is non-optional). A `PageRequest` argument adds the
  standard Spring Data `page` / `size` / `sort` query parameters.
- **Header** parameters are declared on the verb attribute:
  `header("Idempotency-Key", required, description = "…")` emits an `in: header`
  parameter (and the handler reads it like any axum header). A `query("…")`
  declaration adds an extra query parameter the same way.

Lumen's `WalletApi` keeps its handlers simple — path-only — so its parameter
inference is just the `:id` segments. The richer query/header story is what the
layered `lumen-ledger` sample's `WalletController` exercises. Its paged-list
endpoint binds a filter query *and* the framework's pagination resolver:

```rust,ignore
// lumen-ledger: web/.../wallet_controller.rs
#[get("/wallets/page", summary = "List wallets by status (paged)")]
async fn list_paged(
    State(api): State<WalletController>,
    Query(query): Query<StatusQuery>,
    PageRequest(pageable): PageRequest,
) -> WebResult<Json<Page<WalletResponse>>> {
    let page = api.service.list_by_status(query.status, pageable).await.map_err(service_to_web)?;
    Ok(Json(page))
}
```

What just happened: `Query<StatusQuery>` expanded the one `status` field of the
`StatusQuery` schema into an `in: query` parameter, and `PageRequest` added `page`,
`size`, and `sort` — so Swagger UI renders four query inputs for this endpoint with
zero parameter boilerplate. Its `open` handler shows the header form, declaring an
`Idempotency-Key` request header right on the verb attribute:

```rust,ignore
// lumen-ledger: web/.../wallet_controller.rs
#[post(
    "/wallets",
    summary = "Open a wallet",
    status = 201,
    header("Idempotency-Key", description = "optional client-supplied key to make retries safe")
)]
async fn open(/* … */) -> WebResult<(StatusCode, Json<WalletResponse>)> { /* … */ }
```

— so callers see and can fill the header in Swagger UI, and the handler reads it
from the `HeaderMap` like any other header.

## Step 5 — Attach per-operation metadata

Beyond the path, each verb attribute takes optional metadata that lands on the
OpenAPI operation. The full form is:

```text
#[get("/x", summary = "…", description = "…", tags = ["A", "B"], status = 200, deprecated, request = T, response = T)]
```

| Argument | Effect on the operation |
|----------|-------------------------|
| `summary = "…"` | the one-line summary |
| `description = "…"` | the longer description |
| `tags = ["A", "B"]` | grouping tags (override the controller tag, below) |
| `status = 201` | the success status code (defaults to 201 for `POST`, else 200) |
| `deprecated` | marks the operation `deprecated: true` (bare flag; `deprecated = false` to unset) |
| `request = T` | the request body schema name — overrides inference |
| `response = T` | the success response schema name — overrides inference |

Lumen's `transfer` operation uses summary, description, an explicit `tags`, and a
`status`:

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

What just happened: the macro stamped the summary, description, `Transfers` tag, and
`200` status onto the operation, then *still* inferred `TransferRequest` and
`TransferResult` from the signature. Metadata and inference compose — you only spell
out what the signature cannot.

### Controller-level tags

`#[rest_controller(tag = "…")]` sets a default tag for **every** operation on the
controller — the analog of Spring's `@Tag(name = …)`. Lumen tags its whole wallet
surface:

```rust,ignore
// src/web.rs
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi { /* … */ }
```

Tag resolution per operation is layered:

1. an explicit per-method `tags = [...]` wins; otherwise
2. the `#[rest_controller(tag)]` default applies; otherwise
3. the generator derives a tag from the controller type name by stripping a
   trailing `Api` / `Controller` suffix (`WalletApi` → `Wallet`, `CatalogController`
   → `Catalog`).

Lumen sets the controller tag explicitly to `"Wallets"`, so in its spec `open`,
`get`, `deposit`, and `withdraw` carry the **Wallets** tag (the controller default),
while `transfer`, `transfer_compliance`, and `transfer_2pc` carry **Transfers**
(their per-method `tags = ["Transfers"]` override). Swagger UI groups the operations
under those two headings.

### Overriding inference with `request = ` / `response = `

When a body type cannot be read from the signature — a handler that takes a raw
`axum::body::Bytes`, returns an `impl IntoResponse`, or otherwise hides its DTO —
name it explicitly. `request = T` / `response = T` take the schema **name** (the
type's last path segment, matching what `#[derive(Schema)]` registers it under) and
**take precedence** over inference:

```rust,ignore
#[post("/import", summary = "Bulk import", request = ImportBatch, response = ImportReport)]
async fn import(/* a non-Json body */) -> impl axum::response::IntoResponse { /* … */ }
```

What just happened: even though the signature reveals no `Json<T>`, the operation
now `$ref`s `ImportBatch` and `ImportReport` (provided both derive `Schema`). Lumen
never needs this — every handler's body is a `Json<T>` of a `#[derive(Schema)]`
type, so the inference covers it — but the escape hatch is there for the cases a
signature cannot express.

## Step 6 — Read the worked example end to end

Putting it together for Lumen's `WalletApi`:

- The controller is `#[rest_controller(path = "/api/v1", tag = "Wallets")]`.
- Its `#[derive(Schema)]` DTOs — `OpenWallet`, `WalletView`, `AmountBody`,
  `TransferRequest`, `TransferResult`, `TccTransferResult` — each become a
  `#/components/schemas/*` entry and are `$ref`ed by the operations that use them.
- Each operation's request/response is inferred from its `Json<T>` parameter and
  return; its summary / description / tags / status come from the verb attribute;
  and the `transfers/*` operations group under **Transfers**.
- `transfer_compliance` takes `Json<TransferRequest>` (a registered schema, so its
  request `$ref`s `TransferRequest`) but returns `Json<serde_json::Value>`, so its
  response carries **no** `$ref` — and that is correct, not a gap.

Every operation also gets a `default` RFC 9457 response referencing
`#/components/schemas/ProblemDetail`, which the generator always adds to the
document. So the uniform error shape from [chapter 6](./06-first-http-api.md) is
documented for every endpoint automatically — Swagger UI shows an error response on
each operation without you writing one.

> **Tip** **Checkpoint.** `curl -s localhost:8081/v3/api-docs | jq
> '.components.schemas | keys'` lists every registered schema, including
> `ProblemDetail`. `jq '.paths."/api/v1/transfers".post.responses | keys'` shows
> both the `200` and the `default` (problem) response.

## Step 7 — One descriptor table, three surfaces

The `#[rest_controller]` descriptor table is read by **three** surfaces, so they can
never drift:

- the **OpenAPI document** at `/v3/api-docs`,
- the admin dashboard's **`/admin/api/mappings`** route table
  ([Observability & Admin](./15-observability.md)), and
- the **startup report**'s `:: routes (N) ::` block
  ([Bootstrap](./04b-bootstrap.md)).

Add a route, and all three update from the same registry on the next build. The
startup report even prints the operation and component-schema counts so you can
confirm the spec is live:

```text
:: openapi :: N operations | K component schemas (served at /v3/api-docs) ::
```

What just happened: because the document, the admin mappings view, and the boot log
all read one inventory, "what the API does" has a single source of truth. There is
no second hand-maintained spec file to fall behind your code.

## Step 8 — Export the spec with the CLI

The `firefly` CLI can write an OpenAPI document for tooling and CI:

```bash
firefly openapi                              # OpenAPI 3.1 JSON to stdout
firefly openapi --format yaml -o openapi.yaml
```

There is a scope caveat worth understanding (covered in full in [The
CLI](./19-cli.md)). A *compiled* binary cannot boot an arbitrary application to
enumerate its live routes — the routes live in the consumer's own crate, and there
is no DI container to introspect from a generic tool. So `firefly openapi` emits a
metadata-stamped **skeleton**: the `info` block (read from `firefly.yaml` /
`Cargo.toml`), the always-present `ProblemDetail` component, and empty `paths`. The
wire shape is identical to what a live app serves — only the route list is blank.

To capture Lumen's **real** routes, run the service and fetch `/v3/api-docs`. That
document, built by the framework's `from_inventory()`, *is* the live spec:

```bash
cargo run --bin lumen &
curl -s http://localhost:8081/v3/api-docs | jq .
```

> **Tip** **Checkpoint.** `firefly openapi | jq '.openapi'` prints `"3.1.0"` even
> outside a running app, and `jq '.components.schemas.ProblemDetail'` is present.
> The skeleton's `paths` is `{}`; the live spec at `:8081/v3/api-docs` has your
> wallet routes filled in.

## Step 9 — Generate a typed client from the spec

The inverse direction: given an OpenAPI document, generate a typed Rust client over
the framework's `RestClient` — the Rust analog of springdoc's OpenAPI-generated
WebClient SDK.

```bash
# capture the live spec, then generate a client from it
curl -s http://localhost:8081/v3/api-docs -o wallet-openapi.json
firefly openapi-client --spec wallet-openapi.json -o src/generated.rs --client-name WalletClient
```

What just happened: the generator walked the spec and emitted a model `struct` per
object schema (and an `enum` per string enumeration), with serde renames and
optional fields preserved, plus one `async fn` per operation — typed path/query
parameters, a JSON request body, and the success-response type — each calling
`RestClient` under the hood. The generated client is the same shape you would
hand-write; the layered `lumen-ledger` sample ships exactly such an SDK, which you
will meet in [Layered Microservices](./22-layered-microservices.md).

> **Tip** **Checkpoint.** After the second command, `src/generated.rs` exists and
> contains a `pub struct WalletClient` plus `WalletResponse` / `WalletStatus` models
> mirroring the spec's component schemas. Unmapped shapes degrade to
> `serde_json::Value` rather than failing the generation.

## Recap

In this chapter you saw that the controller you already wrote *is* the API
documentation:

- Firefly serves a live **OpenAPI 3.1** spec (`/v3/api-docs`, aliased
  `/openapi.json`), **Swagger UI** (`/swagger-ui`), and **ReDoc** (`/redoc`) on the
  **management** port, built from the inventory at boot with zero application code.
- The spec advertises the **public API base URL** as its `server` (falling back to
  `localhost`, overridable with `FIREFLY_OPENAPI_SERVER_URL`), so "Try it out"
  targets the API, not the docs origin.
- `#[derive(Schema)]` turns a DTO into a `#/components/schemas/{Type}` at
  macro-expansion time, honouring serde `rename` / `rename_all` / `skip`, treating
  `Option` / `Box` / `Arc` / `Rc` as transparent, mapping collections to arrays and
  maps, `$ref`-ing nested DTOs, and rendering field-less enums as string
  enumerations.
- Request bodies, response bodies, and path/query/header parameters are **inferred**
  from the handler signature (`Json<T>` / `Valid<T>` bodies, `Query<T>` and
  `PageRequest` query params, `:id` path segments, declared `header(...)` params) —
  with a `$ref` emitted only for actually-registered schemas, so the document never
  dangles.
- Per-operation metadata (`summary`, `description`, `tags`, `status`, `deprecated`)
  and the controller-level `tag` shape each operation; `request = ` / `response = `
  override the inference when a signature cannot express the DTO.
- Every operation carries a `default` RFC 9457 `ProblemDetail` response, so the
  uniform error contract is documented automatically.
- One descriptor table feeds the spec, the `/admin/api/mappings` view, and the
  startup report — a single source of truth — and the `firefly openapi` /
  `openapi-client` commands export the spec and generate a typed client from it.

Nothing in `samples/lumen` changed: the routing declarations you already wrote
produced Swagger UI, ReDoc, and a valid OpenAPI 3.1 spec for free.

## Exercises

1. **Read the live spec.** With `cargo run` running, `curl -s
   localhost:8081/v3/api-docs | jq '.paths | keys'`. Confirm every wallet and
   transfer route from chapter 6 is present, then `jq '.components.schemas | keys'`
   to see each `#[derive(Schema)]` DTO plus `ProblemDetail`.
2. **Watch a rename flow through.** In `jq`, inspect
   `.components.schemas.TransferResult.properties` and confirm the property keys are
   `stepsExecuted` / `stepsRolledBack` (the serde wire names), not the snake_case
   idents. Then temporarily remove one `#[serde(rename = "…")]` in `src/transfer.rs`,
   rebuild, and watch the schema property name change.
3. **Move the server URL.** Start Lumen with
   `FIREFLY_OPENAPI_SERVER_URL=https://api.lumen.example cargo run`, then
   `curl -s localhost:8081/v3/api-docs | jq '.servers'`. Confirm the URL changed —
   this is the value Swagger UI's "Try it out" will call.
4. **Deprecate an operation.** Add the bare `deprecated` flag to one verb attribute
   in `src/web.rs` (e.g. `#[post("/wallets/:id/withdraw", summary = "Withdraw funds",
   status = 200, deprecated)]`), rebuild, and confirm
   `jq '.paths."/api/v1/wallets/{id}/withdraw".post.deprecated'` is `true` and
   Swagger UI strikes the operation through.
5. **Export and diff.** Run `firefly openapi --format yaml -o skeleton.yaml`, then
   `curl -s localhost:8081/v3/api-docs | jq . > live.json`. Note that the CLI
   skeleton has empty `paths` while the live document carries your routes — and that
   both share the same `info` block and `ProblemDetail` component.

## Where to go next

- Build the read model behind the `WalletView` these docs describe in
  **[Persistence & Reactive Repositories](./07-persistence.md)**.
- See where the OpenAPI document is built and mounted in the boot pipeline in
  **[Bootstrap](./04b-bootstrap.md)**, and the `/admin/api/mappings` view it shares a
  source with in **[Observability & Admin](./15-observability.md)**.
- Consume an OpenAPI-generated client against a real upstream service in **[Layered
  Microservices](./22-layered-microservices.md)**, building on **[HTTP
  Clients](./13-http-clients.md)**.
