# `firefly-idp-aws-cognito`

> **Tier:** Adapter · **Status:** Stub (port-asserting) · **Backing tech:** AWS Cognito — AWS SDK CognitoIdentityProvider · **Go module:** `idpawscognito`

## Overview

`firefly-idp-aws-cognito` is the placeholder `firefly_idp::Adapter` adapter
for AWS Cognito — AWS SDK CognitoIdentityProvider. The crate and types are
declared, the port implementation compiles, and sentinel-error smoke tests
guard the wire shape — but the SaaS / cloud SDK integration is **not yet
wired**. Every method returns the `not_implemented()` sentinel.

```rust
/// Bytes-equal to the Go module's ErrNotImplemented error value.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpawscognito: not yet implemented";

/// Builds the sentinel as `firefly_idp::Error::Provider(ERR_NOT_IMPLEMENTED)`.
pub fn not_implemented() -> firefly_idp::Error;
```

## Why ship a stub?

* The framework's tier diagram stays correct (no missing module).
* The port boundary stays locked — when the real implementation lands
  in v26.06, no consuming code needs to change.
* The wire contract is exercised end-to-end before the integration
  ships, via the smoke tests that assert the sentinel return.

## Public surface

```rust
pub struct Config {
    pub base_url: String,
    pub realm: String,
    pub client_id: String,
    pub client_secret: String,
    pub tenant: String,
    pub user_pool_id: String,
    pub region: String,
}

pub struct Adapter { /* retains Config */ }

impl Adapter {
    pub fn new(cfg: Config) -> Self;
    pub fn config(&self) -> &Config;
}

impl firefly_idp::Adapter for Adapter { /* every method → not_implemented() */ }
```

`Adapter::name()` returns the stable identifier `"awscognito-stub"`.

## Configuration

`Config`'s fields cover every wiring variable the production adapter
needs (`base_url`, `realm`, `client_id`, `client_secret`, `tenant`,
`user_pool_id`, `region`); they are accepted and retained today so
consuming configuration code stays stable when the implementation lands.

## Quick start

```rust
use firefly_idp_aws_cognito::{not_implemented, Adapter, Config};

let idp = Adapter::new(Config {
    user_pool_id: "eu-west-1_AbCdEfGhI".into(),
    region: "eu-west-1".into(),
    ..Config::default()
});
assert_eq!(firefly_idp::Adapter::name(&idp), "awscognito-stub");

// Until the integration ships, every call fails with the sentinel:
// idp.login("alice", "s3cret").await == Err(not_implemented())
```

## Roadmap

The real implementation is scheduled for **v26.06.x**, mirroring the Go
port's sequencing (see the Go repo's `docs/AUDIT.md` § Roadmap).

## Testing

```bash
cargo test -p firefly-idp-aws-cognito
```

Smoke tests assert (a) compile-time port satisfaction (the struct
implements `firefly_idp::Adapter` and is usable behind
`Arc<dyn Adapter>`), and (b) every method returns the
`not_implemented()` sentinel, whose message is bytes-equal to the Go
module's `ErrNotImplemented`. Once the production adapter ships, these
tests are deleted in favour of integration tests against a real
provider container / mock server.
