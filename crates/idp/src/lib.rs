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

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

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
    /// Any other adapter-specific failure; the message is rendered verbatim.
    #[error("{0}")]
    Provider(String),
}

impl Error {
    /// Builds an adapter-specific [`Error::Provider`] from any message.
    pub fn provider(message: impl Into<String>) -> Self {
        Error::Provider(message.into())
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
        assert_send_sync::<Box<dyn Adapter>>();
        assert_send_sync::<Arc<dyn Adapter>>();
    }
}
