//! OAuth2 login handler — browser-facing authorization_code flow
//! (pyfly: `pyfly.security.oauth2.login.OAuth2LoginHandler`).
//!
//! [`OAuth2LoginHandler::router`] yields an axum [`Router`] with three
//! routes:
//!
//! - `GET /oauth2/authorization/:registration_id` — redirects the
//!   browser to the provider's authorization endpoint with
//!   `state`/`nonce` (and a PKCE S256 challenge when the registration
//!   enables it).
//! - `GET /login/oauth2/code/:registration_id` — handles the provider
//!   callback: validates `state`, exchanges the code for tokens
//!   (sending the PKCE verifier), validates the OIDC `id_token`
//!   against the provider JWKS when available (signature + issuer +
//!   audience + nonce), otherwise fetches userinfo, then stores the
//!   resulting [`Authentication`] in the session (rotating the session
//!   id against fixation).
//! - `POST /logout` — invalidates the session and redirects to `/`.
//!
//! Session state goes through the local [`LoginSession`] /
//! [`LoginSessionStore`] traits so `firefly-session` (or any cookie
//! store) can plug in; [`InMemoryLoginSession`] +
//! [`FixedLoginSessionStore`] cover tests and single-user tools.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use http::{header, HeaderMap, StatusCode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use super::client::{ClientRegistration, ClientRegistrationRepository};
use crate::authentication::Authentication;
use crate::csrf::random_urlsafe;
use crate::jwks::{claims_to_authentication, JwksVerifier};

/// Session key holding the one-time OAuth2 `state` parameter.
pub const SESSION_KEY_STATE: &str = "oauth2_state";
/// Session key holding the one-time OIDC `nonce` parameter.
pub const SESSION_KEY_NONCE: &str = "oauth2_nonce";
/// Session key holding the one-time PKCE code verifier.
pub const SESSION_KEY_PKCE_VERIFIER: &str = "oauth2_pkce_verifier";
/// Session key holding the JSON-serialized [`Authentication`] after a
/// successful login (pyfly: `SECURITY_CONTEXT`).
pub const SESSION_KEY_SECURITY_CONTEXT: &str = "SECURITY_CONTEXT";
/// Session key holding the post-login redirect target.
pub const SESSION_KEY_REDIRECT_URI: &str = "oauth2_redirect_uri";

/// Per-browser session state — the slice of pyfly's `HttpSession` the
/// login flow needs. Values are strings; structured data (the security
/// context) is stored as JSON.
#[async_trait]
pub trait LoginSession: Send + Sync {
    /// Returns the attribute for `key`, if present.
    async fn get_attribute(&self, key: &str) -> Option<String>;
    /// Sets the attribute `key` to `value`.
    async fn set_attribute(&self, key: &str, value: String);
    /// Removes the attribute for `key` (no-op when absent).
    async fn remove_attribute(&self, key: &str);
    /// Rotates the session id, keeping attributes — called on
    /// successful login to prevent session fixation.
    async fn rotate_id(&self);
    /// Invalidates the session: drops all attributes and the id.
    async fn invalidate(&self);
}

/// Resolves the [`LoginSession`] for an incoming request — the
/// pluggable seam where `firefly-session` (cookie-keyed storage)
/// hooks in.
#[async_trait]
pub trait LoginSessionStore: Send + Sync {
    /// Returns the session for the request with `headers` (creating
    /// one if needed).
    async fn session(&self, headers: &HeaderMap) -> Arc<dyn LoginSession>;
}

/// In-memory [`LoginSession`] backed by a `HashMap`.
#[derive(Debug, Default)]
pub struct InMemoryLoginSession {
    id: RwLock<String>,
    attributes: RwLock<HashMap<String, String>>,
}

impl InMemoryLoginSession {
    /// Returns an empty session with a random id.
    pub fn new() -> Self {
        Self {
            id: RwLock::new(random_urlsafe(16)),
            attributes: RwLock::new(HashMap::new()),
        }
    }

    /// The current session id (rotated on login).
    pub async fn id(&self) -> String {
        self.id.read().await.clone()
    }
}

#[async_trait]
impl LoginSession for InMemoryLoginSession {
    async fn get_attribute(&self, key: &str) -> Option<String> {
        self.attributes.read().await.get(key).cloned()
    }

    async fn set_attribute(&self, key: &str, value: String) {
        self.attributes.write().await.insert(key.to_string(), value);
    }

    async fn remove_attribute(&self, key: &str) {
        self.attributes.write().await.remove(key);
    }

    async fn rotate_id(&self) {
        *self.id.write().await = random_urlsafe(16);
    }

    async fn invalidate(&self) {
        self.attributes.write().await.clear();
        *self.id.write().await = random_urlsafe(16);
    }
}

/// A [`LoginSessionStore`] that hands every request the same session —
/// suitable for tests and single-user tooling; production deployments
/// plug in a cookie-keyed store.
#[derive(Debug, Default)]
pub struct FixedLoginSessionStore {
    session: Arc<InMemoryLoginSession>,
}

impl FixedLoginSessionStore {
    /// Builds the store with a fresh shared session.
    pub fn new() -> Self {
        Self {
            session: Arc::new(InMemoryLoginSession::new()),
        }
    }

    /// The shared session (for assertions in tests).
    pub fn session(&self) -> Arc<InMemoryLoginSession> {
        Arc::clone(&self.session)
    }
}

#[async_trait]
impl LoginSessionStore for FixedLoginSessionStore {
    async fn session(&self, _headers: &HeaderMap) -> Arc<dyn LoginSession> {
        self.session.clone() as Arc<dyn LoginSession>
    }
}

/// Returns a `(code_verifier, code_challenge)` pair for PKCE S256
/// (RFC 7636). The verifier is 86 unreserved characters (43–128
/// required by the RFC); the challenge is the unpadded URL-safe base64
/// of its SHA-256.
pub fn generate_pkce() -> (String, String) {
    let verifier = random_urlsafe(64);
    let challenge = pkce_challenge(&verifier);
    (verifier, challenge)
}

/// Computes the S256 challenge for `verifier`.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Percent-encodes a query value the way Python's `urlencode` does
/// (`quote_plus`: space → `+`, unreserved kept, the rest `%XX`).
fn urlencode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Encodes `(key, value)` pairs as an `application/x-www-form-urlencoded`
/// query string.
fn urlencode_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode_component(k), urlencode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Shared state behind the login routes.
struct LoginState {
    clients: Arc<dyn ClientRegistrationRepository>,
    sessions: Arc<dyn LoginSessionStore>,
    http: reqwest::Client,
}

/// Creates the axum routes for the OAuth2 authorization_code login
/// flow (pyfly: `OAuth2LoginHandler`).
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use firefly_security::oauth2::{
///     google, FixedLoginSessionStore, InMemoryClientRegistrationRepository, OAuth2LoginHandler,
/// };
///
/// let repo = InMemoryClientRegistrationRepository::new([google("cid", "secret", "https://app/cb")]);
/// let router = OAuth2LoginHandler::new(Arc::new(repo), Arc::new(FixedLoginSessionStore::new()))
///     .router();
/// ```
pub struct OAuth2LoginHandler {
    state: Arc<LoginState>,
}

impl std::fmt::Debug for OAuth2LoginHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2LoginHandler").finish_non_exhaustive()
    }
}

impl OAuth2LoginHandler {
    /// Builds the handler around a client-registration repository and
    /// a session store.
    pub fn new(
        clients: Arc<dyn ClientRegistrationRepository>,
        sessions: Arc<dyn LoginSessionStore>,
    ) -> Self {
        Self {
            state: Arc::new(LoginState {
                clients,
                sessions,
                http: reqwest::Client::new(),
            }),
        }
    }

    /// Returns the login flow routes:
    /// `GET /oauth2/authorization/:registration_id`,
    /// `GET /login/oauth2/code/:registration_id`, `POST /logout`.
    pub fn router(&self) -> Router {
        Router::new()
            .route(
                "/oauth2/authorization/:registration_id",
                get(handle_authorization),
            )
            .route("/login/oauth2/code/:registration_id", get(handle_callback))
            .route("/logout", post(handle_logout))
            .with_state(Arc::clone(&self.state))
    }
}

/// Renders the pyfly login-error JSON envelope.
fn error_json(status: StatusCode, error: &str, message: &str) -> Response {
    let body = serde_json::json!({ "error": error, "message": message }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("static error response must build")
}

/// Renders a 302 redirect (pyfly uses 302, not axum's 303/307
/// helpers).
fn redirect(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .expect("static redirect must build")
}

/// `GET /oauth2/authorization/:registration_id` — redirect the user to
/// the provider's authorization endpoint.
async fn handle_authorization(
    State(state): State<Arc<LoginState>>,
    Path(registration_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(registration) = state.clients.find_by_registration_id(&registration_id) else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "unknown_registration",
            &format!("No registration found for '{registration_id}'"),
        );
    };

    let session = state.sessions.session(&headers).await;
    let state_param = random_urlsafe(32);
    session
        .set_attribute(SESSION_KEY_STATE, state_param.clone())
        .await;
    let nonce = random_urlsafe(32);
    session
        .set_attribute(SESSION_KEY_NONCE, nonce.clone())
        .await;

    let scope = registration.scopes.join(" ");
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", &registration.client_id),
        ("redirect_uri", &registration.redirect_uri),
        ("scope", &scope),
        ("state", &state_param),
        ("nonce", &nonce),
    ];
    // PKCE (RFC 7636): stash the verifier in the session, send only
    // the S256 challenge.
    let challenge;
    if registration.use_pkce {
        let (verifier, c) = generate_pkce();
        session
            .set_attribute(SESSION_KEY_PKCE_VERIFIER, verifier)
            .await;
        challenge = c;
        params.push(("code_challenge", &challenge));
        params.push(("code_challenge_method", "S256"));
    }
    let url = format!(
        "{}?{}",
        registration.authorization_uri,
        urlencode_pairs(&params)
    );
    redirect(&url)
}

/// `GET /login/oauth2/code/:registration_id` — handle the provider
/// callback and exchange the code for tokens.
async fn handle_callback(
    State(state): State<Arc<LoginState>>,
    Path(registration_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(registration) = state.clients.find_by_registration_id(&registration_id) else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "unknown_registration",
            &format!("No registration found for '{registration_id}'"),
        );
    };

    // Validate state parameter (CSRF protection).
    let session = state.sessions.session(&headers).await;
    let expected_state = session.get_attribute(SESSION_KEY_STATE).await;
    let received_state = query.get("state");
    match (&expected_state, received_state) {
        (Some(expected), Some(received)) if expected == received => {}
        _ => {
            return error_json(
                StatusCode::BAD_REQUEST,
                "invalid_state",
                "OAuth2 state parameter mismatch",
            );
        }
    }
    // Consume state (one-time use).
    session.remove_attribute(SESSION_KEY_STATE).await;

    // Check for an error response from the provider.
    if let Some(error) = query.get("error") {
        let description = query
            .get("error_description")
            .filter(|d| !d.is_empty())
            .unwrap_or(error);
        return error_json(StatusCode::BAD_REQUEST, error, description);
    }

    let Some(code) = query.get("code") else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "missing_code",
            "No authorization code in callback",
        );
    };

    // PKCE: retrieve and consume the one-time verifier stashed at
    // authorization time.
    let code_verifier = if registration.use_pkce {
        let v = session.get_attribute(SESSION_KEY_PKCE_VERIFIER).await;
        session.remove_attribute(SESSION_KEY_PKCE_VERIFIER).await;
        v
    } else {
        None
    };

    // Exchange the authorization code for tokens.
    let token_response = exchange_code(&state.http, &registration, code, code_verifier).await;
    let Some(access_token) = token_response
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return error_json(
            StatusCode::BAD_GATEWAY,
            "token_exchange_failed",
            "Failed to obtain access token",
        );
    };

    // Prefer verified OIDC ID-token claims when present; the id_token
    // is signature/issuer/audience/nonce validated against the
    // provider JWKS before any claim is trusted.
    let nonce = session.get_attribute(SESSION_KEY_NONCE).await;
    session.remove_attribute(SESSION_KEY_NONCE).await;
    let mut authentication: Option<Authentication> = None;
    if let Some(id_token) = token_response.get("id_token").and_then(Value::as_str) {
        if !registration.jwks_uri.is_empty() {
            match validate_id_token(&registration, id_token, nonce.as_deref()).await {
                Some(auth) => authentication = Some(auth),
                None => {
                    return error_json(
                        StatusCode::UNAUTHORIZED,
                        "invalid_id_token",
                        "ID token validation failed",
                    );
                }
            }
        }
    }

    // Otherwise build the identity from the userinfo endpoint.
    let authentication = match authentication {
        Some(auth) => auth,
        None => {
            let user_info = fetch_user_info(&state.http, &registration, access_token).await;
            authentication_from_user_info(&user_info)
        }
    };

    // A configured userinfo/OIDC flow that yields no principal is a
    // hard failure, not a silently-stored anonymous session.
    if authentication.principal.is_empty() {
        return error_json(
            StatusCode::UNAUTHORIZED,
            "login_failed",
            "Could not determine the authenticated user",
        );
    }

    // Rotate the session id on successful authentication to prevent
    // session fixation.
    session.rotate_id().await;
    let serialized =
        serde_json::to_string(&authentication).expect("Authentication serializes to JSON");
    session
        .set_attribute(SESSION_KEY_SECURITY_CONTEXT, serialized)
        .await;

    let redirect_uri = session
        .get_attribute(SESSION_KEY_REDIRECT_URI)
        .await
        .unwrap_or_else(|| "/".to_string());
    session.remove_attribute(SESSION_KEY_REDIRECT_URI).await;
    redirect(&redirect_uri)
}

/// `POST /logout` — invalidate the session and redirect to the root.
async fn handle_logout(State(state): State<Arc<LoginState>>, headers: HeaderMap) -> Response {
    let session = state.sessions.session(&headers).await;
    session.invalidate().await;
    redirect("/")
}

/// Exchanges an authorization code for tokens via the token endpoint;
/// transport or non-200 failures yield an empty object (pyfly logs and
/// returns `{}`).
async fn exchange_code(
    http: &reqwest::Client,
    registration: &ClientRegistration,
    code: &str,
    code_verifier: Option<String>,
) -> Value {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &registration.redirect_uri),
        ("client_id", &registration.client_id),
        ("client_secret", &registration.client_secret),
    ];
    if let Some(verifier) = code_verifier.as_deref() {
        // PKCE proof of possession.
        form.push(("code_verifier", verifier));
    }

    let response = http
        .post(&registration.token_uri)
        .form(&form)
        .header(header::ACCEPT, "application/json")
        .send()
        .await;
    match response {
        Ok(resp) if resp.status().is_success() => resp
            .json()
            .await
            .unwrap_or(Value::Object(Default::default())),
        _ => Value::Object(Default::default()),
    }
}

/// Validates an OIDC ID token against the provider JWKS and nonce;
/// returns the [`Authentication`] built from verified claims, or
/// `None` when validation fails.
async fn validate_id_token(
    registration: &ClientRegistration,
    id_token: &str,
    nonce: Option<&str>,
) -> Option<Authentication> {
    let mut verifier = JwksVerifier::new(&registration.jwks_uri).audience(&registration.client_id);
    if !registration.issuer_uri.is_empty() {
        verifier = verifier.issuer(&registration.issuer_uri);
    }
    let claims = verifier.validate(id_token).await.ok()?;
    if let Some(expected_nonce) = nonce {
        if claims.get("nonce").and_then(Value::as_str) != Some(expected_nonce) {
            return None;
        }
    }
    Some(claims_to_authentication(&claims))
}

/// Fetches user info from the provider's userinfo endpoint; transport
/// or non-200 failures yield an empty object.
async fn fetch_user_info(
    http: &reqwest::Client,
    registration: &ClientRegistration,
    access_token: &str,
) -> Value {
    if registration.user_info_uri.is_empty() {
        return Value::Object(Default::default());
    }
    let response = http
        .get(&registration.user_info_uri)
        .bearer_auth(access_token)
        .header(header::ACCEPT, "application/json")
        .send()
        .await;
    match response {
        Ok(resp) if resp.status().is_success() => resp
            .json()
            .await
            .unwrap_or(Value::Object(Default::default())),
        _ => Value::Object(Default::default()),
    }
}

/// Builds an [`Authentication`] from an OAuth2 userinfo response —
/// pyfly's `_build_security_context`: the principal is
/// `sub` | `id` | `login` (stringified), and every claim is kept.
fn authentication_from_user_info(user_info: &Value) -> Authentication {
    let empty = serde_json::Map::new();
    let map = user_info.as_object().unwrap_or(&empty);
    let principal = ["sub", "id", "login"]
        .iter()
        .find_map(|k| map.get(*k))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    let username = ["preferred_username", "login", "name", "email"]
        .iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))
        .unwrap_or(&principal)
        .to_string();
    Authentication {
        principal,
        username,
        claims: map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from pyfly: test_generate_pkce_is_valid_s256
    #[test]
    fn generate_pkce_is_valid_s256() {
        let (verifier, challenge) = generate_pkce();
        assert!(
            (43..=128).contains(&verifier.len()),
            "len {}",
            verifier.len()
        );
        assert_eq!(challenge, pkce_challenge(&verifier));
        assert!(!challenge.contains('='));
    }

    #[test]
    fn urlencode_matches_python_quote_plus() {
        assert_eq!(
            urlencode_pairs(&[("scope", "openid profile email")]),
            "scope=openid+profile+email"
        );
        assert_eq!(
            urlencode_pairs(&[("redirect_uri", "https://app/cb")]),
            "redirect_uri=https%3A%2F%2Fapp%2Fcb"
        );
        assert_eq!(urlencode_pairs(&[("a", "1"), ("b", "~_.-")]), "a=1&b=~_.-");
    }

    #[test]
    fn user_info_mapping_prefers_sub_then_id_then_login() {
        let auth = authentication_from_user_info(&serde_json::json!({"sub": "s1", "id": 7}));
        assert_eq!(auth.principal, "s1");

        // GitHub-style numeric id is stringified, login becomes username.
        let auth =
            authentication_from_user_info(&serde_json::json!({"id": 12345, "login": "alice"}));
        assert_eq!(auth.principal, "12345");
        assert_eq!(auth.username, "alice");

        let auth = authentication_from_user_info(&serde_json::json!({"login": "bob"}));
        assert_eq!(auth.principal, "bob");

        let auth = authentication_from_user_info(&serde_json::json!({"email": "x@y.z"}));
        assert!(auth.principal.is_empty());
    }

    #[tokio::test]
    async fn in_memory_session_rotates_and_invalidates() {
        let session = InMemoryLoginSession::new();
        let original = session.id().await;
        session.set_attribute("k", "v".into()).await;
        session.rotate_id().await;
        assert_ne!(session.id().await, original, "id rotated");
        assert_eq!(session.get_attribute("k").await.as_deref(), Some("v"));
        session.invalidate().await;
        assert_eq!(session.get_attribute("k").await, None);
    }
}
