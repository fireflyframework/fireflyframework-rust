# `firefly-ecm-esignature-adobe-sign`

> **Tier:** Adapter · **Status:** Production (Adobe Acrobat Sign REST API v6) · **Backing tech:** Adobe Sign — OAuth bearer token / integration key + REST v6

## Overview

`firefly-ecm-esignature-adobe-sign` is the Adobe Sign / Adobe Acrobat Sign
[`firefly_ecm::ESignatureProvider`] adapter. `RestProvider` is a **real REST
integration** over [`reqwest`](https://docs.rs/reqwest), porting pyfly's
`AdobeSignESignatureAdapter`. Every operation calls the live Adobe Acrobat Sign
REST API v6 — there is no stub and no `not_implemented` path:

| Operation | Adobe Acrobat Sign REST v6 call |
|---|---|
| `create` | `POST /agreements` (state `IN_PROCESS`) |
| `status` / `get` | `GET /agreements/{agreementId}` |
| `cancel` | `PUT /agreements/{agreementId}/state` (state `CANCELLED`) |
| `recipients` | `GET /agreements/{agreementId}/members` |
| `download` | `GET /agreements/{agreementId}/combinedDocument` |

`create` builds the agreement-create payload from a transient document id and
parses the returned agreement `id`; `get` projects the agreement resource onto a
`firefly_ecm::ESignatureEnvelope` (mapped status, provider-side id, and the
`displayDate`/`createdDate` send timestamp); `recipients` projects the
`participantSets[]` → `memberInfos[]` into signer states; `download` returns the
combined signed PDF bytes; `cancel` transitions the agreement to `CANCELLED`.

## Quick start

```rust
use firefly_ecm::{ESignatureProvider, SignatureRequest};
use firefly_ecm_esignature_adobe_sign::RestProvider;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly_ecm::EcmError> {
    let provider = RestProvider::new(
        "https://api.eu1.adobesign.com/api/rest/v6",
        "integration-key-or-token",
    );
    assert_eq!(provider.name(), "adobe-sign");

    let id = provider
        .create(SignatureRequest {
            document_id: "transient-doc-1".into(),
            signers: vec!["alice@example.com".into()],
            title: "Loan agreement".into(),
            provider: "adobesign".into(),
        })
        .await?;
    let _status = provider.status(&id).await?;
    let _envelope = provider.get(&id).await?;        // full agreement metadata
    let _recipients = provider.recipients(&id).await?;
    let _signed_pdf = provider.download(&id).await?;  // combined PDF bytes
    Ok(())
}
```

`RestProvider::new(api_base, access_token)` accepts an integration key or OAuth
access token, sent as a bearer credential. `.with_client(reqwest::Client)`
reuses a caller-provided client for connection pooling, custom timeouts, or TLS.
The `document_id` is the Adobe **transient document id** obtained from a prior
`POST /transientDocuments` upload.

### Status mapping (pyfly parity)

| Adobe `status` | `SignatureStatus` |
|---|---|
| `OUT_FOR_SIGNATURE`, `WAITING_FOR_MY_SIGNATURE`, `WAITING_FOR_OTHERS`, `DRAFT` | `Pending` |
| `SIGNED`, `COMPLETED` | `Signed` |
| `CANCELLED`, `DECLINED` | `Declined` |
| `EXPIRED` | `Expired` |
| _(unknown)_ | `Pending` |

`status` mapping is case-insensitive and exported as `map_status(&str)`.

## Public surface

| Item | Description |
|---|---|
| `RestProvider` | Real Adobe Sign `ESignatureProvider` over `reqwest`; `RestProvider::new(api_base, access_token)`, `.with_client(reqwest::Client)` |
| `RestProvider::recipients(&id)` | `GET /agreements/{id}/members` → `Vec<SignerState>` |
| `RestProvider::download(&id)` | `GET /agreements/{id}/combinedDocument` → combined signed PDF bytes |
| `map_status(&str)` | Adobe agreement `status` → `SignatureStatus` (pyfly `_map_status` table) |
| `VERSION` | Framework version stamp |

## Capability notes

The framework `SignatureStatus` enum has four states; Adobe's `DRAFT` collapses
onto `Pending`. `recipients` and `download` are inherent methods on
`RestProvider` (the `ESignatureProvider` port models `create`/`status`/`cancel`/
`get`); callers holding a concrete `RestProvider` get the richer API, while
callers behind `dyn ESignatureProvider` use the port surface. The `get`
send-timestamp is sourced from Adobe's `displayDate` (falling back to
`createdDate`); Adobe does not expose a distinct completion timestamp on the
agreement resource, so completion is conveyed via the mapped `Signed` status.
Every operation calls the real Adobe API — no operation is stubbed.

## Testing

```bash
cargo test -p firefly-ecm-esignature-adobe-sign
```

The REST behavior tests (`tests/rest_test.rs`, ported from pyfly's
`test_adobe_sign_behavior.py`) spin up an in-process axum mock on port 0 and
assert both the outbound request the adapter builds (method, path, auth header,
JSON payload) and how each canned response is parsed into the domain types —
covering `create`, `status`, `get`, `cancel`, `recipients`, and `download`, plus
the `404` → `NotFound`/`None` paths. No network, Docker, or real Adobe Sign is
involved.

> **SaaS note:** Adobe Acrobat Sign is a hosted service with no local emulator,
> so the integration is exercised against a high-fidelity in-process mock that
> reproduces Adobe's request/response contract; the production code path is the
> real REST client.
