# `firefly-webhooks`

> **Tier:** Adapter · **Status:** Full · **Java original:** `firefly-webhooks` · **Go module:** `webhooks`

## Overview

`firefly-webhooks` is the framework's **inbound webhook ingestion subsystem**.
HTTP requests arriving at `POST /api/webhooks/{provider}` are
validated against the registered signature scheme, optionally
enriched, and dispatched to per-provider `Processor`s; failures are
sent to a dead-letter queue for replay.

Modules mirror the Go module's sub-packages (and the .NET project split):

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

Bring your own by implementing the `Validator` trait. The signature
wire formats are byte-identical to the Go port: every validator is
tested against header values produced by the `firefly-testkit` signers
(`sign_hmac`, `sign_stripe`, `sign_github`, `sign_twilio`).

## Pipeline

```text
Pipeline::process(ev)
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
    pub fn new(dlq: Arc<dyn Dlq>) -> Self;          // Go: NewPipeline(dlq)
    pub fn without_dlq() -> Self;                   // Go: NewPipeline(nil)
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

Non-`POST` methods get **405**; a missing provider segment gets **400** —
matching the Go handler. Header keys on `Inbound.headers` use Go's
canonical MIME casing (`X-Event-Type`), so the JSON wire shape is
identical across ports.

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

> Porting note: the Go `sdk.Client.Forward` builds the headed request
> but then forwards through `RESTClient.Do`, which JSON-re-encodes the
> payload and drops the per-call headers — untested dead code that its
> own documentation contradicts. The Rust port implements the
> documented contract (raw body + headers); without it no forwarded
> event could ever pass signature validation.

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
401 / 404 / 405 / 400 / 500 negative paths — every Go test case, plus
cross-crate proof that all four validators accept
`firefly-testkit`-signed payloads and an SDK round-trip that replays a
dead-lettered event end-to-end over a real socket.
