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

//! Remember-me authentication — the Rust analog of Spring Security's
//! `rememberMe()` (`TokenBasedRememberMeServices`).
//!
//! [`TokenBasedRememberMeServices`] mints a signed, expiring token from the
//! username, an expiry, and the user's stored password hash plus a server key —
//! so the token auto-expires, can't be forged without the key, and is
//! invalidated by a password change. [`auto_login`](RememberMeServices::auto_login)
//! validates the token (signature + expiry, against the
//! [`UserDetailsService`](crate::UserDetailsService)) and returns an
//! [`Authentication`] marked **remembered** (`is_remembered()` → `true`,
//! `is_fully_authenticated()` → `false`), so a sensitive route can demand a
//! fresh login.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::authentication::{Authentication, REMEMBERED_CLAIM};
use crate::csrf::constant_time_eq;
use crate::userdetails::UserDetailsService;

/// Default remember-me token lifetime — 14 days, matching Spring's
/// `AbstractRememberMeServices.TWO_WEEKS_S`.
pub const DEFAULT_REMEMBER_ME_SECONDS: u64 = 14 * 24 * 60 * 60;

/// Mints and validates remember-me tokens — Spring's `RememberMeServices`.
#[async_trait]
pub trait RememberMeServices: Send + Sync {
    /// Validates a remember-me token (cookie value) and returns the
    /// **remembered** [`Authentication`], or `None` when the token is invalid,
    /// expired, forged, or the user no longer exists.
    async fn auto_login(&self, token: &str) -> Option<Authentication>;
}

/// Hash-based remember-me — Spring's `TokenBasedRememberMeServices`.
///
/// Token = `base64url(username:expiry:signature)` where
/// `signature = base64url(SHA-256(username:expiry:password:key))`. The password
/// (stored hash) in the signature means changing the password invalidates every
/// outstanding token; the `key` means only this server can mint one.
pub struct TokenBasedRememberMeServices {
    key: String,
    token_validity_seconds: u64,
    user_details_service: Arc<dyn UserDetailsService>,
}

impl TokenBasedRememberMeServices {
    /// Builds the service with `key` (server secret) over `user_details_service`,
    /// using the default 14-day validity.
    #[must_use]
    pub fn new(key: impl Into<String>, user_details_service: Arc<dyn UserDetailsService>) -> Self {
        Self {
            key: key.into(),
            token_validity_seconds: DEFAULT_REMEMBER_ME_SECONDS,
            user_details_service,
        }
    }

    /// Overrides the token validity (seconds).
    #[must_use]
    pub fn token_validity_seconds(mut self, seconds: u64) -> Self {
        self.token_validity_seconds = seconds;
        self
    }

    /// Mints a remember-me token (cookie value) for `username`, signed with the
    /// user's stored `password` hash. Call on a successful login when the user
    /// opted in to "remember me".
    #[must_use]
    pub fn make_token(&self, username: &str, password: &str) -> String {
        let expiry = now_secs().saturating_add(self.token_validity_seconds);
        let signature = self.sign(username, expiry, password);
        URL_SAFE_NO_PAD.encode(format!("{username}:{expiry}:{signature}"))
    }

    /// The signature over `username:expiry:password:key`.
    fn sign(&self, username: &str, expiry: u64, password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{username}:{expiry}:{password}:{}", self.key).as_bytes());
        URL_SAFE_NO_PAD.encode(hasher.finalize())
    }
}

#[async_trait]
impl RememberMeServices for TokenBasedRememberMeServices {
    async fn auto_login(&self, token: &str) -> Option<Authentication> {
        let decoded = URL_SAFE_NO_PAD.decode(token).ok()?;
        let decoded = String::from_utf8(decoded).ok()?;
        // Parse from the right so a username may itself contain ':'.
        let mut parts = decoded.rsplitn(3, ':');
        let signature = parts.next()?;
        let expiry: u64 = parts.next()?.parse().ok()?;
        let username = parts.next()?;
        if now_secs() > expiry {
            return None;
        }
        let user = self
            .user_details_service
            .load_user_by_username(username)
            .await
            .ok()??;
        let expected = self.sign(username, expiry, &user.password);
        if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
            return None;
        }
        let mut auth = user.to_authentication();
        // Mark the context as remember-me (not fully authenticated).
        auth.claims
            .insert(REMEMBERED_CLAIM.to_string(), Value::Bool(true));
        Some(auth)
    }
}

/// The current wall-clock time in epoch seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::userdetails::{InMemoryUserDetailsService, UserDetails};

    const KEY: &str = "server-secret-key";

    fn service() -> TokenBasedRememberMeServices {
        let uds = Arc::new(
            InMemoryUserDetailsService::new()
                // The "password" here is the stored hash used in the signature.
                .with_user(UserDetails::new(
                    "alice",
                    "stored-hash-v1",
                    vec!["USER".into()],
                )),
        );
        TokenBasedRememberMeServices::new(KEY, uds)
    }

    #[tokio::test]
    async fn token_round_trips_and_marks_remembered() {
        let svc = service();
        let token = svc.make_token("alice", "stored-hash-v1");
        let auth = svc.auto_login(&token).await.expect("auto-login");
        assert_eq!(auth.principal, "alice");
        assert!(auth.has_role("USER"));
        // Remember-me is authenticated, but NOT fully authenticated.
        assert!(auth.is_authenticated());
        assert!(auth.is_remembered());
        assert!(!auth.is_fully_authenticated());
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let svc = service().token_validity_seconds(0);
        // Forge an already-past expiry directly (deterministic).
        let past = now_secs().saturating_sub(10);
        let sig = svc.sign("alice", past, "stored-hash-v1");
        let token = URL_SAFE_NO_PAD.encode(format!("alice:{past}:{sig}"));
        assert!(svc.auto_login(&token).await.is_none());
    }

    #[tokio::test]
    async fn tampered_and_wrong_key_tokens_are_rejected() {
        let svc = service();
        let token = svc.make_token("alice", "stored-hash-v1");

        // Flip a character in the encoded token.
        let mut raw = String::from_utf8(URL_SAFE_NO_PAD.decode(&token).unwrap()).unwrap();
        raw.push('x');
        let tampered = URL_SAFE_NO_PAD.encode(raw);
        assert!(svc.auto_login(&tampered).await.is_none());

        // A token minted with a different key does not verify here.
        let other = TokenBasedRememberMeServices::new(
            "different-key",
            Arc::new(
                InMemoryUserDetailsService::new().with_user(UserDetails::new(
                    "alice",
                    "stored-hash-v1",
                    vec![],
                )),
            ),
        );
        let foreign = other.make_token("alice", "stored-hash-v1");
        assert!(svc.auto_login(&foreign).await.is_none());
    }

    #[tokio::test]
    async fn password_change_invalidates_existing_tokens() {
        // Mint a token against the old stored hash...
        let token = service().make_token("alice", "stored-hash-v1");
        // ...but the user store now holds a new hash (password changed).
        let uds = Arc::new(
            InMemoryUserDetailsService::new().with_user(UserDetails::new(
                "alice",
                "stored-hash-v2",
                vec![],
            )),
        );
        let svc = TokenBasedRememberMeServices::new(KEY, uds);
        assert!(svc.auto_login(&token).await.is_none());
    }

    #[tokio::test]
    async fn unknown_user_token_is_rejected() {
        let svc = service();
        let sig = svc.sign("ghost", now_secs() + 100, "x");
        let token = URL_SAFE_NO_PAD.encode(format!("ghost:{}:{sig}", now_secs() + 100));
        assert!(svc.auto_login(&token).await.is_none());
    }
}
