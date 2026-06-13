# Your First HTTP API

> By the end of this chapter Lumen has a real HTTP surface: a `WalletApi`
> controller declared with `#[rest_controller]`, a `POST /api/v1/wallets`
> endpoint that opens a wallet and answers `201 Created`, a `GET
> /api/v1/wallets/:id` that returns a `WalletView`, and typed errors that render
> as RFC 9457 problem documents — with an in-process test that drives the whole
> router without binding a socket. This is the chapter where Lumen stops being a
> banner and starts being a service.

So far Lumen compiles, runs, and serves an actuator, and you know how its
composition root resolves collaborators. Now you give it endpoints. The HTTP
layer is axum; Firefly contributes the controller macro, the problem rendering,
and the correlation/idempotency middleware you met in the Quickstart, all woven
in by the `WebStack`. You write handlers; the framework supplies the wiring.

## The `#[rest_controller]` macro

Lumen's wallet endpoints live on one type, `WalletApi`, whose `impl` block
carries `#[rest_controller]`. The macro reads each verb attribute and generates
a `WalletApi::routes(state) -> axum::Router` function — so a controller is just
an `impl` with annotated methods, and the routing table is derived from the
code rather than maintained beside it.

```rust,ignore
// src/web.rs
use std::sync::Arc;
use axum::extract::{Path, State};
use axum::Json;
use firefly::prelude::*;
use firefly::web::{WebError, WebResult};

use crate::commands::{GetWallet, OpenWallet};
use crate::domain::WalletView;

/// The wallet HTTP surface. It carries the CQRS `Bus` it dispatches through;
/// the controller stays thin and delegates every decision to a handler.
#[derive(Clone)]
pub struct WalletApi {
    pub bus: Arc<Bus>,
    // (the ledger and query cache fields arrive in later chapters)
}

/// `#[rest_controller(path = "...")]` generates `WalletApi::routes(state) ->
/// axum::Router`. Each method carries one verb mapping and returns
/// `WebResult<T>`, so a handler error renders as RFC 9457
/// `application/problem+json`.
#[rest_controller(path = "/api/v1")]
impl WalletApi {
    /// `POST /api/v1/wallets` — open a wallet. Validation failures surface as
    /// 422 problems; success answers `201 Created` with the view.
    #[post("/wallets")]
    async fn open(
        State(api): State<WalletApi>,
        Json(body): Json<OpenWallet>,
    ) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
        let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
        Ok((axum::http::StatusCode::CREATED, Json(view)))
    }

    /// `GET /api/v1/wallets/:id` — fetch the read-model view. An unknown id
    /// renders as a 404 problem.
    #[get("/wallets/:id")]
    async fn get(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<WalletView>> {
        let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
        Ok(Json(view))
    }
}
```

Three things to read here:

- **The path is composed.** `#[rest_controller(path = "/api/v1")]` is the prefix;
  `#[post("/wallets")]` and `#[get("/wallets/:id")]` are the suffixes. The macro
  joins them into `/api/v1/wallets` and `/api/v1/wallets/:id`.
- **Each handler is a plain axum handler.** `State`, `Path`, and `Json` are
  axum's own extractors — Firefly does not replace them. You write the function;
  the macro only registers it on the router.
- **The controller is thin.** `open` and `get` translate HTTP into a message and
  hand it to the CQRS `Bus`, then translate the result (or error) back into an
  HTTP response. The wallet *logic* lives behind the bus, where
  [CQRS](./09-cqrs.md) puts it. Treat `api.bus.send(...)` / `api.bus.query(...)`
  here as "dispatch to the handler that knows how"; the bus, the commands, and
  the read model are the subjects of chapters 7 through 11.

> **Spring parity.** `#[rest_controller(path = "/api/v1")]` is `@RestController`
> + `@RequestMapping("/api/v1")`, and `#[get]` / `#[post]` are `@GetMapping` /
> `@PostMapping` (pyfly's `@rest_controller` + `@get` / `@post`). The macro even
> emits a route descriptor per endpoint for the actuator `/mappings` view and
> the OpenAPI generator — the Rust equivalent of Spring's
> `RequestMappingHandlerMapping`.

## The wire shape — `WalletView`

The view a handler returns is a plain `serde` struct. It is the *read model*
projection of a wallet — flat, query-optimized, and decoupled from the internal
aggregate. The balance travels as an integer count of minor units (cents), so
`€10.00` is the JSON number `1000`:

```rust,ignore
// src/domain.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletView {
    pub id: String,
    pub owner: String,
    /// The current balance, in minor units (cents).
    pub balance: i64,
    /// The aggregate version (number of events applied).
    pub version: i64,
}
```

The request body Lumen accepts on `POST /api/v1/wallets` is just as ordinary —
the `OpenWallet` message, with a `#[serde(rename)]` so the JSON field is
`openingBalance` while the Rust field stays snake_case:

```json
{ "owner": "alice", "openingBalance": 1000 }
```

## Typed errors → RFC 9457 problems

A handler that returns `WebResult<T>` turns any error into the right
`application/problem+json` response via `?`. `WebResult<T>` is an alias whose
error arm is a `WebError`, and the framework knows how to render it. Lumen's
controller maps the bus's error channel onto a precise HTTP status with one
helper:

```rust,ignore
// src/web.rs
use crate::domain::DomainError;

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

> **Spring parity.** `WebResult<T>` + `FireflyError` is the
> `ResponseEntity` / `@ExceptionHandler` story, but the problem rendering is
> built in: you never write an error-to-status mapping for the framework's own
> errors. The same RFC 9457 contract holds across the Java, Go, .NET, and Python
> ports — a 404 from Lumen looks like a 404 from pyfly's Lumen.

## Mounting the routes

The controller's `routes(state)` function returns a plain `axum::Router`, which
the composition root wraps in the `WebStack` middleware chain. `LumenApp::router`
is that one place — construct the controller from the resolved collaborators,
call `WalletApi::routes`, and hand it to `apply_middleware`:

```rust,ignore
// src/web.rs
impl LumenApp {
    /// Builds the public router: the macro-generated wallet routes wrapped in
    /// the web middleware chain.
    pub fn router(&self) -> axum::Router {
        let state = WalletApi { bus: Arc::clone(&self.bus) };
        let routes = WalletApi::routes(state);
        self.web.apply_middleware(routes)
    }
}
```

Every request to a wallet route now passes through the canonical chain you got
for free in the Quickstart — the RFC 9457 problem layer, correlation-id
propagation, and idempotency replay — before it reaches your handler. You wrote
the two handlers; the rest of the request lifecycle is the framework's.

> **Note** — `LumenApp::router` is the *only* function that changes as Lumen
> grows. The streaming endpoint merges into it in
> [Production](./20-production.md); the JWT security layer wraps it in
> [Security](./14-security.md). `main` keeps calling `app.router()` and never
> learns the difference — the composition root absorbs every addition.

## Proving it works — an in-process round-trip

Because `router()` is a self-contained `axum::Router`, Lumen's tests drive it
**in-process** with `tower::ServiceExt::oneshot` — no socket bound, no port to
race on. This is the first end-to-end test, the open-then-get round-trip:

```rust,ignore
// tests/http.rs
use axum::body::Body;
use axum::http::{Request, StatusCode};
use firefly_sample_lumen::build_router;
use firefly_sample_lumen::domain::WalletView;
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    // POST /api/v1/wallets → 201 Created with the opened view.
    let res = build_router()
        .await
        .oneshot(
            Request::post("/api/v1/wallets")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "owner": "alice", "openingBalance": 1_000
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "open should 201");

    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let opened: WalletView = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);
    assert_eq!(opened.version, 1);

    // GET /api/v1/wallets/:id → 200 OK with the same view.
    let res = build_router()
        .await
        .oneshot(
            Request::get(&format!("/api/v1/wallets/{}", opened.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
```

`build_router()` is the testable composition root: it is `build_app().await`
followed by `.router()`, returning exactly the `axum::Router` `main` serves. The
test exercises the macro-generated routes, the JSON contract, and the
status-code mapping — the same code path a real client hits, minus the network.

The error paths are tested the same way. An empty `owner` is a `422` problem; an
id that was never opened is a `404` problem — and both assert the
`application/problem+json` content type, so the RFC 9457 contract is part of the
test suite, not just the prose:

```rust,ignore
#[tokio::test]
async fn unknown_wallet_is_404_problem() {
    let res = build_router()
        .await
        .oneshot(
            Request::get("/api/v1/wallets/wlt_does_not_exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let ct = res.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.contains("application/problem+json"));
}
```

> **Spring parity.** `oneshot` against `build_router()` is `MockMvc` /
> `WebTestClient` (pyfly's `TestClient`): the whole controller stack runs in the
> test process with no live server. [Testing](./18-testing.md) builds this into
> a full strategy.

## Recap — what changed in Lumen

| Before | After this chapter |
|--------|--------------------|
| an empty public router | a `WalletApi` controller declared with `#[rest_controller]` and two real endpoints |
| no client contract | `POST /api/v1/wallets` → `201` + `WalletView`, `GET /api/v1/wallets/:id` → `200`/`404`, all JSON |
| errors unconsidered | typed `FireflyError` → RFC 9457 `application/problem+json` with the right status |
| nothing to test | a `tower::oneshot` round-trip that drives the full router in-process, content-type assertions included |

The controller is deliberately thin: it speaks HTTP and delegates the wallet
logic to the bus. That seam is what the next several chapters fill in — the read
model the `GET` serves, the domain that enforces the rules, and the CQRS
handlers the `POST` dispatches to.

## Exercises

1. **Add a route.** Give `WalletApi` a `#[get("/wallets")]` `list` method that
   returns `WebResult<Json<Vec<WalletView>>>`. Watch `WalletApi::routes` pick it
   up automatically — you never touch a routing table.
2. **Shape an error.** Make `cqrs_to_web` (or a small handler of your own) return
   `FireflyError::conflict("wallet already closed")` and confirm the response is
   a `409` with `application/problem+json`. Try `bad_request` and `forbidden`
   too, and read the rendered `type`/`title`/`status` for each.
3. **Honor idempotency.** `POST /api/v1/wallets` twice with the same
   `Idempotency-Key` header and identical body; confirm the second response
   carries `Idempotent-Replay: true`. Then change the body under the same key and
   observe the `409`. You wrote none of this — it came with `apply_middleware`.
4. **Write the round-trip yourself.** Copy the `open_then_get` test, change the
   owner and opening balance, and assert the returned `balance` matches. Run
   `cargo test -p firefly-sample-lumen` and watch it pass against the real
   router.

Next, give the `GET` endpoint a real backing store with
[Persistence & Reactive Repositories](./07-persistence.md), then put the rules
behind the bus in [Domain-Driven Design](./08-domain-driven-design.md) and
[CQRS](./09-cqrs.md).
