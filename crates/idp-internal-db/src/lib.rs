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

mod totp;

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, NaiveDate, Utc};
use firefly_idp::{Error, MfaChallenge, Result, Role, SessionIntrospection, Token, User};
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
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

/// Mutable state guarded by one lock: users by id plus the username index, the
/// role catalogue, the opaque-token / refresh registries, and the MFA state.
///
/// The opaque-token registry is what makes server-side `logout` and
/// `introspect` possible despite tokens being stateless JWTs: every minted
/// access token is *also* recorded here (`token → user_id`), so it can be
/// revoked or introspected by value — mirroring pyfly's `_tokens` dict.
#[derive(Default)]
struct Inner {
    /// Users keyed by id.
    users: HashMap<String, Record>,
    /// username → id index used by [`Adapter::login`].
    by_username: HashMap<String, String>,
    /// Role catalogue: role name → [`Role`] (with description/scopes).
    roles: HashMap<String, Role>,
    /// Live access-token registry: access token → user id (pyfly `_tokens`).
    tokens: HashMap<String, String>,
    /// Live refresh-token registry: refresh token → user id (pyfly `_refresh`).
    refresh: HashMap<String, String>,
    /// TOTP secrets for MFA-enabled users: user id → base32 secret.
    mfa_secrets: HashMap<String, String>,
    /// Pending MFA challenges: challenge id → user id (single-use).
    mfa_challenges: HashMap<String, String>,
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

    /// Mints a [`Token`] for `user` and records its access and refresh tokens
    /// in the opaque-token registries so [`firefly_idp::Adapter::logout`] and
    /// [`firefly_idp::Adapter::introspect`] can later resolve them by value.
    ///
    /// The JWT wire shape is unchanged (Go parity): the registry is an
    /// additive, server-side index layered over the stateless token.
    fn mint_and_register(&self, user: &User) -> Result<Token> {
        let token = self.mint_token(user)?;
        let mut inner = self.inner.write().expect("user store lock poisoned");
        inner
            .tokens
            .insert(token.access_token.clone(), user.id.clone());
        inner
            .refresh
            .insert(token.refresh_token.clone(), user.id.clone());
        Ok(token)
    }

    /// Generates a fresh opaque, URL-safe token (256 bits of entropy), used as
    /// the challenge identifier handed to MFA clients.
    fn opaque_token() -> String {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Records a single-use MFA challenge for `user_id` and returns the
    /// client-facing [`MfaChallenge`] (opaque `challenge_id`, empty `user_id`
    /// to avoid enumeration). Fails with [`Error::UserNotFound`] if the user
    /// does not exist.
    fn new_challenge(&self, user_id: &str) -> Result<MfaChallenge> {
        let challenge_id = Self::opaque_token();
        let mut inner = self.inner.write().expect("user store lock poisoned");
        if !inner.users.contains_key(user_id) {
            return Err(Error::UserNotFound);
        }
        inner
            .mfa_challenges
            .insert(challenge_id.clone(), user_id.to_string());
        Ok(MfaChallenge::new(challenge_id))
    }

    /// Enrolls TOTP MFA for `user_id`, returning the provisioning secret to be
    /// shown to the user (e.g. as an `otpauth://` QR code).
    ///
    /// Adapter-specific (not part of the [`firefly_idp::Adapter`] port) —
    /// mirrors pyfly's `InternalDbIdpAdapter.enable_mfa`. The secret is a
    /// base32 string; codes are HMAC-SHA256 TOTP (see the `totp` module — this
    /// differs from pyfly's HMAC-SHA1 because the workspace ships only `sha2`).
    /// Fails with [`Error::UserNotFound`] when the user does not exist.
    pub async fn enable_mfa(&self, user_id: &str) -> Result<String> {
        let secret = totp::generate_secret();
        let mut inner = self.inner.write().expect("user store lock poisoned");
        if !inner.users.contains_key(user_id) {
            return Err(Error::UserNotFound);
        }
        inner
            .mfa_secrets
            .insert(user_id.to_string(), secret.clone());
        Ok(secret)
    }

    /// Generates the current TOTP code for an MFA-enrolled `user_id`.
    ///
    /// Adapter-specific test/automation helper: equivalent to
    /// `pyotp.TOTP(secret).now()` for the user's enrolled secret (HMAC-SHA256).
    /// Fails with [`Error::InvalidCredentials`] when the user has no enrolled
    /// secret.
    pub async fn current_totp(&self, user_id: &str) -> Result<String> {
        let secret = {
            let inner = self.inner.read().expect("user store lock poisoned");
            inner
                .mfa_secrets
                .get(user_id)
                .cloned()
                .ok_or(Error::InvalidCredentials)?
        };
        totp::totp_now(&secret)
            .ok_or_else(|| Error::provider("idp/internal-db: invalid TOTP secret"))
    }

    /// Creates named roles in the catalogue (idempotent), returning the
    /// resulting [`Role`] entries in argument order.
    ///
    /// Adapter-specific (not part of the port) — mirrors pyfly's
    /// `InternalDbIdpAdapter.create_roles`. Use [`Self::set_role_description`]
    /// to enrich a catalogue entry afterwards.
    pub async fn create_roles(&self, roles: &[&str]) -> Vec<Role> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        roles
            .iter()
            .map(|&name| {
                inner
                    .roles
                    .entry(name.to_string())
                    .or_insert_with(|| Role::new(name))
                    .clone()
            })
            .collect()
    }

    /// Sets the `description` of a catalogue role, creating the entry if absent.
    /// Returns `true` if the role existed (or was created) — always `true`.
    ///
    /// Adapter-specific helper mirroring pyfly's test mutating
    /// `adapter._roles["superadmin"].description`. Lets callers enrich a role
    /// without a public mutable handle into the catalogue.
    pub async fn set_role_description(&self, role: &str, description: &str) {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        inner
            .roles
            .entry(role.to_string())
            .or_insert_with(|| Role::new(role))
            .description = description.to_string();
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
        if !user.enabled {
            return Err(Error::InvalidCredentials);
        }
        if !bcrypt::verify(password, &hash).unwrap_or(false) {
            return Err(Error::InvalidCredentials);
        }
        // MFA gate: when a TOTP secret is enrolled for this user, login cannot
        // mint a token directly — it returns an [`Error::MfaRequired`] carrying
        // a fresh challenge the caller completes with `mfa_verify`. (pyfly
        // models this as `AuthResult.mfa_required=True`; Rust uses a fallible
        // result so the stateless `Token` need not carry a "pending" flag.)
        let mfa_secret = {
            let inner = self.inner.read().expect("user store lock poisoned");
            inner.mfa_secrets.get(&user.id).cloned()
        };
        if mfa_secret.is_some() {
            let challenge = self.new_challenge(&user.id)?;
            return Err(Error::MfaRequired(challenge));
        }
        self.mint_and_register(&user)
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
        self.mint_and_register(&user)
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

    /// Removes a user by id, dropping the username index and any enrolled MFA
    /// secret. Live tokens for the user are left in the registry but become
    /// inert — [`Self::introspect`] reports them inactive once the user is gone.
    async fn delete_user(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        let record = inner.users.remove(id).ok_or(Error::UserNotFound)?;
        inner.by_username.remove(&record.user.username);
        inner.mfa_secrets.remove(id);
        Ok(())
    }

    /// Returns `"internal-db"`.
    fn name(&self) -> &str {
        "internal-db"
    }

    // -----------------------------------------------------------------
    // Extended surface (pyfly parity).
    // -----------------------------------------------------------------

    /// Revokes an access token from the registry. Returns `true` when a live
    /// session existed and was removed, `false` when the token was unknown.
    /// Mirrors pyfly's `logout` (which pops `_tokens`).
    async fn logout(&self, access_token: &str) -> Result<bool> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        Ok(inner.tokens.remove(access_token).is_some())
    }

    /// Introspects an access token against the registry (RFC 7662). An unknown
    /// or revoked token — or one whose user has since been deleted — yields
    /// [`SessionIntrospection::inactive`]; an active token reports the user id,
    /// username, and the user's roles as scopes (mirroring pyfly).
    async fn introspect(&self, access_token: &str) -> Result<SessionIntrospection> {
        let inner = self.inner.read().expect("user store lock poisoned");
        let Some(user_id) = inner.tokens.get(access_token) else {
            return Ok(SessionIntrospection::inactive());
        };
        match inner.users.get(user_id) {
            Some(record) => Ok(SessionIntrospection {
                active: true,
                user_id: record.user.id.clone(),
                username: record.user.username.clone(),
                scopes: record.user.roles.clone(),
            }),
            None => Ok(SessionIntrospection::inactive()),
        }
    }

    /// Looks up a user by login name; fails with [`Error::UserNotFound`] when
    /// no user has that username.
    async fn find_by_username(&self, username: &str) -> Result<User> {
        let inner = self.inner.read().expect("user store lock poisoned");
        inner
            .by_username
            .get(username)
            .and_then(|id| inner.users.get(id))
            .map(|r| r.user.clone())
            .ok_or(Error::UserNotFound)
    }

    /// Returns up to `limit` users (insertion order is not guaranteed, matching
    /// pyfly's dict-values slice on an unordered map).
    async fn list_users(&self, limit: usize) -> Result<Vec<User>> {
        let inner = self.inner.read().expect("user store lock poisoned");
        Ok(inner
            .users
            .values()
            .take(limit)
            .map(|r| r.user.clone())
            .collect())
    }

    /// Changes a user's password after verifying `old_password`. Returns `true`
    /// on success, `false` when the user is unknown or `old_password` is wrong
    /// — mirroring pyfly's `change_password` (no error on mismatch).
    async fn change_password(
        &self,
        user_id: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<bool> {
        let current = {
            let inner = self.inner.read().expect("user store lock poisoned");
            match inner.users.get(user_id) {
                Some(r) => r.hash.clone(),
                None => return Ok(false),
            }
        };
        if !bcrypt::verify(old_password, &current).unwrap_or(false) {
            return Ok(false);
        }
        let hash =
            bcrypt::hash(new_password, self.cost).map_err(|e| Error::provider(e.to_string()))?;
        let mut inner = self.inner.write().expect("user store lock poisoned");
        if let Some(record) = inner.users.get_mut(user_id) {
            record.hash = hash;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Resets a user's password to a freshly generated opaque value and returns
    /// it (the caller is responsible for delivering it out-of-band). Fails with
    /// [`Error::UserNotFound`] when the user does not exist.
    async fn reset_password(&self, user_id: &str) -> Result<String> {
        let new_password = Self::opaque_token();
        let hash =
            bcrypt::hash(&new_password, self.cost).map_err(|e| Error::provider(e.to_string()))?;
        let mut inner = self.inner.write().expect("user store lock poisoned");
        let record = inner.users.get_mut(user_id).ok_or(Error::UserNotFound)?;
        record.hash = hash;
        Ok(new_password)
    }

    /// Public self-registration: forces the account enabled and strips the
    /// privileged `admin` role, then provisions it via [`Self::create_user`].
    /// Mirrors pyfly's `register_user`.
    async fn register_user(&self, mut user: User, password: &str) -> Result<User> {
        user.enabled = true;
        user.roles.retain(|r| r != "admin");
        self.create_user(user, password).await
    }

    /// Resolves an access token to its owning [`User`]; fails with
    /// [`Error::UserNotFound`] when the token is unknown or revoked.
    async fn get_user_info(&self, access_token: &str) -> Result<User> {
        let inner = self.inner.read().expect("user store lock poisoned");
        inner
            .tokens
            .get(access_token)
            .and_then(|id| inner.users.get(id))
            .map(|r| r.user.clone())
            .ok_or(Error::UserNotFound)
    }

    /// Creates a single-use TOTP challenge for `user_id`. The returned
    /// [`MfaChallenge`] carries only the opaque `challenge_id`.
    async fn mfa_challenge(&self, user_id: &str) -> Result<MfaChallenge> {
        self.new_challenge(user_id)
    }

    /// Verifies a TOTP `code` against the (single-use) `challenge_id` and, on
    /// success, mints and registers a fresh [`Token`].
    ///
    /// The challenge is consumed (removed) regardless of whether the code is
    /// valid, so a stolen challenge id cannot be brute-forced. Fails with
    /// [`Error::InvalidCredentials`] on an unknown/consumed challenge, a user
    /// without an enrolled secret, a wrong code, or a since-deleted user.
    async fn mfa_verify(&self, challenge_id: &str, code: &str) -> Result<Token> {
        let user_id = {
            let mut inner = self.inner.write().expect("user store lock poisoned");
            inner
                .mfa_challenges
                .remove(challenge_id)
                .ok_or(Error::InvalidCredentials)?
        };
        let secret = {
            let inner = self.inner.read().expect("user store lock poisoned");
            inner
                .mfa_secrets
                .get(&user_id)
                .cloned()
                .ok_or(Error::InvalidCredentials)?
        };
        if !totp::verify(&secret, code, 1) {
            return Err(Error::InvalidCredentials);
        }
        let user = {
            let inner = self.inner.read().expect("user store lock poisoned");
            inner
                .users
                .get(&user_id)
                .map(|r| r.user.clone())
                .ok_or(Error::InvalidCredentials)?
        };
        self.mint_and_register(&user)
    }

    /// Returns the [`Role`] objects assigned to `user_id`, enriched from the
    /// role catalogue (a role created via [`Self::create_roles`] keeps its
    /// description/scopes; an unenriched role is returned bare). An unknown user
    /// yields an empty list, matching pyfly.
    async fn get_roles(&self, user_id: &str) -> Result<Vec<Role>> {
        let inner = self.inner.read().expect("user store lock poisoned");
        let Some(record) = inner.users.get(user_id) else {
            return Ok(Vec::new());
        };
        Ok(record
            .user
            .roles
            .iter()
            .map(|name| {
                inner
                    .roles
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| Role::new(name.clone()))
            })
            .collect())
    }

    /// Grants `role` to `user_id` (idempotent) and registers it in the
    /// catalogue. Returns `true` on success, `false` when the user is unknown.
    async fn assign_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        let Some(record) = inner.users.get_mut(user_id) else {
            return Ok(false);
        };
        if !record.user.roles.iter().any(|r| r == role) {
            record.user.roles.push(role.to_string());
        }
        inner
            .roles
            .entry(role.to_string())
            .or_insert_with(|| Role::new(role));
        Ok(true)
    }

    /// Revokes `role` from `user_id`. Returns `true` on success, `false` when
    /// the user is unknown or did not hold the role.
    async fn revoke_role(&self, user_id: &str, role: &str) -> Result<bool> {
        let mut inner = self.inner.write().expect("user store lock poisoned");
        let Some(record) = inner.users.get_mut(user_id) else {
            return Ok(false);
        };
        let before = record.user.roles.len();
        record.user.roles.retain(|r| r != role);
        Ok(record.user.roles.len() != before)
    }

    /// Lists every role in the catalogue (order is unspecified).
    async fn list_roles(&self) -> Result<Vec<Role>> {
        let inner = self.inner.read().expect("user store lock poisoned");
        Ok(inner.roles.values().cloned().collect())
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

    // =================================================================
    // pyfly parity — ported test cases from tests/idp/test_idp.py and
    // tests/idp/test_idp_mfa_and_extensions.py (the internal-db subset;
    // the vendor-adapter protocol/NotImplementedError cases live in the
    // idp-keycloak/idp-azure-ad/idp-aws-cognito crates).
    //
    // Adaptation notes:
    //  * pyfly raises PermissionError on bad credentials/MFA → Rust returns
    //    Error::InvalidCredentials.
    //  * pyfly's login() returns AuthResult(mfa_required=True) when MFA is
    //    enabled and no code is supplied → Rust returns Err(MfaRequired(chal)).
    //  * pyfly's TOTP is HMAC-SHA1 (pyotp); Rust is HMAC-SHA256 (workspace
    //    ships sha2 only). The flow is self-consistent so behavior matches:
    //    enable_mfa → current_totp / verify round-trips.
    // =================================================================

    /// Builds an enabled user whose id equals its username (the adapter
    /// defaults an empty id to the username), mirroring pyfly fixtures.
    fn named(username: &str) -> User {
        User {
            username: username.into(),
            enabled: true,
            ..User::default()
        }
    }

    // ---- ported from test_idp.py::test_create_login_logout -------------

    #[tokio::test]
    async fn create_login_introspect_logout() {
        let a = test_adapter();
        let user = a
            .create_user(
                User {
                    email: "a@x.com".into(),
                    ..named("alice")
                },
                "secret123",
            )
            .await
            .unwrap();
        let tok = a.login("alice", "secret123").await.unwrap();
        assert!(!tok.access_token.is_empty());

        let intro = a.introspect(&tok.access_token).await.unwrap();
        assert!(intro.active);
        assert_eq!(intro.user_id, user.id);

        assert!(a.logout(&tok.access_token).await.unwrap());
        assert!(!a.introspect(&tok.access_token).await.unwrap().active);
        // Logging out an already-revoked token reports no live session.
        assert!(!a.logout(&tok.access_token).await.unwrap());
    }

    // ---- ported from test_idp.py::test_login_failure ------------------

    #[tokio::test]
    async fn login_failure_is_invalid_credentials() {
        let a = test_adapter();
        a.create_user(named("bob"), "rightpass").await.unwrap();
        assert_eq!(
            a.login("bob", "wrongpass").await.unwrap_err(),
            Error::InvalidCredentials
        );
    }

    // ---- ported from test_idp.py::test_change_password ----------------

    #[tokio::test]
    async fn change_password_then_login_with_new() {
        let a = test_adapter();
        let user = a
            .create_user(named("charlie"), "old-pw-1234")
            .await
            .unwrap();
        assert!(a
            .change_password(&user.id, "old-pw-1234", "new-pw-5678")
            .await
            .unwrap());
        // New password works, old one does not.
        a.login("charlie", "new-pw-5678").await.unwrap();
        assert_eq!(
            a.login("charlie", "old-pw-1234").await.unwrap_err(),
            Error::InvalidCredentials
        );
    }

    #[tokio::test]
    async fn change_password_wrong_old_or_unknown_user_returns_false() {
        let a = test_adapter();
        let user = a.create_user(named("dave"), "correct-pw-1").await.unwrap();
        assert!(!a
            .change_password(&user.id, "wrong-old", "whatever-99")
            .await
            .unwrap());
        assert!(!a.change_password("ghost", "x", "y").await.unwrap());
    }

    #[tokio::test]
    async fn reset_password_issues_working_credential() {
        let a = test_adapter();
        let user = a.create_user(named("erin"), "initial-pw-1").await.unwrap();
        let fresh = a.reset_password(&user.id).await.unwrap();
        assert!(!fresh.is_empty());
        // The returned password authenticates; the old one no longer does.
        a.login("erin", &fresh).await.unwrap();
        assert_eq!(
            a.login("erin", "initial-pw-1").await.unwrap_err(),
            Error::InvalidCredentials
        );
        assert_eq!(
            a.reset_password("ghost").await.unwrap_err(),
            Error::UserNotFound
        );
    }

    // ---- ported from test_idp.py::test_role_management ----------------

    #[tokio::test]
    async fn role_management_assign_and_revoke() {
        let a = test_adapter();
        let user = a.create_user(named("dora"), "pw-1234567").await.unwrap();
        assert!(a.assign_role(&user.id, "admin").await.unwrap());
        assert!(a
            .get_user(&user.id)
            .await
            .unwrap()
            .roles
            .contains(&"admin".to_string()));
        assert!(a.revoke_role(&user.id, "admin").await.unwrap());
        assert!(!a
            .get_user(&user.id)
            .await
            .unwrap()
            .roles
            .contains(&"admin".to_string()));
        // Revoking a role the user lacks, or for an unknown user, is false.
        assert!(!a.revoke_role(&user.id, "admin").await.unwrap());
        assert!(!a.assign_role("ghost", "admin").await.unwrap());
    }

    // ---- ported from test_idp_mfa_and_extensions.py (internal-db) -----

    #[tokio::test]
    async fn mfa_enable_and_challenge_flow() {
        let a = test_adapter();
        let user = a
            .create_user(
                User {
                    email: "mfa@x.com".into(),
                    ..named("mfa_user")
                },
                "pass1234!",
            )
            .await
            .unwrap();

        // 1. Enable MFA — returns the provisioning secret.
        let secret = a.enable_mfa(&user.id).await.unwrap();
        assert!(!secret.is_empty());

        // 2. Login WITHOUT mfa code → MfaRequired with a challenge, no token.
        let err = a.login("mfa_user", "pass1234!").await.unwrap_err();
        let challenge = match err {
            Error::MfaRequired(c) => c,
            other => panic!("expected MfaRequired, got {other:?}"),
        };
        assert_eq!(challenge.method, "TOTP");
        assert!(challenge.user_id.is_empty(), "no user id leaked to client");

        // 3. Verify with a VALID TOTP code → issues real tokens.
        let code = a.current_totp(&user.id).await.unwrap();
        let auth = a.mfa_verify(&challenge.challenge_id, &code).await.unwrap();
        assert!(!auth.access_token.is_empty());

        // 4. The issued token resolves via introspect.
        let intro = a.introspect(&auth.access_token).await.unwrap();
        assert!(intro.active);
        assert_eq!(intro.user_id, user.id);
    }

    #[tokio::test]
    async fn mfa_verify_wrong_code_raises() {
        let a = test_adapter();
        let user = a.create_user(named("mfa_bad"), "pass1234!").await.unwrap();
        a.enable_mfa(&user.id).await.unwrap();
        let challenge = match a.login("mfa_bad", "pass1234!").await.unwrap_err() {
            Error::MfaRequired(c) => c,
            other => panic!("expected MfaRequired, got {other:?}"),
        };
        assert_eq!(
            a.mfa_verify(&challenge.challenge_id, "000000")
                .await
                .unwrap_err(),
            Error::InvalidCredentials
        );
    }

    #[tokio::test]
    async fn mfa_login_with_valid_inline_code() {
        // pyfly: login() with mfa_code supplied inline bypasses the challenge
        // redirect. The Rust port models the inline path as: complete a
        // challenge in one shot via mfa_challenge + mfa_verify with a live code.
        let a = test_adapter();
        let user = a
            .create_user(named("mfa_inline"), "pass1234!")
            .await
            .unwrap();
        a.enable_mfa(&user.id).await.unwrap();
        let challenge = a.mfa_challenge(&user.id).await.unwrap();
        let code = a.current_totp(&user.id).await.unwrap();
        let auth = a.mfa_verify(&challenge.challenge_id, &code).await.unwrap();
        assert!(!auth.access_token.is_empty());
    }

    #[tokio::test]
    async fn mfa_verify_consumed_challenge_raises() {
        let a = test_adapter();
        let user = a.create_user(named("mfa_exp"), "pass1234!").await.unwrap();
        a.enable_mfa(&user.id).await.unwrap();
        let challenge = match a.login("mfa_exp", "pass1234!").await.unwrap_err() {
            Error::MfaRequired(c) => c,
            other => panic!("expected MfaRequired, got {other:?}"),
        };
        let code = a.current_totp(&user.id).await.unwrap();
        a.mfa_verify(&challenge.challenge_id, &code).await.unwrap(); // first use ok
                                                                     // Second use — the challenge is consumed.
        assert_eq!(
            a.mfa_verify(&challenge.challenge_id, &code)
                .await
                .unwrap_err(),
            Error::InvalidCredentials
        );
    }

    #[tokio::test]
    async fn enable_mfa_unknown_user_is_not_found() {
        let a = test_adapter();
        assert_eq!(
            a.enable_mfa("ghost").await.unwrap_err(),
            Error::UserNotFound
        );
    }

    // ---- get_user_info ------------------------------------------------

    #[tokio::test]
    async fn get_user_info_resolves_token() {
        let a = test_adapter();
        let user = a
            .create_user(named("info_user"), "pw123456!")
            .await
            .unwrap();
        let tok = a.login("info_user", "pw123456!").await.unwrap();
        let resolved = a.get_user_info(&tok.access_token).await.unwrap();
        assert_eq!(resolved.id, user.id);
        assert_eq!(resolved.username, "info_user");
    }

    #[tokio::test]
    async fn get_user_info_unknown_token_returns_not_found() {
        let a = test_adapter();
        assert_eq!(
            a.get_user_info("totally-bogus-token").await.unwrap_err(),
            Error::UserNotFound
        );
    }

    // ---- register_user ------------------------------------------------

    #[tokio::test]
    async fn register_user_always_enabled_and_admin_stripped() {
        let a = test_adapter();
        let new_user = User {
            username: "reg_user".into(),
            email: "reg@x.com".into(),
            enabled: false,
            roles: vec!["admin".into(), "user".into()],
            ..User::default()
        };
        let registered = a.register_user(new_user, "reg-pass-1234!").await.unwrap();
        assert!(registered.enabled);
        assert!(!registered.roles.contains(&"admin".to_string()));
        assert!(registered.roles.contains(&"user".to_string()));
        // The user can log in immediately.
        let auth = a.login("reg_user", "reg-pass-1234!").await.unwrap();
        assert!(!auth.access_token.is_empty());
    }

    // ---- get_roles ----------------------------------------------------

    #[tokio::test]
    async fn get_roles_returns_assigned_roles() {
        let a = test_adapter();
        let user = a
            .create_user(named("role_user"), "pw123456!")
            .await
            .unwrap();
        a.assign_role(&user.id, "editor").await.unwrap();
        a.assign_role(&user.id, "viewer").await.unwrap();
        let names: std::collections::HashSet<String> = a
            .get_roles(&user.id)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(
            names,
            std::collections::HashSet::from(["editor".to_string(), "viewer".to_string()])
        );
    }

    #[tokio::test]
    async fn get_roles_unknown_user_returns_empty() {
        let a = test_adapter();
        assert!(a.get_roles("nonexistent-id").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_roles_with_catalogue_enriched_role() {
        // Roles created via create_roles + set_role_description carry their
        // description through get_roles (pyfly's _roles enrichment).
        let a = test_adapter();
        a.create_roles(&["superadmin"]).await;
        a.set_role_description("superadmin", "Full access").await;
        let user = a
            .create_user(named("super_user"), "pw123456!")
            .await
            .unwrap();
        a.assign_role(&user.id, "superadmin").await.unwrap();
        let roles = a.get_roles(&user.id).await.unwrap();
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, "superadmin");
        assert_eq!(roles[0].description, "Full access");
    }

    // ---- list_users / find_by_username / list_roles -------------------

    #[tokio::test]
    async fn find_by_username_and_list_users() {
        let a = test_adapter();
        a.create_user(named("u1"), "pw-aaaaaaa").await.unwrap();
        a.create_user(named("u2"), "pw-bbbbbbb").await.unwrap();
        assert_eq!(a.find_by_username("u1").await.unwrap().username, "u1");
        assert_eq!(
            a.find_by_username("ghost").await.unwrap_err(),
            Error::UserNotFound
        );
        assert_eq!(a.list_users(100).await.unwrap().len(), 2);
        // limit is honored.
        assert_eq!(a.list_users(1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_roles_and_list_roles() {
        let a = test_adapter();
        a.create_roles(&["alpha", "beta", "alpha"]).await; // idempotent
        let names: std::collections::HashSet<String> = a
            .list_roles()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(
            names,
            std::collections::HashSet::from(["alpha".to_string(), "beta".to_string()])
        );
    }

    // ---- introspect of a deleted user -> inactive ---------------------

    #[tokio::test]
    async fn introspect_after_user_deleted_is_inactive() {
        let a = test_adapter();
        let user = a.create_user(named("temp"), "pw-1234567").await.unwrap();
        let tok = a.login("temp", "pw-1234567").await.unwrap();
        assert!(a.introspect(&tok.access_token).await.unwrap().active);
        a.delete_user(&user.id).await.unwrap();
        assert!(!a.introspect(&tok.access_token).await.unwrap().active);
    }

    // ---- disabled user cannot log in ----------------------------------

    #[tokio::test]
    async fn disabled_user_cannot_login() {
        let a = test_adapter();
        let u = User {
            enabled: false,
            ..named("frozen")
        };
        a.create_user(u, "pw-1234567").await.unwrap();
        assert_eq!(
            a.login("frozen", "pw-1234567").await.unwrap_err(),
            Error::InvalidCredentials
        );
    }

    // ---- extended surface usable behind Arc<dyn Adapter> --------------

    #[tokio::test]
    async fn extended_surface_via_trait_object() {
        let a = Arc::new(test_adapter());
        a.create_user(named("obj_user"), "pw-1234567")
            .await
            .unwrap();
        let idp: Arc<dyn firefly_idp::Adapter> = a;
        let tok = idp.login("obj_user", "pw-1234567").await.unwrap();
        assert!(idp.introspect(&tok.access_token).await.unwrap().active);
        assert!(idp.assign_role("obj_user", "viewer").await.unwrap());
        let roles = idp.get_roles("obj_user").await.unwrap();
        assert_eq!(roles.len(), 1);
        assert!(idp.logout(&tok.access_token).await.unwrap());
    }
}
