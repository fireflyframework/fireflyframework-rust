# `firefly-ecm-esignature-logalty`

> **Tier:** Adapter · **Status:** Full (REST) + legacy stub · **Backing tech:** Logalty — EU qualified / eIDAS e-signature, REST + `X-Api-Key`

## Overview

`firefly-ecm-esignature-logalty` is the Logalty
[`firefly_ecm::ESignatureProvider`] adapter (EU qualified / eIDAS e-signature).
`RestProvider` is a **real REST integration** over
[`reqwest`](https://docs.rs/reqwest), porting pyfly's
`LogaltyESignatureAdapter`: it builds the envelope-create payload, parses the
returned `envelopeId`, maps Logalty's `status` strings onto
`firefly_ecm::SignatureStatus`, and deletes envelopes on cancel. Requests
authenticate with the `X-Api-Key` header.

The original contract-only `Provider` stub is **retained for backward
compatibility** with the Go-parity release: every method returns the
`ERR_NOT_IMPLEMENTED` sentinel, byte-for-byte equal to the Go port's
`ErrNotImplemented`:

```rust
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/ecmesignaturelogalty: not yet implemented";
```

New code should prefer `RestProvider`; `Provider` remains for callers that
wired the stub before the REST adapter landed.

## Quick start (REST)

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
    Ok(())
}
```

### Status mapping (pyfly parity)

| Logalty `status` | `SignatureStatus` |
|---|---|
| `DRAFT`, `SENT`, `PENDING` | `Pending` |
| `SIGNED`, `COMPLETED` | `Signed` |
| `DECLINED` | `Declined` |
| `EXPIRED` | `Expired` |
| _(unknown)_ | `Pending` |

## Quick start (legacy stub)

```rust
use firefly_ecm::{ESignatureProvider, SignatureRequest};
use firefly_ecm_esignature_logalty::{is_not_implemented, Config, Provider};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let provider = Provider::new(Config::default());
    assert_eq!(provider.name(), "ecmesignaturelogalty-stub");

    let err = provider.create(SignatureRequest::default()).await.unwrap_err();
    assert!(is_not_implemented(&err));
    assert_eq!(
        err.to_string(),
        "firefly/ecmesignaturelogalty: not yet implemented",
    );
}
```

## Configuration

```rust
pub struct Config {
    // Fields cover every wiring variable the production adapter needs:
    // OAuth2 client_credentials wiring for Logalty.
    pub base_url: String,        // Logalty REST API base URL
    pub client_id: String,       // OAuth2 client identifier
    pub client_secret: String,   // OAuth2 client secret
    pub integration_key: String, // Integration key for the Logalty tenant
    pub user_guid: String,       // GUID of the impersonated user
}
```

The stub stores the configuration untouched (readable via
`Provider::config()`), so consuming code can wire its settings today and swap
in the real adapter without changes.

## Public surface

| Item | Description |
|---|---|
| `RestProvider` | Real Logalty `ESignatureProvider` over `reqwest`; `RestProvider::new(api_base, api_key)`, `.with_client(reqwest::Client)` |
| `map_status(&str)` | Logalty `status` → `SignatureStatus` (pyfly `_map_status` table) |
| `Config` | OAuth2 client_credentials wiring (legacy stub) |
| `Provider` | Legacy port-asserting stub; `Provider::new(cfg)` |
| `ERR_NOT_IMPLEMENTED` | Sentinel message, bytes-equal to Go's `ErrNotImplemented` |
| `not_implemented()` | Builds the sentinel as `EcmError::Provider` |
| `is_not_implemented(&EcmError)` | Analog of Go's `errors.Is(err, ErrNotImplemented)` |
| `VERSION` | Framework version stamp |

## Testing

```bash
cargo test -p firefly-ecm-esignature-logalty
```

The REST behavior tests (`tests/rest_test.rs`, ported from pyfly's
`test_logalty_behavior.py`) spin up an in-process axum mock on port 0 and
assert both the outbound request the adapter builds (method, path, `X-Api-Key`
header, JSON payload) and how each canned response is parsed into the domain
types — no network, Docker, or real Logalty. The legacy stub smoke tests still
assert port satisfaction and the `ERR_NOT_IMPLEMENTED` sentinel for back-compat.
