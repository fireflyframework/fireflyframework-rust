//! firefly-idp-internal-db — self-hosted IdP backed by an in-process user
//! store with bcrypt-hashed passwords and HMAC-SHA256-signed JWTs.
//!
//! This crate is the **Full** reference implementation of the
//! [`firefly_idp::Adapter`] port (the vendor adapters — Keycloak, Azure AD,
//! AWS Cognito — ship as typed stubs). Suitable for:
//!
//! * Development and integration tests.
//! * Small standalone services that don't need an external IdP.
//! * The default IdP wiring of the Orders sample.
//!
//! # Token shape
//!
//! Access and refresh tokens are HS256 JWTs whose claim payload is
//! byte-compatible with the Go port (`idpinternaldb`): claims serialize in
//! the order `exp`, `iat`, `iss`, `roles` (omitted when the user has no
//! roles), `sub`, `un` — exactly the alphabetical key order produced by Go's
//! `encoding/json` map marshalling. Signatures are HMAC-SHA256 over
//! `base64url(header) + "." + base64url(claims)` with unpadded URL-safe
//! base64, so tokens minted by any sibling port verify here and vice versa.
//!
//! The refresh token is currently the same value as the access token —
//! adequate for in-process testing; production deployments should use the
//! proper IDP modules with distinct refresh-token semantics.
//!
//! # Quick start
//!
//! ```
//! use firefly_idp::{Adapter as _, User};
//! use firefly_idp_internal_db::{Adapter, Config};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let idp = Adapter::new(Config {
//!     jwt_secret: b"super-secret-please-rotate".to_vec(),
//!     token_ttl: std::time::Duration::from_secs(3600),
//!     issuer: "orders-service".into(),
//! });
//!
//! let alice = User {
//!     username: "alice".into(),
//!     email: "alice@example.com".into(),
//!     roles: vec!["USER".into()],
//!     enabled: true,
//!     ..User::default()
//! };
//! idp.create_user(alice, "Hunter-2-pass!").await.unwrap();
//! let token = idp.login("alice", "Hunter-2-pass!").await.unwrap();
//! let user = idp.validate(&token.access_token).await.unwrap();
//! assert_eq!(user.username, "alice");
//! # }
//! ```

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use firefly_idp::{Error, Result, Token, User};
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// bcrypt work factor used for stored password hashes — mirrors Go's
/// `bcrypt.DefaultCost` (10) so hashes are interchangeable across ports.
const BCRYPT_COST: u32 = 10;

/// Tunes [`Adapter`].
///
/// `jwt_secret` is required — it signs and verifies every issued token.
/// A zero `token_ttl` is clamped to one hour by [`Adapter::new`], matching
/// the Go port's `New` behavior.
#[derive(Debug, Clone)]
pub struct Config {
    /// Symmetric HS256 signing key — required.
    pub jwt_secret: Vec<u8>,
    /// Access-token lifetime; defaults to 1 h.
    pub token_ttl: Duration,
    /// Value surfaced in the `iss` claim.
    pub issuer: String,
}

impl Default for Config {
    /// Returns a 1-hour token TTL config — callers must supply `jwt_secret`.
    ///
    /// Mirrors Go's `DefaultConfig()`: `TokenTTL: time.Hour`,
    /// `Issuer: "firefly-internal-db"`.
    fn default() -> Self {
        Self {
            jwt_secret: Vec::new(),
            token_ttl: Duration::from_secs(3600),
            issuer: "firefly-internal-db".into(),
        }
    }
}

/// Stored user entry: profile plus bcrypt password hash.
struct Record {
    user: User,
    hash: String,
}

/// Mutable state guarded by one lock: users by id plus the username index.
#[derive(Default)]
struct Inner {
    /// Users keyed by id.
    users: HashMap<String, Record>,
    /// username → id index used by [`Adapter::login`].
    by_username: HashMap<String, String>,
}

/// The in-memory IdP implementation of the [`firefly_idp::Adapter`] port.
///
/// Cheap to construct, safe to share behind an `Arc` across request
/// handlers; all state lives in an interior `RwLock`.
pub struct Adapter {
    cfg: Config,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// bcrypt work factor; [`BCRYPT_COST`] in production, lowered in tests.
    cost: u32,
    inner: RwLock<Inner>,
}

impl Adapter {
    /// Returns a fresh `Adapter`. A zero `token_ttl` is clamped to one hour.
    pub fn new(mut cfg: Config) -> Self {
        if cfg.token_ttl.is_zero() {
            cfg.token_ttl = Duration::from_secs(3600);
        }
        let encoding_key = EncodingKey::from_secret(&cfg.jwt_secret);
        let decoding_key = DecodingKey::from_secret(&cfg.jwt_secret);
        Self {
            encoding_key,
            decoding_key,
            cost: BCRYPT_COST,
            inner: RwLock::new(Inner::default()),
            cfg,
        }
    }

    /// Builds an HS256 JWT [`Token`] envelope for `user`.
    fn mint_token(&self, user: &User) -> Result<Token> {
        let now = Utc::now();
        let ttl_secs = self.cfg.token_ttl.as_secs() as i64;
        let claims = Claims {
            exp: now.timestamp() + ttl_secs,
            iat: now.timestamp(),
            iss: self.cfg.issuer.clone(),
            roles: if user.roles.is_empty() {
                None
            } else {
                Some(user.roles.clone())
            },
            sub: user.id.clone(),
            un: user.username.clone(),
        };
        let jwt = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding_key)
            .map_err(|e| Error::provider(format!("idp/internal-db: {e}")))?;
        Ok(Token {
            access_token: jwt.clone(),
            token_type: "Bearer".into(),
            expires_in: ttl_secs,
            // Symmetric — production adapters issue distinct tokens.
            refresh_token: jwt,
            issued_at: now,
            ..Token::default()
        })
    }

    /// Checks an HS256 JWT and returns its claims.
    ///
    /// Mirrors Go's verify(): only the signature and — when present — `exp`
    /// are checked; every other claim (`aud`, `iss`, `nbf`, …) is ignored.
    /// Error messages mirror the Go port byte-for-byte:
    /// `idp/internal-db: malformed jwt`, `idp/internal-db: bad signature`,
    /// `idp/internal-db: token expired`.
    fn verify(&self, token: &str) -> Result<Claims> {
        let mut validation = Validation::new(Algorithm::HS256);
        // Go checks `exp` strictly (no leeway) and only when present.
        validation.leeway = 0;
        validation.required_spec_claims.clear();
        validation.validate_exp = true;
        // Go ignores every claim other than `exp`, including `aud`.
        // jsonwebtoken's default (`validate_aud = true` with no expected
        // audience) would otherwise reject any validly-signed token that
        // happens to carry an `aud` claim.
        validation.validate_aud = false;
        jsonwebtoken::decode::<Claims>(token, &self.decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|e| match e.kind() {
                ErrorKind::InvalidSignature => Error::provider("idp/internal-db: bad signature"),
                ErrorKind::ExpiredSignature => Error::provider("idp/internal-db: token expired"),
                _ => Error::provider("idp/internal-db: malformed jwt"),
            })
    }
}

#[async_trait]
impl firefly_idp::Adapter for Adapter {
    /// Authenticates `username`/`password` against the in-memory store.
    ///
    /// Both an unknown username and a bcrypt mismatch surface as
    /// [`Error::InvalidCredentials`] — callers can't probe for usernames.
    async fn login(&self, username: &str, password: &str) -> Result<Token> {
        let (user, hash) = {
            let inner = self.inner.read().expect("user store lock poisoned");
            let id = inner
                .by_username
                .get(username)
                .ok_or(Error::InvalidCredentials)?;
            let record = inner.users.get(id).ok_or(Error::InvalidCredentials)?;
            (record.user.clone(), record.hash.clone())
        };
        if !bcrypt::verify(password, &hash).unwrap_or(false) {
            return Err(Error::InvalidCredentials);
        }
        self.mint_token(&user)
    }

    /// Exchanges a (still valid) refresh token for a fresh [`Token`].
    async fn refresh(&self, refresh_token: &str) -> Result<Token> {
        let claims = self.verify(refresh_token)?;
        let user = {
            let inner = self.inner.read().expect("user store lock poisoned");
            inner
                .users
                .get(&claims.sub)
                .map(|r| r.user.clone())
                .ok_or(Error::UserNotFound)?
        };
        self.mint_token(&user)
    }

    /// Verifies an access token and returns the authenticated [`User`].
    ///
    /// Fails with [`Error::UserNotFound`] if the token verifies but the user
    /// has since been deleted.
    async fn validate(&self, access_token: &str) -> Result<User> {
        let claims = self.verify(access_token)?;
        let inner = self.inner.read().expect("user store lock poisoned");
        inner
            .users
            .get(&claims.sub)
            .map(|r| r.user.clone())
            .ok_or(Error::UserNotFound)
    }

    /// Looks up a user by id.
    async fn get_user(&self, id: &str) -> Result<User> {
        let inner = self.inner.read().expect("user store lock poisoned");
        inner
            .users
            .get(id)
            .map(|r| r.user.clone())
            .ok_or(Error::UserNotFound)
    }

    /// Adds a user with a bcrypt-hashed password.
    ///
    /// An empty id defaults to the username; a zero `created_at`
    /// (`0001-01-01T00:00:00Z`) is stamped with the current UTC instant.
    /// A duplicate id is rejected with a `Provider` error rendered as
    /// `idp/internal-db: id "<id>" already exists`.
    async fn create_user(&self, mut user: User, password: &str) -> Result<User> {
        let hash = bcrypt::hash(password, self.cost).map_err(|e| Error::provider(e.to_string()))?;
        let mut inner = self.inner.write().expect("user store lock poisoned");
        if user.id.is_empty() {
            user.id = user.username.clone();
        }
        if inner.users.contains_key(&user.id) {
            return Err(Error::provider(format!(
                "idp/internal-db: id {:?} already exists",
                user.id
            )));
        }
        if user.created_at == zero_time() {
            user.created_at = Utc::now();
        }
        inner
            .by_username
            .insert(user.username.clone(), user.id.clone());
        inner.users.insert(
            user.id.clone(),
            Record {
                user: user.clone(),
                hash,
            },
        );
        Ok(user)
    }

    /// Replaces the stored profile of an existing user (password unchanged).
    async fn update_user(&self, user: User) -> Result<User> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        let record = inner.users.get_mut(&user.id).ok_or(Error::UserNotFound)?;
        record.user = user.clone();
        Ok(user)
    }

    /// Removes a user by id, dropping the username index entry too.
    async fn delete_user(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        let record = inner.users.remove(id).ok_or(Error::UserNotFound)?;
        inner.by_username.remove(&record.user.username);
        Ok(())
    }

    /// Returns `"internal-db"`.
    fn name(&self) -> &str {
        "internal-db"
    }
}

/// JWT claim set minted by [`Adapter`].
///
/// Field order is load-bearing: serde serializes struct fields in
/// declaration order, and `exp`, `iat`, `iss`, `roles`, `sub`, `un` is the
/// alphabetical order Go's `encoding/json` produces for the equivalent
/// claims map — keeping payloads byte-identical across ports.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// Expiry (Unix seconds).
    #[serde(default)]
    exp: i64,
    /// Issued-at (Unix seconds).
    #[serde(default)]
    iat: i64,
    /// Issuer — [`Config::issuer`].
    #[serde(default)]
    iss: String,
    /// Role names; omitted entirely when the user has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    roles: Option<Vec<String>>,
    /// Subject — the user id.
    #[serde(default)]
    sub: String,
    /// Username convenience claim.
    #[serde(default)]
    un: String,
}

/// The Go `time.Time` zero value (`0001-01-01T00:00:00Z`) — the default
/// `created_at` of [`User`], treated as "unset" by [`Adapter::create_user`].
fn zero_time() -> DateTime<Utc> {
    NaiveDate::from_ymd_opt(1, 1, 1)
        .expect("valid date")
        .and_hms_opt(0, 0, 0)
        .expect("valid time")
        .and_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use firefly_idp::Adapter as _;
    use std::sync::Arc;

    /// Low bcrypt work factor so the suite stays fast (Go runs DefaultCost,
    /// but its bcrypt is much cheaper to spin up per test binary).
    const TEST_COST: u32 = 4;

    const SECRET: &[u8] = b"super-secret";

    fn test_adapter() -> Adapter {
        let mut a = Adapter::new(Config {
            jwt_secret: SECRET.to_vec(),
            token_ttl: Duration::ZERO,
            issuer: "test".into(),
        });
        a.cost = TEST_COST;
        a
    }

    fn alice() -> User {
        User {
            username: "alice".into(),
            email: "alice@example.com".into(),
            roles: vec!["user".into()],
            enabled: true,
            ..User::default()
        }
    }

    /// Builds a token with the Go port's exact header bytes
    /// (`{"alg":"HS256","typ":"JWT"}` — alphabetical key order) so we can
    /// prove cross-port verification and craft expired/forged tokens.
    fn go_style_token(secret: &[u8], claims_json: &str) -> String {
        let hp = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let cp = URL_SAFE_NO_PAD.encode(claims_json);
        let message = format!("{hp}.{cp}");
        let sig = jsonwebtoken::crypto::sign(
            message.as_bytes(),
            &EncodingKey::from_secret(secret),
            Algorithm::HS256,
        )
        .expect("sign");
        format!("{message}.{sig}")
    }

    // -----------------------------------------------------------------
    // Port of Go TestAdapterFlow.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn adapter_flow() {
        let a = test_adapter();
        let created = a.create_user(alice(), "Hunter-2-pass!").await.unwrap();
        assert!(!created.id.is_empty(), "id not set");

        assert_eq!(
            a.login("alice", "wrong").await.unwrap_err(),
            Error::InvalidCredentials,
            "bad password"
        );
        let tok = a.login("alice", "Hunter-2-pass!").await.unwrap();
        assert!(!tok.access_token.is_empty(), "token empty");

        let user = a.validate(&tok.access_token).await.unwrap();
        assert_eq!(user.username, "alice");

        a.refresh(&tok.refresh_token).await.expect("refresh");

        a.delete_user(&created.id).await.unwrap();
        assert_eq!(
            a.get_user(&created.id).await.unwrap_err(),
            Error::UserNotFound,
            "expected NotFound"
        );
    }

    // -----------------------------------------------------------------
    // Port of Go TestAdapterImplementsPort.
    // -----------------------------------------------------------------

    #[test]
    fn adapter_implements_port() {
        let _: Arc<dyn firefly_idp::Adapter> = Arc::new(Adapter::new(Config {
            jwt_secret: b"k".to_vec(),
            token_ttl: Duration::ZERO,
            issuer: String::new(),
        }));
    }

    // -----------------------------------------------------------------
    // Rust-specific: configuration semantics.
    // -----------------------------------------------------------------

    #[test]
    fn default_config_matches_go() {
        let cfg = Config::default();
        assert!(cfg.jwt_secret.is_empty());
        assert_eq!(cfg.token_ttl, Duration::from_secs(3600));
        assert_eq!(cfg.issuer, "firefly-internal-db");
    }

    #[tokio::test]
    async fn new_clamps_zero_ttl_to_one_hour() {
        let a = test_adapter(); // built with token_ttl: Duration::ZERO
        a.create_user(alice(), "pw").await.unwrap();
        let tok = a.login("alice", "pw").await.unwrap();
        assert_eq!(tok.expires_in, 3600);
    }

    // -----------------------------------------------------------------
    // Rust-specific: user CRUD semantics.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn create_user_defaults_id_to_username_and_stamps_created_at() {
        let a = test_adapter();
        let created = a.create_user(alice(), "pw").await.unwrap();
        assert_eq!(created.id, "alice");
        assert_ne!(created.created_at, zero_time());
        assert!(
            Utc::now().signed_duration_since(created.created_at) < chrono::Duration::seconds(5)
        );
    }

    #[tokio::test]
    async fn create_user_preserves_explicit_id_and_created_at() {
        let a = test_adapter();
        let stamp = Utc::now() - chrono::Duration::days(1);
        let u = User {
            id: "u-42".into(),
            created_at: stamp,
            ..alice()
        };
        let created = a.create_user(u, "pw").await.unwrap();
        assert_eq!(created.id, "u-42");
        assert_eq!(created.created_at, stamp);
        assert_eq!(a.get_user("u-42").await.unwrap().username, "alice");
    }

    #[tokio::test]
    async fn create_user_rejects_duplicate_id() {
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let err = a.create_user(alice(), "pw").await.unwrap_err();
        assert_eq!(
            err,
            Error::Provider(r#"idp/internal-db: id "alice" already exists"#.into())
        );
        assert_eq!(
            err.to_string(),
            r#"idp/internal-db: id "alice" already exists"#
        );
    }

    #[tokio::test]
    async fn update_user_replaces_profile() {
        let a = test_adapter();
        let created = a.create_user(alice(), "pw").await.unwrap();
        let updated = a
            .update_user(User {
                email: "new@example.com".into(),
                ..created.clone()
            })
            .await
            .unwrap();
        assert_eq!(updated.email, "new@example.com");
        assert_eq!(
            a.get_user(&created.id).await.unwrap().email,
            "new@example.com"
        );
        // Password is untouched by profile updates.
        a.login("alice", "pw").await.unwrap();
    }

    #[tokio::test]
    async fn update_and_delete_missing_user_return_not_found() {
        let a = test_adapter();
        assert_eq!(
            a.update_user(alice()).await.unwrap_err(),
            Error::UserNotFound
        );
        assert_eq!(
            a.delete_user("ghost").await.unwrap_err(),
            Error::UserNotFound
        );
        assert_eq!(a.get_user("ghost").await.unwrap_err(), Error::UserNotFound);
    }

    #[tokio::test]
    async fn login_unknown_user_is_invalid_credentials() {
        let a = test_adapter();
        assert_eq!(
            a.login("nobody", "pw").await.unwrap_err(),
            Error::InvalidCredentials
        );
    }

    // -----------------------------------------------------------------
    // Rust-specific: token verification failure modes (Go error strings
    // byte-for-byte).
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn validate_malformed_token() {
        let a = test_adapter();
        assert_eq!(
            a.validate("not-a-jwt").await.unwrap_err(),
            Error::Provider("idp/internal-db: malformed jwt".into())
        );
    }

    #[tokio::test]
    async fn validate_bad_signature() {
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let now = Utc::now().timestamp();
        let forged = go_style_token(
            b"other-secret",
            &format!(
                r#"{{"exp":{},"iat":{now},"iss":"test","sub":"alice","un":"alice"}}"#,
                now + 3600
            ),
        );
        assert_eq!(
            a.validate(&forged).await.unwrap_err(),
            Error::Provider("idp/internal-db: bad signature".into())
        );
    }

    #[tokio::test]
    async fn validate_expired_token() {
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let past = Utc::now().timestamp() - 120;
        let expired = go_style_token(
            SECRET,
            &format!(r#"{{"exp":{past},"iat":{past},"iss":"test","sub":"alice","un":"alice"}}"#),
        );
        assert_eq!(
            a.validate(&expired).await.unwrap_err(),
            Error::Provider("idp/internal-db: token expired".into())
        );
    }

    #[tokio::test]
    async fn validate_and_refresh_after_delete_return_not_found() {
        let a = test_adapter();
        let created = a.create_user(alice(), "pw").await.unwrap();
        let tok = a.login("alice", "pw").await.unwrap();
        a.delete_user(&created.id).await.unwrap();
        // Token still verifies cryptographically, but the user is gone.
        assert_eq!(
            a.validate(&tok.access_token).await.unwrap_err(),
            Error::UserNotFound
        );
        assert_eq!(
            a.refresh(&tok.refresh_token).await.unwrap_err(),
            Error::UserNotFound
        );
    }

    #[tokio::test]
    async fn validate_accepts_go_minted_token() {
        // A token with Go's exact header bytes and claim ordering verifies,
        // proving cross-port token compatibility.
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let now = Utc::now().timestamp();
        let token = go_style_token(
            SECRET,
            &format!(
                r#"{{"exp":{},"iat":{now},"iss":"test","roles":["user"],"sub":"alice","un":"alice"}}"#,
                now + 3600
            ),
        );
        let user = a.validate(&token).await.unwrap();
        assert_eq!(user.username, "alice");
        assert_eq!(user.email, "alice@example.com");
        a.refresh(&token).await.expect("go-minted refresh token");
    }

    #[tokio::test]
    async fn validate_accepts_token_with_aud_claim() {
        // Go's verify() checks only the signature and (when present) `exp`;
        // every other claim — including `aud` — is ignored. Regression guard:
        // jsonwebtoken's default `validate_aud = true` (with no expected
        // audience configured) would otherwise reject this validly-signed
        // token as `malformed jwt`.
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let now = Utc::now().timestamp();
        let token = go_style_token(
            SECRET,
            &format!(
                r#"{{"aud":"orders","exp":{},"iat":{now},"iss":"test","sub":"alice","un":"alice"}}"#,
                now + 3600
            ),
        );
        let user = a.validate(&token).await.expect("aud claim must be ignored");
        assert_eq!(user.username, "alice");
        a.refresh(&token)
            .await
            .expect("refresh token with aud claim");
    }

    // -----------------------------------------------------------------
    // Rust-specific: minted wire shapes pinned to the Go port.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn claims_wire_shape_matches_go() {
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let tok = a.login("alice", "pw").await.unwrap();

        let mut parts = tok.access_token.split('.');
        let header = URL_SAFE_NO_PAD.decode(parts.next().unwrap()).unwrap();
        let payload = URL_SAFE_NO_PAD.decode(parts.next().unwrap()).unwrap();
        assert!(parts.next().is_some(), "missing signature segment");

        // Header carries exactly alg=HS256 / typ=JWT.
        let h: serde_json::Value = serde_json::from_slice(&header).unwrap();
        assert_eq!(h["alg"], "HS256");
        assert_eq!(h["typ"], "JWT");

        // Claims are byte-identical to Go's sorted-map encoding.
        let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        let exp = v["exp"].as_i64().unwrap();
        let iat = v["iat"].as_i64().unwrap();
        assert_eq!(exp, iat + 3600);
        let want = format!(
            r#"{{"exp":{exp},"iat":{iat},"iss":"test","roles":["user"],"sub":"alice","un":"alice"}}"#
        );
        assert_eq!(String::from_utf8(payload).unwrap(), want);
    }

    #[tokio::test]
    async fn roles_claim_omitted_when_user_has_none() {
        let a = test_adapter();
        let u = User {
            roles: Vec::new(),
            ..alice()
        };
        a.create_user(u, "pw").await.unwrap();
        let tok = a.login("alice", "pw").await.unwrap();
        let payload = URL_SAFE_NO_PAD
            .decode(tok.access_token.split('.').nth(1).unwrap())
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert!(v.get("roles").is_none(), "roles should be omitted: {v}");
    }

    #[tokio::test]
    async fn token_envelope_matches_go() {
        let a = test_adapter();
        a.create_user(alice(), "pw").await.unwrap();
        let tok = a.login("alice", "pw").await.unwrap();
        assert_eq!(tok.token_type, "Bearer");
        assert_eq!(tok.expires_in, 3600);
        assert_eq!(tok.refresh_token, tok.access_token, "symmetric tokens");
        assert!(tok.id_token.is_empty() && tok.scope.is_empty());
        assert!(Utc::now().signed_duration_since(tok.issued_at) < chrono::Duration::seconds(5));
    }

    // -----------------------------------------------------------------
    // Rust-specific: ergonomics under concurrency and trait-object use.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn shared_behind_arc_across_tasks() {
        let a = Arc::new(test_adapter());
        a.create_user(alice(), "pw").await.unwrap();
        let idp: Arc<dyn firefly_idp::Adapter> = a;
        assert_eq!(idp.name(), "internal-db");

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let idp = Arc::clone(&idp);
                tokio::spawn(async move { idp.login("alice", "pw").await })
            })
            .collect();
        for h in handles {
            let tok = h.await.unwrap().unwrap();
            assert_eq!(tok.token_type, "Bearer");
        }
    }

    #[test]
    fn adapter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Adapter>();
        assert_send_sync::<Config>();
    }
}
