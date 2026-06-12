# Testing

Firefly services are designed to be testable at every level: in-process unit
tests with no I/O, in-process integration tests that exercise the real
middleware and routers via `tower::oneshot`, reactive-pipeline tests, and
real-infrastructure integration tests against a `docker-compose` stack. This
chapter walks each tier, with `firefly-testkit` providing the shared helpers.

## Unit tests with no infrastructure

The default stack — `MemoryAdapter` cache, `InMemoryBroker` broker,
`ReactiveMemoryRepository` — needs nothing external, so most logic tests run as
plain `#[tokio::test]`s. Dispatch a command through a real bus, assert the
result:

```rust,ignore
#[tokio::test]
async fn create_user_returns_id() {
    let bus = firefly_cqrs::Bus::new();
    bus.register(|c: CreateUser| async move {
        Ok::<_, firefly_cqrs::CqrsError>(UserCreated { id: "u1".into(), name: c.name })
    });

    let out: UserCreated = bus.send(CreateUser { name: "alice".into() }).await.unwrap();
    assert_eq!(out.id, "u1");
}
```

## In-process HTTP tests

Test the full middleware chain and your routes without binding a socket: build
the router with `Core::apply_middleware` and drive it with
`tower::ServiceExt::oneshot`. This exercises the real problem renderer,
correlation, and idempotency layers:

```rust,ignore
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;

#[tokio::test]
async fn unknown_order_is_404_problem() {
    let core = firefly_starter_core::Core::new(Default::default());
    let app = core.apply_middleware(api_router());

    let res = app
        .oneshot(Request::get("/orders/missing").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // body is application/problem+json
}
```

## Asserting emitted events

`firefly-testkit`'s `SpyBroker` records what a handler published so you can
assert on it. `must_encode` / `must_decode` are panic-on-failure JSON helpers
(a panic fails the test, matching `t.Fatalf` semantics):

```rust
use firefly_testkit::{must_encode, SpyBroker};

#[derive(serde::Serialize)]
struct Order { id: String }

#[test]
fn place_order_emits() {
    let spy = SpyBroker::new();
    let place_order = |order: &Order| {
        let body = must_encode(order);
        spy.record("orders", "OrderPlaced", &body);
    };
    place_order(&Order { id: "o1".into() });
    assert_eq!(spy.find_by_type("OrderPlaced").len(), 1);
}
```

## Testing webhook receivers

The testkit ships HMAC signers that match each inbound validator byte-for-byte,
so a signed request validates exactly as a real Stripe / GitHub / Twilio webhook
would:

```rust
use axum::{body::Body, http::{Request, StatusCode}};
use firefly_testkit::sign_stripe;
use tower::ServiceExt;

# fn app() -> axum::Router { axum::Router::new() }
#[tokio::test]
async fn stripe_webhook_accepts_signed_body() {
    let secret = b"whsec_test";
    let body = br#"{"type":"charge.succeeded"}"#;

    let req = Request::post("/api/webhooks/stripe")
        .header("Stripe-Signature", sign_stripe(secret, body, 1_700_000_000))
        .body(Body::from(&body[..]))
        .unwrap();

    let _res = app().oneshot(req).await.unwrap();
    // assert ACCEPTED against a real webhook router
}
```

`sign_hmac`, `sign_github`, and `sign_twilio` cover the other validators.

## Testing reactive pipelines

A reactive pipeline is tested by driving it to a terminal — `block()`,
`collect_list()`, or `count()` — and asserting the resolved value. This is the
firefly-reactive analog of Reactor's `StepVerifier`:

```rust
use firefly_reactive::Flux;

#[tokio::test]
async fn pipeline_filters_and_maps() {
    let out = Flux::range(1, 5)
        .filter(|x| x % 2 == 1)
        .map(|x| x * 10)
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out, vec![10, 30, 50]);
}
```

For error paths, assert the terminal `Err`:

```rust,ignore
use firefly_reactive::Mono;
use firefly_kernel::FireflyError;

#[tokio::test]
async fn mono_error_propagates() {
    let result = Mono::<i32>::error(FireflyError::internal("boom")).block().await;
    assert!(result.is_err());
}
```

Reactive repositories are tested the same way — drive `ReactiveMemoryRepository`
with `block()` / `collect_list()` (see [Persistence](./07-persistence.md)).

## Real-infrastructure integration tests

For tests that need a real Postgres, Redis, RabbitMQ, Kafka, Keycloak, S3, or
SMTP, the repository ships a `docker-compose.yml` with the full stack:

```yaml
# docker-compose.yml (excerpt)
services:
  postgres:   # postgres:16-alpine        :5432
  redis:      # redis:7-alpine            :6379
  rabbitmq:   # rabbitmq:3-management     :5672 / :15672
  redpanda:   # Kafka-compatible          :9092
  keycloak:   # quay.io/keycloak/keycloak :8080
  localstack: # AWS S3 emulation
  azurite:    # Azure Blob emulation
  mailhog:    # SMTP capture
```

Bring it up, then run the env-gated tests against it:

```bash
docker compose up -d                       # start the backing services

# Each integration test reads a connection URL from the environment and SKIPS
# when it is unset, so `cargo test` on a bare machine stays green. Provide the
# URLs to exercise the real services:
DATABASE_URL=postgres://firefly:firefly@localhost:5432/firefly \
REDIS_URL=redis://localhost:6379/0 \
  cargo test --workspace -- --ignored

docker compose down                        # tear it all down
```

The pattern, in a test: read the connection URL, skip if absent, otherwise
exercise the real adapter. This keeps the default `cargo test` hermetic while
letting CI flip on the full real-infrastructure suite:

```rust,ignore
#[tokio::test]
#[ignore = "requires postgres (DATABASE_URL)"]
async fn postgres_repo_round_trips() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return }; // skip on a bare machine

    let (client, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { let _ = conn.await; });
    // ... drive PostgresReactiveRepository against the live database
}
```

The adapter crates follow this convention throughout — `firefly-data`'s Postgres
repository, `firefly-cache-redis`, the `firefly-eda-*` transports, and the IDP /
ECM vendor adapters each carry live round-trip tests gated behind `#[ignore]`
and an environment variable.

## Running the suites

```bash
cargo test --workspace                 # the whole 67-member workspace, hermetic
cargo test -p firefly-cqrs             # one crate
cargo test --workspace -- --ignored    # the real-infra suite (with the stack up)
make ci                                # fmt-check + clippy -D warnings + build + test
```

With the tests green, the next chapters cover the developer CLI and shipping to
production. Continue to [The CLI](./19-cli.md).
