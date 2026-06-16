# Your First HTTP API

So far Lumen compiles, boots, prints a banner, and serves an actuator — but it
has no endpoints of its own. You also know, from
[Dependency Wiring](./04-dependency-wiring.md), how the framework discovers and
wires the beans it scans. This is the chapter where Lumen stops being a banner
and starts being a *service*: you give it a real HTTP surface, declared with one
macro, mounted for you, and proven by a test that drives the whole router
without ever binding a socket.

The HTTP layer underneath is [axum](https://docs.rs/axum). Firefly does not
hide it — you write ordinary axum handlers — but it *adds* the controller macro,
the problem-rendering, and the correlation/idempotency middleware you met in the
[Quickstart](./02-quickstart.md). You write two handlers; the framework supplies
the wiring and mounts the controller.

By the end of this chapter you will:

- Declare a REST controller as a single DI bean whose collaborators are
  autowired, using `#[derive(Controller)]` and `#[rest_controller]`.
- Map two verbs — `POST /api/v1/wallets` and `GET /api/v1/wallets/:id` — onto
  handler methods, and understand how the macro composes the route paths.
- Return a plain `serde` view (`WalletView`) and turn typed errors into RFC 9457
  `application/problem+json` documents with the right HTTP status.
- Understand *why* you never call `mount` — that adding the controller bean *is*
  mounting it.
- Drive the fully wired router in-process with `tower::oneshot`, with no live
  server and no port to race on.

## Concepts you will meet

Before the first line of code, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — controller.** A *controller* is the object that owns a
> group of HTTP endpoints. Its methods are the *handlers* — one per verb-and-path
> mapping. In Firefly a controller is just a bean with an annotated `impl` block;
> the framework reads the annotations and builds the routing table. The Spring
> analog is a `@RestController`.

> **Note** **Key term — handler / extractor.** A *handler* is the async function
> that runs for one route. An *extractor* is an argument type that pulls a piece
> of the request out for you — the path id, the JSON body, a query object. These
> are axum's own extractors (`Path`, `Json`, `State`); Firefly reuses them and
> adds a few of its own.

> **Note** **Key term — RFC 9457 problem document.** RFC 9457 (which obsoletes
> RFC 7807) defines `application/problem+json` — a small, standard JSON envelope
> for HTTP errors with `type`, `title`, `status`, and `detail` fields. Firefly
> renders every handler error this way automatically, so all your errors speak
> one machine-readable shape. The Spring analog is `ProblemDetail`.

> **Note** **Key term — CQRS bus.** Lumen routes state-changing **commands** and
> read-only **queries** through a shared *bus*. The controller's job is only to
> translate HTTP into a message and dispatch it; the wallet logic lives behind
> the bus. You build that machinery in [CQRS](./09-cqrs.md). For this chapter,
> treat `bus.send(...)` / `bus.query(...)` as "hand this message to the handler
> that knows how". *CQRS* expands to Command/Query Responsibility Segregation.

## Step 1 — Declare the controller bean

Lumen's wallet endpoints all live on one type, `WalletApi`. It is a
`#[derive(Controller)]` DI bean: a plain struct whose collaborators are
`#[autowired]` from the container. Declaring the struct is the first half of a
controller; the annotated `impl` block in [Step 2](#step-2--map-the-verbs) is the
second half.

Open `src/web.rs` and add the imports and the struct:

```rust,ignore
// src/web.rs
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use firefly::cqrs::QueryCache;
use firefly::prelude::*;
use firefly::web::{WebError, WebResult};

use crate::commands::{GetWallet, OpenWallet};
use crate::domain::{DomainError, WalletView};

/// The wallet HTTP surface — a `#[derive(Controller)]` DI bean. Its
/// collaborators are **autowired** from the container, and `#[rest_controller]`
/// auto-mounts it; there is no hand-built state and no manual `routes()` call.
#[derive(Clone, Controller)]
pub struct WalletApi {
    /// The command/query bus the controller dispatches through (autowired).
    #[autowired]
    pub bus: Arc<Bus>,
    /// The application service the transfer saga and event stream use (autowired).
    #[autowired]
    pub ledger: Arc<Ledger>,
    /// The query cache, invalidated after a mutation (autowired).
    #[autowired]
    pub query_cache: Arc<QueryCache>,
}
```

What just happened, block by block:

- The imports bring in axum's extractors (`Path`, `State`, `Json`), the CQRS
  `QueryCache`, the whole high-frequency surface via `firefly::prelude::*` (which
  gives you `Bus`, `Controller`, `#[autowired]`, and the verb macros), and the
  web result/error types (`WebResult`, `WebError`). The `DomainError` import is
  used by the error mapper in [Step 5](#step-5--map-typed-errors-to-rfc-9457-problems).
- `#[derive(Controller)]` marks the struct as a controller bean. It is the same
  stereotype as the other Firefly beans you have seen — the container scans it,
  constructs it, and manages its lifetime.
- Each `#[autowired]` field is a *collaborator* the container resolves and injects
  when it builds the bean. `bus` is the CQRS bus the handlers dispatch through;
  `ledger` is the application service the later saga and streaming endpoints use;
  `query_cache` is invalidated after a write so a read-after-write never serves a
  stale balance. You never construct `WalletApi` yourself — the framework does.
- `Clone` is required because the macro hands a clone of the controller to axum as
  per-route *state*; the struct is `Arc`-backed, so cloning is cheap.

> **Note** **Key term — autowiring.** *Autowiring* is the framework's
> constructor-injection: a `#[autowired]` field is resolved from the container by
> type and handed to the bean at construction. It is exactly Spring's
> `@Autowired`. You declare *what* a controller needs; the container decides *how*
> to supply it.

> **Tip** **Checkpoint.** The struct compiles once the `Bus`, `Ledger`, and
> `QueryCache` beans it autowires exist in the crate (you declare them as
> `#[bean]` factories — the `Bus` is framework-provided, `Ledger` and
> `QueryCache` are Lumen's). If `cargo build` complains that one of these types is
> unresolved, you are ahead of the narrative: the bean factories land in
> [CQRS](./09-cqrs.md). For now, focus on the controller shape.

## Step 2 — Map the verbs

A struct with autowired fields is just a bean. It becomes a controller when its
`impl` block carries `#[rest_controller]` and its methods carry verb attributes.
The macro reads each one and generates a `WalletApi::routes(state) ->
axum::Router` function — so the routing table is *derived from your code*, not
maintained in a separate file beside it.

Add the `impl` block to `src/web.rs`:

```rust,ignore
// src/web.rs (continued)
/// `#[rest_controller(path = "...")]` generates `WalletApi::routes(state) ->
/// axum::Router`. Each method carries one verb mapping and returns
/// `WebResult<T>`, so a handler error renders as RFC 9457
/// `application/problem+json`.
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi {
    /// `POST /api/v1/wallets` — open a wallet. Validation failures surface as
    /// 422 problems; success answers `201 Created` with the view.
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

    /// `GET /api/v1/wallets/:id` — fetch the read-model view. An unknown id
    /// renders as a 404 problem.
    #[get(
        "/wallets/:id",
        summary = "Fetch a wallet",
        description = "Returns the read-model view of a wallet."
    )]
    async fn get(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<WalletView>> {
        let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
        Ok(Json(view))
    }
}
```

There are three things worth reading carefully here.

**The path is composed.** `#[rest_controller(path = "/api/v1")]` is the prefix;
`#[post("/wallets")]` and `#[get("/wallets/:id")]` are the suffixes. The macro
joins them into `/api/v1/wallets` and `/api/v1/wallets/:id`. The `tag`,
`summary`, `description`, and `status` attributes are optional metadata: `tag`
groups the endpoints in the API docs, `summary`/`description` annotate them, and
`status = 201` tells the OpenAPI generator the success status. They change the
*documentation*, not the routing.

**Each handler is a plain axum handler.** `State`, `Path`, and `Json` are axum's
own extractors — Firefly does not replace them. `State(api): State<WalletApi>`
hands you the controller (with its autowired collaborators already in place);
`Path(id): Path<String>` binds the `:id` segment; `Json(body): Json<OpenWallet>`
deserializes the request body. The return type `WebResult<T>` is what lets a
handler error render as a problem document — covered in
[Step 5](#step-5--map-typed-errors-to-rfc-9457-problems).

**The controller is thin.** `open` and `get` translate HTTP into a message and
hand it to the CQRS `Bus`, then translate the result (or error) back into an HTTP
response. The wallet *logic* lives behind the bus, where [CQRS](./09-cqrs.md)
puts it. Read `api.bus.send(...)` (a command) and `api.bus.query(...)` (a query)
as "dispatch to the handler that knows how"; the bus, the commands, and the read
model are the subjects of chapters 7 through 11.

> **Note** **Key term — argument resolver / validating extractor.** Beyond
> `Json`/`Path`/`Query`, `firefly::web` (re-exported in `firefly::prelude`) ships
> extractors that drop into the same handler signature: `Valid<T>` for a JSON
> body and `ValidPath<T>` / `ValidQuery<T>` for path/query objects (a bind
> failure is a **400**, a constraint failure a **422** problem), the `Multipart`
> / `UploadedFile` form-upload extractor, and the `PageRequest` argument resolver
> that binds Spring's `Pageable` from `?page=&size=&sort=`. The layered sample in
> [Layered Microservices](./22-layered-microservices.md) uses all of them. Here
> the plain `Json`/`Path` extractors are enough.

> **Design note.** `#[rest_controller(path = "/api/v1")]` declares a controller
> and its path prefix; `#[get]` / `#[post]` declare the verb mappings. Beyond
> generating the router, the macro emits a route descriptor per endpoint that
> feeds the actuator `/mappings` view and the OpenAPI generator — so the routing
> table is derived from your code rather than maintained beside it, and the
> documentation surfaces stay in sync with the handlers automatically. If you
> have used a batteries-included framework before, this declarative-controller
> style will feel familiar.

> **Tip** **Checkpoint.** `WalletApi` now carries a `#[rest_controller]` `impl`
> with two annotated methods. The macro has generated a `WalletApi::routes(state)`
> function (you never call it by hand) and registered a *mount thunk* into the
> link-time inventory. You will see both pay off in
> [Step 6](#step-6--controllers-are-auto-mounted).

## Step 3 — Define the wire shape

The view a handler returns is a plain `serde` struct. It is the *read model*
projection of a wallet — flat, query-optimized, and decoupled from the internal
aggregate.

> **Note** **Key term — read model / DTO.** A *DTO* (data transfer object) is the
> on-the-wire shape a client sees, deliberately separate from your internal
> domain types. Lumen's `WalletView` is the read-model DTO: a flat projection a
> query returns. Keeping it separate from the `Wallet` aggregate means you can
> evolve the internal model without breaking the API contract.

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
    /// The aggregate version (number of events applied) — lets a client
    /// detect staleness under eventual consistency.
    pub version: i64,
}
```

What just happened: `WalletView` derives `Serialize` / `Deserialize` so it
crosses the wire, and `Schema` so the OpenAPI generator can describe it (the
`Schema` derive is the subject of [OpenAPI](./06a-openapi.md)). The balance
travels as an integer count of *minor units* (cents), so `€10.00` is the JSON
number `1000` — money never rides as a float.

The request body Lumen accepts on `POST /api/v1/wallets` is just as ordinary —
the `OpenWallet` command. A `#[serde(rename)]` on its balance field makes the JSON
key `openingBalance` while the Rust field stays snake_case, so the wire looks
like:

```json
{ "owner": "alice", "openingBalance": 1000 }
```

> **Tip** **Checkpoint.** `WalletView` lives in `src/domain.rs` and the
> controller imports it with `use crate::domain::WalletView;`. The JSON a `GET`
> returns is exactly its four fields: `id`, `owner`, `balance`, `version`.

## Step 4 — Let the client pick the format (optional)

Lumen's handlers answer `application/json` because they return
`Json<WalletView>` — a deliberate, format-pinned contract. But a controller can
also hand the framework a DTO and let the *client* pick the wire format. This step
is optional reading; you can skip to [Step 5](#step-5--map-typed-errors-to-rfc-9457-problems)
and lose nothing of the running narrative.

> **Note** **Key term — content negotiation.** *Content negotiation* lets one
> handler serve several wire formats: the client sends an `Accept` header and the
> framework renders the response with the matching converter. The Spring analog is
> an `HttpMessageConverter` chosen by `produces`.

Wrap the return value in `Negotiate(dto)` and the response is rendered with the
converter the request's `Accept` header selects — `JsonMessageConverter` for
`application/json`, `XmlMessageConverter` for `application/xml` / `text/xml` —
while the request body is read by its `Content-Type` the same way:

```rust,ignore
// a format-agnostic variant of the wallet GET
use firefly::web::Negotiate;

#[get("/wallets/:id")]
async fn get(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
) -> WebResult<Negotiate<WalletView>> {
    let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
    Ok(Negotiate(view))
}
```

The same handler now serves both wire shapes from the one `WalletView`:

```text
GET /api/v1/wallets/wlt_1  Accept: application/json
→ { "id": "wlt_1", "owner": "alice", "balance": 1000, "version": 1 }

GET /api/v1/wallets/wlt_1  Accept: application/xml
→ <response><id>wlt_1</id><owner>alice</owner><balance>1000</balance>...</response>
```

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 380" role="img"
     aria-label="Request lifecycle: an inbound HTTP request passes the Problem, TraceContext, Correlation and ContentNegotiation layers, outermost first, before reaching the rest_controller handler, and errors unwind to an RFC 9457 problem+json response"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="280.0" y="24.0" text-anchor="middle" font-size="12.5" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">inbound HTTP request</text>
<line x1="280.0" y1="30.0" x2="280.0" y2="44.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,52.0 275.5,44.0 284.5,44.0" fill="#b5531f"/>
<rect x="150.0" y="58.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="56.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="77.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ProblemLayer</text><text x="280.0" y="91.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">errors → problem+json</text><line x1="280.0" y1="104.0" x2="280.0" y2="112.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,120.0 275.5,112.0 284.5,112.0" fill="#b5531f"/><rect x="150.0" y="122.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="120.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="141.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">TraceContextLayer</text><text x="280.0" y="155.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">W3C traceparent in / out</text><line x1="280.0" y1="168.0" x2="280.0" y2="176.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,184.0 275.5,176.0 284.5,176.0" fill="#b5531f"/><rect x="150.0" y="186.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="184.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="205.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">CorrelationLayer</text><text x="280.0" y="219.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ensure-or-generate id</text><line x1="280.0" y1="232.0" x2="280.0" y2="240.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,248.0 275.5,240.0 284.5,240.0" fill="#b5531f"/><rect x="150.0" y="250.5" width="260.0" height="48.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="150.0" y="248.0" width="260.0" height="48.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="280.0" y="269.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">ContentNegotiationLayer</text><text x="280.0" y="283.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Accept → JSON / XML</text>
<line x1="280.0" y1="296.0" x2="280.0" y2="310.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="280.0,318.0 275.5,310.0 284.5,310.0" fill="#b5531f"/>
<rect x="180.0" y="320.5" width="200.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="180.0" y="318.0" width="200.0" height="46.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="280.0" y="338.0" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">#[rest_controller]</text><text x="280.0" y="352.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">your handler runs</text>
<text x="540.0" y="84.0" text-anchor="end" font-size="10" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif" font-style="italic">outermost</text>
<text x="540.0" y="276.0" text-anchor="end" font-size="10" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif" font-style="italic">innermost</text>
</svg>
<figcaption>The default layer stack, outermost first (some optional layers — CORS, security headers, metrics — are elided). <code>ProblemLayer</code> wraps everything so any error unwinds to an RFC&nbsp;9457 <code>application/problem+json</code> response; trace-context and correlation open before your handler runs; content negotiation sits closest to the routes.</figcaption>
</figure>

You wire none of this. The `ContentNegotiationLayer` is installed by default — it
sits closest to your routes, so a `Negotiate` response is re-rendered to the
client's `Accept` before the outer middleware edge runs, and a plain `Json<T>`
(or any other) response passes through untouched. An absent or empty `Accept`
defaults to JSON, and an unmatched type falls back to the first registered
converter (JSON), so negotiation never fails the request.

> **Design note.** `Negotiate(dto)` hands the framework a DTO and lets the
> request's `Accept` header pick the wire format, with no controller code. The
> `JsonMessageConverter` / `XmlMessageConverter` pair ships in the registry, and
> the `ContentNegotiationLayer` is installed by default, so negotiation is on out
> of the box. Add a converter — say CBOR — by implementing `MessageConverter` and
> registering it; user converters take priority over the built-ins.

If you want one *house style* — every response in `camelCase`, nulls dropped, the
same inclusion rules everywhere — rather than per-type serde attributes, Firefly
gives you a single object to express that policy: `ObjectMapper`. It is a builder
that sets a property-naming convention, an inclusion rule, and pretty-printing:

```rust,ignore
use firefly::web::{ObjectMapper, PropertyNaming, Inclusion};

// camelCase on the wire, drop nulls, compact output.
let mapper = ObjectMapper::new()
    .naming(PropertyNaming::CamelCase)
    .inclusion(Inclusion::NonNull)
    .pretty(false);
```

The naming and inclusion options are:

| Option                                       | Effect                                              |
|----------------------------------------------|-----------------------------------------------------|
| `PropertyNaming::AsIs` *(default)*           | leave field names untouched                         |
| `PropertyNaming::CamelCase`                  | `opening_balance` → `openingBalance`                |
| `PropertyNaming::SnakeCase`                  | `openingBalance` → `opening_balance`                |
| `PropertyNaming::KebabCase`                  | `opening_balance` → `opening-balance`               |
| `PropertyNaming::PascalCase`                 | `opening_balance` → `OpeningBalance`                |
| `PropertyNaming::ScreamingSnakeCase`         | `opening_balance` → `OPENING_BALANCE`               |
| `Inclusion::Always` *(default)*              | serialize every field                               |
| `Inclusion::NonNull`                         | omit `null` fields                                  |
| `Inclusion::NonEmpty`                        | omit `null`, empty strings, and empty collections   |

The naming transform is *reversible*: a `snake_case` Rust struct speaks
`camelCase` on the wire and reads it back the same way, so the same mapper sits on
both ends of a request/response. If you need the raw transform — for instance to
post-process a `serde_json::Value` you built by hand — `apply_write(value)`
renames toward the wire and `apply_read(value)` renames back toward your structs.

To make the *whole service* observe one policy without decorating every DTO, wrap
a mapper in `MappingJsonConverter` and register it. It implements `MessageConverter`
for `application/json`, and because it registers as a *user* converter it takes
priority over the built-in `JsonMessageConverter`:

```rust,ignore
use firefly::web::{ObjectMapper, PropertyNaming, Inclusion, MappingJsonConverter};

// One mapper expresses the service-wide JSON contract.
let mapper = ObjectMapper::new()
    .naming(PropertyNaming::CamelCase)
    .inclusion(Inclusion::NonNull);

// Wrap it as the JSON converter and register it so every negotiated
// application/json exchange observes the policy.
registry.add(std::sync::Arc::new(MappingJsonConverter::new(mapper)));
```

Registering it once (as a converter bean) applies a global JSON naming and
inclusion policy to the entire HTTP surface, instead of repeating
`#[serde(rename_all = ...)]` on every DTO. Per-type serde attributes still
compose on top: reach for them when one type needs to deviate from the house
style, and let `MappingJsonConverter` carry the default everywhere else.

> **Warning** A renaming mapper rewrites *every* object key in the document — it
> works on the JSON tree, so it cannot tell a struct field from a key inside a
> free-form `HashMap` you carry as data. Use a global naming policy on
> *DTO-shaped* payloads; for a type whose body holds arbitrary string-keyed data,
> leave the global policy at `AsIs` and name that one type with
> `#[serde(rename_all = "camelCase")]` — that is type-aware and never touches
> data keys.

## Step 5 — Map typed errors to RFC 9457 problems

A handler that returns `WebResult<T>` turns any error into the right
`application/problem+json` response via `?`. `WebResult<T>` is an alias whose
error arm is a `WebError`, and the framework knows how to render it. Lumen's
controller maps the bus's error channel onto a precise HTTP status with one
helper.

> **Note** **Key term — `WebResult` / `WebError`.** `WebResult<T>` is
> `Result<T, WebError>`. A `WebError` carries a `FireflyError`, and the framework's
> problem renderer turns it into an `application/problem+json` body with the right
> status code. Returning `WebResult<T>` and using `?` is all it takes — you never
> write the response yourself.

Add the error mapper to `src/web.rs`:

```rust,ignore
// src/web.rs (continued)
/// Maps a bus `CqrsError` onto the precise HTTP problem the domain implies:
/// a validation failure → 422, a not-found detail → 404, an
/// insufficient-funds / non-positive detail → 422, otherwise 500.
fn cqrs_to_web(err: CqrsError) -> WebError {
    match err {
        CqrsError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        CqrsError::Handler(detail) => {
            if detail.ends_with("not found") {
                WebError::from(FireflyError::not_found(detail))
            } else if detail == DomainError::InsufficientFunds.to_string()
                || detail == DomainError::NonPositiveAmount.to_string()
                || detail == DomainError::OwnerRequired.to_string()
            {
                WebError::from(FireflyError::validation(detail))
            } else {
                WebError::from(FireflyError::not_found(detail))
            }
        }
        other => WebError::from(FireflyError::internal(other.to_string())),
    }
}
```

What just happened: `cqrs_to_web` inspects the bus's `CqrsError` and picks the
`FireflyError` constructor that matches the failure — a validation failure becomes
a 422, a "not found" detail a 404, and an unexpected error a 500. The handlers
call it as `.map_err(cqrs_to_web)?`, so the error flows out of the handler as a
`WebError` and the framework's renderer does the rest.

The `FireflyError` constructors map straight to HTTP status — pick the one that
matches the failure and the renderer does the rest:

| Constructor                              | Status | Use                          |
|------------------------------------------|--------|------------------------------|
| `FireflyError::bad_request(detail)`      | 400    | malformed input              |
| `FireflyError::unauthorized(detail)`     | 401    | missing/invalid credentials  |
| `FireflyError::forbidden(detail)`        | 403    | authenticated but not allowed |
| `FireflyError::not_found(detail)`        | 404    | absent resource              |
| `FireflyError::conflict(detail)`         | 409    | state conflict               |
| `FireflyError::validation(detail)`       | 422    | semantic validation failure  |
| `FireflyError::internal(detail)`         | 500    | server fault                 |

A rendered problem for an unknown wallet looks like this — note the dedicated
`application/problem+json` content type, which the tests assert on:

```json
{
  "type": "https://fireflyframework.org/problems/not-found",
  "title": "Not Found",
  "status": 404,
  "detail": "wallet wlt_does_not_exist not found"
}
```

> **Design note.** Returning `WebResult<T>` turns any `FireflyError` into the
> right `application/problem+json` response via `?`, with the problem rendering
> built in — you never write an error-to-status mapping for the framework's own
> errors. The RFC 9457 contract is stable and language-neutral, so a Firefly 404
> presents identically to every client regardless of which service produced it.

> **Tip** **Checkpoint.** `src/web.rs` now holds the `WalletApi` struct, its
> `#[rest_controller]` `impl`, and `cqrs_to_web`. That is a complete HTTP surface
> — two endpoints and their error mapping — without a single line that mounts a
> route or builds a router by hand.

## Step 6 — Controllers are auto-mounted

You never mount the controller. Because `WalletApi` is a `#[derive(Controller)]`
bean, the `#[rest_controller]` macro registered a *mount thunk* into the link-time
inventory alongside the generated `routes(state)` function. At boot,
`FireflyApplication` calls `firefly::web::mount_controllers(&container)`, which
resolves each controller bean from the container (constructing its autowired
collaborators), calls its `routes(state)`, and merges the result — then layers on
security and wraps the whole thing in the web middleware chain:

```rust,ignore
// inside FireflyApplication::bootstrap — you write none of this:
let routes = firefly::web::mount_controllers(&container)         // every #[rest_controller]
    .merge(firefly::web::mount_route_contributors(&container));  // every RouteContributor bean
// security (the FilterChain + BearerLayer beans) is layered onto these routes,
// then the whole router is wrapped in the observability edge:
let api = web.apply_middleware(routes);                          // + trace, metrics, 404, problem
```

> **Note** **Key term — link-time inventory.** The *inventory* is a registry the
> macros write into at compile time: each `#[rest_controller]`, command handler,
> event listener, and `#[scheduled]` task records itself there. At boot the
> framework reads the inventory back and wires everything — no reflection, no
> manual registration list. It is how `main` never changes as Lumen grows.

So adding the controller *is* mounting it: declare the bean, annotate the impl,
and the route table grows. The macro's generated `routes(state)` is still there
(it is what the mount thunk calls), and the `RouteDescriptor` it emits per
endpoint feeds the actuator `/mappings` view and the OpenAPI generator — but you
never call either by hand.

Every request to a wallet route passes through the canonical chain you got for
free in the [Quickstart](./02-quickstart.md) — the RFC 9457 problem layer,
correlation-id propagation, and idempotency replay — before it reaches your
handler. You wrote the two handlers; the rest of the request lifecycle is the
framework's.

> **Note** `main` never changes as Lumen grows. The JWT security layer is
> discovered from a `FilterChain` bean in [Security](./14-security.md); the
> streaming endpoint is added as a `RouteContributor` bean in
> [Production](./20-production.md). Each is a *new bean the scan finds*, not a
> line edited into a composition root — the framework absorbs every addition.

> **Tip** **Checkpoint.** Run `cargo run` and read the startup report's
> `:: routes ::` line — `/api/v1/wallets` and `/api/v1/wallets/:id` now appear in
> it. You added them by declaring a bean, not by touching a router. (The mutations
> will answer `401` until the security beans exist; that is expected and arrives
> in [Security](./14-security.md).)

## Step 7 — Prove it works in-process

Now prove the whole thing round-trips. Lumen's HTTP tests drive the *real,
fully-wired* router **in-process** with `tower::ServiceExt::oneshot` — no socket
bound, no port to race on.

> **Note** **Key term — `bootstrap()` and `oneshot`.** `bootstrap()` is the
> sibling of `run()`: it assembles the same app — the same component scan and
> auto-mount — but returns a `Bootstrapped` value *without serving*, exposing the
> wired `api_router`. `tower::ServiceExt::oneshot` feeds one `Request` to that
> `Router` and returns the `Response`, all in the test process. Together they run
> the real request path with no live server.

The test boot path is a small helper, `build_router()`, in `src/web.rs`. It is
gated to test builds and calls `bootstrap()`, returning the exact `axum::Router`
that `main` serves:

```rust,ignore
// src/web.rs — the in-process router the tests drive (no socket bound).
#[cfg(test)]
pub(crate) async fn build_router() -> axum::Router {
    firefly::FireflyApplication::new(APP_NAME)
        .version(VERSION)
        .bootstrap()
        .await
        .expect("lumen bootstrap")
        .api_router
}
```

Because `bootstrap()` runs the *same* component scan and auto-mount as `run()`,
the test drives the real, fully-wired controller stack — the macro-generated
routes, the JSON contract, and the status-code mapping — the same code path a
real client hits, minus the network. `APP_NAME` and `VERSION` are the two
constants Lumen keeps beside its HTTP surface (you met them in the Quickstart).

The tests themselves live in `src/http_test.rs`, a `#[cfg(test)] mod` compiled
into the crate so it can reach the crate-internal `build_router`. Each test boots
**one** app context and drives every request against it — Spring Boot's
`@SpringBootTest` model — so the singletons stay consistent across a test's
requests (the wallet a command opens is the wallet a later query reads). A couple
of small request helpers keep the tests readable:

```rust,ignore
// src/http_test.rs
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::build_router;
use crate::domain::WalletView;
use crate::security::{mint_token, CUSTOMER_ROLE};

/// A bearer token for a customer — mutations require authentication, which the
/// framework auto-discovers from the security beans.
fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}

/// Sends one request against the (cloned) shared app and returns the response.
async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
}

/// Decodes a JSON response body into a typed value.
async fn body_json<T: serde::de::DeserializeOwned>(res: Response) -> T {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
```

> **Note** Because security is auto-discovered from the `FilterChain` and
> `BearerLayer` beans (the subject of [Security](./14-security.md)), the
> mutating `POST` carries an `Authorization: Bearer …` header. The read-only `GET`
> does not need one. If you have not added the security beans yet, run the
> mutation tests without the header and expect a `401` — that *is* the framework
> enforcing the chain it discovered.

Here is the first end-to-end test, the open-then-get round-trip. The `axum::Router`
is `Arc`-backed and cheap to clone, so each `oneshot` clones the shared app:

```rust,ignore
#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let app = build_router().await;

    // POST /api/v1/wallets → 201 Created with the opened view.
    let res = send(
        &app,
        Request::post("/api/v1/wallets")
            .header("content-type", "application/json")
            .header("authorization", bearer())
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "owner": "alice", "openingBalance": 1_000
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED, "open should 201");
    let opened: WalletView = body_json(res).await;
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);
    assert_eq!(opened.version, 1);

    // GET /api/v1/wallets/:id → 200 OK with the same view.
    let res = send(
        &app,
        Request::get(&format!("/api/v1/wallets/{}", opened.id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let fetched: WalletView = body_json(res).await;
    assert_eq!(fetched.id, opened.id);
    assert_eq!(fetched.balance, 1_000);
}
```

What just happened: the `POST` opens a wallet and the framework answers `201` with
the `WalletView`; the `GET` reads the same wallet back and answers `200` with the
matching view. Both requests went through the entire mounted controller stack,
the CQRS dispatch, and the JSON contract — in one process, with no network.

The error paths are tested the same way. An id that was never opened is a `404`
problem, and the test asserts the `application/problem+json` content type — so the
RFC 9457 contract is part of the suite, not just the prose:

```rust,ignore
#[tokio::test]
async fn unknown_wallet_is_404_problem() {
    let app = build_router().await;
    let res = send(
        &app,
        Request::get("/api/v1/wallets/wlt_does_not_exist")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let ct = res.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("application/problem+json"));
}
```

> **Design note.** `oneshot` against `build_router()` runs the whole controller
> stack in the test process with no live server and no socket bound, so the test
> exercises the real request path at full speed and without port contention.
> [Testing](./18-testing.md) builds this into a full strategy.

> **Tip** **Checkpoint.** Run `cargo test -p firefly-sample-lumen` and watch the
> round-trip and the 404-problem tests pass against the real, framework-assembled
> router. (The full sample also tests deposit/withdraw, the transfer saga, and the
> security chain — those rely on machinery from later chapters.)

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| an empty public router | a `WalletApi` controller declared with `#[derive(Controller)]` + `#[rest_controller]` and two real endpoints |
| no client contract | `POST /api/v1/wallets` → `201` + `WalletView`, `GET /api/v1/wallets/:id` → `200`/`404`, all JSON |
| errors unconsidered | typed `FireflyError` → RFC 9457 `application/problem+json` with the right status, via `cqrs_to_web` |
| nothing to test | a `tower::oneshot` round-trip that drives the full router in-process, content-type assertions included |

You also now know:

- That a controller is *just a bean plus an annotated `impl`* — `#[autowired]`
  collaborators in the struct, verb attributes on the methods — and that the macro
  derives the routing table from your code.
- That you never mount a controller: `mount_controllers(&container)` resolves and
  merges every `#[rest_controller]` at boot, so adding the bean *is* adding the
  routes, and `main` never changes.
- That `WebResult<T>` plus a `FireflyError` constructor turns any handler error
  into the right `application/problem+json`, with no response-writing by hand.
- That `bootstrap()` is the test seam: `build_router()` drives the fully-wired
  router in-process with `tower::oneshot`, no socket bound.

The controller is deliberately thin: it speaks HTTP and delegates the wallet logic
to the bus. That seam is what the next several chapters fill in — the read model
the `GET` serves, the domain that enforces the rules, and the CQRS handlers the
`POST` dispatches to.

## Exercises

1. **Add a route.** Give `WalletApi` a `#[get("/wallets")]` `list` method that
   returns `WebResult<Json<Vec<WalletView>>>`. Run Lumen and watch the new path
   appear in the startup report's `:: routes ::` line and in
   `WalletApi::routes` — you never touch a routing table.
2. **Shape an error.** Make `cqrs_to_web` (or a small handler of your own) return
   `FireflyError::conflict("wallet already closed")` and confirm the response is a
   `409` with `application/problem+json`. Try `bad_request` and `forbidden` too,
   and read the rendered `type`/`title`/`status` for each against the table in
   [Step 5](#step-5--map-typed-errors-to-rfc-9457-problems).
3. **Negotiate the format.** Switch the `GET` handler's return type to
   `Negotiate<WalletView>` (Step 4), run Lumen, and request the same wallet twice
   — once with `Accept: application/json` and once with `Accept: application/xml`.
   Confirm one handler serves both wire shapes.
4. **Write the round-trip yourself.** Copy `open_then_get_round_trips_through_cqrs`,
   change the owner and opening balance, and assert the returned `balance` matches.
   Run `cargo test -p firefly-sample-lumen` and watch it pass against the real
   router.
5. **Honor idempotency.** `POST /api/v1/wallets` twice with the same
   `Idempotency-Key` header and identical body; confirm the second response
   replays the stored result. Then change the body under the same key and observe
   the `409`. You wrote none of this — it came with the middleware chain.

## Where to go next

- See how the macro turns your `#[rest_controller]` and `#[derive(Schema)]` types
  into a live spec in **[OpenAPI & API Docs](./06a-openapi.md)**.
- Give the `GET` endpoint a real backing store with
  **[Persistence & Reactive Repositories](./07-persistence.md)**.
- Put the wallet rules behind the bus in
  **[Domain-Driven Design](./08-domain-driven-design.md)** and
  **[CQRS](./09-cqrs.md)** — the machinery `bus.send(...)` / `bus.query(...)`
  dispatch to.
