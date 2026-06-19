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

//! UserDetails-based (DAO) authentication — the Rust analog of Spring
//! Security's `UserDetails` / `UserDetailsService` / `UserDetailsChecker` /
//! `DaoAuthenticationProvider`.
//!
//! A [`UserDetailsService`] loads a [`UserDetails`] (the stored credential +
//! the four account-status flags) by username; the [`DaoAuthenticationProvider`]
//! — an [`AuthenticationProvider`] for [`AuthenticationRequest::UsernamePassword`]
//! — runs the account-status checks, verifies the password with a
//! [`PasswordEncoder`], and yields an [`Authentication`]. It is enumeration-safe
//! (an unknown user and a wrong password both fail as `Bad credentials`, with
//! comparable encoder work, closing the timing oracle) and plugs straight into
//! the [`ProviderManager`](crate::ProviderManager) spine alongside the bearer
//! provider.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::authentication::{Authentication, SecurityError};
use crate::authentication_manager::{AuthenticationProvider, AuthenticationRequest};
use crate::password::{BcryptPasswordEncoder, PasswordEncoder};

/// A user record for DAO authentication — the Rust analog of Spring's
/// `UserDetails`: the stored (encoded) credential plus the four account-status
/// flags. The flags default to healthy (`true`) via [`UserDetails::new`].
#[derive(Debug, Clone)]
pub struct UserDetails {
    /// The username (becomes the [`Authentication`] principal).
    pub username: String,
    /// The stored, *encoded* password (e.g. a bcrypt or `{id}`-prefixed hash).
    pub password: String,
    /// Roles granted to the user.
    pub roles: Vec<String>,
    /// Fine-grained authorities (scopes / permissions).
    pub authorities: Vec<String>,
    /// Whether the account is enabled (Spring `isEnabled`).
    pub enabled: bool,
    /// Whether the account is **not** locked (Spring `isAccountNonLocked`).
    pub account_non_locked: bool,
    /// Whether the account is **not** expired (Spring `isAccountNonExpired`).
    pub account_non_expired: bool,
    /// Whether the credentials are **not** expired (Spring
    /// `isCredentialsNonExpired`).
    pub credentials_non_expired: bool,
}

impl UserDetails {
    /// Builds a healthy user (all status flags `true`) with the given roles.
    pub fn new(
        username: impl Into<String>,
        password: impl Into<String>,
        roles: Vec<String>,
    ) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            roles,
            authorities: Vec::new(),
            enabled: true,
            account_non_locked: true,
            account_non_expired: true,
            credentials_non_expired: true,
        }
    }

    /// Sets the fine-grained authorities (builder).
    #[must_use]
    pub fn with_authorities(mut self, authorities: Vec<String>) -> Self {
        self.authorities = authorities;
        self
    }

    /// Sets the `enabled` flag (builder).
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets the `account_non_locked` flag (builder).
    #[must_use]
    pub fn with_account_non_locked(mut self, value: bool) -> Self {
        self.account_non_locked = value;
        self
    }

    /// Sets the `account_non_expired` flag (builder).
    #[must_use]
    pub fn with_account_non_expired(mut self, value: bool) -> Self {
        self.account_non_expired = value;
        self
    }

    /// Sets the `credentials_non_expired` flag (builder).
    #[must_use]
    pub fn with_credentials_non_expired(mut self, value: bool) -> Self {
        self.credentials_non_expired = value;
        self
    }

    /// Builds the authenticated [`Authentication`] for this user (principal =
    /// username; never carries the password).
    #[must_use]
    pub fn to_authentication(&self) -> Authentication {
        Authentication {
            principal: self.username.clone(),
            username: self.username.clone(),
            roles: self.roles.clone(),
            authorities: self.authorities.clone(),
            claims: HashMap::new(),
        }
    }
}

/// Loads a [`UserDetails`] by username — Spring's `UserDetailsService`.
///
/// Returns `Ok(None)` when no such user exists (Spring throws
/// `UsernameNotFoundException`); `Err` is reserved for backend failures.
#[async_trait]
pub trait UserDetailsService: Send + Sync {
    /// Loads the user named `username`, or `Ok(None)` when absent.
    async fn load_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<UserDetails>, SecurityError>;
}

/// Validates a [`UserDetails`]'s account-status flags — Spring's
/// `UserDetailsChecker`.
pub trait UserDetailsChecker: Send + Sync {
    /// Returns an authentication error when the account is locked, disabled, or
    /// expired; `Ok(())` otherwise.
    fn check(&self, user: &UserDetails) -> Result<(), SecurityError>;
}

/// The default [`UserDetailsChecker`] — Spring's `AccountStatusUserDetailsChecker`
/// (locked → disabled → expired, in that order).
#[derive(Debug, Clone, Copy, Default)]
pub struct AccountStatusUserDetailsChecker;

impl UserDetailsChecker for AccountStatusUserDetailsChecker {
    fn check(&self, user: &UserDetails) -> Result<(), SecurityError> {
        if !user.account_non_locked {
            return Err(SecurityError::verification("Account locked"));
        }
        if !user.enabled {
            return Err(SecurityError::verification("Account disabled"));
        }
        if !user.account_non_expired {
            return Err(SecurityError::verification("Account expired"));
        }
        Ok(())
    }
}

/// An in-memory [`UserDetailsService`] — Spring's `InMemoryUserDetailsManager`,
/// for tests and small apps.
#[derive(Debug, Clone, Default)]
pub struct InMemoryUserDetailsService {
    users: HashMap<String, UserDetails>,
}

impl InMemoryUserDetailsService {
    /// Builds an empty service.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a user (builder); a later user with the same username replaces it.
    #[must_use]
    pub fn with_user(mut self, user: UserDetails) -> Self {
        self.users.insert(user.username.clone(), user);
        self
    }
}

#[async_trait]
impl UserDetailsService for InMemoryUserDetailsService {
    async fn load_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<UserDetails>, SecurityError> {
        Ok(self.users.get(username).cloned())
    }
}

/// Authenticates username/password credentials against a [`UserDetailsService`]
/// using a [`PasswordEncoder`] — Spring's `DaoAuthenticationProvider`.
///
/// Flow: load the user → account-status checks ([`UserDetailsChecker`]) → verify
/// the password → credentials-expiry check → [`Authentication`]. An unknown user
/// runs a dummy password verification and fails as `Bad credentials`, identical
/// to a wrong password, so neither the response nor its latency reveals whether
/// the account exists.
pub struct DaoAuthenticationProvider {
    user_details_service: Arc<dyn UserDetailsService>,
    password_encoder: Arc<dyn PasswordEncoder + Send + Sync>,
    checker: Arc<dyn UserDetailsChecker>,
    /// A pre-encoded throwaway password used to equalise the not-found timing
    /// (Spring's `userNotFoundEncodedPassword`).
    user_not_found_encoded: String,
}

impl DaoAuthenticationProvider {
    /// Builds the provider over `user_details_service` + `password_encoder`,
    /// with the default [`AccountStatusUserDetailsChecker`].
    #[must_use]
    pub fn new(
        user_details_service: Arc<dyn UserDetailsService>,
        password_encoder: Arc<dyn PasswordEncoder + Send + Sync>,
    ) -> Self {
        // Pre-encode a throwaway password for the not-found timing path. If the
        // configured encoder cannot hash (a misconfiguration), fall back to a
        // bcrypt hash so the dummy is never empty — an empty hash would verify
        // near-instantly and reopen the user-enumeration timing oracle.
        let user_not_found_encoded = password_encoder
            .hash("firefly-user-not-found-placeholder")
            .or_else(|_| BcryptPasswordEncoder::new().hash("firefly-user-not-found-placeholder"))
            .unwrap_or_default();
        Self {
            user_details_service,
            password_encoder,
            checker: Arc::new(AccountStatusUserDetailsChecker),
            user_not_found_encoded,
        }
    }

    /// Overrides the account-status checker (builder).
    #[must_use]
    pub fn with_checker(mut self, checker: Arc<dyn UserDetailsChecker>) -> Self {
        self.checker = checker;
        self
    }
}

impl std::fmt::Debug for DaoAuthenticationProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaoAuthenticationProvider")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AuthenticationProvider for DaoAuthenticationProvider {
    fn supports(&self, request: &AuthenticationRequest) -> bool {
        matches!(request, AuthenticationRequest::UsernamePassword { .. })
    }

    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<Authentication, SecurityError> {
        let AuthenticationRequest::UsernamePassword { username, password } = request else {
            return Err(SecurityError::verification(
                "DaoAuthenticationProvider: not a username/password request",
            ));
        };

        let user = match self
            .user_details_service
            .load_user_by_username(username)
            .await?
        {
            Some(user) => user,
            None => {
                // Unknown user: spend comparable verification time, then fail
                // identically to a wrong password (no enumeration oracle).
                let _ = self
                    .password_encoder
                    .verify(password, &self.user_not_found_encoded);
                return Err(SecurityError::verification("Bad credentials"));
            }
        };

        // Pre-authentication account-status checks.
        self.checker.check(&user)?;

        // Credential check (a malformed stored hash or a mismatch both fail).
        if !self
            .password_encoder
            .verify(password, &user.password)
            .unwrap_or(false)
        {
            return Err(SecurityError::verification("Bad credentials"));
        }

        // Post-authentication credentials-expiry check.
        if !user.credentials_non_expired {
            return Err(SecurityError::verification("Credentials expired"));
        }

        Ok(user.to_authentication())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authentication_manager::{AuthenticationManager, ProviderManager};
    use crate::password::BcryptPasswordEncoder;

    fn encoder() -> Arc<dyn PasswordEncoder + Send + Sync> {
        Arc::new(BcryptPasswordEncoder::with_rounds(4))
    }

    /// A UserDetailsService with `alice` (password `pw`, role `USER`) using a
    /// low-cost bcrypt hash, plus whatever status overrides the test applies.
    fn service_with(alice: UserDetails) -> Arc<dyn UserDetailsService> {
        Arc::new(InMemoryUserDetailsService::new().with_user(alice))
    }

    fn alice() -> UserDetails {
        let hash = BcryptPasswordEncoder::with_rounds(4).hash("pw").unwrap();
        UserDetails::new("alice", hash, vec!["USER".into()])
    }

    fn provider(uds: Arc<dyn UserDetailsService>) -> DaoAuthenticationProvider {
        DaoAuthenticationProvider::new(uds, encoder())
    }

    fn req() -> AuthenticationRequest {
        AuthenticationRequest::username_password("alice", "pw")
    }

    #[tokio::test]
    async fn authenticates_a_valid_user() {
        let p = provider(service_with(alice()));
        let auth = p.authenticate(&req()).await.unwrap();
        assert_eq!(auth.principal, "alice");
        assert!(auth.has_role("USER"));
    }

    #[tokio::test]
    async fn wrong_password_and_unknown_user_both_fail_as_bad_credentials() {
        let p = provider(service_with(alice()));
        let wrong = p
            .authenticate(&AuthenticationRequest::username_password("alice", "nope"))
            .await
            .unwrap_err();
        let unknown = p
            .authenticate(&AuthenticationRequest::username_password("ghost", "pw"))
            .await
            .unwrap_err();
        // Enumeration-safe: identical error for both.
        assert_eq!(wrong.to_string(), "Bad credentials");
        assert_eq!(unknown.to_string(), "Bad credentials");
    }

    #[tokio::test]
    async fn account_status_flags_are_enforced() {
        // Disabled.
        let p = provider(service_with(alice().with_enabled(false)));
        assert_eq!(
            p.authenticate(&req()).await.unwrap_err().to_string(),
            "Account disabled"
        );
        // Locked.
        let p = provider(service_with(alice().with_account_non_locked(false)));
        assert_eq!(
            p.authenticate(&req()).await.unwrap_err().to_string(),
            "Account locked"
        );
        // Account expired.
        let p = provider(service_with(alice().with_account_non_expired(false)));
        assert_eq!(
            p.authenticate(&req()).await.unwrap_err().to_string(),
            "Account expired"
        );
        // Credentials expired (checked after a correct password).
        let p = provider(service_with(alice().with_credentials_non_expired(false)));
        assert_eq!(
            p.authenticate(&req()).await.unwrap_err().to_string(),
            "Credentials expired"
        );
    }

    #[tokio::test]
    async fn plugs_into_the_provider_manager_spine() {
        let mgr = ProviderManager::new(vec![Arc::new(provider(service_with(alice())))]);
        let auth = mgr.authenticate(req()).await.unwrap();
        assert_eq!(auth.principal, "alice");
    }

    // A UserDetailsService backend failure (not a missing user) propagates as an
    // error, distinct from the enumeration-safe "Bad credentials".
    #[tokio::test]
    async fn backend_error_propagates() {
        struct Failing;
        #[async_trait]
        impl UserDetailsService for Failing {
            async fn load_user_by_username(
                &self,
                _username: &str,
            ) -> Result<Option<UserDetails>, SecurityError> {
                Err(SecurityError::verification("db down"))
            }
        }
        let p = DaoAuthenticationProvider::new(Arc::new(Failing), encoder());
        assert_eq!(
            p.authenticate(&req()).await.unwrap_err().to_string(),
            "db down"
        );
    }
}
