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

//! WebAuthn / passkey login — the Rust analog of Spring Security 6.4's
//! `webAuthn()` (FIDO2 / passkeys).
//!
//! A passwordless, phishing-resistant flow built on the [`webauthn-rs`] relying
//! party. As in Spring, a ceremony has two HTTP legs — an *options* leg that
//! issues a cryptographic challenge, and a *verify* leg that consumes the
//! authenticator's signed response — for each of registration and
//! authentication:
//!
//! 1. **Register** a passkey for an already-identified user:
//!    - `POST /webauthn/register/options` (`{ "username": … }`) →
//!      [`start_passkey_registration`](WebAuthnRelyingParty::start_passkey_registration)
//!      returns a `CreationChallengeResponse` (the
//!      `PublicKeyCredentialCreationOptions` the browser hands to
//!      `navigator.credentials.create`). The in-progress
//!      [`PasskeyRegistration`] state is stashed server-side, keyed by username.
//!    - `POST /webauthn/register` (the browser's `RegisterPublicKeyCredential`
//!      JSON) →
//!      [`finish_passkey_registration`](WebAuthnRelyingParty::finish_passkey_registration)
//!      validates the attestation and persists the resulting [`Passkey`] via the
//!      [`PasskeyCredentialRepository`].
//! 2. **Authenticate** with a registered passkey:
//!    - `POST /webauthn/authenticate/options` (`{ "username": … }`) →
//!      [`start_passkey_authentication`](WebAuthnRelyingParty::start_passkey_authentication)
//!      returns a `RequestChallengeResponse`; the [`PasskeyAuthentication`] state
//!      is stashed server-side.
//!    - `POST /login/webauthn` (the browser's `PublicKeyCredential` JSON) →
//!      [`finish_passkey_authentication`](WebAuthnRelyingParty::finish_passkey_authentication)
//!      verifies the assertion. On success it bumps the stored credential's
//!      signature counter, builds an [`Authentication`], rotates the
//!      [`firefly_session::Session`] id (anti-fixation), and stores the security
//!      context under [`SESSION_KEY_SECURITY_CONTEXT`](crate::oauth2::SESSION_KEY_SECURITY_CONTEXT)
//!      — exactly where [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer)
//!      restores it on later requests.
//!
//! ## State that must live server-side
//!
//! The `webauthn-rs` `PasskeyRegistration` / `PasskeyAuthentication` values
//! returned by the `start_*` calls bind a one-time challenge to its ceremony and
//! **must** be retained, server-side, between the options leg and the verify leg
//! — losing them, or letting the client round-trip them, opens replay attacks.
//! They are deliberately *not* serialisable here (we do not enable
//! `webauthn-rs`'s `danger-allow-state-serialisation`); instead the in-memory
//! [`InMemoryCeremonyStore`] keeps them in a `Mutex<HashMap<username, _>>`. A
//! distributed deployment supplies its own [`CeremonyStateStore`] over a shared,
//! short-TTL store.
//!
//! ## Storage
//!
//! Two repositories model what Spring splits across `UserCredentialRepository`
//! and `PublicKeyCredentialUserEntityRepository`:
//!
//! - [`PasskeyCredentialRepository`] — store / list a username's [`Passkey`]s and
//!   apply the counter update returned by a successful assertion.
//! - [`PublicKeyCredentialUserEntityRepository`] — maps a username to a stable
//!   per-user handle ([`Uuid`]); this handle is the WebAuthn `user.id` and must
//!   not change across a user's credentials.
//!
//! [`InMemoryPasskeyRepository`] and [`InMemoryUserEntityRepository`] ship for
//! single-process apps; production wires database-backed implementations.
//!
//! [`webauthn-rs`]: https://docs.rs/webauthn-rs

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use webauthn_rs::prelude::{
    AuthenticationResult, CreationChallengeResponse, Passkey, PasskeyAuthentication,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse,
    Url, Uuid, Webauthn, WebauthnBuilder,
};

use firefly_session::Session;

use crate::authentication::Authentication;
use crate::oauth2::SESSION_KEY_SECURITY_CONTEXT;

/// Configuration for the WebAuthn relying party — the Rust analog of Spring's
/// `WebAuthnRelyingPartyRegistration` properties.
///
/// `serde(default)` lets it bind from configuration with sane fallbacks
/// (`localhost` on `https://localhost:8080`), matching the developer-friendly
/// defaults of the Spring/Go ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebAuthnProperties {
    /// The relying-party id — the registrable domain the credentials are scoped
    /// to (e.g. `example.com`). Credentials are bound to this and cannot be used
    /// against another origin.
    pub rp_id: String,
    /// The human-readable relying-party name shown by the authenticator UI.
    pub rp_name: String,
    /// The full origin URL(s) the browser ceremony is served from (scheme +
    /// host + optional port, e.g. `https://example.com`). The first is the
    /// primary; any extras are additional allowed origins.
    pub allowed_origins: Vec<String>,
}

impl Default for WebAuthnProperties {
    fn default() -> Self {
        Self {
            rp_id: "localhost".to_string(),
            rp_name: "Firefly".to_string(),
            allowed_origins: vec!["https://localhost:8080".to_string()],
        }
    }
}

/// The WebAuthn relying party — a thin, ergonomic wrapper over
/// [`webauthn_rs::Webauthn`] that exposes just the four passkey ceremony calls
/// the login flow needs.
pub struct WebAuthnRelyingParty {
    webauthn: Webauthn,
    primary_origin: Url,
}

impl WebAuthnRelyingParty {
    /// Builds a relying party for `rp_id`/`rp_name` served from `origin`
    /// (`https://host[:port]`).
    ///
    /// # Errors
    ///
    /// Returns [`WebAuthnError::InvalidConfiguration`] if `origin` is not a
    /// valid URL, or the underlying [`WebauthnBuilder`] rejects the
    /// configuration (e.g. the origin's host does not match `rp_id`).
    pub fn new(rp_id: &str, rp_name: &str, origin: &str) -> Result<Self, WebAuthnError> {
        let rp_origin = Url::parse(origin)
            .map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?;
        let webauthn = WebauthnBuilder::new(rp_id, &rp_origin)
            .map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?
            .rp_name(rp_name)
            .build()
            .map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?;
        Ok(Self {
            webauthn,
            primary_origin: rp_origin,
        })
    }

    /// Builds a relying party from [`WebAuthnProperties`].
    ///
    /// # Errors
    ///
    /// As [`WebAuthnRelyingParty::new`]; additionally errors if
    /// `allowed_origins` is empty.
    pub fn from_properties(props: &WebAuthnProperties) -> Result<Self, WebAuthnError> {
        let primary = props.allowed_origins.first().ok_or_else(|| {
            WebAuthnError::InvalidConfiguration("at least one allowed origin is required".into())
        })?;
        let rp_origin =
            Url::parse(primary).map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?;
        let mut builder = WebauthnBuilder::new(&props.rp_id, &rp_origin)
            .map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?
            .rp_name(&props.rp_name);
        // Register any additional origins (e.g. www vs apex, native app schemes).
        for extra in props.allowed_origins.iter().skip(1) {
            let url = Url::parse(extra)
                .map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?;
            builder = builder.append_allowed_origin(&url);
        }
        let webauthn = builder
            .build()
            .map_err(|e| WebAuthnError::InvalidConfiguration(e.to_string()))?;
        Ok(Self {
            webauthn,
            primary_origin: rp_origin,
        })
    }

    /// The primary origin URL ceremonies are served from — handy for driving the
    /// browser-side `navigator.credentials` call (and for tests).
    #[must_use]
    pub fn origin(&self) -> &Url {
        &self.primary_origin
    }

    /// Begins registering a passkey for `user_handle`/`username`, excluding any
    /// credentials the user already has so a device isn't enrolled twice.
    ///
    /// Returns the challenge to send the browser and the in-progress state to
    /// stash server-side until [`finish_passkey_registration`](Self::finish_passkey_registration).
    ///
    /// # Errors
    ///
    /// Propagates a [`WebAuthnError::Ceremony`] if the engine rejects the
    /// request.
    pub fn start_passkey_registration(
        &self,
        user_handle: Uuid,
        username: &str,
        existing: &[Passkey],
    ) -> Result<(CreationChallengeResponse, PasskeyRegistration), WebAuthnError> {
        let exclude = if existing.is_empty() {
            None
        } else {
            Some(existing.iter().map(|p| p.cred_id().clone()).collect())
        };
        self.webauthn
            .start_passkey_registration(user_handle, username, username, exclude)
            .map_err(WebAuthnError::from)
    }

    /// Completes registration, validating the authenticator's attestation
    /// against the stashed `state`, yielding the [`Passkey`] to persist.
    ///
    /// # Errors
    ///
    /// [`WebAuthnError::Ceremony`] if attestation/challenge validation fails.
    pub fn finish_passkey_registration(
        &self,
        credential: &RegisterPublicKeyCredential,
        state: &PasskeyRegistration,
    ) -> Result<Passkey, WebAuthnError> {
        self.webauthn
            .finish_passkey_registration(credential, state)
            .map_err(WebAuthnError::from)
    }

    /// Begins authenticating against the user's registered `passkeys`.
    ///
    /// Returns the challenge to send the browser and the in-progress state to
    /// stash until [`finish_passkey_authentication`](Self::finish_passkey_authentication).
    ///
    /// # Errors
    ///
    /// [`WebAuthnError::Ceremony`] if the engine rejects the request (e.g. the
    /// user has no credentials).
    pub fn start_passkey_authentication(
        &self,
        passkeys: &[Passkey],
    ) -> Result<(RequestChallengeResponse, PasskeyAuthentication), WebAuthnError> {
        self.webauthn
            .start_passkey_authentication(passkeys)
            .map_err(WebAuthnError::from)
    }

    /// Completes authentication, verifying the assertion against the stashed
    /// `state`. The returned [`AuthenticationResult`] carries the signature
    /// counter that callers must fold back into the stored credential (see
    /// [`PasskeyCredentialRepository::update_credential`]).
    ///
    /// # Errors
    ///
    /// [`WebAuthnError::Ceremony`] if signature/challenge validation fails.
    pub fn finish_passkey_authentication(
        &self,
        credential: &PublicKeyCredential,
        state: &PasskeyAuthentication,
    ) -> Result<AuthenticationResult, WebAuthnError> {
        self.webauthn
            .finish_passkey_authentication(credential, state)
            .map_err(WebAuthnError::from)
    }
}

/// Errors raised by the WebAuthn relying party and HTTP routes.
#[derive(Debug, thiserror::Error)]
pub enum WebAuthnError {
    /// The relying-party configuration (rp_id / origin) is invalid.
    #[error("invalid WebAuthn configuration: {0}")]
    InvalidConfiguration(String),
    /// A registration or authentication ceremony failed (challenge mismatch,
    /// attestation/signature invalid, counter regression, …).
    #[error("WebAuthn ceremony failed: {0}")]
    Ceremony(String),
    /// No in-progress ceremony state was found for the username — the options
    /// leg was skipped, expired, or already consumed.
    #[error("no in-progress WebAuthn ceremony for this user")]
    NoCeremonyInProgress,
    /// The user has no registered passkeys to authenticate with.
    #[error("no registered passkeys for this user")]
    NoCredentials,
}

impl From<webauthn_rs::prelude::WebauthnError> for WebAuthnError {
    fn from(e: webauthn_rs::prelude::WebauthnError) -> Self {
        WebAuthnError::Ceremony(e.to_string())
    }
}

/// Persists a user's registered passkeys — the Rust analog of Spring's
/// `UserCredentialRepository` (the credential half).
#[async_trait]
pub trait PasskeyCredentialRepository: Send + Sync {
    /// Stores a newly-registered [`Passkey`] for `username`.
    async fn save(&self, username: &str, passkey: Passkey);
    /// Lists every [`Passkey`] registered for `username` (empty when none).
    async fn find_by_username(&self, username: &str) -> Vec<Passkey>;
    /// Applies the signature-counter update from a successful assertion to the
    /// matching stored credential. A no-op if no credential matches.
    async fn update_credential(&self, username: &str, result: &AuthenticationResult);
}

/// Maps a username to its stable per-user handle (`Uuid`) — the Rust analog of
/// Spring's `PublicKeyCredentialUserEntityRepository`. The handle is the
/// WebAuthn `user.id`; it must be stable across all of a user's credentials and
/// must never be reused for a different user.
#[async_trait]
pub trait PublicKeyCredentialUserEntityRepository: Send + Sync {
    /// Returns the existing handle for `username`, minting and storing a fresh
    /// random one on first use.
    async fn user_handle(&self, username: &str) -> Uuid;
}

/// Holds the one-time `PasskeyRegistration` / `PasskeyAuthentication` ceremony
/// state between an options leg and its verify leg. Keyed by username; the verify
/// leg removes the entry (single-use).
#[async_trait]
pub trait CeremonyStateStore: Send + Sync {
    /// Stashes the in-progress registration state for `username`.
    async fn put_registration(&self, username: &str, state: PasskeyRegistration);
    /// Removes and returns the registration state for `username`, if any.
    async fn take_registration(&self, username: &str) -> Option<PasskeyRegistration>;
    /// Stashes the in-progress authentication state for `username`.
    async fn put_authentication(&self, username: &str, state: PasskeyAuthentication);
    /// Removes and returns the authentication state for `username`, if any.
    async fn take_authentication(&self, username: &str) -> Option<PasskeyAuthentication>;
}

/// In-memory [`PasskeyCredentialRepository`] for single-process apps.
#[derive(Default)]
pub struct InMemoryPasskeyRepository {
    by_user: Mutex<HashMap<String, Vec<Passkey>>>,
}

impl InMemoryPasskeyRepository {
    /// Builds an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PasskeyCredentialRepository for InMemoryPasskeyRepository {
    async fn save(&self, username: &str, passkey: Passkey) {
        self.by_user
            .lock()
            .await
            .entry(username.to_string())
            .or_default()
            .push(passkey);
    }

    async fn find_by_username(&self, username: &str) -> Vec<Passkey> {
        self.by_user
            .lock()
            .await
            .get(username)
            .cloned()
            .unwrap_or_default()
    }

    async fn update_credential(&self, username: &str, result: &AuthenticationResult) {
        if let Some(creds) = self.by_user.lock().await.get_mut(username) {
            for passkey in creds.iter_mut() {
                // `update_credential` returns `Some(_)` for the matching cred id
                // and folds in the new counter / backup state.
                if passkey.update_credential(result).is_some() {
                    break;
                }
            }
        }
    }
}

/// In-memory [`PublicKeyCredentialUserEntityRepository`] for single-process apps.
/// Handles are random v4 UUIDs minted on first sight of a username and held
/// stable thereafter.
#[derive(Default)]
pub struct InMemoryUserEntityRepository {
    handles: Mutex<HashMap<String, Uuid>>,
}

impl InMemoryUserEntityRepository {
    /// Builds an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PublicKeyCredentialUserEntityRepository for InMemoryUserEntityRepository {
    async fn user_handle(&self, username: &str) -> Uuid {
        *self
            .handles
            .lock()
            .await
            .entry(username.to_string())
            .or_insert_with(Uuid::new_v4)
    }
}

/// In-memory [`CeremonyStateStore`] keyed by username. The non-serialisable
/// `webauthn-rs` ceremony states never leave the process, as required.
#[derive(Default)]
pub struct InMemoryCeremonyStore {
    registrations: Mutex<HashMap<String, PasskeyRegistration>>,
    authentications: Mutex<HashMap<String, PasskeyAuthentication>>,
}

impl InMemoryCeremonyStore {
    /// Builds an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CeremonyStateStore for InMemoryCeremonyStore {
    async fn put_registration(&self, username: &str, state: PasskeyRegistration) {
        self.registrations
            .lock()
            .await
            .insert(username.to_string(), state);
    }

    async fn take_registration(&self, username: &str) -> Option<PasskeyRegistration> {
        self.registrations.lock().await.remove(username)
    }

    async fn put_authentication(&self, username: &str, state: PasskeyAuthentication) {
        self.authentications
            .lock()
            .await
            .insert(username.to_string(), state);
    }

    async fn take_authentication(&self, username: &str) -> Option<PasskeyAuthentication> {
        self.authentications.lock().await.remove(username)
    }
}

/// Shared state for the WebAuthn login routes — the relying party plus the three
/// stores and the post-login redirect target.
pub struct WebAuthnState {
    /// The ceremony engine.
    pub relying_party: Arc<WebAuthnRelyingParty>,
    /// Stores registered passkeys.
    pub credentials: Arc<dyn PasskeyCredentialRepository>,
    /// Maps usernames to stable user handles.
    pub user_entities: Arc<dyn PublicKeyCredentialUserEntityRepository>,
    /// Holds in-progress ceremony state between legs.
    pub ceremonies: Arc<dyn CeremonyStateStore>,
    /// Where to redirect after a successful `POST /login/webauthn` (default
    /// `"/"`).
    pub success_redirect: String,
}

impl WebAuthnState {
    /// Builds state from a relying party, wiring in-memory stores and a `"/"`
    /// success redirect.
    #[must_use]
    pub fn new(relying_party: Arc<WebAuthnRelyingParty>) -> Self {
        Self {
            relying_party,
            credentials: Arc::new(InMemoryPasskeyRepository::new()),
            user_entities: Arc::new(InMemoryUserEntityRepository::new()),
            ceremonies: Arc::new(InMemoryCeremonyStore::new()),
            success_redirect: "/".to_string(),
        }
    }

    /// Overrides the credential repository.
    #[must_use]
    pub fn credentials(mut self, repo: Arc<dyn PasskeyCredentialRepository>) -> Self {
        self.credentials = repo;
        self
    }

    /// Overrides the user-entity repository.
    #[must_use]
    pub fn user_entities(
        mut self,
        repo: Arc<dyn PublicKeyCredentialUserEntityRepository>,
    ) -> Self {
        self.user_entities = repo;
        self
    }

    /// Overrides the ceremony-state store.
    #[must_use]
    pub fn ceremonies(mut self, store: Arc<dyn CeremonyStateStore>) -> Self {
        self.ceremonies = store;
        self
    }

    /// Overrides the post-login redirect target.
    #[must_use]
    pub fn success_redirect(mut self, target: impl Into<String>) -> Self {
        self.success_redirect = target.into();
        self
    }
}

#[derive(Deserialize)]
struct UsernameBody {
    username: String,
}

/// `POST /webauthn/register/options` — begin registering a passkey for the named
/// (already-identified) user. Returns the `CreationChallengeResponse` for
/// `navigator.credentials.create` and stashes the registration state.
async fn handle_register_options(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<UsernameBody>,
) -> Response {
    let handle = state.user_entities.user_handle(&body.username).await;
    let existing = state.credentials.find_by_username(&body.username).await;
    match state
        .relying_party
        .start_passkey_registration(handle, &body.username, &existing)
    {
        Ok((challenge, registration)) => {
            state
                .ceremonies
                .put_registration(&body.username, registration)
                .await;
            (StatusCode::OK, Json(challenge)).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "passkey registration options failed");
            crate::problem::unauthorized("Could not begin passkey registration")
        }
    }
}

#[derive(Deserialize)]
struct RegisterBody {
    username: String,
    credential: RegisterPublicKeyCredential,
}

/// `POST /webauthn/register` — finish registration: validate the attestation
/// against the stashed state and persist the resulting passkey.
async fn handle_register(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<RegisterBody>,
) -> Response {
    let Some(registration) = state.ceremonies.take_registration(&body.username).await else {
        return crate::problem::unauthorized("No passkey registration in progress");
    };
    match state
        .relying_party
        .finish_passkey_registration(&body.credential, &registration)
    {
        Ok(passkey) => {
            state.credentials.save(&body.username, passkey).await;
            StatusCode::OK.into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "passkey registration failed");
            crate::problem::unauthorized("Passkey registration failed")
        }
    }
}

/// `POST /webauthn/authenticate/options` — begin authenticating the named user.
/// Returns the `RequestChallengeResponse` for `navigator.credentials.get` and
/// stashes the authentication state.
async fn handle_authenticate_options(
    State(state): State<Arc<WebAuthnState>>,
    Json(body): Json<UsernameBody>,
) -> Response {
    let passkeys = state.credentials.find_by_username(&body.username).await;
    if passkeys.is_empty() {
        return crate::problem::unauthorized("No registered passkeys for this user");
    }
    match state.relying_party.start_passkey_authentication(&passkeys) {
        Ok((challenge, authentication)) => {
            state
                .ceremonies
                .put_authentication(&body.username, authentication)
                .await;
            (StatusCode::OK, Json(challenge)).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "passkey authentication options failed");
            crate::problem::unauthorized("Could not begin passkey authentication")
        }
    }
}

#[derive(Deserialize)]
struct AuthenticateBody {
    username: String,
    credential: PublicKeyCredential,
}

/// `POST /login/webauthn` — finish authentication: verify the assertion, fold
/// the new signature counter into the stored credential, establish the session
/// security context (rotating the session id against fixation), and return the
/// success redirect. Mirrors Spring 6.4's `/login/webauthn` endpoint.
async fn handle_login(
    State(state): State<Arc<WebAuthnState>>,
    Extension(session): Extension<Session>,
    Json(body): Json<AuthenticateBody>,
) -> Response {
    let Some(authentication) = state.ceremonies.take_authentication(&body.username).await else {
        return crate::problem::unauthorized("No passkey authentication in progress");
    };

    let result = match state
        .relying_party
        .finish_passkey_authentication(&body.credential, &authentication)
    {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = %e, "passkey authentication failed");
            return crate::problem::unauthorized("Passkey authentication failed");
        }
    };

    // Fold the new signature counter / backup state back into the stored
    // credential (cloning detection + replay resistance on the next ceremony).
    state
        .credentials
        .update_credential(&body.username, &result)
        .await;

    let auth = Authentication {
        principal: body.username.clone(),
        username: body.username.clone(),
        ..Default::default()
    };

    // Anti-fixation: rotate the session id on authentication, then store the
    // security context where `SessionAuthenticationLayer` restores it.
    session.rotate_id().await;
    let serialized = serde_json::to_string(&auth).expect("Authentication serializes to JSON");
    let _ = session
        .set_attribute(SESSION_KEY_SECURITY_CONTEXT, serialized)
        .await;

    redirect(&state.success_redirect)
}

/// Builds the WebAuthn login routes:
/// `POST /webauthn/register/options`, `POST /webauthn/register`,
/// `POST /webauthn/authenticate/options`, and `POST /login/webauthn`.
///
/// Mount behind a [`firefly_session::SessionLayer`] so a [`Session`] is
/// available on the request; pair with a
/// [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer) to restore
/// the established context on subsequent requests.
pub fn webauthn_routes(state: Arc<WebAuthnState>) -> Router {
    Router::new()
        .route("/webauthn/register/options", post(handle_register_options))
        .route("/webauthn/register", post(handle_register))
        .route(
            "/webauthn/authenticate/options",
            post(handle_authenticate_options),
        )
        .route("/login/webauthn", post(handle_login))
        .with_state(state)
}

/// 302 redirect to `location`.
fn redirect(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(http::header::LOCATION, location)
        .body(axum::body::Body::empty())
        .expect("static redirect response must build")
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_session::SessionInner;
    use tower::ServiceExt;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_authenticator_rs::WebauthnAuthenticator;

    const ORIGIN: &str = "https://localhost:8080";
    const RP_ID: &str = "localhost";

    fn rp() -> WebAuthnRelyingParty {
        WebAuthnRelyingParty::new(RP_ID, "Firefly Test", ORIGIN).expect("relying party builds")
    }

    // --- repository unit tests --------------------------------------------

    #[tokio::test]
    async fn user_handle_is_stable_per_username_and_distinct_across_users() {
        let repo = InMemoryUserEntityRepository::new();
        let alice1 = repo.user_handle("alice").await;
        let alice2 = repo.user_handle("alice").await;
        let bob = repo.user_handle("bob").await;
        assert_eq!(alice1, alice2, "same username yields a stable handle");
        assert_ne!(alice1, bob, "different usernames get distinct handles");
    }

    #[tokio::test]
    async fn ceremony_store_is_single_use() {
        // We need a real PasskeyRegistration to store; mint one via the RP.
        let rp = rp();
        let handle = Uuid::new_v4();
        let (_ccr, reg) = rp
            .start_passkey_registration(handle, "alice", &[])
            .expect("start registration");
        let store = InMemoryCeremonyStore::new();
        store.put_registration("alice", reg).await;
        assert!(
            store.take_registration("alice").await.is_some(),
            "first take succeeds"
        );
        assert!(
            store.take_registration("alice").await.is_none(),
            "second take is empty (single-use)"
        );
    }

    /// Drives the full ceremony at the relying-party level with the software
    /// authenticator: register a passkey, then authenticate with it, and confirm
    /// the credential round-trips. This proves the `webauthn-rs` integration end
    /// to end without the HTTP layer.
    #[test]
    fn full_ceremony_round_trips_at_relying_party_level() {
        let rp = rp();
        let origin = Url::parse(ORIGIN).unwrap();
        // `falsify_uv: true` so the soft authenticator satisfies our
        // `UserVerificationPolicy::Required` ceremonies.
        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));

        // Registration.
        let handle = Uuid::new_v4();
        let (ccr, reg_state) = rp
            .start_passkey_registration(handle, "alice", &[])
            .expect("start registration");
        let reg_credential = authenticator
            .do_registration(origin.clone(), ccr)
            .expect("authenticator registers");
        let passkey = rp
            .finish_passkey_registration(&reg_credential, &reg_state)
            .expect("finish registration");

        // Authentication against the freshly-registered passkey.
        let (rcr, auth_state) = rp
            .start_passkey_authentication(std::slice::from_ref(&passkey))
            .expect("start authentication");
        let auth_credential = authenticator
            .do_authentication(origin, rcr)
            .expect("authenticator authenticates");
        let result = rp
            .finish_passkey_authentication(&auth_credential, &auth_state)
            .expect("finish authentication");

        // The assertion verified against the credential we registered.
        assert_eq!(
            result.cred_id(),
            passkey.cred_id(),
            "assertion is for the registered credential"
        );
    }

    // --- end-to-end ceremony through the HTTP router -----------------------

    /// Drives the full flow through the axum `Router`: register-options →
    /// authenticator → register-verify, then authenticate-options →
    /// authenticator → `/login/webauthn`, and asserts the session security
    /// context ends up set to the right principal.
    #[tokio::test]
    async fn e2e_register_then_login_through_router_sets_session_context() {
        let state = Arc::new(WebAuthnState::new(Arc::new(rp())));
        let app = webauthn_routes(state);
        let origin = Url::parse(ORIGIN).unwrap();
        let mut authenticator = WebauthnAuthenticator::new(SoftPasskey::new(true));

        // 1. POST /webauthn/register/options -> CreationChallengeResponse.
        let ccr: CreationChallengeResponse = post_json(
            &app,
            "/webauthn/register/options",
            &serde_json::json!({ "username": "alice" }),
            None,
        )
        .await;

        // 2. Software authenticator produces the attestation.
        let reg_credential = authenticator
            .do_registration(origin.clone(), ccr)
            .expect("authenticator registers");

        // 3. POST /webauthn/register -> 200 OK, passkey stored.
        let resp = post_raw(
            &app,
            "/webauthn/register",
            &serde_json::json!({ "username": "alice", "credential": reg_credential }),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "registration verifies");

        // 4. POST /webauthn/authenticate/options -> RequestChallengeResponse.
        let rcr: RequestChallengeResponse = post_json(
            &app,
            "/webauthn/authenticate/options",
            &serde_json::json!({ "username": "alice" }),
            None,
        )
        .await;

        // 5. Software authenticator produces the assertion.
        let auth_credential = authenticator
            .do_authentication(origin, rcr)
            .expect("authenticator authenticates");

        // 6. POST /login/webauthn with a session -> 302 + session context set.
        let session = Session::new(SessionInner::new("sid"));
        let resp = post_raw(
            &app,
            "/login/webauthn",
            &serde_json::json!({ "username": "alice", "credential": auth_credential }),
            Some(session.clone()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FOUND, "login redirects on success");
        assert_eq!(resp.headers()[http::header::LOCATION], "/");

        let ctx = session
            .attribute::<String>(SESSION_KEY_SECURITY_CONTEXT)
            .await
            .expect("security context stored on session");
        let auth: Authentication = serde_json::from_str(&ctx).unwrap();
        assert_eq!(auth.principal, "alice");
        assert_eq!(auth.username, "alice");
    }

    #[tokio::test]
    async fn login_without_started_ceremony_is_unauthorized() {
        let state = Arc::new(WebAuthnState::new(Arc::new(rp())));
        let app = webauthn_routes(state);
        let session = Session::new(SessionInner::new("sid"));
        // A bare (but well-formed-enough to deserialize) credential never reaches
        // verification because no ceremony was started for this user.
        let bogus = minimal_public_key_credential();
        let resp = post_raw(
            &app,
            "/login/webauthn",
            &serde_json::json!({ "username": "nobody", "credential": bogus }),
            Some(session.clone()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(
            session
                .attribute::<String>(SESSION_KEY_SECURITY_CONTEXT)
                .await
                .is_none(),
            "no context is established on failure"
        );
    }

    // --- helpers -----------------------------------------------------------

    /// POSTs `body` as JSON, optionally with a `Session` extension, returning the
    /// raw response.
    async fn post_raw(
        app: &Router,
        uri: &str,
        body: &serde_json::Value,
        session: Option<Session>,
    ) -> Response {
        let mut req = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        if let Some(session) = session {
            req.extensions_mut().insert(session);
        }
        app.clone().oneshot(req).await.unwrap()
    }

    /// POSTs `body` as JSON and deserializes a `200 OK` JSON response into `T`.
    async fn post_json<T: serde::de::DeserializeOwned>(
        app: &Router,
        uri: &str,
        body: &serde_json::Value,
        session: Option<Session>,
    ) -> T {
        let resp = post_raw(app, uri, body, session).await;
        assert_eq!(resp.status(), StatusCode::OK, "{uri} returned non-200");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("response deserializes")
    }

    /// A `PublicKeyCredential` shaped enough to deserialize but never valid —
    /// used to prove the "no ceremony in progress" guard fires before any
    /// cryptographic check.
    fn minimal_public_key_credential() -> serde_json::Value {
        serde_json::json!({
            "id": "AAAA",
            "rawId": "AAAA",
            "response": {
                "authenticatorData": "AAAA",
                "clientDataJSON": "AAAA",
                "signature": "AAAA"
            },
            "type": "public-key",
            "extensions": {}
        })
    }
}
