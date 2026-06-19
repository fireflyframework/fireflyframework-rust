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
use ldap3::{LdapConnAsync, Scope, SearchEntry};

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

/// Collapses a search result to its single entry, mirroring Spring's
/// `IncorrectResultSizeDataAccessException`: `Ok(None)` for no match,
/// `Ok(Some(entry))` for exactly one, and `Err` when the search is **ambiguous**
/// (more than one entry).
///
/// An ambiguous user search must never silently bind against — or read
/// authorities from — an arbitrary first match (RFC 4511 leaves result ordering
/// unspecified), so the providers fail closed instead.
fn single_entry(entries: Vec<LdapEntry>) -> Result<Option<LdapEntry>, SecurityError> {
    let mut it = entries.into_iter();
    let first = it.next();
    if it.next().is_some() {
        return Err(SecurityError::verification(
            "ambiguous directory search: more than one entry matched",
        ));
    }
    Ok(first)
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
    ///
    /// A directory error is **propagated**, not swallowed: Spring's
    /// `DefaultLdapAuthoritiesPopulator` surfaces the search exception rather
    /// than silently authenticating with zero roles, which would be a hard-to-
    /// diagnose privilege loss on a transient directory hiccup.
    async fn authorities_for(
        &self,
        user_dn: &str,
        username: &str,
    ) -> Result<Vec<String>, SecurityError> {
        let (Some(base), Some(filter)) = (&self.group_search_base, &self.group_search_filter)
        else {
            return Ok(Vec::new());
        };
        let filter = filter
            .replace("{0}", &escape_filter_value(user_dn))
            .replace("{1}", &escape_filter_value(username));
        let groups = self
            .ops
            .search(base, &filter, &[self.group_role_attribute.as_str()])
            .await?;
        Ok(groups
            .iter()
            .filter_map(|g| g.first(&self.group_role_attribute))
            .map(|name| format!("{}{}", self.role_prefix, name.to_uppercase()))
            .collect())
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

        // 1. Resolve the user DN. An unknown user fails with the same error
        //    *value* as a wrong password (no text-based enumeration). Note: as
        //    in Spring's `BindAuthenticator`, the unknown-user path skips the
        //    bind, so a residual *timing* channel remains — search-then-bind
        //    authentication cannot bind a DN it never found.
        let filter = self
            .user_search_filter
            .replace("{0}", &escape_filter_value(username));
        let user_dn = single_entry(
            self.ops
                .search(&self.user_search_base, &filter, &[])
                .await?,
        )?
        .map(|e| e.dn)
        .ok_or_else(|| SecurityError::verification("Bad credentials"))?;

        // 2. Bind as the user — the directory verifies the password.
        self.ops
            .bind(&user_dn, password)
            .await
            .map_err(|_| SecurityError::verification("Bad credentials"))?;

        // 3. Group authorities (a directory error here fails the login rather
        //    than silently dropping the user's roles).
        let roles = self.authorities_for(&user_dn, username).await?;

        Ok(Authentication {
            principal: user_dn,
            username: username.clone(),
            roles,
            ..Default::default()
        })
    }
}

/// The leading `CN` (relative-DN) value of a distinguished name, e.g.
/// `"CN=Admins,OU=Groups,DC=ex,DC=com"` → `"Admins"`. Used to turn an Active
/// Directory `memberOf` group DN into a role name.
#[must_use]
pub fn cn_from_dn(dn: &str) -> Option<&str> {
    let rdn = dn.split(',').next()?.trim();
    let (key, value) = rdn.split_once('=')?;
    key.trim().eq_ignore_ascii_case("cn").then(|| value.trim())
}

/// Active Directory authentication — the Rust analog of Spring's
/// `ActiveDirectoryLdapAuthenticationProvider`.
///
/// AD authenticates by binding as the user's `userPrincipalName`
/// (`username@domain`); the directory verifies the password. The provider then
/// reads the user's `memberOf` group DNs and maps each leading `CN` to a
/// `ROLE_<CN>` authority. As with [`LdapAuthenticationProvider`], an empty
/// password is rejected (anonymous-bind bypass) and a bad credential fails
/// uniformly.
pub struct ActiveDirectoryLdapAuthenticationProvider {
    ops: Arc<dyn LdapOperations>,
    domain: String,
    root_dn: String,
    role_prefix: String,
}

impl ActiveDirectoryLdapAuthenticationProvider {
    /// Builds the provider for AD `domain` (e.g. `"example.com"`), searching
    /// under `root_dn` (e.g. `"dc=example,dc=com"`) for the authenticated user's
    /// `memberOf` groups.
    #[must_use]
    pub fn new(
        ops: Arc<dyn LdapOperations>,
        domain: impl Into<String>,
        root_dn: impl Into<String>,
    ) -> Self {
        Self {
            ops,
            domain: domain.into(),
            root_dn: root_dn.into(),
            role_prefix: ROLE_PREFIX.to_string(),
        }
    }

    /// Overrides the role prefix prepended to each group name (default `ROLE_`).
    #[must_use]
    pub fn role_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.role_prefix = prefix.into();
        self
    }
}

#[async_trait]
impl AuthenticationProvider for ActiveDirectoryLdapAuthenticationProvider {
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
        if password.is_empty() {
            return Err(SecurityError::verification("Bad credentials"));
        }

        // The bind principal is the userPrincipalName (username@domain), unless
        // the caller already supplied a full UPN.
        let upn = if username.contains('@') {
            username.clone()
        } else {
            format!("{username}@{}", self.domain)
        };

        // Bind as the user — AD verifies the password.
        self.ops
            .bind(&upn, password)
            .await
            .map_err(|_| SecurityError::verification("Bad credentials"))?;

        // Read the user's DN + memberOf groups. A directory error propagates
        // (no silent role loss) and an ambiguous result is rejected rather than
        // reading authorities from an arbitrary first entry.
        let filter = format!(
            "(&(objectClass=user)(userPrincipalName={}))",
            escape_filter_value(&upn)
        );
        let entry = single_entry(
            self.ops
                .search(&self.root_dn, &filter, &["memberOf"])
                .await?,
        )?;

        let (principal, roles) = match entry {
            Some(entry) => {
                let roles = entry
                    .attrs
                    .get("memberOf")
                    .map(|dns| {
                        dns.iter()
                            .filter_map(|dn| cn_from_dn(dn))
                            .map(|cn| format!("{}{}", self.role_prefix, cn.to_uppercase()))
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    if entry.dn.is_empty() {
                        upn.clone()
                    } else {
                        entry.dn
                    },
                    roles,
                )
            }
            None => (upn.clone(), Vec::new()),
        };

        Ok(Authentication {
            principal,
            username: username.clone(),
            roles,
            ..Default::default()
        })
    }
}

/// The production [`LdapOperations`] adapter, backed by [`ldap3`].
///
/// Each operation opens a fresh async connection to the directory `url`.
/// Searches bind first with the configured manager DN/password (set via
/// [`with_manager`](Self::with_manager)) when present, else search anonymously;
/// [`bind`](LdapOperations::bind) opens its own connection to test the user's
/// credentials, so a failed user bind never disturbs the search binding.
pub struct Ldap3Operations {
    url: String,
    manager_dn: Option<String>,
    manager_password: Option<String>,
}

impl Ldap3Operations {
    /// Builds the adapter for the directory at `url` (e.g.
    /// `"ldaps://ad.example.com:636"`), searching anonymously until
    /// [`with_manager`](Self::with_manager) sets a search binding.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            manager_dn: None,
            manager_password: None,
        }
    }

    /// Sets the manager DN/password used to bind before searches.
    #[must_use]
    pub fn with_manager(mut self, dn: impl Into<String>, password: impl Into<String>) -> Self {
        self.manager_dn = Some(dn.into());
        self.manager_password = Some(password.into());
        self
    }
}

/// Maps an `ldap3` error to a [`SecurityError`].
fn ldap_err(e: impl std::fmt::Display) -> SecurityError {
    SecurityError::verification(format!("ldap: {e}"))
}

#[async_trait]
impl LdapOperations for Ldap3Operations {
    async fn search(
        &self,
        base: &str,
        filter: &str,
        attrs: &[&str],
    ) -> Result<Vec<LdapEntry>, SecurityError> {
        let (conn, mut ldap) = LdapConnAsync::new(&self.url).await.map_err(ldap_err)?;
        ldap3::drive!(conn);
        if let (Some(dn), Some(pw)) = (&self.manager_dn, &self.manager_password) {
            ldap.simple_bind(dn, pw)
                .await
                .map_err(ldap_err)?
                .success()
                .map_err(ldap_err)?;
        }
        let (rs, _res) = ldap
            .search(base, Scope::Subtree, filter, attrs.to_vec())
            .await
            .map_err(ldap_err)?
            .success()
            .map_err(ldap_err)?;
        // `SearchEntry::construct` panics on a malformed / non-schema-conformant
        // entry and `ldap3` 0.11 offers no fallible variant; a compromised or
        // MITM'd directory could send one. Catch the unwind so a bad entry
        // becomes a clean `Err` (fail closed) rather than aborting the in-flight
        // authentication task.
        let entries: Result<Vec<LdapEntry>, SecurityError> = rs
            .into_iter()
            .map(|e| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| SearchEntry::construct(e)))
                    .map(|se| LdapEntry {
                        dn: se.dn,
                        attrs: se.attrs,
                    })
                    .map_err(|_| SecurityError::verification("ldap: malformed directory entry"))
            })
            .collect();
        let _ = ldap.unbind().await;
        entries
    }

    async fn bind(&self, dn: &str, password: &str) -> Result<(), SecurityError> {
        let (conn, mut ldap) = LdapConnAsync::new(&self.url).await.map_err(ldap_err)?;
        ldap3::drive!(conn);
        // `success()` turns a non-zero LDAP result code (e.g. invalidCredentials)
        // into an error, so a failed bind is a clean `Err`.
        let result = ldap
            .simple_bind(dn, password)
            .await
            .map_err(ldap_err)?
            .success();
        let _ = ldap.unbind().await;
        result.map(|_| ()).map_err(ldap_err)
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
        /// `memberOf` group DNs attached to the user entry (Active Directory).
        member_of: Vec<String>,
        /// When set, the user search returns a SECOND entry (ambiguous result).
        duplicate_user: bool,
        /// When set, the group search fails with a directory error.
        fail_group_search: bool,
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
                let mut attrs = HashMap::new();
                if !self.member_of.is_empty() {
                    attrs.insert("memberOf".to_string(), self.member_of.clone());
                }
                let Some(dn) = self.user_dn.clone() else {
                    return Ok(Vec::new());
                };
                let mut entries = vec![LdapEntry {
                    dn,
                    attrs: attrs.clone(),
                }];
                if self.duplicate_user {
                    // A second, attacker-controlled entry matching the same filter.
                    entries.push(LdapEntry {
                        dn: "uid=evil,ou=people,dc=ex,dc=com".into(),
                        attrs,
                    });
                }
                Ok(entries)
            } else if base == self.group_search_base {
                if self.fail_group_search {
                    return Err(SecurityError::verification("group search failed"));
                }
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

        // Unknown user → no search hit, fails with the same error value.
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
    async fn ambiguous_user_search_is_rejected() {
        // Two entries match the user filter → fail closed (Spring's
        // IncorrectResultSizeDataAccessException), never bind an arbitrary first
        // match even when its password would succeed.
        let mock = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            user_dn: Some("uid=alice,ou=people,dc=ex,dc=com".into()),
            valid_bind: Some(("uid=alice,ou=people,dc=ex,dc=com".into(), "pw".into())),
            duplicate_user: true,
            ..MockLdap::default()
        });
        assert!(provider(mock)
            .authenticate(&up("alice", "pw"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn group_search_error_fails_the_login_not_silent_role_loss() {
        // A directory error during authorities population must propagate, not
        // authenticate the user with an empty (under-privileged) role set.
        let mock = Arc::new(MockLdap {
            user_search_base: "ou=people,dc=ex,dc=com".into(),
            group_search_base: "ou=groups,dc=ex,dc=com".into(),
            user_dn: Some("uid=alice,ou=people,dc=ex,dc=com".into()),
            valid_bind: Some(("uid=alice,ou=people,dc=ex,dc=com".into(), "pw".into())),
            fail_group_search: true,
            ..MockLdap::default()
        });
        assert!(provider(mock)
            .authenticate(&up("alice", "pw"))
            .await
            .is_err());
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

    #[test]
    fn cn_from_dn_extracts_leading_cn() {
        assert_eq!(
            cn_from_dn("CN=Admins,OU=Groups,DC=ex,DC=com"),
            Some("Admins")
        );
        assert_eq!(cn_from_dn("cn=Ops Team, ou=g"), Some("Ops Team"));
        // A non-CN leading RDN yields nothing.
        assert_eq!(cn_from_dn("OU=Groups,DC=ex"), None);
        assert_eq!(cn_from_dn("garbage"), None);
    }

    // --- Active Directory provider -----------------------------------------

    #[tokio::test]
    async fn active_directory_binds_upn_and_maps_member_of_to_roles() {
        let mock = Arc::new(MockLdap {
            // AD searches under the root DN for the user's memberOf.
            user_search_base: "dc=example,dc=com".into(),
            user_dn: Some("CN=Alice,OU=People,DC=example,DC=com".into()),
            // The bind principal is the userPrincipalName (alice@example.com).
            valid_bind: Some(("alice@example.com".into(), "pw".into())),
            member_of: vec![
                "CN=Admins,OU=Groups,DC=example,DC=com".into(),
                "CN=Users,OU=Groups,DC=example,DC=com".into(),
            ],
            ..MockLdap::default()
        });
        let provider = ActiveDirectoryLdapAuthenticationProvider::new(
            mock,
            "example.com",
            "dc=example,dc=com",
        );

        let auth = provider
            .authenticate(&up("alice", "pw"))
            .await
            .expect("authenticated");
        assert_eq!(auth.principal, "CN=Alice,OU=People,DC=example,DC=com");
        assert!(auth.has_role("ADMINS"));
        assert!(auth.has_role("USERS"));

        // Wrong password → the UPN bind fails → Bad credentials.
        let mock2 = Arc::new(MockLdap {
            valid_bind: Some(("alice@example.com".into(), "pw".into())),
            ..MockLdap::default()
        });
        let provider2 = ActiveDirectoryLdapAuthenticationProvider::new(
            mock2,
            "example.com",
            "dc=example,dc=com",
        );
        assert!(provider2.authenticate(&up("alice", "nope")).await.is_err());
    }

    #[tokio::test]
    async fn active_directory_rejects_empty_password() {
        let mock = Arc::new(MockLdap {
            valid_bind: Some(("alice@example.com".into(), String::new())),
            ..MockLdap::default()
        });
        let provider = ActiveDirectoryLdapAuthenticationProvider::new(
            mock,
            "example.com",
            "dc=example,dc=com",
        );
        assert!(provider.authenticate(&up("alice", "")).await.is_err());
    }

    #[tokio::test]
    async fn active_directory_rejects_ambiguous_member_of_search() {
        // The UPN bind succeeds, but the post-bind directory search is ambiguous
        // → refuse to read authorities from an arbitrary entry; fail the login.
        let mock = Arc::new(MockLdap {
            user_search_base: "dc=example,dc=com".into(),
            user_dn: Some("CN=Alice,OU=People,DC=example,DC=com".into()),
            valid_bind: Some(("alice@example.com".into(), "pw".into())),
            duplicate_user: true,
            ..MockLdap::default()
        });
        let provider = ActiveDirectoryLdapAuthenticationProvider::new(
            mock,
            "example.com",
            "dc=example,dc=com",
        );
        assert!(provider.authenticate(&up("alice", "pw")).await.is_err());
    }

    // Live `ldap3` adapter smoke test — skipped unless FIREFLY_TEST_LDAP_URL is
    // set (e.g. a test OpenLDAP/AD), mirroring the env-gated Postgres tests.
    #[tokio::test]
    async fn ldap3_adapter_binds_against_a_live_directory() {
        let Ok(url) = std::env::var("FIREFLY_TEST_LDAP_URL") else {
            eprintln!("skipping ldap3 adapter test: set FIREFLY_TEST_LDAP_URL to run");
            return;
        };
        let (Ok(dn), Ok(pw)) = (
            std::env::var("FIREFLY_TEST_LDAP_BIND_DN"),
            std::env::var("FIREFLY_TEST_LDAP_BIND_PW"),
        ) else {
            eprintln!("skipping: set FIREFLY_TEST_LDAP_BIND_DN / _PW to run");
            return;
        };
        let ops = Ldap3Operations::new(url);
        // A correct bind succeeds; a wrong password fails closed.
        ops.bind(&dn, &pw).await.expect("valid bind succeeds");
        assert!(ops.bind(&dn, "definitely-wrong").await.is_err());
    }
}
