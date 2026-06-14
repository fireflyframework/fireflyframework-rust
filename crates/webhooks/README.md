# `firefly-webhooks`

> **Tier:** Adapter · **Status:** Stable

## Overview

`firefly-webhooks` is the framework's **inbound webhook ingestion subsystem**.
HTTP requests arriving at `POST /api/webhooks/{provider}` are
validated against the registered signature scheme, optionally
enriched, and dispatched to per-provider `Processor`s; failures are
sent to a dead-letter queue for replay.

The crate is split into focused modules:

| Module       | What it provides                                                        |
|--------------|-------------------------------------------------------------------------|
| `interfaces` | `Inbound` DTO + `Validator`, `Processor` ports                          |
| `core`       | `Pipeline`, in-memory `MemoryDlq`, four canonical signature validators  |
| `processor`  | SPI surface for service-supplied per-provider processors                |
| `web`        | `POST /api/webhooks/{provider}` ingestion `axum::Router`                |
| `sdk`        | Typed forwarder (replay DLQ entries; cross-service composition)         |

Everything is re-exported flat from the crate root.

## Built-in validators

| Validator           | Header(s)               | Algorithm                                            |
|---------------------|-------------------------|------------------------------------------------------|
| `HmacValidator`     | `X-Signature` (default) | HMAC-SHA256 hex (with optional `sha256=` prefix)     |
| `StripeValidator`   | `Stripe-Signature`      | `t=<unix>,v1=<hmac-hex>` with 5-min tolerance        |
| `GitHubValidator`   | `X-Hub-Signature-256`   | HMAC-SHA256 hex                                      |
| `TwilioValidator`   | `X-Twilio-Signature`    | HMAC-SHA1 base64 of `URL + sorted(form k+v)`         |

Bring your own by implementing the `Validator` trait. Every validator
is tested against header values produced by the `firefly-testkit`
signers (`sign_hmac`, `sign_stripe`, `sign_github`, `sign_twilio`).

`TwilioValidator` dispatches on the request's `Content-Type`: form
parameters participate only when the media type is
`application/x-www-form-urlencoded`; for any other body (JSON, a
missing `Content-Type`, multipart, …) the signed string is the URL
alone. A malformed `Content-Type` or form body is treated as a
signature mismatch.

## Pipeline

```text
Pipeline::process(ev)
   │
   ├─ dedupe (optional EventStore; skip duplicate → Ok)
   │
   ├─ enrich (optional hook)
   │
   ├─ for each registered Processor for ev.provider:
   │     │  process(&ev)
   │     │  on error → Dlq::push, abort downstream processors
   │
   └─ return first error
```

## Public surface

### `interfaces`

```rust
pub struct Inbound {
    pub id: String,
    pub provider: String,
    pub event_type: String,            // JSON: "eventType"
    pub headers: BTreeMap<String, String>,
    pub payload: Vec<u8>,              // JSON: base64, as Go marshals []byte
    pub received_at: DateTime<Utc>,    // JSON: "receivedAt"
}

pub trait Validator: Send + Sync {
    fn provider(&self) -> &str;
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), WebhookError>;
}

#[async_trait]
pub trait Processor: Send + Sync {
    fn provider(&self) -> &str;
    async fn process(&self, ev: &Inbound) -> Result<(), WebhookError>;
}
```

### `core`

```rust
#[async_trait]
pub trait Dlq: Send + Sync {
    async fn push(&self, ev: Inbound, error: &WebhookError) -> Result<(), WebhookError>;
}
pub struct MemoryDlq { /* entries(), len(), is_empty() */ }
pub struct DlqEntry { pub event: Inbound, pub err: String, pub time: DateTime<Utc> }

pub struct Pipeline { /* … */ }
impl Pipeline {
    pub fn new(dlq: Arc<dyn Dlq>) -> Self;
    pub fn without_dlq() -> Self;
    pub fn register_validator(&self, v: impl Validator + 'static);
    pub fn register_processor(&self, p: impl Processor + 'static);
    pub fn enrich(&self, hook: impl Fn(&mut Inbound) + Send + Sync + 'static);
    pub fn validators(&self) -> HashMap<String, Arc<dyn Validator>>;
    pub async fn process(&self, ev: Inbound) -> Result<(), WebhookError>;
}

HmacValidator::new(provider, secret)        // X-Signature, hex; both configurable
StripeValidator::new(secret)                // 5-min tolerance; with_clock / with_tolerance
GitHubValidator::new(secret)
TwilioValidator::new(auth_token, post_url)

WebhookError::SignatureMismatch             // "firefly/webhooks: signature mismatch"
```

## Idempotency `EventStore`

An optional dedup store the pipeline consults **before** dispatch so a
redelivered webhook (same idempotency key) is recognised and skipped
instead of re-processed.

```rust,ignore
#[async_trait]
pub trait EventStore: Send + Sync {
    async fn already_processed(&self, idempotency_key: &str) -> Result<bool, WebhookError>;
    async fn remember(&self, idempotency_key: &str) -> Result<(), WebhookError>;
}
pub struct MemoryEventStore { /* new(), len(), is_empty() */ }

pub const DEFAULT_IDEMPOTENCY_HEADER: &str = "X-Idempotency-Key";

impl Pipeline {
    pub fn register_event_store(&self, s: impl EventStore + 'static);
    pub fn register_event_store_arc(&self, s: Arc<dyn EventStore>);
    pub fn with_idempotency_header(&self, header: impl Into<String>); // default X-Idempotency-Key
}
```

When a store is registered and the event carries the idempotency header
(read from `Inbound.headers`, canonical-MIME cased):

- a key already recorded → `process` returns `Ok(())` **without
  dispatching** (the ingestion endpoint answers `202 Accepted`);
- a fresh key is recorded with `remember` **before** the processors run;
- an event with no idempotency header is dispatched unconditionally;
- a failed `already_processed` lookup is fail-closed (surfaced, no
  dispatch).

`MemoryEventStore` covers tests and single-instance services;
multi-instance deployments use the Redis adapter below for cross-process
dedupe.

```rust,ignore
let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
pipeline.register_validator(StripeValidator::new(b"whsec_test"));
pipeline.register_event_store(MemoryEventStore::new());
// Duplicate X-Idempotency-Key deliveries are skipped (202), processed once.
```

#### Redis-backed `RedisEventStore` (feature `redis`)

`RedisEventStore` is a distributed, durable idempotency store so a
webhook redelivered to a **different** instance is still recognised as a
duplicate. Each key is a TTL-expiring Redis string, so the store
self-prunes without a background job. Defaults are a key prefix
`webhook:idem:` and a TTL of `86_400`s (24 h).

| `EventStore` method | Redis command            |
|---------------------|--------------------------|
| `already_processed` | `EXISTS <prefix><key>`   |
| `remember`          | `SET <prefix><key> "1" EX <ttl>` |

```toml
# Cargo.toml — opt in (off by default to keep the core crate lean)
firefly-webhooks = { version = "26.6", features = ["redis"] }
```

```rust,ignore
use firefly_webhooks::RedisEventStore;

let store = RedisEventStore::connect("redis://127.0.0.1:6379/0")
    .await?
    .with_key_prefix("webhook:idem:") // default; override as needed
    .with_ttl_seconds(86_400);        // default (24 h)
pipeline.register_event_store_arc(std::sync::Arc::new(store));
```

The `already_processed` → `remember` pair is a non-atomic check-then-set;
serialise it behind a distributed lock if you need strict once-exactly
semantics.

### `web`

```rust
pub fn router(pipeline: Arc<Pipeline>) -> axum::Router
```

The handler performs:

1. Read body fully.
2. Look up the provider's `Validator`. **404** if unknown provider.
3. Verify the signature. **401** on mismatch.
4. Build an `Inbound`. Call `Pipeline::process(ev)`.
5. **202 Accepted** on success; **500** on processor error.

Non-`POST` methods get **405**; a missing provider segment gets **400**.
Header keys on `Inbound.headers` use canonical MIME casing
(`X-Event-Type`), and `Host` is omitted, giving a stable JSON wire
shape.

### `sdk`

```rust
pub struct Client { /* … */ }
impl Client {
    pub fn new(base_url: impl AsRef<str>) -> Self;
    pub async fn forward(
        &self,
        provider: &str,
        payload: &[u8],
        headers: &[(&str, &str)],
    ) -> Result<(), firefly_client::ClientError>;
}
```

`forward` POSTs the payload as the **raw request body** with the given
headers — which must include the provider's signature header (e.g.
`Stripe-Signature`), since the framework's validators run on the
receiving end. Retries (3 attempts, 100 ms doubling backoff capped at
2 s, on 429/5xx and network errors), `X-Correlation-Id` propagation,
and RFC 7807 error decoding behave exactly like the framework REST
client.

> Design note: the payload is sent verbatim as the request body with the
> caller-supplied headers preserved — re-encoding the body or dropping
> the per-call headers would strip the provider signature, so no
> forwarded event could ever pass validation on the receiving end.

## Quick start

```rust
use std::sync::Arc;

use firefly_webhooks::{web, MemoryDlq, Pipeline, StripeValidator};

#[tokio::main]
async fn main() {
    let secret = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_default();

    let pipeline = Arc::new(Pipeline::new(Arc::new(MemoryDlq::new())));
    pipeline.register_validator(StripeValidator::new(secret.into_bytes()));
    // pipeline.register_processor(StripeProcessor); // your business handler

    let app = web::router(pipeline);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

## Testing

```bash
cargo test -p firefly-webhooks
```

Covers HMAC validator success + tamper detection, the pipeline DLQ
flow, the enrichment hook, and the ingestion endpoint (via
`tower::ServiceExt::oneshot`) against known-good signatures plus
401 / 404 / 405 / 400 / 500 negative paths, plus cross-crate proof that
all four validators accept `firefly-testkit`-signed payloads and an SDK
round-trip that replays a dead-lettered event end-to-end over a real
socket.
