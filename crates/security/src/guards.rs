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

//! Typed authorization guards — the Rust replacement for pyfly's
//! string-SpEL method security (`@pre_authorize("hasRole('ADMIN')")`).
//!
//! Instead of parsing an expression language at runtime, guards are
//! plain predicates over [`Authentication`], composed with
//! [`and`](AuthorizationGuard::and) / [`or`](AuthorizationGuard::or) /
//! [`not`](AuthorizationGuard::not):
//!
//! ```rust
//! use firefly_security::{guards, require, Authentication};
//!
//! // pyfly: @pre_authorize("hasRole('ADMIN') or hasAuthority('reports:read')")
//! let guard = guards::has_role("ADMIN").or(guards::has_authority("reports:read"));
//!
//! // Arbitrary typed predicates replace SpEL's `principal.user_id == ...`:
//! let self_only = require(|auth: &Authentication| auth.principal == "u1");
//!
//! let auth = Authentication { principal: "u1".into(), ..Default::default() };
//! assert!(guard.authorize(Some(&auth)).is_err()); // 403 Forbidden
//! assert!(self_only.authorize(Some(&auth)).is_ok());
//! ```
//!
//! Role checks can be made hierarchy-aware by pre-expanding the
//! authentication with
//! [`RoleHierarchy::expand_authentication`](crate::RoleHierarchy::expand_authentication).

use std::sync::Arc;

use crate::authentication::{Authentication, SecurityError, ANONYMOUS_ID};

/// A composable authorization predicate over [`Authentication`] — the
/// typed analog of one pyfly security expression.
#[derive(Clone)]
pub struct AuthorizationGuard {
    predicate: Arc<dyn Fn(&Authentication) -> bool + Send + Sync>,
}

impl std::fmt::Debug for AuthorizationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationGuard").finish_non_exhaustive()
    }
}

/// Builds a guard from a typed predicate — the replacement for pyfly's
/// `evaluate_security_expression(expr, ctx)` with a string expression.
pub fn require<F>(predicate: F) -> AuthorizationGuard
where
    F: Fn(&Authentication) -> bool + Send + Sync + 'static,
{
    AuthorizationGuard {
        predicate: Arc::new(predicate),
    }
}

impl AuthorizationGuard {
    /// Evaluates the predicate against `auth` without the
    /// authenticated-principal precheck.
    pub fn check(&self, auth: &Authentication) -> bool {
        (self.predicate)(auth)
    }

    /// Authorizes `auth`: no/anonymous/empty principal →
    /// [`SecurityError::Unauthenticated`]; predicate false →
    /// [`SecurityError::Forbidden`] — the same split pyfly's
    /// method-security decorators produce (401 vs 403).
    pub fn authorize(&self, auth: Option<&Authentication>) -> Result<(), SecurityError> {
        let Some(auth) = auth else {
            return Err(SecurityError::Unauthenticated);
        };
        if auth.principal.is_empty() || auth.principal == ANONYMOUS_ID {
            return Err(SecurityError::Unauthenticated);
        }
        if self.check(auth) {
            Ok(())
        } else {
            Err(SecurityError::Forbidden)
        }
    }

    /// Both guards must pass (SpEL `and`).
    pub fn and(self, other: AuthorizationGuard) -> AuthorizationGuard {
        require(move |auth| self.check(auth) && other.check(auth))
    }

    /// Either guard may pass (SpEL `or`).
    pub fn or(self, other: AuthorizationGuard) -> AuthorizationGuard {
        require(move |auth| self.check(auth) || other.check(auth))
    }

    /// Inverts the guard (SpEL `not`).
    ///
    /// Named `not` to mirror the SpEL operator it replaces; it is a
    /// consuming combinator, not [`std::ops::Not`].
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> AuthorizationGuard {
        require(move |auth| !self.check(auth))
    }
}

/// Passes for any authenticated principal (SpEL `isAuthenticated()`).
pub fn authenticated() -> AuthorizationGuard {
    require(|_| true)
}

/// Always passes the predicate (SpEL `permitAll()`); note that
/// [`AuthorizationGuard::authorize`] still rejects anonymous callers —
/// use no guard at all for genuinely public surfaces.
pub fn permit_all() -> AuthorizationGuard {
    require(|_| true)
}

/// Never passes (SpEL `denyAll()`).
pub fn deny_all() -> AuthorizationGuard {
    require(|_| false)
}

/// Requires `role` (SpEL `hasRole('X')`).
pub fn has_role(role: impl Into<String>) -> AuthorizationGuard {
    let role = role.into();
    require(move |auth| auth.has_role(&role))
}

/// Requires at least one of `roles` (SpEL `hasAnyRole(...)`).
pub fn has_any_role(roles: &[&str]) -> AuthorizationGuard {
    let roles: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
    require(move |auth| roles.iter().any(|r| auth.has_role(r)))
}

/// Requires `authority` among the principal's authorities or roles
/// (SpEL `hasAuthority('X')`).
pub fn has_authority(authority: impl Into<String>) -> AuthorizationGuard {
    let authority = authority.into();
    require(move |auth| auth.has_authority(&authority))
}

/// Requires at least one of `authorities` (SpEL `hasAnyAuthority(...)`).
pub fn has_any_authority(authorities: &[&str]) -> AuthorizationGuard {
    let authorities: Vec<String> = authorities.iter().map(|a| a.to_string()).collect();
    require(move |auth| authorities.iter().any(|a| auth.has_authority(a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(principal: &str, roles: &[&str], authorities: &[&str]) -> Authentication {
        Authentication {
            principal: principal.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            authorities: authorities.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        }
    }

    // Ported from pyfly method-security semantics: anonymous → 401,
    // missing role → 403, satisfied → pass.
    #[test]
    fn authorize_splits_unauthenticated_and_forbidden() {
        let guard = has_role("ADMIN");
        assert_eq!(guard.authorize(None), Err(SecurityError::Unauthenticated));
        assert_eq!(
            guard.authorize(Some(&Authentication::anonymous())),
            Err(SecurityError::Unauthenticated)
        );
        assert_eq!(
            guard.authorize(Some(&user("u1", &["USER"], &[]))),
            Err(SecurityError::Forbidden)
        );
        assert_eq!(guard.authorize(Some(&user("u1", &["ADMIN"], &[]))), Ok(()));
    }

    #[test]
    fn role_and_authority_helpers() {
        let alice = user("u1", &["USER"], &["read"]);
        assert!(has_role("USER").check(&alice));
        assert!(!has_role("ADMIN").check(&alice));
        assert!(has_any_role(&["ADMIN", "USER"]).check(&alice));
        assert!(has_authority("read").check(&alice));
        assert!(has_authority("USER").check(&alice)); // roles count
        assert!(!has_authority("write").check(&alice));
        assert!(has_any_authority(&["write", "read"]).check(&alice));
    }

    #[test]
    fn combinators_compose() {
        let alice = user("u1", &["USER"], &["read"]);
        assert!(has_role("USER").and(has_authority("read")).check(&alice));
        assert!(!has_role("USER").and(has_role("ADMIN")).check(&alice));
        assert!(has_role("ADMIN").or(has_role("USER")).check(&alice));
        assert!(has_role("ADMIN").not().check(&alice));
        assert!(deny_all().not().check(&alice));
        assert!(permit_all().check(&alice));
        assert!(authenticated().check(&alice));
    }

    #[test]
    fn typed_predicates_replace_spel() {
        // pyfly: @pre_authorize("principal.user_id == 'u1'")
        let self_only = require(|auth: &Authentication| auth.principal == "u1");
        assert!(self_only.check(&user("u1", &[], &[])));
        assert!(!self_only.check(&user("u2", &[], &[])));
    }

    #[test]
    fn hierarchy_expansion_makes_guards_hierarchy_aware() {
        let h = crate::RoleHierarchy::from_string("ADMIN > USER");
        let admin = user("u1", &["ADMIN"], &[]);
        assert!(!has_role("USER").check(&admin)); // back-compat: no hierarchy
        assert!(has_role("USER").check(&h.expand_authentication(&admin)));
        assert!(!has_role("SUPERUSER").check(&h.expand_authentication(&admin)));
    }
}
