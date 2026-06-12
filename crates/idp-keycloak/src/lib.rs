//! firefly-idp-keycloak — a real [`firefly_idp::Adapter`] for Keycloak.
//!
//! Talks to a Keycloak server's REST API over [`reqwest`] — no Keycloak SDK is
//! pulled in. It is a behavior-for-behavior port of pyfly's
//! `KeycloakIdpAdapter`, covering:
//!
//! * **Admin grant caching** — the `client_credentials` admin token is cached
//!   with an expiry margin (`max(expires_in - 10, 1)` seconds) so every later
//!   admin call reuses a live token but never one about to expire (mirroring
//!   pyfly's monotonic deadline).
//! * **User CRUD** against `/admin/realms/{realm}/users` (create parses the
//!   `Location` header tail for the new id; get/find/update/delete/list).
//! * **OIDC flows** against `/realms/{realm}/protocol/openid-connect/*` —
//!   password-grant login, refresh, token introspection, logout, and the
//!   userinfo lookup.
//! * **Reset / change password** via the admin `reset-password` endpoint.
//! * **Realm role-mappings** — assign/revoke/list/get roles.
//!
//! Per pyfly, [`mfa_challenge`](Adapter::mfa_challenge) /
//! [`mfa_verify`](Adapter::mfa_verify) stay sentinel returns
//! ([`ERR_NOT_IMPLEMENTED`]) because Keycloak performs MFA server-side during
//! the browser auth flow.
//!
//! # Wire compatibility
//!
//! The outbound request shapes (URLs, verbs, form/JSON bodies, auth headers)
//! are byte-for-byte the same as the pyfly adapter, and are asserted against an
//! in-process [`axum`](https://docs.rs/axum) mock server in the test suite.
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_idp::Adapter as _;
//! use firefly_idp_keycloak::{Adapter, Config};
//!
//! # async fn run() -> firefly_idp::Result<()> {
//! let idp = Adapter::new(Config {
//!     base_url: "https://keycloak.example.com".into(),
//!     realm: "firefly".into(),
//!     client_id: "admin-cli".into(),
//!     client_secret: "s3cret".into(),
//!     ..Config::default()
//! });
//! let token = idp.login("alice", "pw").await?;
//! println!("access token: {}", token.access_token);
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use firefly_idp as idp;
use firefly_idp::{Error, MfaChallenge, Result, Role, SessionIntrospection, Token, User};
use serde_json::{json, Value};
use tokio::sync::Mutex;

/// Wire-stable sentinel returned by the operations Keycloak performs
/// server-side ([`Adapter::mfa_challenge`] / [`Adapter::mfa_verify`]).
///
/// Retained verbatim from the contract-only stub this crate replaced, so any
/// code that matched against it keeps compiling. Bytes-equal to the Go module's
/// `idpkeycloak.ErrNotImplemented`.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpkeycloak: not yet implemented";

/// Builds the [`ERR_NOT_IMPLEMENTED`] sentinel as an [`idp::Error::Provider`].
///
/// Mirrors pyfly's `KeycloakIdpAdapter.mfa_challenge`/`mfa_verify` raising
/// `NotImplementedError` because Keycloak runs MFA server-side.
pub fn not_implemented() -> idp::Error {
    idp::Error::Provider(ERR_NOT_IMPLEMENTED.to_string())
}

/// Typed configuration carrying the wiring the adapter authenticates with.
///
/// Field-for-field compatible with the configuration shape this crate shipped
/// as a stub: the Keycloak adapter uses `base_url`, `realm`, `client_id`,
/// `client_secret`, and `verify_ssl`; the remaining vendor fields (`tenant`,
/// `user_pool_id`, `region`) are retained so the configuration surface stays
/// uniform across the IdP adapter family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Keycloak server base URL, e.g. `https://keycloak.example.com`. A
    /// trailing `/` is trimmed at construction.
    pub base_url: String,
    /// Keycloak realm the adapter authenticates against.
    pub realm: String,
    /// OIDC client identifier registered in the realm.
    pub client_id: String,
    /// OIDC client secret for confidential-client flows.
    pub client_secret: String,
    /// Whether to verify TLS certificates (default `true`).
    pub verify_ssl: bool,
    /// Vendor tenant identifier (used by sibling adapters; unused here).
    pub tenant: String,
    /// Vendor user-pool identifier (used by sibling adapters; unused here).
    pub user_pool_id: String,
    /// Vendor region (used by sibling adapters; unused here).
    pub region: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            realm: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            verify_ssl: true,
            tenant: String::new(),
            user_pool_id: String::new(),
            region: String::new(),
        }
    }
}

/// The login outcome — the resolved [`User`] plus the minted [`Token`].
///
/// The Rust analogue of pyfly's `AuthResult`. The port's [`Adapter::login`]
/// returns only the [`Token`] (the stateless OIDC envelope), so callers that
/// also want the resolved user call [`Adapter::login_full`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuthResult {
    /// The user resolved from the realm after a successful grant.
    pub user: User,
    /// The minted access/refresh token envelope.
    pub token: Token,
}

/// A real [`firefly_idp::Adapter`] backed by a live Keycloak server.
#[derive(Debug, Clone)]
pub struct Adapter {
    cfg: Config,
    http: reqwest::Client,
    admin_token: Arc<Mutex<Option<CachedToken>>>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    deadline: Instant,
}

impl Adapter {
    /// Returns an [`Adapter`] wired with `cfg`.
    ///
    /// The trailing slash of `base_url` is trimmed. The shared [`reqwest`]
    /// client honours [`Config::verify_ssl`]; building it cannot realistically
    /// fail, so a misconfiguration falls back to the default client.
    pub fn new(cfg: Config) -> Self {
        let mut cfg = cfg;
        cfg.base_url = cfg.base_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(!cfg.verify_ssl)
            .build()
            .unwrap_or_default();
        Self {
            cfg,
            http,
            admin_token: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the configuration the adapter was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn admin_path(&self) -> String {
        format!("{}/admin/realms/{}", self.cfg.base_url, self.cfg.realm)
    }

    fn token_url(&self) -> String {
        format!(
            "{}/realms/{}/protocol/openid-connect/token",
            self.cfg.base_url, self.cfg.realm
        )
    }

    fn provider_err(context: &str, e: impl std::fmt::Display) -> Error {
        Error::provider(format!("idp/keycloak: {context}: {e}"))
    }

    /// Returns a live admin bearer token, fetching (and caching) a fresh
    /// `client_credentials` grant when the cache is empty or within the safety
    /// margin of expiry.
    async fn admin_token(&self) -> Result<String> {
        {
            let guard = self.admin_token.lock().await;
            if let Some(cached) = guard.as_ref() {
                if Instant::now() < cached.deadline {
                    return Ok(cached.value.clone());
                }
            }
        }

        let resp = self
            .http
            .post(self.token_url())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Self::provider_err("admin token request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: admin token grant failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("admin token decode failed", e))?;
        let access = payload
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::provider("idp/keycloak: admin token response missing access_token")
            })?
            .to_string();
        let expires_in = payload
            .get("expires_in")
            .and_then(Value::as_f64)
            .unwrap_or(60.0);
        // Refresh margin: re-fetch a touch before expiry (max(expires_in-10,1)s).
        let margin = (expires_in - 10.0).max(1.0);
        let deadline = Instant::now() + Duration::from_secs_f64(margin);

        let mut guard = self.admin_token.lock().await;
        *guard = Some(CachedToken {
            value: access.clone(),
            deadline,
        });
        Ok(access)
    }

    /// Password-grant login returning the resolved user and minted token.
    ///
    /// Behavior-equal to pyfly's `KeycloakIdpAdapter.login`: a non-200 token
    /// response maps to [`Error::InvalidCredentials`] (and no user-lookup
    /// follow-up is attempted); on success the username is resolved via
    /// [`find_by_username`](Adapter::find_by_username), falling back to a
    /// minimal user when absent.
    pub async fn login_full(&self, username: &str, password: &str) -> Result<AuthResult> {
        let resp = self
            .http
            .post(self.token_url())
            .form(&[
                ("grant_type", "password"),
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
                ("username", username),
                ("password", password),
            ])
            .send()
            .await
            .map_err(|e| Self::provider_err("login request failed", e))?;
        if resp.status().as_u16() != 200 {
            return Err(Error::InvalidCredentials);
        }
        let tokens: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("login decode failed", e))?;
        let token = token_from_grant(&tokens);
        let user = match idp::Adapter::find_by_username(self, username).await {
            Ok(u) => u,
            Err(Error::UserNotFound) => User {
                username: username.to_string(),
                ..User::default()
            },
            Err(e) => return Err(e),
        };
        Ok(AuthResult { user, token })
    }
}

fn token_from_grant(tokens: &Value) -> Token {
    Token {
        access_token: tokens
            .get("access_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        token_type: tokens
            .get("token_type")
            .and_then(Value::as_str)
            .unwrap_or("Bearer")
            .to_string(),
        expires_in: tokens
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3600),
        refresh_token: tokens
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        id_token: tokens
            .get("id_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        scope: tokens
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        ..Token::default()
    }
}

/// Maps a Keycloak user representation into the port's [`User`].
fn from_kc(data: &Value) -> User {
    let attributes = data
        .get("attributes")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    User {
        id: data
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        username: data
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        email: data
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        roles: Vec::new(),
        attributes,
        enabled: data.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        ..User::default()
    }
}

fn role_from_kc(data: &Value) -> Role {
    Role {
        name: data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        description: data
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        scopes: Vec::new(),
    }
}

fn random_password() -> String {
    // URL-safe-ish random token (~22 base64url chars from 16 bytes), matching
    // pyfly's `secrets.token_urlsafe(16)` shape without adding a dependency.
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = uuid_like();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{seed}{nanos:x}")
}

fn uuid_like() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[async_trait]
impl idp::Adapter for Adapter {
    async fn login(&self, username: &str, password: &str) -> Result<Token> {
        Ok(self.login_full(username, password).await?.token)
    }

    async fn refresh(&self, refresh_token: &str) -> Result<Token> {
        let resp = self
            .http
            .post(self.token_url())
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|e| Self::provider_err("refresh request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: refresh failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let tokens: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("refresh decode failed", e))?;
        let mut token = token_from_grant(&tokens);
        // Keycloak rotates refresh tokens; keep the supplied one when absent.
        if token.refresh_token.is_empty() {
            token.refresh_token = refresh_token.to_string();
        }
        Ok(token)
    }

    async fn validate(&self, access_token: &str) -> Result<User> {
        // Validation resolves the token to its owner via the userinfo endpoint.
        self.get_user_info(access_token).await
    }

    async fn get_user(&self, id: &str) -> Result<User> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/users/{id}", self.admin_path()))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("get_user request failed", e))?;
        if resp.status().as_u16() == 404 {
            return Err(Error::UserNotFound);
        }
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: get_user failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("get_user decode failed", e))?;
        Ok(from_kc(&data))
    }

    async fn create_user(&self, user: User, password: &str) -> Result<User> {
        let token = self.admin_token().await?;
        let payload = json!({
            "username": user.username,
            "email": user.email,
            "enabled": user.enabled,
            "emailVerified": user.attributes.get("emailVerified").and_then(Value::as_bool).unwrap_or(false),
            "firstName": user.attributes.get("firstName").and_then(Value::as_str).unwrap_or_default(),
            "lastName": user.attributes.get("lastName").and_then(Value::as_str).unwrap_or_default(),
            "credentials": [{"type": "password", "value": password, "temporary": false}],
            "attributes": user.attributes,
        });
        let resp = self
            .http
            .post(format!("{}/users", self.admin_path()))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| Self::provider_err("create_user request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: create_user failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let mut created = user;
        if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
            if let Ok(loc) = loc.to_str() {
                if let Some(id) = loc.rsplit('/').next() {
                    if !id.is_empty() {
                        created.id = id.to_string();
                    }
                }
            }
        }
        Ok(created)
    }

    async fn update_user(&self, user: User) -> Result<User> {
        let token = self.admin_token().await?;
        let payload = json!({
            "email": user.email,
            "enabled": user.enabled,
            "emailVerified": user.attributes.get("emailVerified").and_then(Value::as_bool).unwrap_or(false),
            "firstName": user.attributes.get("firstName").and_then(Value::as_str).unwrap_or_default(),
            "lastName": user.attributes.get("lastName").and_then(Value::as_str).unwrap_or_default(),
            "attributes": user.attributes,
        });
        let resp = self
            .http
            .put(format!("{}/users/{}", self.admin_path(), user.id))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| Self::provider_err("update_user request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: update_user failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        Ok(user)
    }

    async fn delete_user(&self, id: &str) -> Result<()> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .delete(format!("{}/users/{id}", self.admin_path()))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("delete_user request failed", e))?;
        let code = resp.status().as_u16();
        if code == 200 || code == 204 {
            Ok(())
        } else if code == 404 {
            Err(Error::UserNotFound)
        } else {
            Err(Error::provider(format!(
                "idp/keycloak: delete_user failed: HTTP {code}"
            )))
        }
    }

    fn name(&self) -> &str {
        "keycloak"
    }

    // -- extended surface (pyfly parity) ----------------------------------

    async fn logout(&self, access_token: &str) -> Result<bool> {
        let resp = self
            .http
            .post(format!(
                "{}/realms/{}/protocol/openid-connect/logout",
                self.cfg.base_url, self.cfg.realm
            ))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| Self::provider_err("logout request failed", e))?;
        let code = resp.status().as_u16();
        Ok(code == 200 || code == 204)
    }

    async fn introspect(&self, access_token: &str) -> Result<SessionIntrospection> {
        let resp = self
            .http
            .post(format!(
                "{}/realms/{}/protocol/openid-connect/token/introspect",
                self.cfg.base_url, self.cfg.realm
            ))
            .form(&[
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
                ("token", access_token),
            ])
            .send()
            .await
            .map_err(|e| Self::provider_err("introspect request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: introspect failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("introspect decode failed", e))?;
        let scopes = data
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Ok(SessionIntrospection {
            active: data.get("active").and_then(Value::as_bool).unwrap_or(false),
            user_id: data
                .get("sub")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            username: data
                .get("preferred_username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            scopes,
        })
    }

    async fn find_by_username(&self, username: &str) -> Result<User> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/users", self.admin_path()))
            .bearer_auth(&token)
            .query(&[("username", username), ("exact", "true")])
            .send()
            .await
            .map_err(|e| Self::provider_err("find_by_username request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: find_by_username failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let arr: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("find_by_username decode failed", e))?;
        arr.as_array()
            .and_then(|a| a.first())
            .map(from_kc)
            .ok_or(Error::UserNotFound)
    }

    async fn list_users(&self, limit: usize) -> Result<Vec<User>> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/users", self.admin_path()))
            .bearer_auth(&token)
            .query(&[("max", limit.to_string())])
            .send()
            .await
            .map_err(|e| Self::provider_err("list_users request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: list_users failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let arr: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("list_users decode failed", e))?;
        Ok(arr
            .as_array()
            .map(|a| a.iter().map(from_kc).collect())
            .unwrap_or_default())
    }

    async fn change_password(
        &self,
        user_id: &str,
        _old_password: &str,
        new_password: &str,
    ) -> Result<bool> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .put(format!(
                "{}/users/{user_id}/reset-password",
                self.admin_path()
            ))
            .bearer_auth(&token)
            .json(&json!({"type": "password", "value": new_password, "temporary": false}))
            .send()
            .await
            .map_err(|e| Self::provider_err("change_password request failed", e))?;
        let code = resp.status().as_u16();
        Ok(code == 200 || code == 204)
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
            .http
            .get(format!(
                "{}/realms/{}/protocol/openid-connect/userinfo",
                self.cfg.base_url, self.cfg.realm
            ))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| Self::provider_err("get_user_info request failed", e))?;
        if resp.status().as_u16() != 200 {
            return Err(Error::UserNotFound);
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("get_user_info decode failed", e))?;
        Ok(User {
            id: data
                .get("sub")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            username: data
                .get("preferred_username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            email: data
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            ..User::default()
        })
    }

    async fn mfa_challenge(&self, _user_id: &str) -> Result<MfaChallenge> {
        Err(not_implemented())
    }

    async fn mfa_verify(&self, _challenge_id: &str, _code: &str) -> Result<Token> {
        Err(not_implemented())
    }

    async fn get_roles(&self, user_id: &str) -> Result<Vec<Role>> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!(
                "{}/users/{user_id}/role-mappings/realm",
                self.admin_path()
            ))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("get_roles request failed", e))?;
        if resp.status().as_u16() != 200 {
            return Ok(Vec::new());
        }
        let arr: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("get_roles decode failed", e))?;
        Ok(arr
            .as_array()
            .map(|a| a.iter().map(role_from_kc).collect())
            .unwrap_or_default())
    }

    async fn assign_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let token = self.admin_token().await?;
        let lookup = self
            .http
            .get(format!("{}/roles/{role}", self.admin_path()))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("assign_role lookup failed", e))?;
        if lookup.status().as_u16() != 200 {
            return Ok(false);
        }
        let role_obj: Value = lookup
            .json()
            .await
            .map_err(|e| Self::provider_err("assign_role decode failed", e))?;
        let resp = self
            .http
            .post(format!(
                "{}/users/{user_id}/role-mappings/realm",
                self.admin_path()
            ))
            .bearer_auth(&token)
            .json(&json!([role_obj]))
            .send()
            .await
            .map_err(|e| Self::provider_err("assign_role request failed", e))?;
        let code = resp.status().as_u16();
        Ok(code == 200 || code == 204)
    }

    async fn revoke_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let token = self.admin_token().await?;
        let lookup = self
            .http
            .get(format!("{}/roles/{role}", self.admin_path()))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("revoke_role lookup failed", e))?;
        if lookup.status().as_u16() != 200 {
            return Ok(false);
        }
        let role_obj: Value = lookup
            .json()
            .await
            .map_err(|e| Self::provider_err("revoke_role decode failed", e))?;
        let resp = self
            .http
            .request(
                reqwest::Method::DELETE,
                format!("{}/users/{user_id}/role-mappings/realm", self.admin_path()),
            )
            .bearer_auth(&token)
            .json(&json!([role_obj]))
            .send()
            .await
            .map_err(|e| Self::provider_err("revoke_role request failed", e))?;
        let code = resp.status().as_u16();
        Ok(code == 200 || code == 204)
    }

    async fn list_roles(&self) -> Result<Vec<Role>> {
        let token = self.admin_token().await?;
        let resp = self
            .http
            .get(format!("{}/roles", self.admin_path()))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("list_roles request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/keycloak: list_roles failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let arr: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("list_roles decode failed", e))?;
        Ok(arr
            .as_array()
            .map(|a| a.iter().map(role_from_kc).collect())
            .unwrap_or_default())
    }
}
