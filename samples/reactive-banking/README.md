# reactive-banking — the full-ecosystem reference service

The **flagship "everything wired together"** sample for the Firefly Framework
for Rust. It is a reactive (Spring WebFlux / Reactor-style) banking service
that threads the entire ecosystem through one realistic flow and proves — with
an in-process end-to-end test and a real-infrastructure end-to-end test — that
the pieces work *together*.

```
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│                           HTTP  (firefly-starter-web + JWT)                                │
│   POST /accounts  /:id/deposit  /:id/withdraw  /transfers     GET /:id     GET /:id/events │
└───────────────┬──────────────────────────────────────────────────────────────────────────┘
                │  Authorization: Bearer <JWT>   (CUSTOMER role on mutating routes)
                ▼
        reactive CQRS bus  (firefly-cqrs: send_mono / query_mono → Mono<R>)
                │
   ┌────────────┴───────────────┐                         ┌──────────────────────────────────┐
   │  command handlers          │                         │  POST /transfers                 │
   │   OpenAccount / Deposit /   │                         │   ──► SAGA (firefly-orchestration)│
   │   Withdraw                  │                         │      step debit  ─► step credit  │
   │        │                    │                         │      compensate (refund) on fail │
   │        ▼                    │                         └──────────────┬───────────────────┘
   │   Bank  (application service, shared by handlers AND the saga)       │
   │        │  rehydrate aggregate ─► run domain command ─► append (optimistic concurrency)   │
   │        ▼                                                             │
   │   event store  (firefly-eventsourcing: Account aggregate, DomainEvent stream)  ◄─────────┘
   │        │  publish each DomainEvent
   │        ▼
   │   EDA broker  (firefly-eda in-memory · firefly-eda-kafka for Kafka)
   │        │  subscribe
   │        ▼
   │   projection  ─► reactive read model (firefly-data ReactiveCrudRepository<AccountView>)
   │                     ▲                          (in-memory · real Postgres reactive repo)
   │   query handler ────┘   GET /:id serves the projected view (falls back to folding the stream)
   │
   └─► GET /:id/events  ─► Flux<AccountEvent> ─► application/x-ndjson | text/event-stream
                              (reactive server push, backpressured)

        SDK  (firefly-client WebClient)  ─► consumes /:id/events lazily as a Flux<AccountEvent>
```

## What this demonstrates

| Capability                       | Where                                                            | Crate(s) |
|----------------------------------|-----------------------------------------------------------------|----------|
| **Reactive HTTP responses**      | `Mono<Json>` for reads/writes, `Flux` → NDJSON/SSE for the stream | `firefly-reactive`, `firefly-web` |
| **Reactive CQRS dispatch**       | `bus.send_mono(cmd)` / `bus.query_mono(q)` → `Mono<R>`          | `firefly-cqrs` |
| **Event sourcing**               | the `Account` aggregate folds its `DomainEvent` stream           | `firefly-eventsourcing` |
| **CQRS read model + projection** | EDA subscriber rebuilds an `AccountView` read model              | `firefly-eda`, `firefly-data` |
| **Reactive repository**          | `ReactiveCrudRepository<AccountView, String>` (Spring Data R2DBC analog) | `firefly-data` |
| **Sagas + compensation**         | money transfer: debit → credit, refund the debit on failure      | `firefly-orchestration` |
| **Reactive WebClient SDK**       | opens/deposits and consumes the event stream as a `Flux`         | `firefly-client` |
| **JWT security + RBAC**          | mutating routes require a `CUSTOMER` bearer token                | `firefly-security` |
| **Observability + actuator**     | banner, `/actuator/*`, request metrics, graceful lifecycle       | `firefly-starter-web`, `firefly-observability`, `firefly-actuator` |
| **Pluggable infra**              | in-memory by default; real Postgres + Kafka when configured      | `firefly-eda-kafka`, `tokio-postgres` |

Amounts are integer **minor units (cents)** throughout, so money arithmetic is
exact.

## Endpoints

| Method & path                              | Auth        | Reactive surface                       | Description                          |
|--------------------------------------------|-------------|----------------------------------------|--------------------------------------|
| `POST /api/v1/accounts`                    | `CUSTOMER`  | `send_mono` → `Mono<Json>`             | Open an account (`201` + `Location`) |
| `POST /api/v1/accounts/:id/deposit`        | `CUSTOMER`  | `send_mono` → `Mono<Json>`             | Credit an account                    |
| `POST /api/v1/accounts/:id/withdraw`       | `CUSTOMER`  | `send_mono` → `Mono<Json>`             | Debit an account                     |
| `POST /api/v1/transfers`                   | `CUSTOMER`  | saga → `Mono<Json>`                    | Move funds (saga w/ compensation)    |
| `GET  /api/v1/accounts/:id`                | public      | `query_mono` → `Mono<Json>`            | Read-model view of an account        |
| `GET  /api/v1/accounts/:id/events`         | public      | `Flux` → `application/x-ndjson` / SSE  | **Streaming** account events         |
| `GET  /actuator/*` (admin port)            | public      | —                                      | health / info / metrics / version    |

The streaming endpoint defaults to NDJSON; request SSE with `?format=sse` or
`Accept: text/event-stream`.

### Wire shapes

```jsonc
// POST /api/v1/accounts            → 201 Created, Location: /api/v1/accounts/<id>
{ "owner": "alice", "openingBalance": 1000 }
// → { "id": "acc_…", "owner": "alice", "balance": 1000, "version": 1 }

// POST /api/v1/accounts/:id/deposit  (and /withdraw)
{ "amount": 500 }
// → { "id": "acc_…", "owner": "alice", "balance": 1500, "version": 2 }

// POST /api/v1/transfers
{ "from": "acc_a", "to": "acc_b", "amount": 300 }
// → { "status": "completed", "from": "acc_a", "to": "acc_b", "amount": 300,
//     "stepsExecuted": ["debit","credit"], "stepsRolledBack": [] }
// a failed transfer compensates and returns 422 with the failing leg's detail.

// GET /api/v1/accounts/:id/events    (application/x-ndjson, one per line)
{ "account_id": "acc_…", "version": 1, "type": "AccountOpened",  "amount": 1000, "time": "…" }
{ "account_id": "acc_…", "version": 2, "type": "MoneyDeposited", "amount": 500,  "time": "…" }
{ "account_id": "acc_…", "version": 3, "type": "MoneyWithdrawn", "amount": -200, "time": "…" }
```

## Module map

| Module          | Contents                                                                |
|-----------------|-------------------------------------------------------------------------|
| `domain`        | The event-sourced `Account` aggregate, domain events, `AccountView`     |
| `repository`    | `ReactiveCrudRepository` read model (in-memory + Postgres)              |
| `commands`      | CQRS messages + the `Bank` application service + handler registration   |
| `saga`          | The money-transfer saga (debit → credit, compensation)                 |
| `projections`   | The EDA subscriber that rebuilds the read model                         |
| `security`      | JWT verifier + token minting + the RBAC filter chain                    |
| `web`           | Router composition + the reactive HTTP handlers                         |
| `sdk`           | The reactive `WebClient` SDK (`BankClient`)                             |

## Running

All commands assume:

```bash
export PATH="/opt/homebrew/bin:$PATH"
cd /path/to/fireflyframework-rust
```

### Without infrastructure (in-memory)

The default: in-memory event store, in-memory EDA broker, in-memory reactive
read model. Nothing external is required.

```bash
cargo run -p firefly-sample-reactive-banking
# public API   → http://127.0.0.1:8080
# actuator     → http://127.0.0.1:8081/actuator/health
```

Drive it from the shell (mint a token the demo verifier accepts — in a real
deployment you would obtain this from your IdP):

```bash
# Open an account (needs a CUSTOMER bearer token — see security::mint_token).
# The repo's tests/SDK mint tokens programmatically; for a manual probe,
# the GET read + the events stream are public:
curl -s http://127.0.0.1:8080/api/v1/accounts/acc_demo            # 404 until opened
curl -s http://127.0.0.1:8080/api/v1/accounts/acc_demo/events     # NDJSON stream
curl -s http://127.0.0.1:8081/actuator/health
```

Override the bind addresses with `BANKING_ADDR` / `BANKING_ADMIN_ADDR`.

### With real infrastructure (Postgres + Kafka)

Point the service at a real Postgres (backs the reactive read-model
repository) and/or a real Kafka cluster (carries the domain events). Either can
be enabled independently:

```bash
export FIREFLY_TEST_POSTGRES_URL="postgres://firefly:firefly@localhost:5432/firefly"
export FIREFLY_TEST_KAFKA_BROKERS="localhost:9092"
cargo run -p firefly-sample-reactive-banking
# /actuator/info now reports  "readModel":"postgres"  "eventBus":"kafka"
```

The Postgres path self-provisions its `account_view` table on boot. The Kafka
path uses the `firefly-eda-kafka` broker (a real `librdkafka` producer +
consumer-group loop).

## Testing

```bash
cargo test  -p firefly-sample-reactive-banking
cargo clippy -p firefly-sample-reactive-banking --all-targets -- -D warnings
cargo fmt   -p firefly-sample-reactive-banking --check
```

The suite is the heart of the sample — it proves the ecosystem works together:

- **`tests/e2e.rs`** — boots the real router on an ephemeral `127.0.0.1:0`
  server and drives it with the reactive `WebClient` SDK through the full flow:
  open → deposit → withdraw → transfer (saga happy path) → transfer that fails
  and **compensates** → `GET` reflects the projection → the `/events` endpoint
  emits the account's events as NDJSON, consumed reactively as a `Flux`. JWT
  auth is enforced (401 without a token, 200/201 with). No `sleep` exceeds
  200 ms.
- **`tests/handlers.rs`** — HTTP-boundary tests via `tower::oneshot`: wire
  shapes, RFC 7807 problems, the NDJSON **and** SSE framing of the streaming
  endpoint, and the JWT/RBAC enforcement.
- **`tests/actuator.rs`** — the inherited management surface
  (`/actuator/health` · `info` · `metrics` · `version`).
- **`tests/real_infra.rs`** — the **real cross-infra** e2e: the same flow
  against a real Postgres reactive repo **and** real Kafka event bus +
  projection. It is gated on `FIREFLY_TEST_POSTGRES_URL` **and**
  `FIREFLY_TEST_KAFKA_BROKERS`; when either is unset it **skips cleanly**
  (prints a `SKIP` notice), so `cargo test` on a bare machine stays green. Run
  it for real with both set:

  ```bash
  FIREFLY_TEST_POSTGRES_URL=postgres://firefly:firefly@localhost:5432/firefly \
  FIREFLY_TEST_KAFKA_BROKERS=localhost:9092 \
    cargo test -p firefly-sample-reactive-banking --test real_infra -- --nocapture
  ```

The `src/main.rs` binary is **not** exercised by the test suite — the tests
drive `build_app` / `build_router` in-process instead.

## License

Apache-2.0. Part of the [Firefly Framework for Rust](https://github.com/fireflyframework/fireflyframework-rust).
