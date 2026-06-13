# `firefly-callbacks`

> **Tier:** Adapter · **Status:** Full · **Go module:** `callbacks` · **Java original:** `firefly-callbacks` · **.NET project:** `FireflyFramework.Callbacks.{Interfaces,Models,Core,Web,Sdk}`

## Overview

`firefly-callbacks` is the framework's **outbound webhook subsystem**.
Services publish business events; the dispatcher signs each payload
with HMAC-SHA256, retries with exponential backoff, and records every
attempt to a pluggable `Store` for audit. A REST admin endpoint
manages targets; an SDK type-safely calls the admin endpoint from
upstream services.

Sub-modules mirror the Go sub-packages (and the .NET project split):

| Module                  | What it provides                                                          |
|-------------------------|---------------------------------------------------------------------------|
| `interfaces`            | DTOs (`Target`, `CallbackEvent`, `Attempt`) + `Store`, `Dispatcher` ports |
| `models`                | In-memory `MemoryStore` implementing `Store`                              |
| `core`                  | HMAC-signing `HmacDispatcher` with retry, audit-log recording             |
| `web`                   | REST admin handler (CRUD targets, list attempts)                          |
| `sdk`                   | Typed client for the admin REST API                                       |

Everything is re-exported flat from the crate root.

## Wire format

`POST <target.url>` with body == `event.payload`, plus headers:

| Header                | Value                                          |
|-----------------------|------------------------------------------------|
| `Content-Type`        | `application/json`                             |
| `X-Firefly-Event`     | `event.event_type`                             |
| `X-Firefly-Event-Id`  | `event.id`                                     |
| `X-Firefly-Timestamp` | Unix seconds when the request was sent         |
| `X-Firefly-Signature` | `sha256=<hmac-hex>` keyed on `target.secret`   |
| `X-Correlation-Id`    | When `event.correlation_id` is set             |
| (custom)              | Anything from `target.headers`                 |

Header names and the `sha256=<lowercase hex>` HMAC-SHA256 encoding are
byte-identical to the Java / .NET / Go / Python ports — a webhook
receiver written against any of them verifies this crate's deliveries
unchanged.

## Retry policy

`DispatcherConfig { max_attempts, initial_delay }` — defaults:
3 attempts, 200 ms initial delay, doubling, **capped at 5 min** per
retry (pyfly's `min(backoff_ms * 2**(attempt-1), 300_000)`). Each
attempt records an `Attempt` audit row regardless of outcome. Per-target
delivery failures are best-effort: they are audited and the dispatcher
continues with the next matching target, exactly as in the Go port.

**Retryable vs permanent (pyfly #194).** A non-2xx *response* is retried
only when its status is transient — `408`, `429`, or any `5xx`. A 4xx
other than `408`/`429` (a deterministic `400`/`401`/`403`/`404`/`422`) is
a **permanent** client error: the dispatcher stops retrying it
immediately rather than burning the whole attempt budget against a target
that will keep rejecting it. Transport errors (no response) are always
retried. This matches pyfly's `_is_retryable` exactly.

> Retry tuning is **dispatcher-global** (`DispatcherConfig`), not
> per-`Target`. The wire shape of `Target` is byte-for-byte the Go port's
> struct (camelCase, `omitempty`, `secret` never serialised), so adding
> per-target `max_attempts`/`backoff_ms`/`tenant` fields — and the
> per-tenant `dispatch(tenant_id, …)` fan-out pyfly offers — is a
> deliberate divergence kept out to preserve that wire contract and the
> single-tenant Go model. Run one dispatcher per tenant/policy for the
> same effect.

## Public surface

### `interfaces`

```rust,ignore
pub struct Target {
    pub id: String,
    pub url: String,
    pub secret: String,            // #[serde(skip)] — Go's `json:"-"`
    pub event_types: Vec<String>,  // empty = match-all
    pub headers: HashMap<String, String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

pub struct CallbackEvent {
    pub id: String,
    pub event_type: String,        // JSON "type"
    pub payload: Vec<u8>,          // JSON base64, like Go's []byte
    pub headers: HashMap<String, String>,
    pub correlation_id: String,
}

pub struct Attempt {
    pub id: String,                // 24 lowercase hex chars
    pub event_id: String,
    pub target_id: String,
    pub status: u16,               // 0 when no response was produced
    pub body: String,
    pub error: String,
    pub attempt: u32,              // 1-based
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[async_trait]
pub trait Store: Send + Sync {
    async fn upsert_target(&self, t: Target) -> Result<Target, CallbackError>;
    async fn get_target(&self, id: &str) -> Result<Target, CallbackError>;
    async fn list_targets(&self) -> Result<Vec<Target>, CallbackError>;
    async fn delete_target(&self, id: &str) -> Result<(), CallbackError>;
    async fn record_attempt(&self, a: Attempt) -> Result<(), CallbackError>;
    async fn list_attempts(&self, event_id: &str) -> Result<Vec<Attempt>, CallbackError>;
}

#[async_trait]
pub trait Dispatcher: Send + Sync {
    async fn dispatch(&self, ev: CallbackEvent) -> Result<(), CallbackError>;
}

pub enum CallbackError {
    NotFound,                                          // "firefly/callbacks: not found"
    DeliveryFailed { status: u16, error: Option<String> },
    Store(String),
}
```

`CallbackError` `Display` strings are bytes-equal to the Go sentinels
(`firefly/callbacks: not found`,
`callback delivery failed: status=%d err=%v`).

### `core`

```rust,ignore
pub struct DispatcherConfig {
    pub http_client: Option<reqwest::Client>,   // Go: HTTPClient
    pub max_attempts: u32,                      // Go: MaxAttempts
    pub initial_delay: Duration,                // Go: InitialDelay
    pub clock: Option<Arc<dyn Clock>>,          // Go: Now func() time.Time
    pub authorized_domains: Vec<AuthorizedDomain>, // pyfly: SSRF allowlist (#190)
}
// DispatcherConfig::default() == Go's core.Default() (empty allowlist)

pub struct HmacDispatcher { /* Go: core.Dispatcher */ }
impl HmacDispatcher {
    // Zero-valued cfg fields are filled with the defaults, like Go.
    pub fn new(store: Arc<dyn Store>, cfg: DispatcherConfig) -> Self;
}
impl Dispatcher for HmacDispatcher { /* dispatch(ev) */ }

pub const HEADER_EVENT: &str = "X-Firefly-Event";
pub const HEADER_EVENT_ID: &str = "X-Firefly-Event-Id";
pub const HEADER_TIMESTAMP: &str = "X-Firefly-Timestamp";
pub const HEADER_SIGNATURE: &str = "X-Firefly-Signature";
```

### `web`

```rust,ignore
pub fn handler(store: Arc<dyn Store>) -> axum::Router;
// Routes:
//   GET    /callbacks/targets
//   POST   /callbacks/targets         (upsert, 201)
//   GET    /callbacks/targets/{id}
//   DELETE /callbacks/targets/{id}    (204)
//   GET    /callbacks/attempts/{eventId}
```

Error responses reproduce Go's `http.Error` wire format
(`text/plain; charset=utf-8`, `X-Content-Type-Options: nosniff`,
message + `\n`). JSON responses reproduce Go's `writeJSON`
(`json.Encoder.Encode`), terminating every document with `\n`, and
`/callbacks/attempts/{eventId}` answers JSON `null` (so `null\n` on
the wire) when the event has no recorded attempts — byte parity with
the Go port's nil slice through `encoding/json`.

### `sdk`

```rust,ignore
pub struct CallbacksClient { /* Go: sdk.Client */ }
impl CallbacksClient {
    pub fn new(base_url: impl AsRef<str>) -> Self;                        // Go: sdk.New
    pub async fn targets(&self) -> Result<Vec<Target>, ClientError>;      // Go: Targets
    pub async fn upsert(&self, t: &Target) -> Result<Target, ClientError>; // Go: Upsert
    pub async fn delete(&self, id: &str) -> Result<(), ClientError>;      // Go: Delete
}
```

Built on `firefly_client::RestClient` (as the Go SDK builds on
`client.NewREST`), so it inherits correlation-id propagation and
429/5xx retry for free.

## pyfly parity — `AuthorizedDomain` allowlist (SSRF protection)

Mirrors pyfly's `CallbackConfig.authorized_domains` / `_is_authorized`
(#190): an optional outbound-URL allowlist enforced on dispatch.

```rust,ignore
pub struct AuthorizedDomain {
    pub domain: String,       // e.g. "customer.example.com" (trimmed, case-insensitive)
    pub description: String,  // free-form; omitted from JSON when empty
}
impl AuthorizedDomain {
    pub fn new(domain: impl Into<String>) -> Self;
}
// From<&str> / From<String> for ergonomic construction.
```

When `DispatcherConfig.authorized_domains` is **non-empty**, a matching
target is delivered to only if its URL **host** equals an authorized
domain or is a subdomain of one (`host == domain` or `host` ends with
`".{domain}"`), matched case-insensitively. A target failing the check
is **rejected before any HTTP request**: a rejection audit row is
recorded (`status: 0`, `attempt: 0`, `error: "domain not authorized"` —
pyfly's failed-execution audit) and the dispatcher continues with the
next target. A URL with no parseable host is fail-closed (never
authorized).

An **empty** allowlist (the default) disables the check entirely, so
existing Go-parity behaviour — every target reachable — is preserved.

```rust,ignore
let dispatcher = HmacDispatcher::new(
    store.clone(),
    DispatcherConfig {
        authorized_domains: vec![AuthorizedDomain::new("customer.example.com")],
        ..DispatcherConfig::default()
    },
);
// Targets at *.customer.example.com are delivered; any other host is blocked + audited.
```

## Quick start

```rust,ignore
use std::sync::Arc;

use firefly_callbacks::{
    CallbackEvent, Dispatcher, DispatcherConfig, HmacDispatcher, MemoryStore, Store, Target,
};

let store = Arc::new(MemoryStore::new());
store
    .upsert_target(Target {
        id: "customers".into(),
        url: "https://customer.example.com/cb".into(),
        secret: "shared-secret".into(),
        active: true,
        event_types: vec!["order.placed".into(), "order.shipped".into()],
        ..Target::default()
    })
    .await?;

let dispatcher = HmacDispatcher::new(store.clone(), DispatcherConfig::default());
dispatcher
    .dispatch(CallbackEvent {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: "order.placed".into(),
        payload: br#"{"id":"o1","customer":"alice"}"#.to_vec(),
        ..CallbackEvent::default()
    })
    .await?;
```

Serve the admin API and call it with the SDK:

```rust,ignore
use firefly_callbacks::{handler, CallbacksClient};

let app = handler(store.clone()); // axum::Router — merge or serve standalone
// …
let sdk = CallbacksClient::new("http://callbacks-admin.internal");
let targets = sdk.targets().await?;
```

## Testing

```bash
cargo test -p firefly-callbacks
```

Covers HMAC signing (including a known-answer vector), 5xx retry
behaviour, event-type filtering, inactive-target skipping, transport
failures, audit-trail recording, the REST admin CRUD (in-process via
`tower::ServiceExt::oneshot`), the JSON wire shapes, and the SDK round
trip against a live admin server on a random port.
