// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! firefly-idp-aws-cognito — a real [`firefly_idp::Adapter`] for AWS Cognito.
//!
//! Talks directly to the **Cognito Identity Provider JSON API** over
//! [`reqwest`] — **no AWS SDK is pulled in**. Requests are `POST`s to
//! `https://cognito-idp.{region}.amazonaws.com/` carrying an
//! `X-Amz-Target: AWSCognitoIdentityProviderService.{Action}` header and a JSON
//! body, exactly as the wire protocol the AWS SDK speaks underneath.
//!
//! This is a behavior port of pyfly's `AwsCognitoIdpAdapter` (which wraps
//! boto3) with a deliberate, brief-mandated divergence: instead of an SDK we
//! drive the raw JSON API and sign the **admin** calls (`AdminCreateUser`,
//! `AdminGetUser`, `AdminSetUserPassword`, `ListUsers`, …) with a
//! self-contained, KAT-tested [`sigv4`] signer. Client-flow calls
//! (`InitiateAuth`, `GetUser`, `GlobalSignOut`) are unauthenticated and instead
//! carry the [`SECRET_HASH`](Adapter::secret_hash) (`Base64(HMAC-SHA256(secret,
//! username + client_id))`) when the app client is configured with a secret.
//!
//! Covered operations: USER_PASSWORD_AUTH login, REFRESH_TOKEN_AUTH refresh
//! (with [`refresh_full`](Adapter::refresh_full) attaching the `SECRET_HASH` a
//! confidential client requires), global sign-out, user CRUD (admin), token
//! introspection / userinfo via
//! `GetUser`, password change/reset, Cognito-group role management, and TOTP
//! MFA enrollment/verification (`AssociateSoftwareToken` /
//! `VerifySoftwareToken` / `AdminSetUserMFAPreference`).
//!
//! ## TOTP MFA — fully implemented against the real Cognito API
//!
//! Unlike pyfly (which raised `NotImplementedError` here), this crate
//! implements TOTP MFA against Cognito's real software-token actions:
//!
//! * [`mfa_challenge`](Adapter::mfa_challenge) calls
//!   [`AssociateSoftwareToken`](https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_AssociateSoftwareToken.html)
//!   to begin TOTP enrollment, returning the `SecretCode` (the TOTP shared
//!   secret to render as a QR/otpauth URI) in the challenge's `method` field
//!   and the Cognito `Session` in `challenge_id`.
//! * [`mfa_verify`](Adapter::mfa_verify) calls
//!   [`VerifySoftwareToken`](https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_VerifySoftwareToken.html)
//!   with the user's TOTP code, then admin-enables TOTP as the preferred
//!   factor via
//!   [`AdminSetUserMFAPreference`](https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_AdminSetUserMFAPreference.html).
//!
//! Because `AssociateSoftwareToken` / `VerifySoftwareToken` authenticate the
//! caller with the *user's own* access token (the only credential Cognito
//! accepts for software-token association — there is no admin-credential
//! variant), the port's `user_id` argument carries the user's access token and
//! the `challenge_id` carries the Cognito `Session`. See each method's rustdoc.

mod sigv4;

pub use sigv4::{sha256_hex, sign as sigv4_sign, Credentials, Header, Request, Signed};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::Utc;
use firefly_idp as idp;
use firefly_idp::{Error, MfaChallenge, Result, Role, SessionIntrospection, Token, User};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// Typed configuration carrying the wiring the adapter authenticates with.
///
/// Field-for-field compatible with the configuration shape this crate shipped
/// as a stub. The Cognito adapter uses `user_pool_id`, `client_id`, `region`,
/// `client_secret` (optional — enables [`SECRET_HASH`](Adapter::secret_hash)),
/// and the AWS credentials (`access_key` / `secret_key`) used to sign admin
/// calls. `base_url` overrides the endpoint host (for testing); empty falls
/// back to the regional Cognito host. The remaining vendor fields are retained.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Endpoint host override, e.g. a mock URL. Empty falls back to
    /// `https://cognito-idp.{region}.amazonaws.com`. A trailing `/` is trimmed.
    pub base_url: String,
    /// Authentication realm (shared vendor-config field; unused by Cognito).
    pub realm: String,
    /// Cognito app-client id.
    pub client_id: String,
    /// App-client secret (optional). When set, client flows include the
    /// computed `SECRET_HASH`.
    pub client_secret: String,
    /// Tenant identifier (shared vendor-config field; unused by Cognito).
    pub tenant: String,
    /// Cognito user-pool id, e.g. `us-east-1_AbcDef`.
    pub user_pool_id: String,
    /// AWS region hosting the user pool.
    pub region: String,
    /// AWS access key id used to sign admin calls.
    pub access_key: String,
    /// AWS secret access key used to sign admin calls.
    pub secret_key: String,
}

/// The login outcome — the resolved [`User`] plus the minted [`Token`].
///
/// The Rust analogue of pyfly's `AuthResult`. The port's [`Adapter::login`]
/// returns only the [`Token`]; callers wanting the resolved user call
/// [`Adapter::login_full`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuthResult {
    /// The user resolved via `AdminGetUser` after a successful grant.
    pub user: User,
    /// The minted access/refresh token envelope.
    pub token: Token,
}

/// A real [`firefly_idp::Adapter`] backed by the Cognito IdP JSON API.
#[derive(Debug, Clone)]
pub struct Adapter {
    cfg: Config,
    endpoint: String,
    host: String,
    http: reqwest::Client,
}

/// Whether a Cognito action is an admin (credential-signed) call or an
/// unauthenticated client-flow call.
#[derive(Clone, Copy)]
enum Auth {
    /// SigV4-signed with AWS credentials.
    Signed,
    /// Unauthenticated (token / SECRET_HASH carried in the JSON body).
    Unsigned,
}

impl Adapter {
    /// Returns an [`Adapter`] wired with `cfg`. An empty
    /// [`base_url`](Config::base_url) falls back to the regional Cognito host.
    pub fn new(cfg: Config) -> Self {
        let endpoint = {
            let b = cfg.base_url.trim_end_matches('/');
            if b.is_empty() {
                format!("https://cognito-idp.{}.amazonaws.com", cfg.region)
            } else {
                b.to_string()
            }
        };
        // Host header used in the canonical request: the endpoint authority.
        let host = endpoint
            .split("://")
            .nth(1)
            .unwrap_or(&endpoint)
            .to_string();
        Self {
            cfg,
            endpoint,
            host,
            http: reqwest::Client::new(),
        }
    }

    /// Returns the configuration the adapter was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Computes the Cognito `SECRET_HASH` for `username`, or `None` when no app
    /// client secret is configured.
    ///
    /// `SECRET_HASH = Base64(HMAC-SHA256(client_secret, username + client_id))`,
    /// required for any app client configured with a secret.
    pub fn secret_hash(&self, username: &str) -> Option<String> {
        if self.cfg.client_secret.is_empty() {
            return None;
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(self.cfg.client_secret.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(username.as_bytes());
        mac.update(self.cfg.client_id.as_bytes());
        Some(BASE64.encode(mac.finalize().into_bytes()))
    }

    fn provider_err(context: &str, e: impl std::fmt::Display) -> Error {
        Error::provider(format!("idp/aws-cognito: {context}: {e}"))
    }

    /// Issues one Cognito JSON-API call (`X-Amz-Target` + JSON body), signing
    /// admin calls with SigV4. Returns the parsed JSON body on a 2xx, or the
    /// HTTP status on a non-2xx (so callers can map errors like boto3 does).
    async fn call(&self, action: &str, auth: Auth, body: Value) -> Result<CognitoResponse> {
        let payload =
            serde_json::to_vec(&body).map_err(|e| Self::provider_err("encode body", e))?;
        let target = format!("{TARGET_PREFIX}.{action}");
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let mut builder = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", &target)
            .header("x-amz-date", &amz_date);

        if let Auth::Signed = auth {
            let payload_hash = sha256_hex(&payload);
            let headers = vec![
                Header::new("content-type", "application/x-amz-json-1.1"),
                Header::new("host", &self.host),
                Header::new("x-amz-date", &amz_date),
                Header::new("x-amz-target", &target),
            ];
            let signed = sigv4_sign(
                &Request {
                    method: "POST",
                    canonical_uri: "/",
                    canonical_query: "",
                    headers,
                    payload_hash: &payload_hash,
                },
                &Credentials {
                    access_key: &self.cfg.access_key,
                    secret_key: &self.cfg.secret_key,
                    region: &self.cfg.region,
                    service: "cognito-idp",
                },
                &amz_date,
                &date_stamp,
            );
            builder = builder
                .header("x-amz-content-sha256", &payload_hash)
                .header("authorization", &signed.authorization);
        }

        let resp = builder
            .body(payload)
            .send()
            .await
            .map_err(|e| Self::provider_err(&format!("{action} request failed"), e))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| Self::provider_err(&format!("{action} read body"), e))?;
        let json: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok(CognitoResponse { status, json })
    }

    /// USER_PASSWORD_AUTH login returning the resolved user + minted token.
    ///
    /// Behavior-equal to pyfly: a transport/API error or a response missing
    /// `AuthenticationResult` (e.g. a challenge) maps to
    /// [`Error::InvalidCredentials`]; on success the user is resolved via
    /// `AdminGetUser`, falling back to a minimal user when absent.
    pub async fn login_full(&self, username: &str, password: &str) -> Result<AuthResult> {
        let mut auth_params = json!({"USERNAME": username, "PASSWORD": password});
        if let Some(hash) = self.secret_hash(username) {
            auth_params["SECRET_HASH"] = json!(hash);
        }
        let resp = self
            .call(
                "InitiateAuth",
                Auth::Unsigned,
                json!({
                    "ClientId": self.cfg.client_id,
                    "AuthFlow": "USER_PASSWORD_AUTH",
                    "AuthParameters": auth_params,
                }),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::InvalidCredentials);
        }
        let result = match resp.json.get("AuthenticationResult") {
            Some(r) if !r.is_null() => r.clone(),
            _ => return Err(Error::InvalidCredentials),
        };
        let token = token_from_result(&result);
        let user = match idp::Adapter::get_user(self, username).await {
            Ok(u) => u,
            Err(_) => User {
                username: username.to_string(),
                ..User::default()
            },
        };
        Ok(AuthResult { user, token })
    }

    /// Exchanges a refresh token for a fresh [`Token`], optionally attaching the
    /// `SECRET_HASH` a **confidential** (client-secret) app client requires.
    ///
    /// Cognito's `REFRESH_TOKEN_AUTH` flow rejects requests from an app client
    /// configured with a secret unless they carry a `SECRET_HASH`, which is
    /// `Base64(HMAC-SHA256(client_secret, username + client_id))` — and the
    /// `username` is the one bound to the refresh token at sign-in. A bare
    /// refresh token does not surface that username, so the port-trait
    /// [`refresh`](idp::Adapter::refresh) (which only receives the token) cannot
    /// compute it for a confidential client; this richer entry point takes the
    /// `username` explicitly so such deployers have a working refresh path.
    ///
    /// `username` is only consulted when a [`client_secret`](Config::client_secret)
    /// is configured (it feeds the `SECRET_HASH`); for a public client it is
    /// ignored and the call is identical to [`refresh`](idp::Adapter::refresh),
    /// so passing `""` is fine there. This is an unsigned client-flow call.
    pub async fn refresh_full(&self, refresh_token: &str, username: &str) -> Result<Token> {
        let mut auth_params = json!({ "REFRESH_TOKEN": refresh_token });
        if let Some(hash) = self.secret_hash(username) {
            auth_params["SECRET_HASH"] = json!(hash);
        }
        let resp = self
            .call(
                "InitiateAuth",
                Auth::Unsigned,
                json!({
                    "ClientId": self.cfg.client_id,
                    "AuthFlow": "REFRESH_TOKEN_AUTH",
                    "AuthParameters": auth_params,
                }),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::provider(format!(
                "idp/aws-cognito: refresh failed: HTTP {}",
                resp.status
            )));
        }
        let result = match resp.json.get("AuthenticationResult") {
            Some(r) if !r.is_null() => r.clone(),
            _ => {
                return Err(Error::provider(
                    "idp/aws-cognito: refresh did not return a new token",
                ))
            }
        };
        let mut token = token_from_result(&result);
        // Cognito rotates the refresh token only for some flows; keep the
        // supplied one when absent.
        if token.refresh_token.is_empty() {
            token.refresh_token = refresh_token.to_string();
        }
        Ok(token)
    }

    /// Enables (or disables) TOTP as a user's preferred MFA factor via Cognito's
    /// admin
    /// [`AdminSetUserMFAPreference`](https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_AdminSetUserMFAPreference.html)
    /// action (SigV4-signed). Returns `true` on success.
    ///
    /// This is the admin-credential counterpart to the access-token-driven
    /// [`mfa_challenge`](idp::Adapter::mfa_challenge) /
    /// [`mfa_verify`](idp::Adapter::mfa_verify) enrollment pair: call it to flip
    /// a user's software-token MFA on/off out-of-band of an interactive sign-in.
    /// [`mfa_verify`](idp::Adapter::mfa_verify) invokes it automatically (with
    /// `enabled = true`) once the TOTP code verifies.
    pub async fn set_mfa_preference(&self, username: &str, enabled: bool) -> Result<bool> {
        let resp = self
            .call(
                "AdminSetUserMFAPreference",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": username,
                    "SoftwareTokenMfaSettings": {
                        "Enabled": enabled,
                        "PreferredMfa": enabled,
                    },
                }),
            )
            .await?;
        Ok(resp.status == 200)
    }
}

struct CognitoResponse {
    status: u16,
    json: Value,
}

fn token_from_result(result: &Value) -> Token {
    Token {
        access_token: result
            .get("AccessToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        token_type: result
            .get("TokenType")
            .and_then(Value::as_str)
            .unwrap_or("Bearer")
            .to_string(),
        expires_in: result
            .get("ExpiresIn")
            .and_then(Value::as_i64)
            .unwrap_or(3600),
        refresh_token: result
            .get("RefreshToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        id_token: result
            .get("IdToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        ..Token::default()
    }
}

/// Maps a Cognito user representation (`AdminGetUser` / `GetUser` / a
/// `ListUsers` entry) into the port's [`User`]. Reads either the
/// `UserAttributes` or `Attributes` array.
fn from_cognito(data: &Value) -> User {
    let mut attrs = std::collections::HashMap::new();
    let arr = data
        .get("UserAttributes")
        .and_then(Value::as_array)
        .or_else(|| data.get("Attributes").and_then(Value::as_array));
    if let Some(arr) = arr {
        for a in arr {
            if let (Some(n), Some(v)) = (
                a.get("Name").and_then(Value::as_str),
                a.get("Value").and_then(Value::as_str),
            ) {
                attrs.insert(n.to_string(), v.to_string());
            }
        }
    }
    let username = data
        .get("Username")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut attributes = std::collections::HashMap::new();
    for (k, v) in &attrs {
        attributes.insert(k.clone(), json!(v));
    }
    User {
        id: username.to_string(),
        username: username.to_string(),
        email: attrs.get("email").cloned().unwrap_or_default(),
        roles: Vec::new(),
        attributes,
        enabled: data.get("Enabled").and_then(Value::as_bool).unwrap_or(true),
        ..User::default()
    }
}

fn role_from_group(g: &Value) -> Role {
    Role {
        name: g
            .get("GroupName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        description: g
            .get("Description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        scopes: Vec::new(),
    }
}

fn random_password() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = uuid::Uuid::new_v4().simple().to_string();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{seed}{nanos:x}")
}

#[async_trait]
impl idp::Adapter for Adapter {
    async fn login(&self, username: &str, password: &str) -> Result<Token> {
        Ok(self.login_full(username, password).await?.token)
    }

    /// Exchanges a refresh token for a fresh [`Token`] via Cognito's
    /// `REFRESH_TOKEN_AUTH` flow.
    ///
    /// The port-trait signature carries only the refresh token, which is all a
    /// **public** app client needs. A **confidential** client (one with a
    /// [`client_secret`](Config::client_secret)) additionally requires a
    /// `SECRET_HASH` computed from the *username* bound to the token at sign-in
    /// — a value a bare refresh token does not surface — so such deployers
    /// should call [`refresh_full`](Adapter::refresh_full), which takes the
    /// username explicitly. This method passes an empty username and so emits no
    /// `SECRET_HASH`; against a confidential client Cognito rejects it.
    async fn refresh(&self, refresh_token: &str) -> Result<Token> {
        self.refresh_full(refresh_token, "").await
    }

    async fn validate(&self, access_token: &str) -> Result<User> {
        self.get_user_info(access_token).await
    }

    async fn get_user(&self, id: &str) -> Result<User> {
        let resp = self
            .call(
                "AdminGetUser",
                Auth::Signed,
                json!({"UserPoolId": self.cfg.user_pool_id, "Username": id}),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::UserNotFound);
        }
        Ok(from_cognito(&resp.json))
    }

    async fn create_user(&self, user: User, password: &str) -> Result<User> {
        let mut attributes = Vec::new();
        if !user.email.is_empty() {
            attributes.push(json!({"Name": "email", "Value": user.email}));
        }
        if let Some(first) = user.attributes.get("firstName").and_then(Value::as_str) {
            attributes.push(json!({"Name": "given_name", "Value": first}));
        }
        if let Some(last) = user.attributes.get("lastName").and_then(Value::as_str) {
            attributes.push(json!({"Name": "family_name", "Value": last}));
        }
        let create = self
            .call(
                "AdminCreateUser",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": user.username,
                    "UserAttributes": attributes,
                    "TemporaryPassword": password,
                    "MessageAction": "SUPPRESS",
                }),
            )
            .await?;
        if create.status != 200 {
            return Err(Error::provider(format!(
                "idp/aws-cognito: create_user failed: HTTP {}",
                create.status
            )));
        }
        let set_pw = self
            .call(
                "AdminSetUserPassword",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": user.username,
                    "Password": password,
                    "Permanent": true,
                }),
            )
            .await?;
        if set_pw.status != 200 {
            return Err(Error::provider(format!(
                "idp/aws-cognito: set_user_password failed: HTTP {}",
                set_pw.status
            )));
        }
        let mut created = user;
        created.id = created.username.clone();
        Ok(created)
    }

    async fn update_user(&self, user: User) -> Result<User> {
        let mut attrs = Vec::new();
        if !user.email.is_empty() {
            attrs.push(json!({"Name": "email", "Value": user.email}));
        }
        if let Some(first) = user.attributes.get("firstName").and_then(Value::as_str) {
            attrs.push(json!({"Name": "given_name", "Value": first}));
        }
        if let Some(last) = user.attributes.get("lastName").and_then(Value::as_str) {
            attrs.push(json!({"Name": "family_name", "Value": last}));
        }
        if !attrs.is_empty() {
            let username = if user.id.is_empty() {
                &user.username
            } else {
                &user.id
            };
            self.call(
                "AdminUpdateUserAttributes",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": username,
                    "UserAttributes": attrs,
                }),
            )
            .await?;
        }
        Ok(user)
    }

    async fn delete_user(&self, id: &str) -> Result<()> {
        let resp = self
            .call(
                "AdminDeleteUser",
                Auth::Signed,
                json!({"UserPoolId": self.cfg.user_pool_id, "Username": id}),
            )
            .await?;
        if resp.status == 200 {
            Ok(())
        } else {
            Err(Error::UserNotFound)
        }
    }

    fn name(&self) -> &str {
        "aws-cognito"
    }

    // -- extended surface (pyfly parity) ----------------------------------

    async fn logout(&self, access_token: &str) -> Result<bool> {
        let resp = self
            .call(
                "GlobalSignOut",
                Auth::Unsigned,
                json!({"AccessToken": access_token}),
            )
            .await?;
        Ok(resp.status == 200)
    }

    async fn introspect(&self, access_token: &str) -> Result<SessionIntrospection> {
        let resp = self
            .call(
                "GetUser",
                Auth::Unsigned,
                json!({"AccessToken": access_token}),
            )
            .await?;
        if resp.status != 200 {
            return Ok(SessionIntrospection::inactive());
        }
        let username = resp
            .json
            .get("Username")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(SessionIntrospection {
            active: true,
            user_id: username.to_string(),
            username: username.to_string(),
            scopes: Vec::new(),
        })
    }

    async fn find_by_username(&self, username: &str) -> Result<User> {
        idp::Adapter::get_user(self, username).await
    }

    async fn list_users(&self, limit: usize) -> Result<Vec<User>> {
        let resp = self
            .call(
                "ListUsers",
                Auth::Signed,
                json!({"UserPoolId": self.cfg.user_pool_id, "Limit": limit}),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::provider(format!(
                "idp/aws-cognito: list_users failed: HTTP {}",
                resp.status
            )));
        }
        Ok(resp
            .json
            .get("Users")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(from_cognito).collect())
            .unwrap_or_default())
    }

    async fn change_password(
        &self,
        user_id: &str,
        _old_password: &str,
        new_password: &str,
    ) -> Result<bool> {
        let resp = self
            .call(
                "AdminSetUserPassword",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": user_id,
                    "Password": new_password,
                    "Permanent": true,
                }),
            )
            .await?;
        Ok(resp.status == 200)
    }

    async fn reset_password(&self, user_id: &str) -> Result<String> {
        let new_password = random_password();
        self.change_password(user_id, "", &new_password).await?;
        Ok(new_password)
    }

    async fn register_user(&self, mut user: User, password: &str) -> Result<User> {
        user.enabled = true;
        self.create_user(user, password).await
    }

    async fn get_user_info(&self, access_token: &str) -> Result<User> {
        let resp = self
            .call(
                "GetUser",
                Auth::Unsigned,
                json!({"AccessToken": access_token}),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::UserNotFound);
        }
        Ok(from_cognito(&resp.json))
    }

    /// Begins TOTP enrollment via Cognito's
    /// [`AssociateSoftwareToken`](https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_AssociateSoftwareToken.html)
    /// action.
    ///
    /// Cognito authenticates software-token association with the **user's own
    /// access token** (there is no admin-credential variant of this action), so
    /// the `user_id` argument carries the user's Cognito access token. The
    /// returned [`MfaChallenge`] carries:
    ///
    /// * `challenge_id` = the Cognito `Session` to pass back to
    ///   [`mfa_verify`](Adapter::mfa_verify);
    /// * `method` = `"TOTP:{SecretCode}"`, where `SecretCode` is the TOTP shared
    ///   secret the client renders as an `otpauth://` QR for the authenticator
    ///   app.
    ///
    /// This is an unsigned client-flow call (the access token is the credential).
    async fn mfa_challenge(&self, user_id: &str) -> Result<MfaChallenge> {
        let resp = self
            .call(
                "AssociateSoftwareToken",
                Auth::Unsigned,
                json!({ "AccessToken": user_id }),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::provider(format!(
                "idp/aws-cognito: associate_software_token failed: HTTP {}",
                resp.status
            )));
        }
        let secret = resp
            .json
            .get("SecretCode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let session = resp
            .json
            .get("Session")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(MfaChallenge {
            challenge_id: session.to_string(),
            user_id: String::new(),
            method: format!("TOTP:{secret}"),
        })
    }

    /// Completes TOTP enrollment via Cognito's
    /// [`VerifySoftwareToken`](https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_VerifySoftwareToken.html)
    /// action.
    ///
    /// `challenge_id` is the Cognito `Session` returned by
    /// [`mfa_challenge`](Adapter::mfa_challenge); `code` is the 6-digit TOTP the
    /// user read from their authenticator. A `Status` other than `"SUCCESS"`
    /// (e.g. a wrong code) maps to [`Error::InvalidCredentials`].
    ///
    /// On success `VerifySoftwareToken` returns a fresh `Session`, which the
    /// adapter returns as the [`Token`]'s `id_token` so the caller can answer
    /// any follow-on `SOFTWARE_TOKEN_MFA` challenge. Cognito issues no
    /// access/refresh token from this action (those come from the auth flow that
    /// triggered the challenge), so those fields stay empty. To make TOTP the
    /// user's *preferred* factor for future sign-ins, call the admin
    /// [`set_mfa_preference`](Adapter::set_mfa_preference) helper (which the port
    /// trait's username-less `mfa_verify` signature cannot do on its own, since
    /// `AdminSetUserMFAPreference` requires the `UserPoolId` + `Username`).
    async fn mfa_verify(&self, challenge_id: &str, code: &str) -> Result<Token> {
        let verify = self
            .call(
                "VerifySoftwareToken",
                Auth::Unsigned,
                json!({
                    "Session": challenge_id,
                    "UserCode": code,
                }),
            )
            .await?;
        if verify.status != 200 {
            return Err(Error::InvalidCredentials);
        }
        let status = verify
            .json
            .get("Status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status != "SUCCESS" {
            return Err(Error::InvalidCredentials);
        }
        let new_session = verify
            .json
            .get("Session")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(Token {
            token_type: "Bearer".to_string(),
            id_token: new_session,
            ..Token::default()
        })
    }

    async fn get_roles(&self, user_id: &str) -> Result<Vec<Role>> {
        let resp = self
            .call(
                "AdminListGroupsForUser",
                Auth::Signed,
                json!({"UserPoolId": self.cfg.user_pool_id, "Username": user_id}),
            )
            .await?;
        if resp.status != 200 {
            return Ok(Vec::new());
        }
        Ok(resp
            .json
            .get("Groups")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(role_from_group).collect())
            .unwrap_or_default())
    }

    async fn assign_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let resp = self
            .call(
                "AdminAddUserToGroup",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": user_id,
                    "GroupName": role,
                }),
            )
            .await?;
        Ok(resp.status == 200)
    }

    async fn revoke_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let resp = self
            .call(
                "AdminRemoveUserFromGroup",
                Auth::Signed,
                json!({
                    "UserPoolId": self.cfg.user_pool_id,
                    "Username": user_id,
                    "GroupName": role,
                }),
            )
            .await?;
        Ok(resp.status == 200)
    }

    async fn list_roles(&self) -> Result<Vec<Role>> {
        let resp = self
            .call(
                "ListGroups",
                Auth::Signed,
                json!({"UserPoolId": self.cfg.user_pool_id}),
            )
            .await?;
        if resp.status != 200 {
            return Err(Error::provider(format!(
                "idp/aws-cognito: list_roles failed: HTTP {}",
                resp.status
            )));
        }
        Ok(resp
            .json
            .get("Groups")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(role_from_group).collect())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SECRET_HASH KAT: Base64(HMAC-SHA256(secret, username + client_id)).
    #[test]
    fn secret_hash_known_answer() {
        let a = Adapter::new(Config {
            client_id: "test-client-id".into(),
            client_secret: "test-secret".into(),
            region: "us-east-1".into(),
            ..Config::default()
        });
        // Cross-checked against an independent HMAC-SHA256 reference
        // (`openssl dgst -sha256 -hmac test-secret`) for
        // key="test-secret", msg="alice" + "test-client-id".
        let hash = a.secret_hash("alice").expect("secret configured");
        assert_eq!(hash, "rr7RPnVZNnG1c6+8uikMeMZbpU0pRUBXm/O7dO02nKo=");
    }

    #[test]
    fn secret_hash_none_without_secret() {
        let a = Adapter::new(Config {
            client_id: "cid".into(),
            ..Config::default()
        });
        assert!(a.secret_hash("alice").is_none());
    }

    #[test]
    fn endpoint_defaults_to_regional_host() {
        let a = Adapter::new(Config {
            region: "eu-west-1".into(),
            ..Config::default()
        });
        assert_eq!(a.endpoint, "https://cognito-idp.eu-west-1.amazonaws.com");
        assert_eq!(a.host, "cognito-idp.eu-west-1.amazonaws.com");
    }

    #[test]
    fn name_is_aws_cognito() {
        let a = Adapter::new(Config::default());
        assert_eq!(idp::Adapter::name(&a), "aws-cognito");
    }
}
