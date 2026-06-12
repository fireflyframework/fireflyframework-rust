//! `/idp` REST controller — mounts the [`Adapter`] port over HTTP.
//!
//! The Rust port of pyfly's `pyfly.idp.web.IdpController`
//! (`@request_mapping("/idp")`): authentication (`/login`, `/refresh`,
//! `/logout`, `/introspect`), token-bound user info (`/userinfo`), public
//! self-registration (`/register`), and admin user / role management. The
//! router is generic over the port — call [`router`] with any
//! `Arc<dyn Adapter>` (the `firefly-idp-internal-db` adapter, a vendor
//! adapter, or a test fake) and merge the result into the service's axum
//! app, or serve it from a dedicated admin port.
//!
//! Available only when the crate's `web` feature is enabled.
//!
//! # Routes (under `/idp`)
//!
//! | Method & path                              | Port call                  |
//! |--------------------------------------------|----------------------------|
//! | `POST /idp/login`                          | [`Adapter::login`] (+ `mfa_verify` when `mfa_code` is supplied) |
//! | `POST /idp/refresh`                        | [`Adapter::refresh`]       |
//! | `POST /idp/logout`                         | [`Adapter::logout`]        |
//! | `POST /idp/introspect`                     | [`Adapter::introspect`]    |
//! | `POST /idp/validate`                       | [`Adapter::validate`]      |
//! | `GET  /idp/userinfo` (Bearer)              | [`Adapter::get_user_info`] |
//! | `POST /idp/register`                       | [`Adapter::register_user`] |
//! | `POST /idp/admin/users`                    | [`Adapter::create_user`]   |
//! | `GET  /idp/admin/users`                    | [`Adapter::list_users`]    |
//! | `GET  /idp/admin/users/{user_id}`          | [`Adapter::get_user`]      |
//! | `DELETE /idp/admin/users/{user_id}`        | [`Adapter::delete_user`]   |
//! | `GET  /idp/admin/users/{user_id}/roles`    | [`Adapter::get_roles`]     |
//! | `POST /idp/admin/users/{user_id}/roles/{role}`   | [`Adapter::assign_role`] |
//! | `DELETE /idp/admin/users/{user_id}/roles/{role}` | [`Adapter::revoke_role`] |
//! | `GET  /idp/admin/roles`                    | [`Adapter::list_roles`]    |
//!
//! # Status mapping
//!
//! [`Error::InvalidCredentials`] → `401`, [`Error::UserNotFound`] → `404`,
//! [`Error::MfaRequired`] → `401` with the challenge body,
//! [`Error::NotSupported`] → `501`, [`Error::Provider`] → `500`. Each error
//! body is `{"error": "<message>"}`, mirroring pyfly's JSON error shape.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Adapter, Error, MfaChallenge, User};

/// Shared router state: the boxed IdP port every handler dispatches to.
#[derive(Clone)]
struct IdpState {
    idp: Arc<dyn Adapter>,
}

/// Returns an axum [`Router`] mounting `idp` under the `/idp` base path —
/// the Rust spelling of pyfly's `IdpController`. The router carries no
/// further state, so it merges straight into the application router (or a
/// dedicated admin router).
///
/// ```
/// # #[cfg(feature = "web")]
/// # {
/// use std::sync::Arc;
/// use firefly_idp::{web, Adapter};
///
/// fn mount(idp: Arc<dyn Adapter>) -> axum::Router {
///     axum::Router::new().merge(web::router(idp))
/// }
/// # }
/// ```
pub fn router(idp: Arc<dyn Adapter>) -> Router {
    let state = IdpState { idp };
    Router::new()
        // -- Authentication --------------------------------------------
        .route("/idp/login", post(login))
        .route("/idp/refresh", post(refresh))
        .route("/idp/logout", post(logout))
        .route("/idp/introspect", post(introspect))
        .route("/idp/validate", post(validate))
        .route("/idp/userinfo", get(userinfo))
        .route("/idp/register", post(register))
        // -- Admin: users ----------------------------------------------
        .route("/idp/admin/users", post(create_user).get(list_users))
        .route(
            "/idp/admin/users/:user_id",
            get(get_user).delete(delete_user),
        )
        .route("/idp/admin/users/:user_id/roles", get(get_roles))
        .route(
            "/idp/admin/users/:user_id/roles/:role",
            post(assign_role).delete(revoke_role),
        )
        // -- Admin: roles ----------------------------------------------
        .route("/idp/admin/roles", get(list_roles))
        .with_state(state)
}

// ── request / response bodies (pyfly IdpController DTOs) ─────────────────

/// `POST /idp/login` body — pyfly's `LoginBody`. The optional `mfa_code`
/// completes a second factor: when present, the handler routes through
/// [`Adapter::mfa_verify`] after the password step.
#[derive(Debug, Clone, Deserialize)]
struct LoginBody {
    username: String,
    password: String,
    #[serde(default)]
    mfa_code: Option<String>,
}

/// A body carrying a single token — pyfly's `TokenBody`, reused by
/// `/refresh`, `/logout`, `/introspect`, and `/validate`.
#[derive(Debug, Clone, Deserialize)]
struct TokenBody {
    token: String,
}

/// `POST /idp/admin/users` / `POST /idp/register` body — pyfly's
/// `CreateUserBody`.
#[derive(Debug, Clone, Deserialize)]
struct CreateUserBody {
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    last_name: String,
    password: String,
    #[serde(default)]
    roles: Vec<String>,
}

impl CreateUserBody {
    /// Builds the [`User`] view from the request body, threading
    /// `first_name`/`last_name` into the user's attribute map (the port's
    /// [`User`] has no dedicated name fields, mirroring pyfly's `IdpUser`
    /// keeping them while the Rust port stores extras under `attributes`).
    fn into_user(self) -> (User, String) {
        let mut user = User {
            username: self.username,
            email: self.email,
            roles: self.roles,
            enabled: true,
            ..User::default()
        };
        if !self.first_name.is_empty() {
            user.attributes
                .insert("first_name".into(), json!(self.first_name));
        }
        if !self.last_name.is_empty() {
            user.attributes
                .insert("last_name".into(), json!(self.last_name));
        }
        (user, self.password)
    }
}

/// `GET /idp/admin/users?limit=N` query — pyfly's `limit` query param
/// (default 100).
#[derive(Debug, Clone, Deserialize)]
struct ListUsersQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

/// `{"success": bool}` — pyfly's boolean-op response envelope (logout,
/// delete, role grant/revoke).
#[derive(Debug, Clone, Serialize)]
struct SuccessBody {
    success: bool,
}

// ── handlers ────────────────────────────────────────────────────────────

/// `POST /idp/login` — password authentication, optionally completing a
/// supplied MFA code, returning the minted [`Token`]. Mirrors pyfly's
/// `IdpController.login`.
async fn login(State(st): State<IdpState>, Json(body): Json<LoginBody>) -> Response {
    match st.idp.login(&body.username, &body.password).await {
        Ok(token) => json_ok(&token),
        Err(Error::MfaRequired(challenge)) => match &body.mfa_code {
            // A code was supplied: complete the second factor.
            Some(code) => match st.idp.mfa_verify(&challenge.challenge_id, code).await {
                Ok(token) => json_ok(&token),
                Err(err) => error_response(&err),
            },
            // No code yet: report the pending challenge (pyfly's
            // `mfa_required` AuthResult).
            None => mfa_challenge_response(&challenge),
        },
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/refresh` — exchanges a refresh token for a fresh [`Token`].
async fn refresh(State(st): State<IdpState>, Json(body): Json<TokenBody>) -> Response {
    match st.idp.refresh(&body.token).await {
        Ok(token) => json_ok(&token),
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/logout` — server-side token revocation; `{"success": bool}`.
async fn logout(State(st): State<IdpState>, Json(body): Json<TokenBody>) -> Response {
    match st.idp.logout(&body.token).await {
        Ok(ok) => json_ok(&SuccessBody { success: ok }),
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/introspect` — RFC 7662 token introspection.
async fn introspect(State(st): State<IdpState>, Json(body): Json<TokenBody>) -> Response {
    match st.idp.introspect(&body.token).await {
        Ok(result) => json_ok(&result),
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/validate` — verifies an access token and returns the owning
/// [`User`] (the port's `validate`).
async fn validate(State(st): State<IdpState>, Json(body): Json<TokenBody>) -> Response {
    match st.idp.validate(&body.token).await {
        Ok(user) => json_ok(&user),
        Err(err) => error_response(&err),
    }
}

/// `GET /idp/userinfo` — resolves the `Authorization: Bearer …` token to
/// its owning [`User`] (the port's `get_user_info`). `401` when the header
/// is missing or malformed.
async fn userinfo(State(st): State<IdpState>, headers: axum::http::HeaderMap) -> Response {
    let Some(token) = bearer_token(&headers) else {
        return error_response(&Error::InvalidCredentials);
    };
    match st.idp.get_user_info(&token).await {
        Ok(user) => json_ok(&user),
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/register` — public self-registration (the port's
/// `register_user`, which forces the account enabled and strips privileged
/// roles).
async fn register(State(st): State<IdpState>, Json(body): Json<CreateUserBody>) -> Response {
    let (user, password) = body.into_user();
    match st.idp.register_user(user, &password).await {
        Ok(created) => json_created(&created),
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/admin/users` — provisions a user (the port's `create_user`).
async fn create_user(State(st): State<IdpState>, Json(body): Json<CreateUserBody>) -> Response {
    let (user, password) = body.into_user();
    match st.idp.create_user(user, &password).await {
        Ok(created) => json_created(&created),
        Err(err) => error_response(&err),
    }
}

/// `GET /idp/admin/users/{user_id}` — looks up a user by id.
async fn get_user(State(st): State<IdpState>, Path(user_id): Path<String>) -> Response {
    match st.idp.get_user(&user_id).await {
        Ok(user) => json_ok(&user),
        Err(err) => error_response(&err),
    }
}

/// `GET /idp/admin/users?limit=N` — lists up to `limit` users.
async fn list_users(State(st): State<IdpState>, Query(q): Query<ListUsersQuery>) -> Response {
    match st.idp.list_users(q.limit).await {
        Ok(users) => json_ok(&users),
        Err(err) => error_response(&err),
    }
}

/// `DELETE /idp/admin/users/{user_id}` — removes a user; `{"success":true}`.
async fn delete_user(State(st): State<IdpState>, Path(user_id): Path<String>) -> Response {
    match st.idp.delete_user(&user_id).await {
        Ok(()) => json_ok(&SuccessBody { success: true }),
        Err(err) => error_response(&err),
    }
}

/// `GET /idp/admin/users/{user_id}/roles` — the [`crate::Role`]s assigned
/// to a user.
async fn get_roles(State(st): State<IdpState>, Path(user_id): Path<String>) -> Response {
    match st.idp.get_roles(&user_id).await {
        Ok(roles) => json_ok(&roles),
        Err(err) => error_response(&err),
    }
}

/// `POST /idp/admin/users/{user_id}/roles/{role}` — grants a role;
/// `{"success": bool}`.
async fn assign_role(
    State(st): State<IdpState>,
    Path((user_id, role)): Path<(String, String)>,
) -> Response {
    match st.idp.assign_role(&user_id, &role).await {
        Ok(ok) => json_ok(&SuccessBody { success: ok }),
        Err(err) => error_response(&err),
    }
}

/// `DELETE /idp/admin/users/{user_id}/roles/{role}` — revokes a role;
/// `{"success": bool}`.
async fn revoke_role(
    State(st): State<IdpState>,
    Path((user_id, role)): Path<(String, String)>,
) -> Response {
    match st.idp.revoke_role(&user_id, &role).await {
        Ok(ok) => json_ok(&SuccessBody { success: ok }),
        Err(err) => error_response(&err),
    }
}

/// `GET /idp/admin/roles` — the provider's full role catalogue.
async fn list_roles(State(st): State<IdpState>) -> Response {
    match st.idp.list_roles().await {
        Ok(roles) => json_ok(&roles),
        Err(err) => error_response(&err),
    }
}

// ── helpers ─────────────────────────────────────────────────────────────

/// Extracts the bearer token from an `Authorization: Bearer <token>`
/// header (case-insensitive scheme), or `None` when absent/malformed.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = raw.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token.to_string())
    } else {
        None
    }
}

/// `200 OK` with a JSON body.
fn json_ok<T: Serialize>(value: &T) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

/// `201 Created` with a JSON body — used by the provisioning endpoints
/// (`create_user` / `register`).
fn json_created<T: Serialize>(value: &T) -> Response {
    (StatusCode::CREATED, Json(value)).into_response()
}

/// `401` with the pending MFA challenge — the HTTP rendering of
/// [`Error::MfaRequired`] (pyfly's `mfa_required` login outcome).
fn mfa_challenge_response(challenge: &MfaChallenge) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "mfa_required": true,
            "mfa_challenge": challenge,
        })),
    )
        .into_response()
}

/// Maps an [`Error`] onto an HTTP status + `{"error": message}` body.
fn error_response(err: &Error) -> Response {
    let status = match err {
        Error::InvalidCredentials | Error::MfaRequired(_) => StatusCode::UNAUTHORIZED,
        Error::UserNotFound => StatusCode::NOT_FOUND,
        Error::NotSupported(_) => StatusCode::NOT_IMPLEMENTED,
        Error::Provider(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": err.to_string() }))).into_response()
}
