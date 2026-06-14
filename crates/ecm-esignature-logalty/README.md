# `firefly-ecm-esignature-logalty`

> **Tier:** Adapter · **Status:** Production · **Backing tech:** Logalty — EU qualified / eIDAS e-signature, REST + `X-Api-Key`

## Overview

`firefly-ecm-esignature-logalty` is the Logalty
[`firefly_ecm::ESignatureProvider`] adapter (EU qualified / eIDAS e-signature).
`RestProvider` is a **real REST integration** over
[`reqwest`](https://docs.rs/reqwest). Every operation calls the live Logalty
REST API — there is no stub and no `not_implemented` path:

| Operation | Logalty REST call |
|---|---|
| `create` | `POST /envelopes` |
| `status` / `get` | `GET /envelopes/{envelopeId}` |
| `cancel` | `DELETE /envelopes/{envelopeId}` |
| `recipients` | `GET /envelopes/{envelopeId}` (projects `signers[]`) |
| `download` | `GET /envelopes/{envelopeId}/document` |

`create` builds the envelope-create payload and parses the returned
`envelopeId`; `get` projects the envelope resource onto a
`firefly_ecm::ESignatureEnvelope` (mapped status, provider-side id,
`sentAt`/`signedAt` timestamps, and the per-signer breakdown); `recipients`
lists the signer states from the same envelope resource (Logalty embeds signer
detail in the envelope rather than a dedicated recipients endpoint); `download`
returns the signed PDF bytes; `cancel` deletes the envelope.

## Quick start

```rust
use firefly_ecm::{ESignatureProvider, SignatureRequest};
use firefly_ecm_esignature_logalty::RestProvider;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly_ecm::EcmError> {
    let provider = RestProvider::new(
        "https://tenant.logalty.example/api/v1",
        "secret-api-key",
    );
    assert_eq!(provider.name(), "logalty");

    let id = provider
        .create(SignatureRequest {
            document_id: "doc-42".into(),
            signers: vec!["alice@example.com".into()],
            title: "Sign this".into(),
            provider: "logalty".into(),
        })
        .await?;
    let _status = provider.status(&id).await?;
    let _envelope = provider.get(&id).await?;        // full metadata + signers
    let _recipients = provider.recipients(&id).await?;
    let _signed_pdf = provider.download(&id).await?;  // signed PDF bytes
    Ok(())
}
```

`RestProvider::new(api_base, api_key)` sends the API key as the `X-Api-Key`
header on every request. `.with_client(reqwest::Client)` reuses a
caller-provided client for connection pooling, custom timeouts, or TLS.

### Status mapping

| Logalty `status` | `SignatureStatus` |
|---|---|
| `DRAFT`, `SENT`, `PENDING` | `Pending` |
| `SIGNED`, `COMPLETED` | `Signed` |
| `DECLINED` | `Declined` |
| `EXPIRED` | `Expired` |
| _(unknown)_ | `Pending` |

`status` mapping is case-insensitive and exported as `map_status(&str)`.

## Public surface

| Item | Description |
|---|---|
| `RestProvider` | Real Logalty `ESignatureProvider` over `reqwest`; `RestProvider::new(api_base, api_key)`, `.with_client(reqwest::Client)` |
| `RestProvider::recipients(&id)` | `GET /envelopes/{id}` projected to `Vec<SignerState>` |
| `RestProvider::download(&id)` | `GET /envelopes/{id}/document` → signed PDF bytes |
| `map_status(&str)` | Logalty `status` → `SignatureStatus` |
| `VERSION` | Framework version stamp |

## Capability notes

The framework `SignatureStatus` enum has four states; Logalty's `DRAFT`
collapses onto `Pending`. `recipients` and `download` are inherent methods on
`RestProvider` (the `ESignatureProvider` port models `create`/`status`/`cancel`/
`get`); callers holding a concrete `RestProvider` get the richer API, while
callers behind `dyn ESignatureProvider` use the port surface. Logalty exposes
signer detail through the envelope resource (not a separate recipients
endpoint), so `recipients` and `get` share one `GET /envelopes/{id}` fetch.
Every operation calls the real Logalty API — no operation is stubbed.

## Testing

```bash
cargo test -p firefly-ecm-esignature-logalty
```

The REST behavior tests (`tests/rest_test.rs`) spin up an in-process axum mock
on port 0 and assert
both the outbound request the adapter builds (method, path, `X-Api-Key` header,
JSON payload) and how each canned response is parsed into the domain types —
covering `create`, `status`, `get`, `cancel`, `recipients`, and `download`, plus
the `404` → `NotFound`/`None` paths. No network, Docker, or real Logalty is
involved.

> **SaaS note:** Logalty is a hosted service with no local emulator, so the
> integration is exercised against a high-fidelity in-process mock that
> reproduces Logalty's request/response contract; the production code path is
> the real REST client.
