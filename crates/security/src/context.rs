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

//! The **security context** — a task-local [`Authentication`] slot, the Rust
//! analog of Spring's `SecurityContextHolder` and pyfly's `SecurityContext`
//! contextvar.
//!
//! [`BearerLayer`](crate::BearerLayer) scopes the resolved authentication
//! around the downstream call (see [`with_authentication_scope`]), so any code
//! reached from a guarded route — handlers, CQRS handlers, and the service
//! methods they call — can read the caller through [`current_authentication`]
//! without threading it through every signature. This is what makes the
//! [`#[pre_authorize]`](../firefly_macros/attr.pre_authorize.html) /
//! [`#[post_authorize]`](../firefly_macros/attr.post_authorize.html) method-
//! security macros work on a plain service method that never sees the request.
//!
//! The slot is task-local, so it nests and never leaks across tasks: spawning a
//! detached task drops the scope, exactly like Spring's
//! `MODE_INHERITABLETHREADLOCAL` being *off* by default.

use std::future::Future;

use crate::authentication::ANONYMOUS_ID;
use crate::{Authentication, SecurityError};

tokio::task_local! {
    /// Task-local storage slot for the ambient [`Authentication`] — the Rust
    /// analog of Spring's `SecurityContextHolder` thread-local.
    static CURRENT_AUTH: Authentication;
}

/// Runs `fut` with `auth` as the ambient security context — the Rust analog of
/// Spring's `SecurityContextHolder.setContext(...)` for the duration of a call.
/// Scopes nest: an inner scope shadows the outer authentication.
pub async fn with_authentication_scope<F: Future>(auth: Authentication, fut: F) -> F::Output {
    CURRENT_AUTH.scope(auth, fut).await
}

/// Runs the synchronous closure `f` with `auth` as the ambient security
/// context. Useful from blocking code and plain `#[test]` functions.
pub fn with_authentication_scope_sync<F: FnOnce() -> R, R>(auth: Authentication, f: F) -> R {
    CURRENT_AUTH.sync_scope(auth, f)
}

/// Returns the ambient [`Authentication`], or `None` when no scope is active —
/// the Rust analog of Go's `AuthenticationFrom(ctx)` reading the call context.
pub fn current_authentication() -> Option<Authentication> {
    CURRENT_AUTH.try_with(Clone::clone).ok()
}

/// An access rule the [`#[pre_authorize]`](../firefly_macros/attr.pre_authorize.html)
/// macro compiles to. Each variant mirrors a Spring Security SpEL built-in.
///
/// You rarely construct these by hand; the macro builds the rule from its
/// attribute and hands it to [`check_access`]. They are public so the generated
/// code — which lives in the *caller's* crate — can name them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessRule<'a> {
    /// `isAuthenticated()` — a real, non-anonymous principal must be present.
    Authenticated,
    /// `hasRole('X')` — the authentication must carry role `X`.
    Role(&'a str),
    /// `hasAnyRole('A','B')` — the authentication must carry at least one role.
    AnyRole(&'a [&'a str]),
    /// `hasAuthority('X')` — the authentication must carry authority `X`
    /// (role names count, see [`Authentication::has_authority`]).
    Authority(&'a str),
    /// `hasAnyAuthority('A','B')` — at least one authority must match.
    AnyAuthority(&'a [&'a str]),
}

/// Evaluates `rule` against the ambient security context, returning the
/// [`Authentication`] on success and the precise [`SecurityError`] on denial:
///
/// * [`SecurityError::Unauthenticated`] when no scope is active at all, and
/// * [`SecurityError::Forbidden`] when a principal is present but its
///   authorities don't satisfy `rule`.
///
/// This is the single decision point the `#[pre_authorize]` macro lowers to,
/// so the access semantics live here in the crate, not in generated tokens.
pub fn check_access(rule: &AccessRule<'_>) -> Result<Authentication, SecurityError> {
    let auth = current_authentication().ok_or(SecurityError::Unauthenticated)?;
    let granted = match rule {
        AccessRule::Authenticated => auth.principal != ANONYMOUS_ID && !auth.principal.is_empty(),
        AccessRule::Role(role) => auth.has_role(role),
        AccessRule::AnyRole(roles) => auth.has_any_role(roles),
        AccessRule::Authority(authority) => auth.has_authority(authority),
        AccessRule::AnyAuthority(authorities) => auth.has_any_authority(authorities),
    };
    if granted {
        Ok(auth)
    } else {
        Err(SecurityError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn auth(principal: &str, roles: &[&str], authorities: &[&str]) -> Authentication {
        Authentication {
            principal: principal.into(),
            username: principal.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            authorities: authorities.iter().map(|a| a.to_string()).collect(),
            claims: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn scope_makes_authentication_ambient() {
        assert!(current_authentication().is_none());
        let seen = with_authentication_scope(auth("u1", &["ADMIN"], &[]), async {
            current_authentication().map(|a| a.principal)
        })
        .await;
        assert_eq!(seen.as_deref(), Some("u1"));
        // The scope does not leak once the future completes.
        assert!(current_authentication().is_none());
    }

    #[tokio::test]
    async fn check_access_enforces_each_rule() {
        with_authentication_scope(auth("u1", &["ADMIN"], &["wallet:write"]), async {
            assert!(check_access(&AccessRule::Authenticated).is_ok());
            assert!(check_access(&AccessRule::Role("ADMIN")).is_ok());
            assert!(check_access(&AccessRule::Role("USER")).is_err());
            assert!(check_access(&AccessRule::AnyRole(&["USER", "ADMIN"])).is_ok());
            assert!(check_access(&AccessRule::Authority("wallet:write")).is_ok());
            assert!(check_access(&AccessRule::AnyAuthority(&["x", "wallet:write"])).is_ok());
            // Role names count as authorities (matches `has_authority`).
            assert!(check_access(&AccessRule::Authority("ADMIN")).is_ok());
            assert_eq!(
                check_access(&AccessRule::Role("USER")).unwrap_err(),
                SecurityError::Forbidden
            );
        })
        .await;
    }

    #[tokio::test]
    async fn no_scope_is_unauthenticated_and_anonymous_is_forbidden() {
        // No ambient context at all.
        assert_eq!(
            check_access(&AccessRule::Authenticated).unwrap_err(),
            SecurityError::Unauthenticated
        );
        // An anonymous principal is present but not authenticated.
        with_authentication_scope(Authentication::anonymous(), async {
            assert_eq!(
                check_access(&AccessRule::Authenticated).unwrap_err(),
                SecurityError::Forbidden
            );
        })
        .await;
    }
}
