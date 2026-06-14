# `firefly-ecm-esignature-docusign`

> **Tier:** Adapter · **Status:** Production (DocuSign eSignature REST API v2.1) · **Backing tech:** DocuSign — OAuth bearer token + REST v2.1

## Overview

`firefly-ecm-esignature-docusign` is the DocuSign
[`firefly_ecm::ESignatureProvider`] adapter. `RestProvider` is a **real REST
integration** over [`reqwest`](https://docs.rs/reqwest). Every operation calls
the live DocuSign eSignature REST API v2.1 — there is no stub and no
`not_implemented` path:

| Operation | DocuSign eSignature REST v2.1 call |
|---|---|
| `create` | `POST /v2.1/accounts/{accountId}/envelopes` (status `sent`) |
| `status` / `get` | `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}` |
| `cancel` | `PUT /v2.1/accounts/{accountId}/envelopes/{envelopeId}` (status `voided`) |
| `recipients` | `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}/recipients` |
| `download` | `GET /v2.1/accounts/{accountId}/envelopes/{envelopeId}/documents/combined` |

`create` builds the envelope-create payload and parses the returned
`envelopeId`; `get` projects the envelope resource onto a
`firefly_ecm::ESignatureEnvelope` (mapped status, provider-side id,
`sentDateTime`/`completedDateTime` timestamps, and the per-signer breakdown when
DocuSign inlines `recipients.signers[]`); `recipients` lists the signer states;
`download` returns the combined signed PDF bytes; `cancel` voids the envelope.

## Quick start

```rust
use firefly_ecm::{ESignatureProvider, SignatureRequest};
use firefly_ecm_esignature_docusign::RestProvider;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly_ecm::EcmError> {
    let provider = RestProvider::new(
        "https://demo.docusign.net/restapi",
        "account-123",
        "bearer-token",
    );
    assert_eq!(provider.name(), "docusign");

    let id = provider
        .create(SignatureRequest {
            document_id: "doc-1".into(),
            signers: vec!["alice@example.com".into()],
            title: "Sign please".into(),
            provider: "docusign".into(),
        })
        .await?;
    let _status = provider.status(&id).await?;
    let _envelope = provider.get(&id).await?;       // full metadata + signers
    let _recipients = provider.recipients(&id).await?;
    let _signed_pdf = provider.download(&id).await?; // combined PDF bytes
    Ok(())
}
```

`RestProvider::new(base_url, account_id, access_token)` takes a long-lived
OAuth bearer token (DocuSign JWT-grant token refresh is the caller's
responsibility). `.with_client(reqwest::Client)` reuses a caller-provided client
for connection pooling, custom timeouts, or TLS.

### Status mapping

| DocuSign `status` | `SignatureStatus` |
|---|---|
| `created`, `sent`, `delivered` | `Pending` |
| `completed` | `Signed` |
| `declined`, `voided` | `Declined` |
| `expired` | `Expired` |
| _(unknown)_ | `Pending` |

`status` mapping is case-insensitive and exported as `map_status(&str)`.

## Public surface

| Item | Description |
|---|---|
| `RestProvider` | Real DocuSign `ESignatureProvider` over `reqwest`; `RestProvider::new(base_url, account_id, access_token)`, `.with_client(reqwest::Client)` |
| `RestProvider::recipients(&id)` | `GET .../recipients` → `Vec<SignerState>` |
| `RestProvider::download(&id)` | `GET .../documents/combined` → combined signed PDF bytes |
| `map_status(&str)` | DocuSign envelope `status` → `SignatureStatus` |
| `VERSION` | Framework version stamp |

## Capability notes

The framework `SignatureStatus` enum has four states (`Pending`, `Signed`,
`Declined`, `Expired`); DocuSign's `created` (draft-like) collapses onto
`Pending`. `recipients` and `download` are inherent methods on
`RestProvider` (the `ESignatureProvider` port models `create`/`status`/`cancel`/
`get`); callers holding a concrete `RestProvider` get the richer API, while
callers behind `dyn ESignatureProvider` use the port surface. Every operation
calls the real DocuSign API — no operation is stubbed or unimplemented.

## Testing

```bash
cargo test -p firefly-ecm-esignature-docusign
```

The REST behavior tests (`tests/rest_test.rs`) spin up an in-process axum mock on port 0 and
assert both the outbound request the adapter builds (method, path, auth header,
JSON payload) and how each canned response is parsed into the domain types —
covering `create`, `status`, `get`, `cancel`, `recipients`, and `download`, plus
the `404` → `NotFound`/`None` paths. No network, Docker, or real DocuSign is
involved.

> **SaaS note:** DocuSign is a hosted service with no local emulator, so the
> integration is exercised against a high-fidelity in-process mock that
> reproduces DocuSign's request/response contract; the production code path is
> the real REST client.
