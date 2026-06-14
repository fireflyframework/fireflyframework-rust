# `firefly-idp-aws-cognito`

> **Tier:** Adapter · **Status:** Full · **Backing tech:** Cognito Identity Provider JSON API (`X-Amz-Target`) over `reqwest` + a self-contained, KAT-tested SigV4 signer

## Overview

`firefly-idp-aws-cognito` is a real `firefly_idp::Adapter` for AWS Cognito. It
talks directly to the **Cognito Identity Provider JSON API** over `reqwest` —
**no AWS SDK is pulled in**. Requests are `POST`s to
`https://cognito-idp.{region}.amazonaws.com/` carrying an
`X-Amz-Target: AWSCognitoIdentityProviderService.{Action}` header and a JSON
body, exactly the wire protocol the AWS SDK speaks underneath.

Rather than depending on an SDK, the adapter drives the raw JSON API directly and
signs the **admin** calls with a self-contained SigV4 signer (`src/sigv4.rs`),
validated against the official AWS SigV4 Known-Answer-Test vectors.

```rust
use firefly_idp::Adapter as _;
use firefly_idp_aws_cognito::{Adapter, Config};

let idp = Adapter::new(Config {
    user_pool_id: "us-east-1_AbcDef".into(),
    client_id: "app-client-id".into(),
    region: "us-east-1".into(),
    access_key: "AKIA...".into(),
    secret_key: "...".into(),
    ..Config::default()
});
let token = idp.login("alice", "pw").await?;
```

## What it does

* **Client flows (unsigned)** — `InitiateAuth` with `USER_PASSWORD_AUTH`
  (`login`) and `REFRESH_TOKEN_AUTH` (`refresh` / `refresh_full`), `GetUser`
  (`introspect` / `get_user_info` / `validate`), and `GlobalSignOut` (`logout`).
  When the app client has a secret, the computed `SECRET_HASH` is included.
  Because a bare refresh token does not surface the username `SECRET_HASH` is
  keyed on, confidential (client-secret) clients must refresh via
  `refresh_full(refresh_token, username)`; the port-trait `refresh()` only fits
  public clients.
* **Admin calls (SigV4-signed)** — `AdminCreateUser` + `AdminSetUserPassword`
  (`create_user`), `AdminGetUser` (`get_user` / `find_by_username`),
  `AdminUpdateUserAttributes` (`update_user`), `AdminDeleteUser`,
  `ListUsers`, `ListGroups` (`list_roles`), `AdminListGroupsForUser`
  (`get_roles`), `AdminAddUserToGroup` / `AdminRemoveUserFromGroup`
  (`assign_role` / `revoke_role`), and `AdminSetUserPassword`
  (`change_password` / `reset_password`).
* **TOTP MFA (real API)** — `mfa_challenge` calls `AssociateSoftwareToken`
  (unsigned; the `user_id` argument carries the user's access token) and returns
  the TOTP `SecretCode` (in the challenge's `method` as `"TOTP:{secret}"`) plus
  the Cognito `Session` (in `challenge_id`). `mfa_verify` calls
  `VerifySoftwareToken` with that `Session` and the user's 6-digit code (a
  non-`SUCCESS` status maps to `InvalidCredentials`). The admin
  `set_mfa_preference(username, enabled)` helper calls `AdminSetUserMFAPreference`
  (SigV4-signed) to make TOTP the user's preferred factor.

`SECRET_HASH = Base64(HMAC-SHA256(client_secret, username + client_id))`, exposed
as `Adapter::secret_hash`.

## Configuration

```rust
pub struct Config {
    pub base_url: String,      // endpoint host override (default regional Cognito host)
    pub realm: String,         // shared vendor-config field (unused)
    pub client_id: String,     // Cognito app-client id
    pub client_secret: String, // app-client secret (optional; enables SECRET_HASH)
    pub tenant: String,        // shared vendor-config field (unused)
    pub user_pool_id: String,  // Cognito user-pool id
    pub region: String,        // AWS region
    pub access_key: String,    // AWS access key id (signs admin calls)
    pub secret_key: String,    // AWS secret access key (signs admin calls)
}
```

## SigV4 signer (`src/sigv4.rs`)

A from-scratch, dependency-light implementation of header-based AWS Signature
Version 4 built on the workspace `hmac` / `sha2` / `hex` crates. It is validated
against the official `aws4_testsuite` Known-Answer-Test vectors (`get-vanilla`,
`get-vanilla-query`, `post-header-key-sort`, and the derived signing key), so
its output is byte-for-byte identical to AWS's reference signer.

## Testing

```bash
cargo test -p firefly-idp-aws-cognito
```

Unit tests cover the SigV4 KAT vectors and the SECRET_HASH KAT. Behavior tests
(`tests/cognito_behavior.rs`) drive the real `reqwest` path against an in-process
`axum` mock server (port 0, no network, no AWS credentials), asserting the
`X-Amz-Target` action header, the JSON request body, and (for admin calls) the
SigV4 `Authorization` header.
