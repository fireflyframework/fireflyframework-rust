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

//! LDAP / Active Directory authentication — the Rust analog of Spring
//! Security's `ldapAuthentication()` (`LdapAuthenticationProvider` /
//! `BindAuthenticator` / `DefaultLdapAuthoritiesPopulator`).
//!
//! [`LdapAuthenticationProvider`] is an
//! [`AuthenticationProvider`](crate::AuthenticationProvider) — it plugs into the
//! Tier 1 [`ProviderManager`](crate::ProviderManager) spine — that authenticates
//! username/password credentials by **bind authentication**:
//!
//! 1. Search the directory for the user's DN (under a base, by a filter like
//!    `(uid={0})`, with the username RFC 4515-escaped).
//! 2. **Bind** to the directory as that DN with the supplied password — the
//!    directory itself verifies the credential.
//! 3. Populate authorities from group membership (a group search like
//!    `(member={0})`, mapping each group's name to `ROLE_<NAME>`).
//!
//! The LDAP wire operations are abstracted behind the [`LdapOperations`] port,
//! so the provider logic is unit-tested without a live directory; the real
//! [`ldap3`]-backed adapter ([`Ldap3Operations`]) is the production
//! implementation.
//!
//! An **empty password is rejected** before binding: most directories treat a
//! simple bind with an empty password as an *anonymous* bind that succeeds,
//! which would be an authentication bypass.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::authentication::{Authentication, SecurityError, ROLE_PREFIX};
use crate::authentication_manager::{AuthenticationProvider, AuthenticationRequest};

/// A directory entry returned by [`LdapOperations::search`].
#[derive(Debug, Clone, Default)]
pub struct LdapEntry {
    /// The entry's distinguished name.
    pub dn: String,
    /// Requested attributes, each possibly multi-valued.
    pub attrs: HashMap<String, Vec<String>>,
}

impl LdapEntry {
    /// The first value of attribute `name`, if present.
    #[must_use]
    pub fn first(&self, name: &str) -> Option<&str> {
        self.attrs
            .get(name)
            .and_then(|v| v.first())
            .map(String::as_str)
    }
}

/// The LDAP operations the authentication providers need — the seam that lets
/// the provider logic be unit-tested without a live directory.
#[async_trait]
pub trait LdapOperations: Send + Sync {
    /// Searches under `base` with `filter`, returning each match's DN and the
    /// requested `attrs`.
    async fn search(
        &self,
        base: &str,
        filter: &str,
        attrs: &[&str],
    ) -> Result<Vec<LdapEntry>, SecurityError>;

    /// Attempts a simple bind as `dn` with `password`; `Ok(())` on success,
    /// `Err` on invalid credentials or a bind error.
    async fn bind(&self, dn: &str, password: &str) -> Result<(), SecurityError>;
}

/// Escapes a value for safe inclusion in an LDAP search filter (RFC 4515 §3),
/// preventing LDAP-filter injection through the username/DN.
#[must_use]
pub fn escape_filter_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\5c"),
            '*' => out.push_str("\\2a"),
            '(' => out.push_str("\\28"),
            ')' => out.push_str("\\29"),
            '\0' => out.push_str("\\00"),
            other => out.push(other),
        }
    }
    out
}

/// Bind-authentication provider over a directory — Spring's
/// `LdapAuthenticationProvider` with a `BindAuthenticator` +
/// `DefaultLdapAuthoritiesPopulator`.
pub struct LdapAuthenticationProvider {
    ops: Arc<dyn LdapOperations>,
    user_search_base: String,
    user_search_filter: String,
    group_search_base: Option<String>,
    group_search_filter: Option<String>,
    group_role_attribute: String,
    role_prefix: String,
}

impl LdapAuthenticationProvider {
    /// Builds a provider that finds users under `user_search_base` with
    /// `user_search_filter` (where `{0}` is replaced by the escaped username),
    /// e.g. base `"ou=people,dc=example,dc=com"`, filter `"(uid={0})"`. Group
    /// authorities are off until [`with_group_search`](Self::with_group_search).
    #[must_use]
    pub fn new(
        ops: Arc<dyn LdapOperations>,
        user_search_base: impl Into<String>,
        user_search_filter: impl Into<String>,
    ) -> Self {
        Self {
            ops,
            user_search_base: user_search_base.into(),
            user_search_filter: user_search_filter.into(),
            group_search_base: None,
            group_search_filter: None,
            group_role_attribute: "cn".to_string(),
            role_prefix: ROLE_PREFIX.to_string(),
        }
    }

    /// Enables group-membership authorities: searches `group_search_base` with
    /// `group_search_filter` (`{0}` = the escaped user DN, `{1}` = the escaped
    /// username), reading [`group_role_attribute`](Self::group_role_attribute)
    /// (default `"cn"`) from each group. E.g. base `"ou=groups,dc=example,dc=com"`,
    /// filter `"(member={0})"`.
    #[must_use]
    pub fn with_group_search(
        mut self,
        group_search_base: impl Into<String>,
        group_search_filter: impl Into<String>,
    ) -> Self {
        self.group_search_base = Some(group_search_base.into());
        self.group_search_filter = Some(group_search_filter.into());
        self
    }

    /// Overrides the group attribute mapped to a role (default `"cn"`).
    #[must_use]
    pub fn group_role_attribute(mut self, attribute: impl Into<String>) -> Self {
        self.group_role_attribute = attribute.into();
        self
    }

    /// Overrides the role prefix prepended to each group name (default `ROLE_`).
    #[must_use]
    pub fn role_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.role_prefix = prefix.into();
        self
    }

    /// Collects `ROLE_<GROUP>` authorities for `user_dn` / `username` from the
    /// configured group search (empty when group search is disabled).
    async fn authorities_for(&self, user_dn: &str, username: &str) -> Vec<String> {
        let (Some(base), Some(filter)) = (&self.group_search_base, &self.group_search_filter)
        else {
            return Vec::new();
        };
        let filter = filter
            .replace("{0}", &escape_filter_value(user_dn))
            .replace("{1}", &escape_filter_value(username));
        let groups = self
            .ops
            .search(base, &filter, &[self.group_role_attribute.as_str()])
            .await
            .unwrap_or_default();
        groups
            .iter()
            .filter_map(|g| g.first(&self.group_role_attribute))
            .map(|name| format!("{}{}", self.role_prefix, name.to_uppercase()))
            .collect()
    }
}

#[async_trait]
impl AuthenticationProvider for LdapAuthenticationProvider {
    fn supports(&self, request: &AuthenticationRequest) -> bool {
        matches!(request, AuthenticationRequest::UsernamePassword { .. })
    }

    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<Authentication, SecurityError> {
        let AuthenticationRequest::UsernamePassword { username, password } = request else {
            return Err(SecurityError::verification("unsupported credential kind"));
        };
        // Reject an empty password: a simple bind with one is an *anonymous*
        // bind that most directories accept — an authentication bypass.
        if password.is_empty() {
            return Err(SecurityError::verification("Bad credentials"));
        }

        // 1. Resolve the user DN (enumeration-safe: an unknown user fails the
        //    same way as a wrong password).
        let filter = self
            .user_search_filter
            .replace("{0}", &escape_filter_value(username));
        let user_dn = self
            .ops
            .search(&self.user_search_base, &filter, &[])
            .await?
            .into_iter()
            .next()
            .map(|e| e.dn)
            .ok_or_else(|| SecurityError::verification("Bad credentials"))?;

        // 2. Bind as the user — the directory verifies the password.
        self.ops
            .bind(&user_dn, password)
            .await
            .map_err(|_| SecurityError::verification("Bad credentials"))?;

        // 3. Group authorities.
        let roles = self.authorities_for(&user_dn, username).await;

        Ok(Authentication {
            principal: user_dn,
            username: username.clone(),
            roles,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A scriptable [`LdapOperations`] for the provider unit tests.
    #[derive(Default)]
    struct MockLdap {
        user_search_base: String,
        group_search_base: String,
        /// The DN the user search resolves to (`None` → no such user).
        user_dn: Option<String>,
        /// The single `(dn, password)` pair whose bind succeeds.
        valid_bind: Option<(String, String)>,
        /// Group `cn`s the group search returns.
        group_cns: Vec<String>,
        /// Every filter passed to `search`, for injection-escaping assertions.
        seen_filters: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LdapOperations for MockLdap {
        async fn search(
            &self,
            base: &str,
            filter: &str,
            _attrs: &[&str],
        ) -> Result<Vec<LdapEntry>, SecurityError> {
            self.seen_filters.lock().unwrap().push(filter.to_string());
            if base == self.user_search_base {
                Ok(self
                    .user_dn
                    .clone()
                    .map(|dn| LdapEntry {
                        dn,
                        attrs: HashMap::new(),
                    })
                    .into_iter()
                    .collect())
            } else if base == self.group_search_base {
                Ok(self
                    .group_cns
                    .iter()
                    .map(|cn| LdapEntry {
                        dn: format!("cn={cn},{}", self.group_search_base),
                        attrs: HashMap::from([("cn".to_string(), vec![cn.clone()])]),
                    })
                    .collect())
            } else {
                Ok(Vec::new())
            }
        }

        async fn bind(&self, dn: &str, password: &str) -> Result<(), SecurityError> {
            match &self.valid_bind {
                Some((vd, vp)) if vd == dn && vp == password => Ok(()),
                _ => Err(SecurityError::verification("invalid credentials")),
            }
        }
    }

    fn up(username: &str, password: &str) -> AuthenticationRequest {
        AuthenticationRequest::username_password(username, password)
    }

    fn provider(mock: Arc<MockLdap>) -> LdapAuthenticationProvider {
        LdapAuthenticationProvider::new(mock, "ou=people,dc=ex,dc=com", "(uid={0})")
            .with_group_search("ou=groups,dc=ex,dc=com", "(member={0})")
    }

    #[tokio::test]
    async fn binds_the_user_and_populates_group_roles() {
        let mock = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            group_search_base: "ou=groups,dc=ex,dc=com".into(),
            user_dn: Some("uid=alice,ou=people,dc=ex,dc=com".into()),
            valid_bind: Some(("uid=alice,ou=people,dc=ex,dc=com".into(), "pw".into())),
            group_cns: vec!["admins".into(), "users".into()],
            ..MockLdap::default()
        });
        let auth = provider(mock)
            .authenticate(&up("alice", "pw"))
            .await
            .expect("authenticated");
        assert_eq!(auth.principal, "uid=alice,ou=people,dc=ex,dc=com");
        assert_eq!(auth.username, "alice");
        // Group cns become ROLE_<UPPER>.
        assert!(auth.has_role("ADMINS"));
        assert!(auth.has_role("USERS"));
    }

    #[tokio::test]
    async fn wrong_password_and_unknown_user_both_fail_as_bad_credentials() {
        // Wrong password → bind fails.
        let known = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            user_dn: Some("uid=alice,ou=people,dc=ex,dc=com".into()),
            valid_bind: Some(("uid=alice,ou=people,dc=ex,dc=com".into(), "pw".into())),
            ..MockLdap::default()
        });
        assert!(provider(known)
            .authenticate(&up("alice", "wrong"))
            .await
            .is_err());

        // Unknown user → no search hit, fails the same way (enumeration-safe).
        let unknown = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            user_dn: None,
            ..MockLdap::default()
        });
        assert!(provider(unknown)
            .authenticate(&up("ghost", "pw"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn empty_password_is_rejected_before_binding() {
        // Even a "valid" empty-password bind must be refused (anonymous-bind bypass).
        let mock = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            user_dn: Some("uid=alice,ou=people,dc=ex,dc=com".into()),
            valid_bind: Some(("uid=alice,ou=people,dc=ex,dc=com".into(), String::new())),
            ..MockLdap::default()
        });
        assert!(provider(mock).authenticate(&up("alice", "")).await.is_err());
    }

    #[tokio::test]
    async fn username_is_escaped_in_the_search_filter() {
        let mock = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            user_dn: None,
            ..MockLdap::default()
        });
        let shared = mock.clone();
        // A wildcard-injection username must be neutralized in the filter.
        let _ = provider(mock).authenticate(&up("a*)(uid=*", "pw")).await;
        let filters = shared.seen_filters.lock().unwrap();
        let user_filter = &filters[0];
        assert!(
            user_filter.contains("a\\2a\\29\\28uid=\\2a"),
            "filter not escaped: {user_filter}"
        );
        assert!(!user_filter.contains("a*)(uid=*"));
    }

    #[test]
    fn escape_filter_value_covers_rfc4515_specials() {
        assert_eq!(escape_filter_value("a*b(c)d\\e"), "a\\2ab\\28c\\29d\\5ce");
        assert_eq!(escape_filter_value("plain"), "plain");
    }
}
