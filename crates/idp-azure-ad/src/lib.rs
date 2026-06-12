//! firefly-idp-azure-ad — a real [`firefly_idp::Adapter`] for Azure AD / Entra ID.
//!
//! Talks to the Microsoft Graph `v1.0` API and the
//! `login.microsoftonline.com` token endpoint over [`reqwest`] — no MSAL or
//! Azure SDK is pulled in. It is a behavior-for-behavior port of pyfly's
//! `AzureAdIdpAdapter`, covering:
//!
//! * **App-token caching** — the `client_credentials` Graph app token is
//!   fetched once and cached (mirroring pyfly).
//! * **ROPC login** — the resource-owner password-credentials grant against
//!   `https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token`, then a
//!   user lookup; a non-200 token response maps to
//!   [`Error::InvalidCredentials`](firefly_idp::Error::InvalidCredentials).
//! * **User CRUD** against `/users` (create POSTs the full profile +
//!   `passwordProfile` and captures the returned id; get/find/update/delete/
//!   list). [`find_by_username`](Adapter::find_by_username) delegates to
//!   [`get_user`](Adapter::get_user) (Azure resolves the UPN as the id).
//! * **`/me` introspection / userinfo** with a delegated access token.
//! * **`passwordProfile` patch** for change/reset password.
//! * **Groups-as-roles** — assign/revoke via `/groups/{id}/members/$ref`,
//!   list via `/groups`, and `get_roles` via `/users/{id}/memberOf`.
//!
//! Per pyfly, [`mfa_challenge`](Adapter::mfa_challenge) /
//! [`mfa_verify`](Adapter::mfa_verify) stay sentinel returns
//! ([`ERR_NOT_IMPLEMENTED`]) because Azure AD manages MFA natively via
//! Conditional Access policies.
//!
//! # Endpoint overrides
//!
//! Production defaults are the public Microsoft hosts. For testing against an
//! in-process mock, set [`Config::graph_base_url`] (Graph host) and
//! [`Config::base_url`] (login authority host); empty values fall back to the
//! public hosts.

use std::sync::Arc;

use async_trait::async_trait;
use firefly_idp as idp;
use firefly_idp::{Error, MfaChallenge, Result, Role, SessionIntrospection, Token, User};
use serde_json::{json, Value};
use tokio::sync::Mutex;

/// The public Microsoft Graph `v1.0` base URL.
pub const GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";
/// The public Azure AD login authority host.
pub const LOGIN_BASE_URL: &str = "https://login.microsoftonline.com";
/// The default Graph token scope.
pub const DEFAULT_SCOPE: &str = "https://graph.microsoft.com/.default";

/// Wire-stable sentinel returned by the operations Azure AD performs natively
/// ([`Adapter::mfa_challenge`] / [`Adapter::mfa_verify`]).
///
/// Retained verbatim from the contract-only stub this crate replaced.
pub const ERR_NOT_IMPLEMENTED: &str = "firefly/idpazuread: not yet implemented";

/// Builds the [`ERR_NOT_IMPLEMENTED`] sentinel as an [`Error::Provider`].
///
/// Mirrors pyfly's `AzureAdIdpAdapter.mfa_challenge`/`mfa_verify` raising
/// `NotImplementedError` because Azure AD runs MFA via Conditional Access.
pub fn not_implemented() -> Error {
    Error::provider(ERR_NOT_IMPLEMENTED)
}

/// Typed configuration carrying the wiring the adapter authenticates with.
///
/// Field-for-field compatible with the configuration shape this crate shipped
/// as a stub. The Azure AD adapter uses `tenant`, `client_id`, `client_secret`,
/// and `scope`; `graph_base_url` / `base_url` allow overriding the Graph and
/// login hosts (defaulting to the public Microsoft hosts when empty); the
/// remaining vendor fields are retained for a uniform configuration surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Login authority host override (defaults to [`LOGIN_BASE_URL`] when
    /// empty). A trailing `/` is trimmed at construction.
    pub base_url: String,
    /// Microsoft Graph base URL override (defaults to [`GRAPH_BASE_URL`] when
    /// empty). A trailing `/` is trimmed at construction.
    pub graph_base_url: String,
    /// Realm / domain (shared vendor-config field; unused by Azure AD).
    pub realm: String,
    /// OAuth2 client (application) id.
    pub client_id: String,
    /// OAuth2 client secret.
    pub client_secret: String,
    /// Azure AD tenant (directory) id.
    pub tenant: String,
    /// Token scope (defaults to [`DEFAULT_SCOPE`] when empty).
    pub scope: String,
    /// User-pool id (shared vendor-config field; unused by Azure AD).
    pub user_pool_id: String,
    /// Cloud region (shared vendor-config field; unused by Azure AD).
    pub region: String,
}

/// The login outcome — the resolved [`User`] plus the minted [`Token`].
///
/// The Rust analogue of pyfly's `AuthResult`. The port's [`Adapter::login`]
/// returns only the [`Token`]; callers wanting the resolved user call
/// [`Adapter::login_full`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuthResult {
    /// The user resolved from Graph after a successful grant.
    pub user: User,
    /// The minted access/refresh token envelope.
    pub token: Token,
}

/// A real [`firefly_idp::Adapter`] backed by Microsoft Graph + Azure AD.
#[derive(Debug, Clone)]
pub struct Adapter {
    cfg: Config,
    graph: String,
    login: String,
    scope: String,
    http: reqwest::Client,
    app_token: Arc<Mutex<Option<String>>>,
}

impl Adapter {
    /// Returns an [`Adapter`] wired with `cfg`. Empty host/scope fields fall
    /// back to the public Microsoft defaults; trailing slashes are trimmed.
    pub fn new(cfg: Config) -> Self {
        let graph = {
            let g = cfg.graph_base_url.trim_end_matches('/');
            if g.is_empty() {
                GRAPH_BASE_URL.to_string()
            } else {
                g.to_string()
            }
        };
        let login = {
            let l = cfg.base_url.trim_end_matches('/');
            if l.is_empty() {
                LOGIN_BASE_URL.to_string()
            } else {
                l.to_string()
            }
        };
        let scope = if cfg.scope.is_empty() {
            DEFAULT_SCOPE.to_string()
        } else {
            cfg.scope.clone()
        };
        Self {
            cfg,
            graph,
            login,
            scope,
            http: reqwest::Client::new(),
            app_token: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the configuration the adapter was constructed with.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn token_url(&self) -> String {
        format!("{}/{}/oauth2/v2.0/token", self.login, self.cfg.tenant)
    }

    fn provider_err(context: &str, e: impl std::fmt::Display) -> Error {
        Error::provider(format!("idp/azure-ad: {context}: {e}"))
    }

    /// Returns a cached Graph app token, fetching a `client_credentials` grant
    /// on first use (mirroring pyfly's one-shot cache).
    async fn app_token(&self) -> Result<String> {
        {
            let guard = self.app_token.lock().await;
            if let Some(tok) = guard.as_ref() {
                return Ok(tok.clone());
            }
        }
        let resp = self
            .http
            .post(self.token_url())
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
                ("scope", self.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Self::provider_err("app token request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/azure-ad: app token grant failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("app token decode failed", e))?;
        let token = payload
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::provider("idp/azure-ad: app token response missing access_token")
            })?
            .to_string();
        let mut guard = self.app_token.lock().await;
        *guard = Some(token.clone());
        Ok(token)
    }

    /// ROPC password-grant login returning the resolved user + minted token.
    ///
    /// Behavior-equal to pyfly: a non-200 token response maps to
    /// [`Error::InvalidCredentials`] (no Graph follow-up); on success the
    /// username is resolved via [`get_user`](Adapter::get_user), falling back to
    /// a minimal user when absent.
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
                ("scope", self.scope.as_str()),
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
        let user = match idp::Adapter::get_user(self, username).await {
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
        ..Token::default()
    }
}

/// Maps an Azure AD user representation into the port's [`User`].
fn from_aad(data: &Value) -> User {
    let upn = data
        .get("userPrincipalName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let email = data
        .get("mail")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(upn);
    let mut attributes = std::collections::HashMap::new();
    if let Some(given) = data.get("givenName").and_then(Value::as_str) {
        attributes.insert("firstName".to_string(), json!(given));
    }
    if let Some(surname) = data.get("surname").and_then(Value::as_str) {
        attributes.insert("lastName".to_string(), json!(surname));
    }
    User {
        id: data
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        username: upn.to_string(),
        email: email.to_string(),
        roles: Vec::new(),
        attributes,
        enabled: data
            .get("accountEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        ..User::default()
    }
}

fn role_from_group(g: &Value) -> Role {
    Role {
        name: g
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        description: g
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        scopes: Vec::new(),
    }
}

fn attr_str<'a>(user: &'a User, key: &str) -> &'a str {
    user.attributes
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
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

    async fn refresh(&self, refresh_token: &str) -> Result<Token> {
        let resp = self
            .http
            .post(self.token_url())
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.cfg.client_id.as_str()),
                ("client_secret", self.cfg.client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("scope", self.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Self::provider_err("refresh request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/azure-ad: refresh failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let tokens: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("refresh decode failed", e))?;
        let mut token = token_from_grant(&tokens);
        if token.refresh_token.is_empty() {
            token.refresh_token = refresh_token.to_string();
        }
        Ok(token)
    }

    async fn validate(&self, access_token: &str) -> Result<User> {
        self.get_user_info(access_token).await
    }

    async fn get_user(&self, id: &str) -> Result<User> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .get(format!("{}/users/{id}", self.graph))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("get_user request failed", e))?;
        if resp.status().as_u16() == 404 {
            return Err(Error::UserNotFound);
        }
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/azure-ad: get_user failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("get_user decode failed", e))?;
        Ok(from_aad(&data))
    }

    async fn create_user(&self, user: User, password: &str) -> Result<User> {
        let token = self.app_token().await?;
        let first = attr_str(&user, "firstName");
        let last = attr_str(&user, "lastName");
        let display = format!("{first} {last}");
        let display = display.trim();
        let display = if display.is_empty() {
            user.username.as_str()
        } else {
            display
        };
        let upn = if user.email.is_empty() {
            user.username.as_str()
        } else {
            user.email.as_str()
        };
        let payload = json!({
            "accountEnabled": user.enabled,
            "displayName": display,
            "mailNickname": user.username,
            "userPrincipalName": upn,
            "givenName": first,
            "surname": last,
            "passwordProfile": {
                "forceChangePasswordNextSignIn": false,
                "password": password,
            },
        });
        let resp = self
            .http
            .post(format!("{}/users", self.graph))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| Self::provider_err("create_user request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/azure-ad: create_user failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("create_user decode failed", e))?;
        let mut created = user;
        if let Some(id) = data.get("id").and_then(Value::as_str) {
            created.id = id.to_string();
        }
        Ok(created)
    }

    async fn update_user(&self, user: User) -> Result<User> {
        let token = self.app_token().await?;
        let payload = json!({
            "accountEnabled": user.enabled,
            "givenName": attr_str(&user, "firstName"),
            "surname": attr_str(&user, "lastName"),
        });
        self.http
            .patch(format!("{}/users/{}", self.graph, user.id))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| Self::provider_err("update_user request failed", e))?;
        Ok(user)
    }

    async fn delete_user(&self, id: &str) -> Result<()> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .delete(format!("{}/users/{id}", self.graph))
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
                "idp/azure-ad: delete_user failed: HTTP {code}"
            )))
        }
    }

    fn name(&self) -> &str {
        "azure-ad"
    }

    // -- extended surface (pyfly parity) ----------------------------------

    async fn logout(&self, _access_token: &str) -> Result<bool> {
        // Azure AD has no server-side logout for non-interactive clients.
        Ok(true)
    }

    async fn introspect(&self, access_token: &str) -> Result<SessionIntrospection> {
        let resp = self
            .http
            .get(format!("{}/me", self.graph))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| Self::provider_err("introspect request failed", e))?;
        if resp.status().as_u16() != 200 {
            return Ok(SessionIntrospection::inactive());
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("introspect decode failed", e))?;
        Ok(SessionIntrospection {
            active: true,
            user_id: data
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            username: data
                .get("userPrincipalName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            scopes: Vec::new(),
        })
    }

    async fn find_by_username(&self, username: &str) -> Result<User> {
        // Azure resolves the UPN as the id, so this delegates to get_user.
        idp::Adapter::get_user(self, username).await
    }

    async fn list_users(&self, limit: usize) -> Result<Vec<User>> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .get(format!("{}/users", self.graph))
            .bearer_auth(&token)
            .query(&[("$top", limit.to_string())])
            .send()
            .await
            .map_err(|e| Self::provider_err("list_users request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/azure-ad: list_users failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("list_users decode failed", e))?;
        Ok(data
            .get("value")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(from_aad).collect())
            .unwrap_or_default())
    }

    async fn change_password(
        &self,
        user_id: &str,
        _old_password: &str,
        new_password: &str,
    ) -> Result<bool> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .patch(format!("{}/users/{user_id}", self.graph))
            .bearer_auth(&token)
            .json(&json!({
                "passwordProfile": {
                    "forceChangePasswordNextSignIn": false,
                    "password": new_password,
                }
            }))
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
            .get(format!("{}/me", self.graph))
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
        Ok(from_aad(&data))
    }

    async fn mfa_challenge(&self, _user_id: &str) -> Result<MfaChallenge> {
        Err(not_implemented())
    }

    async fn mfa_verify(&self, _challenge_id: &str, _code: &str) -> Result<Token> {
        Err(not_implemented())
    }

    async fn get_roles(&self, user_id: &str) -> Result<Vec<Role>> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .get(format!("{}/users/{user_id}/memberOf", self.graph))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("get_roles request failed", e))?;
        if resp.status().as_u16() != 200 {
            return Ok(Vec::new());
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("get_roles decode failed", e))?;
        Ok(data
            .get("value")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(role_from_group).collect())
            .unwrap_or_default())
    }

    async fn assign_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .post(format!("{}/groups/{role}/members/$ref", self.graph))
            .bearer_auth(&token)
            .json(&json!({
                "@odata.id": format!("{}/directoryObjects/{user_id}", self.graph),
            }))
            .send()
            .await
            .map_err(|e| Self::provider_err("assign_role request failed", e))?;
        let code = resp.status().as_u16();
        Ok(code == 200 || code == 204)
    }

    async fn revoke_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .delete(format!(
                "{}/groups/{role}/members/{user_id}/$ref",
                self.graph
            ))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("revoke_role request failed", e))?;
        let code = resp.status().as_u16();
        Ok(code == 200 || code == 204)
    }

    async fn list_roles(&self) -> Result<Vec<Role>> {
        let token = self.app_token().await?;
        let resp = self
            .http
            .get(format!("{}/groups", self.graph))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| Self::provider_err("list_roles request failed", e))?;
        if !resp.status().is_success() {
            return Err(Error::provider(format!(
                "idp/azure-ad: list_roles failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp
            .json()
            .await
            .map_err(|e| Self::provider_err("list_roles decode failed", e))?;
        Ok(data
            .get("value")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(role_from_group).collect())
            .unwrap_or_default())
    }
}
