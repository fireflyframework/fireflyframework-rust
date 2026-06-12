# `firefly-ecm-esignature-adobe-sign`

> **Tier:** Adapter · **Status:** Full (REST v6) + legacy stub · **Backing tech:** Adobe Sign — Bearer-token + REST v6

## Overview

`firefly-ecm-esignature-adobe-sign` is the Adobe Sign / Adobe Acrobat Sign
[`firefly_ecm::ESignatureProvider`] adapter. `RestProvider` is a **real REST
integration** over [`reqwest`](https://docs.rs/reqwest), porting pyfly's
`AdobeSignESignatureAdapter`: it builds the agreement-create payload, parses
the returned agreement `id`, maps Adobe's agreement `status` strings onto
`firefly_ecm::SignatureStatus`, and cancels agreements via the `/state`
endpoint.

The original contract-only `Provider` stub is **retained for backward
compatibility** with the Go-parity release: every method returns the
`ERR_NOT_IMPLEMENTED` sentinel, byte-for-byte equal to the Go port's
`ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmesignatureadobesign: not yet implemented";
```

New code should prefer `RestProvider`; `Provider` remains for callers that
wired the stub before the REST adapter landed.

## Quick start (REST)

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
    Ok(())
}
```

### Status mapping (pyfly parity)

| Adobe `status` | `SignatureStatus` |
|---|---|
| `OUT_FOR_SIGNATURE`, `WAITING_FOR_MY_SIGNATURE`, `DRAFT` | `Pending` |
| `SIGNED`, `COMPLETED` | `Signed` |
| `CANCELLED`, `DECLINED` | `Declined` |
| `EXPIRED` | `Expired` |
| _(unknown)_ | `Pending` |

## Quick start (legacy stub)

```rust
use firefly_ecm::{ESignatureProvider, SignatureRequest};
use firefly_ecm_esignature_adobe_sign::{is_not_implemented, Config, Provider};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let provider = Provider::new(Config::default());
    assert_eq!(provider.name(), "ecmesignatureadobesign-stub");

    let err = provider.create(SignatureRequest::default()).await.unwrap_err();
    assert!(is_not_implemented(&err));
    assert_eq!(
        err.to_string(),
        "firefly/ecmesignatureadobesign: not yet implemented",
    );
}
```

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs:
    // OAuth2 / JWT-grant wiring for Adobe Sign OAuth2 refresh-token + REST v6.
    pub base_url: String,        // e.g. https://api.eu1.adobesign.com/api/rest/v6
    pub client_id: String,       // OAuth2 client identifier
    pub client_secret: String,   // OAuth2 client secret
    pub integration_key: String, // integration key (OAuth2 grant)
    pub user_guid: String,       // GUID of the impersonated user
}
```

The stub stores the configuration untouched (readable via
`Provider::config()`), so consuming code can wire its settings today and swap
in the real adapter without changes.

## Public surface

| Item | Description |
|---|---|
| `RestProvider` | Real Adobe Sign `ESignatureProvider` over `reqwest`; `RestProvider::new(api_base, access_token)`, `.with_client(reqwest::Client)` |
| `map_status(&str)` | Adobe agreement `status` → `SignatureStatus` (pyfly `_map_status` table) |
| `Config` | OAuth2 / JWT-grant wiring (legacy stub) |
| `Provider` | Legacy port-asserting stub; `Provider::new(cfg)` |
| `ERR_NOT_IMPLEMENTED` | Sentinel message, bytes-equal to Go's `ErrNotImplemented` |
| `not_implemented()` | Builds the sentinel as `EcmError::Provider` |
| `is_not_implemented(&EcmError)` | Analog of Go's `errors.Is(err, ErrNotImplemented)` |
| `VERSION` | Framework version stamp |

## Testing

```bash
cargo test -p firefly-ecm-esignature-adobe-sign
```

The REST behavior tests (`tests/rest_test.rs`, ported from pyfly's
`test_adobe_sign_behavior.py`) spin up an in-process axum mock on port 0 and
assert both the outbound request the adapter builds (method, path, auth header,
JSON payload) and how each canned response is parsed into the domain types — no
network, Docker, or real Adobe Sign. The legacy stub smoke tests still assert
port satisfaction and the `ERR_NOT_IMPLEMENTED` sentinel for back-compat.
