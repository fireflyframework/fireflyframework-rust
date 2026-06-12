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

//! firefly-idp — the identity-provider port every concrete IdP adapter satisfies.
//!
//! This crate defines the provider-agnostic contract shared by all Firefly
//! identity adapters:
//!
//! * [`Adapter`] — the port (`login`, `refresh`, `validate`, plus user CRUD).
//! * [`User`] — the IdP-agnostic principal view.
//! * [`Token`] — the OIDC-shaped token envelope.
//! * [`Error::InvalidCredentials`], [`Error::UserNotFound`] — the canonical
//!   error sentinels, bytes-equal across the Java/.NET/Go/Python ports.
//!
//! Concrete implementations live in dedicated crates: `firefly-idp-internal-db`
//! (bcrypt + HS256 JWT, in-memory user store — Full), and the
//! `firefly-idp-keycloak`, `firefly-idp-azure-ad`, and `firefly-idp-aws-cognito`
//! vendor stubs.
//!
//! Wire shape is a hard compatibility requirement: [`User`] and [`Token`]
//! serialize with exactly the same JSON field names and empty-field omission
//! rules as the Go port (`encoding/json` `omitempty` semantics), so SDKs can
//! transparently swap providers and runtimes.
//!
//! # Optional REST controller (`web` feature)
//!
//! Enabling the `web` feature compiles the [`web`] module, an axum
//! [`Router`](axum::Router) that mounts any `Arc<dyn Adapter>` over the
//! `/idp` REST surface (`login` / `refresh` / `logout` / `introspect` /
//! `validate` / `userinfo` / `register` plus admin user & role management)
//! — the Rust port of pyfly's `IdpController`. The router is generic over
//! the port, so the internal-db adapter, a vendor adapter, or a test fake
//! all drop straight in.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "web")]
pub mod web;

/// Crate-local result alias: every fallible port operation returns [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Canonical errors shared by every concrete IdP adapter.
///
/// The [`Error::InvalidCredentials`] and [`Error::UserNotFound`] sentinels are
/// wire-stable: their rendered messages are bytes-equal to the Go port's
/// `idp.ErrInvalidCredentials` / `idp.ErrUserNotFound` error values.
/// Adapter-specific failures (provider outages, malformed tokens, duplicate
/// ids, …) travel as [`Error::Provider`] with the adapter's own message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The canonical login-failure error.
    #[error("firefly/idp: invalid credentials")]
    InvalidCredentials,
    /// Returned by lookups against a missing user.
    #[error("firefly/idp: user not found")]
    UserNotFound,
    /// Login succeeded but a second factor is required. Carries the
    /// [`MfaChallenge`] the caller must complete with [`Adapter::mfa_verify`].
    ///
    /// This is the Rust analogue of pyfly's `AuthResult.mfa_required=True`
    /// login outcome — the port models the MFA-pending login as a fallible
    /// result so the stateless [`Token`] need not carry a "pending" flag.
    #[error("firefly/idp: multi-factor authentication required")]
    MfaRequired(MfaChallenge),
    /// The requested port operation is not supported by this adapter.
    ///
    /// Returned by the default trait method bodies so adapters that predate
    /// the extended surface (Keycloak/Azure/Cognito vendor stubs, third-party
    /// adapters) keep compiling without overriding every new method. The
    /// rendered message names the operation, mirroring pyfly's vendor
    /// adapters raising `NotImplementedError` for provider-side operations.
    #[error("firefly/idp: operation not supported: {0}")]
    NotSupported(String),
    /// A specific provider genuinely cannot perform `operation` because the
    /// provider exposes no admin/management API for it — distinct from
    /// [`Error::NotSupported`] (which means *this adapter* has not implemented
    /// an otherwise-available operation).
    ///
    /// This is the precise, documented capability boundary an adapter returns
    /// when it has implemented every operation the provider's API exposes and
    /// the remaining operation is one the provider only performs interactively
    /// server-side (e.g. verifying a one-time MFA code during an interactive
    /// browser/auth-challenge sign-in, which Keycloak and Azure AD do not
    /// surface through any admin REST endpoint). The rendered message names the
    /// `provider`, the `operation`, and a human-readable `reason`.
    #[error("firefly/idp: {provider} cannot perform operation '{operation}': {reason}")]
    UnsupportedByProvider {
        /// The provider that lacks the capability (e.g. `"keycloak"`).
        provider: String,
        /// The port operation that cannot be performed (e.g. `"mfa_verify"`).
        operation: String,
        /// Why the provider cannot perform it (the API gap), for diagnostics.
        reason: String,
    },
    /// Any other adapter-specific failure; the message is rendered verbatim.
    #[error("{0}")]
    Provider(String),
}

impl Error {
    /// Builds an adapter-specific [`Error::Provider`] from any message.
    pub fn provider(message: impl Into<String>) -> Self {
        Error::Provider(message.into())
    }

    /// Builds an [`Error::NotSupported`] naming the unsupported operation.
    pub fn not_supported(operation: impl Into<String>) -> Self {
        Error::NotSupported(operation.into())
    }

    /// Builds an [`Error::UnsupportedByProvider`] documenting a genuine
    /// provider-API capability boundary.
    pub fn unsupported_by_provider(
        provider: impl Into<String>,
        operation: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Error::UnsupportedByProvider {
            provider: provider.into(),
            operation: operation.into(),
            reason: reason.into(),
        }
    }
}

/// The IdP-agnostic user view returned by the port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// Provider-assigned stable identifier.
    #[serde(default)]
    pub id: String,
    /// Login name, unique within the provider realm.
    #[serde(default)]
    pub username: String,
    /// Primary e-mail address; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
    /// Role names granted to the user; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Free-form provider attributes; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<String, serde_json::Value>,
    /// Whether the account may authenticate.
    #[serde(default)]
    pub enabled: bool,
    /// Creation instant; serialized as RFC 3339 under the `createdAt` key.
    #[serde(rename = "createdAt", default = "zero_time")]
    pub created_at: DateTime<Utc>,
}

impl Default for User {
    fn default() -> Self {
        Self {
            id: String::new(),
            username: String::new(),
            email: String::new(),
            roles: Vec::new(),
            attributes: HashMap::new(),
            enabled: false,
            created_at: zero_time(),
        }
    }
}

/// The unified token envelope. Wire-shape matches the OIDC token endpoint
/// response so SDKs can transparently swap providers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    /// The bearer access token.
    #[serde(default)]
    pub access_token: String,
    /// Token type, conventionally `"Bearer"`.
    #[serde(default)]
    pub token_type: String,
    /// Lifetime of the access token in seconds.
    #[serde(default)]
    pub expires_in: i64,
    /// Opaque refresh token; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    /// OIDC ID token; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id_token: String,
    /// Space-delimited granted scopes; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    /// Issuance instant; serialized as RFC 3339 under the `issued_at` key.
    #[serde(default = "zero_time")]
    pub issued_at: DateTime<Utc>,
}

impl Default for Token {
    fn default() -> Self {
        Self {
            access_token: String::new(),
            token_type: String::new(),
            expires_in: 0,
            refresh_token: String::new(),
            id_token: String::new(),
            scope: String::new(),
            issued_at: zero_time(),
        }
    }
}

/// A named role in the provider's role catalogue.
///
/// Mirrors pyfly's `IdpRole` dataclass (`name`, `description`, `scopes`). The
/// JSON field names match pyfly so role catalogues round-trip across runtimes;
/// `description` and `scopes` are omitted when empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    /// Unique role name (e.g. `"admin"`, `"editor"`).
    #[serde(default)]
    pub name: String,
    /// Human-readable description; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// OAuth scopes granted by the role; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

impl Role {
    /// Builds a bare role with the given `name` and no description or scopes.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// A pending multi-factor-authentication challenge.
///
/// Mirrors pyfly's `MfaChallenge` dataclass (`challenge_id`, `user_id`,
/// `method`). The opaque `challenge_id` is the only identifier a client needs;
/// adapters intentionally leave `user_id` empty in challenges handed to clients
/// to avoid user enumeration (matching pyfly's internal-db adapter).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MfaChallenge {
    /// Opaque, single-use challenge identifier handed to the client.
    #[serde(default)]
    pub challenge_id: String,
    /// Subject user id; omitted from JSON when empty (client-facing
    /// challenges carry an empty value to prevent enumeration).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    /// Second-factor method; defaults to `"TOTP"`.
    #[serde(default)]
    pub method: String,
}

impl MfaChallenge {
    /// Builds a `TOTP` challenge carrying only the opaque `challenge_id`.
    pub fn new(challenge_id: impl Into<String>) -> Self {
        Self {
            challenge_id: challenge_id.into(),
            user_id: String::new(),
            method: "TOTP".into(),
        }
    }
}

/// The result of introspecting an access token (RFC 7662-shaped).
///
/// Mirrors pyfly's `SessionIntrospection` dataclass: an inactive token reports
/// `active=false` with every other field empty; an active token reports the
/// owning user and granted scopes. Empty optional fields are omitted from JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIntrospection {
    /// Whether the token is currently valid.
    #[serde(default)]
    pub active: bool,
    /// Owning user id; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    /// Owning username; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Granted scopes (the user's roles); omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

impl SessionIntrospection {
    /// The canonical inactive introspection (`active=false`, all else empty).
    pub fn inactive() -> Self {
        Self::default()
    }
}

/// The Go `time.Time` zero value (`0001-01-01T00:00:00Z`), used as the default
/// for unset timestamps so defaults round-trip identically across ports.
fn zero_time() -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(1, 1, 1)
        .expect("valid date")
        .and_hms_opt(0, 0, 0)
        .expect("valid time")
        .and_utc()
}

/// The IdP port — every concrete provider satisfies it.
///
/// Implementations must be `Send + Sync` so a single adapter instance can be
/// shared behind an `Arc<dyn Adapter>` across request handlers.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Authenticates `username`/`password` and mints a [`Token`].
    ///
    /// Fails with [`Error::InvalidCredentials`] when the pair does not match.
    async fn login(&self, username: &str, password: &str) -> Result<Token>;

    /// Exchanges a refresh token for a fresh [`Token`].
    async fn refresh(&self, refresh_token: &str) -> Result<Token>;

    /// Verifies an access token and returns the authenticated [`User`].
    async fn validate(&self, access_token: &str) -> Result<User>;

    /// Looks up a user by id; fails with [`Error::UserNotFound`] when missing.
    async fn get_user(&self, id: &str) -> Result<User>;

    /// Provisions a new user with the given initial password and returns the
    /// stored view (with provider-assigned id and timestamps).
    async fn create_user(&self, user: User, password: &str) -> Result<User>;

    /// Replaces the stored profile of an existing user and returns the result;
    /// fails with [`Error::UserNotFound`] when missing.
    async fn update_user(&self, user: User) -> Result<User>;

    /// Removes a user by id; fails with [`Error::UserNotFound`] when missing.
    async fn delete_user(&self, id: &str) -> Result<()>;

    /// Returns the adapter's stable identifier (e.g. `"internal-db"`).
    fn name(&self) -> &str;

    // -----------------------------------------------------------------
    // Extended surface (pyfly parity). Every method below has a default
    // body returning [`Error::NotSupported`] so adapters predating this
    // surface keep compiling unchanged. Concrete adapters override the
    // operations they support.
    // -----------------------------------------------------------------

    /// Invalidates an access token (server-side logout). Returns `true` when a
    /// live session existed and was revoked, `false` when the token was already
    /// unknown — mirroring pyfly's `logout` boolean.
    async fn logout(&self, _access_token: &str) -> Result<bool> {
        Err(Error::not_supported("logout"))
    }

    /// Introspects an access token (RFC 7662). An unknown or revoked token
    /// yields [`SessionIntrospection::inactive`] rather than an error.
    async fn introspect(&self, _access_token: &str) -> Result<SessionIntrospection> {
        Err(Error::not_supported("introspect"))
    }

    /// Looks up a user by login name; fails with [`Error::UserNotFound`] when
    /// no user has that username.
    async fn find_by_username(&self, _username: &str) -> Result<User> {
        Err(Error::not_supported("find_by_username"))
    }

    /// Returns up to `limit` users. Adapters may cap or page the result.
    async fn list_users(&self, _limit: usize) -> Result<Vec<User>> {
        Err(Error::not_supported("list_users"))
    }

    /// Changes a user's password after verifying `old_password`. Returns `true`
    /// on success, `false` when the user is unknown or the old password is
    /// wrong — mirroring pyfly's `change_password` boolean.
    async fn change_password(
        &self,
        _user_id: &str,
        _old_password: &str,
        _new_password: &str,
    ) -> Result<bool> {
        Err(Error::not_supported("change_password"))
    }

    /// Resets a user's password to a freshly generated value and returns it.
    async fn reset_password(&self, _user_id: &str) -> Result<String> {
        Err(Error::not_supported("reset_password"))
    }

    /// Public self-registration. Adapters force the account enabled and strip
    /// privileged roles before provisioning, then return the stored view.
    async fn register_user(&self, _user: User, _password: &str) -> Result<User> {
        Err(Error::not_supported("register_user"))
    }

    /// Resolves an access token to its owning [`User`]; fails with
    /// [`Error::UserNotFound`] (or [`Error::InvalidCredentials`]) when the token
    /// is unknown.
    async fn get_user_info(&self, _access_token: &str) -> Result<User> {
        Err(Error::not_supported("get_user_info"))
    }

    /// Creates a TOTP challenge for `user_id`. The returned [`MfaChallenge`]
    /// carries only the opaque `challenge_id`.
    async fn mfa_challenge(&self, _user_id: &str) -> Result<MfaChallenge> {
        Err(Error::not_supported("mfa_challenge"))
    }

    /// Verifies a TOTP `code` against `challenge_id` and, on success, mints a
    /// [`Token`]. Fails with [`Error::InvalidCredentials`] on a wrong code or an
    /// unknown/consumed challenge.
    async fn mfa_verify(&self, _challenge_id: &str, _code: &str) -> Result<Token> {
        Err(Error::not_supported("mfa_verify"))
    }

    /// Returns the [`Role`] objects assigned to `user_id` (empty for an unknown
    /// user).
    async fn get_roles(&self, _user_id: &str) -> Result<Vec<Role>> {
        Err(Error::not_supported("get_roles"))
    }

    /// Grants `role` to `user_id`. Returns `true` on success, `false` when the
    /// user is unknown — mirroring pyfly's `assign_role` boolean.
    async fn assign_role(&self, _user_id: &str, _role: &str) -> Result<bool> {
        Err(Error::not_supported("assign_role"))
    }

    /// Revokes `role` from `user_id`. Returns `true` on success, `false` when
    /// the user is unknown or lacked the role.
    async fn revoke_role(&self, _user_id: &str, _role: &str) -> Result<bool> {
        Err(Error::not_supported("revoke_role"))
    }

    /// Lists every role in the provider's catalogue.
    async fn list_roles(&self) -> Result<Vec<Role>> {
        Err(Error::not_supported("list_roles"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;

    // ---------------------------------------------------------------------
    // Port of Go TestSentinels: guards the wire-stable sentinel error values
    // shipped to every concrete adapter — bytes-equal across runtimes.
    // ---------------------------------------------------------------------

    #[test]
    fn sentinels() {
        assert_eq!(Error::InvalidCredentials, Error::InvalidCredentials);
        assert_eq!(Error::UserNotFound, Error::UserNotFound);
        for e in [Error::InvalidCredentials, Error::UserNotFound] {
            assert!(!e.to_string().is_empty(), "error message empty: {e:?}");
        }
    }

    #[test]
    fn sentinel_messages_match_go() {
        assert_eq!(
            Error::InvalidCredentials.to_string(),
            "firefly/idp: invalid credentials"
        );
        assert_eq!(
            Error::UserNotFound.to_string(),
            "firefly/idp: user not found"
        );
    }

    #[test]
    fn provider_error_renders_message_verbatim() {
        let e = Error::provider("idp/internal-db: malformed jwt");
        assert_eq!(e, Error::Provider("idp/internal-db: malformed jwt".into()));
        assert_eq!(e.to_string(), "idp/internal-db: malformed jwt");
    }

    #[test]
    fn unsupported_by_provider_names_provider_operation_and_reason() {
        let e = Error::unsupported_by_provider(
            "keycloak",
            "mfa_verify",
            "Keycloak verifies TOTP only during the interactive browser sign-in",
        );
        assert_eq!(
            e,
            Error::UnsupportedByProvider {
                provider: "keycloak".into(),
                operation: "mfa_verify".into(),
                reason: "Keycloak verifies TOTP only during the interactive browser sign-in".into(),
            }
        );
        assert_eq!(
            e.to_string(),
            "firefly/idp: keycloak cannot perform operation 'mfa_verify': \
             Keycloak verifies TOTP only during the interactive browser sign-in"
        );
        // Distinct from the generic NotSupported variant.
        assert_ne!(e, Error::not_supported("mfa_verify"));
    }

    // ---------------------------------------------------------------------
    // Rust-specific: wire-shape parity with the Go port's encoding/json tags.
    // ---------------------------------------------------------------------

    fn sample_user() -> User {
        User {
            id: "u1".into(),
            username: "alice".into(),
            email: "alice@example.com".into(),
            roles: vec!["user".into(), "admin".into()],
            attributes: HashMap::from([("dept".to_string(), serde_json::json!("eng"))]),
            enabled: true,
            created_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
        }
    }

    #[test]
    fn user_json_wire_shape_matches_go() {
        let got = serde_json::to_string(&sample_user()).unwrap();
        let want = r#"{"id":"u1","username":"alice","email":"alice@example.com","roles":["user","admin"],"attributes":{"dept":"eng"},"enabled":true,"createdAt":"2025-01-02T03:04:05Z"}"#;
        assert_eq!(got, want);
    }

    #[test]
    fn user_json_omits_empty_optionals() {
        let u = User {
            id: "u2".into(),
            username: "bob".into(),
            ..User::default()
        };
        let got = serde_json::to_string(&u).unwrap();
        let want =
            r#"{"id":"u2","username":"bob","enabled":false,"createdAt":"0001-01-01T00:00:00Z"}"#;
        assert_eq!(got, want);
    }

    #[test]
    fn user_round_trip() {
        let u = sample_user();
        let json = serde_json::to_string(&u).unwrap();
        let back: User = serde_json::from_str(&json).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn user_deserialize_tolerates_missing_fields() {
        // Go's encoding/json leaves missing fields at their zero values.
        let u: User = serde_json::from_str("{}").unwrap();
        assert_eq!(u, User::default());
        assert_eq!(u.created_at, zero_time());
    }

    fn sample_token() -> Token {
        Token {
            access_token: "at".into(),
            token_type: "Bearer".into(),
            expires_in: 900,
            refresh_token: "rt".into(),
            id_token: "idt".into(),
            scope: "openid profile".into(),
            issued_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
        }
    }

    #[test]
    fn token_json_wire_shape_matches_go() {
        let got = serde_json::to_string(&sample_token()).unwrap();
        let want = r#"{"access_token":"at","token_type":"Bearer","expires_in":900,"refresh_token":"rt","id_token":"idt","scope":"openid profile","issued_at":"2025-01-02T03:04:05Z"}"#;
        assert_eq!(got, want);
    }

    #[test]
    fn token_json_omits_empty_optionals() {
        let t = Token {
            access_token: "at".into(),
            token_type: "Bearer".into(),
            expires_in: 900,
            ..Token::default()
        };
        let got = serde_json::to_string(&t).unwrap();
        let want = r#"{"access_token":"at","token_type":"Bearer","expires_in":900,"issued_at":"0001-01-01T00:00:00Z"}"#;
        assert_eq!(got, want);
    }

    #[test]
    fn token_round_trip() {
        let t = sample_token();
        let json = serde_json::to_string(&t).unwrap();
        let back: Token = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn token_deserialize_tolerates_missing_fields() {
        let t: Token = serde_json::from_str("{}").unwrap();
        assert_eq!(t, Token::default());
    }

    #[test]
    fn defaults_mirror_go_zero_values() {
        let u = User::default();
        assert!(u.id.is_empty() && u.username.is_empty() && u.email.is_empty());
        assert!(u.roles.is_empty() && u.attributes.is_empty());
        assert!(!u.enabled);
        assert_eq!(u.created_at.to_rfc3339(), "0001-01-01T00:00:00+00:00");

        let t = Token::default();
        assert!(t.access_token.is_empty() && t.token_type.is_empty());
        assert_eq!(t.expires_in, 0);
        assert_eq!(t.issued_at, zero_time());
    }

    // ---------------------------------------------------------------------
    // Rust-specific: the port is object-safe and usable behind Arc/Box.
    // ---------------------------------------------------------------------

    /// Minimal in-memory adapter standing in for a concrete provider.
    struct StaticAdapter;

    #[async_trait]
    impl Adapter for StaticAdapter {
        async fn login(&self, username: &str, password: &str) -> Result<Token> {
            if username == "alice" && password == "s3cret" {
                Ok(Token {
                    access_token: "at".into(),
                    token_type: "Bearer".into(),
                    expires_in: 900,
                    ..Token::default()
                })
            } else {
                Err(Error::InvalidCredentials)
            }
        }

        async fn refresh(&self, refresh_token: &str) -> Result<Token> {
            if refresh_token == "rt" {
                Ok(Token::default())
            } else {
                Err(Error::provider("static: unknown refresh token"))
            }
        }

        async fn validate(&self, access_token: &str) -> Result<User> {
            if access_token == "at" {
                Ok(sample_user())
            } else {
                Err(Error::InvalidCredentials)
            }
        }

        async fn get_user(&self, id: &str) -> Result<User> {
            if id == "u1" {
                Ok(sample_user())
            } else {
                Err(Error::UserNotFound)
            }
        }

        async fn create_user(&self, mut user: User, _password: &str) -> Result<User> {
            user.id = "u1".into();
            Ok(user)
        }

        async fn update_user(&self, user: User) -> Result<User> {
            if user.id == "u1" {
                Ok(user)
            } else {
                Err(Error::UserNotFound)
            }
        }

        async fn delete_user(&self, id: &str) -> Result<()> {
            if id == "u1" {
                Ok(())
            } else {
                Err(Error::UserNotFound)
            }
        }

        fn name(&self) -> &str {
            "static"
        }
    }

    #[tokio::test]
    async fn adapter_usable_as_trait_object() {
        let idp: Arc<dyn Adapter> = Arc::new(StaticAdapter);
        assert_eq!(idp.name(), "static");

        let token = idp.login("alice", "s3cret").await.unwrap();
        assert_eq!(token.token_type, "Bearer");
        assert_eq!(
            idp.login("alice", "wrong").await.unwrap_err(),
            Error::InvalidCredentials
        );

        let user = idp.validate(&token.access_token).await.unwrap();
        assert_eq!(user.username, "alice");

        assert_eq!(idp.get_user("nope").await.unwrap_err(), Error::UserNotFound);
        let created = idp.create_user(User::default(), "pw").await.unwrap();
        assert_eq!(created.id, "u1");
        idp.delete_user("u1").await.unwrap();
        assert_eq!(
            idp.update_user(User::default()).await.unwrap_err(),
            Error::UserNotFound
        );
        assert_eq!(
            idp.refresh("nope").await.unwrap_err(),
            Error::provider("static: unknown refresh token")
        );
    }

    #[test]
    fn port_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<User>();
        assert_send_sync::<Token>();
        assert_send_sync::<Error>();
        assert_send_sync::<Role>();
        assert_send_sync::<MfaChallenge>();
        assert_send_sync::<SessionIntrospection>();
        assert_send_sync::<Box<dyn Adapter>>();
        assert_send_sync::<Arc<dyn Adapter>>();
    }

    // ---------------------------------------------------------------------
    // pyfly parity: extended-surface DTOs (Role, MfaChallenge,
    // SessionIntrospection) and the MfaRequired / NotSupported errors.
    // ---------------------------------------------------------------------

    #[test]
    fn role_json_wire_shape_matches_pyfly() {
        let r = Role {
            name: "admin".into(),
            description: "Full access".into(),
            scopes: vec!["read".into(), "write".into()],
        };
        let got = serde_json::to_string(&r).unwrap();
        assert_eq!(
            got,
            r#"{"name":"admin","description":"Full access","scopes":["read","write"]}"#
        );
        // Bare role omits empty description and scopes.
        let bare = Role::new("editor");
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"name":"editor"}"#
        );
        // Round-trips.
        let back: Role = serde_json::from_str(&got).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn mfa_challenge_json_shape_and_constructor() {
        let c = MfaChallenge::new("ch-123");
        assert_eq!(c.method, "TOTP");
        assert!(c.user_id.is_empty());
        // Client-facing challenge omits the empty user_id to avoid enumeration.
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"challenge_id":"ch-123","method":"TOTP"}"#
        );
        // A challenge carrying a user_id keeps it.
        let internal = MfaChallenge {
            user_id: "u1".into(),
            ..MfaChallenge::new("ch-1")
        };
        assert_eq!(
            serde_json::to_string(&internal).unwrap(),
            r#"{"challenge_id":"ch-1","user_id":"u1","method":"TOTP"}"#
        );
    }

    #[test]
    fn session_introspection_active_and_inactive() {
        let inactive = SessionIntrospection::inactive();
        assert!(!inactive.active);
        assert_eq!(
            serde_json::to_string(&inactive).unwrap(),
            r#"{"active":false}"#
        );
        let active = SessionIntrospection {
            active: true,
            user_id: "u1".into(),
            username: "alice".into(),
            scopes: vec!["user".into()],
        };
        assert_eq!(
            serde_json::to_string(&active).unwrap(),
            r#"{"active":true,"user_id":"u1","username":"alice","scopes":["user"]}"#
        );
    }

    #[test]
    fn mfa_required_and_not_supported_errors() {
        let chal = MfaChallenge::new("ch-7");
        let e = Error::MfaRequired(chal.clone());
        assert_eq!(e, Error::MfaRequired(chal));
        assert_eq!(
            e.to_string(),
            "firefly/idp: multi-factor authentication required"
        );
        let ns = Error::not_supported("logout");
        assert_eq!(ns, Error::NotSupported("logout".into()));
        assert_eq!(
            ns.to_string(),
            "firefly/idp: operation not supported: logout"
        );
    }

    /// An adapter that implements only the Go-parity required methods — every
    /// extended operation falls through to the default `NotSupported` body,
    /// proving backward compatibility for adapters predating this surface.
    struct MinimalAdapter;

    #[async_trait]
    impl Adapter for MinimalAdapter {
        async fn login(&self, _u: &str, _p: &str) -> Result<Token> {
            Ok(Token::default())
        }
        async fn refresh(&self, _r: &str) -> Result<Token> {
            Ok(Token::default())
        }
        async fn validate(&self, _a: &str) -> Result<User> {
            Ok(User::default())
        }
        async fn get_user(&self, _id: &str) -> Result<User> {
            Ok(User::default())
        }
        async fn create_user(&self, user: User, _p: &str) -> Result<User> {
            Ok(user)
        }
        async fn update_user(&self, user: User) -> Result<User> {
            Ok(user)
        }
        async fn delete_user(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        fn name(&self) -> &str {
            "minimal"
        }
    }

    #[tokio::test]
    async fn extended_methods_default_to_not_supported() {
        let a: Arc<dyn Adapter> = Arc::new(MinimalAdapter);
        // Sanity: required methods still work.
        assert_eq!(a.name(), "minimal");
        a.login("x", "y").await.unwrap();
        // Every extended method falls through to NotSupported(<op>).
        assert_eq!(
            a.logout("t").await.unwrap_err(),
            Error::not_supported("logout")
        );
        assert_eq!(
            a.introspect("t").await.unwrap_err(),
            Error::not_supported("introspect")
        );
        assert_eq!(
            a.find_by_username("u").await.unwrap_err(),
            Error::not_supported("find_by_username")
        );
        assert_eq!(
            a.list_users(10).await.unwrap_err(),
            Error::not_supported("list_users")
        );
        assert_eq!(
            a.change_password("u", "o", "n").await.unwrap_err(),
            Error::not_supported("change_password")
        );
        assert_eq!(
            a.reset_password("u").await.unwrap_err(),
            Error::not_supported("reset_password")
        );
        assert_eq!(
            a.register_user(User::default(), "p").await.unwrap_err(),
            Error::not_supported("register_user")
        );
        assert_eq!(
            a.get_user_info("t").await.unwrap_err(),
            Error::not_supported("get_user_info")
        );
        assert_eq!(
            a.mfa_challenge("u").await.unwrap_err(),
            Error::not_supported("mfa_challenge")
        );
        assert_eq!(
            a.mfa_verify("c", "000000").await.unwrap_err(),
            Error::not_supported("mfa_verify")
        );
        assert_eq!(
            a.get_roles("u").await.unwrap_err(),
            Error::not_supported("get_roles")
        );
        assert_eq!(
            a.assign_role("u", "r").await.unwrap_err(),
            Error::not_supported("assign_role")
        );
        assert_eq!(
            a.revoke_role("u", "r").await.unwrap_err(),
            Error::not_supported("revoke_role")
        );
        assert_eq!(
            a.list_roles().await.unwrap_err(),
            Error::not_supported("list_roles")
        );
    }
}
