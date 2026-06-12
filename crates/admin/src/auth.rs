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

//! Admin API auth guard — the Rust rendering of pyfly's `_auth_failure` /
//! `_guarded` (audit #66).
//!
//! When [`AdminConfig::require_auth`](crate::AdminConfig::require_auth) is set,
//! every `/admin/api/*` route is wrapped with an
//! [`axum::middleware::from_fn`] guard that reads the request-scoped
//! [`Authentication`](firefly_security::Authentication) (populated by
//! `firefly-security`'s bearer layer) and rejects the request when the caller
//! is unauthenticated (401) or lacks an allowed role (403). The SPA shell and
//! static assets stay public so the dashboard can boot and surface 401s from
//! the API.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use firefly_security::authentication_from;

/// Immutable auth policy captured from [`AdminConfig`](crate::AdminConfig),
/// shared by the guard middleware.
#[derive(Clone)]
pub(crate) struct AuthPolicy {
    pub require_auth: bool,
    pub allowed_roles: Arc<Vec<String>>,
}

/// Outcome of evaluating the policy against a request's authentication.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthDecision {
    /// Access permitted (auth disabled, or an allowed role present).
    Allow,
    /// No authenticated principal — render 401.
    Unauthenticated,
    /// Authenticated but lacking an allowed role — render 403.
    Forbidden,
}

impl AuthPolicy {
    /// Evaluates the policy: `Allow` when auth is disabled, otherwise checks
    /// the principal and roles (pyfly's `_auth_failure`).
    pub(crate) fn decide(&self, authenticated: bool, roles: &[String]) -> AuthDecision {
        if !self.require_auth {
            return AuthDecision::Allow;
        }
        if !authenticated {
            return AuthDecision::Unauthenticated;
        }
        if self.allowed_roles.is_empty() {
            return AuthDecision::Allow;
        }
        let allowed = self
            .allowed_roles
            .iter()
            .any(|want| roles.iter().any(|have| have == want));
        if allowed {
            AuthDecision::Allow
        } else {
            AuthDecision::Forbidden
        }
    }
}

/// The axum guard middleware: reads the request's [`Authentication`] and
/// applies the [`AuthPolicy`], returning 401/403 problem JSON on denial.
pub(crate) async fn guard(
    State(policy): State<AuthPolicy>,
    request: Request,
    next: Next,
) -> Response {
    let decision = {
        let auth = authentication_from(&request);
        match auth {
            Some(a) => policy.decide(true, &a.roles),
            None => policy.decide(false, &[]),
        }
    };
    match decision {
        AuthDecision::Allow => next.run(request).await,
        AuthDecision::Unauthenticated => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "Authentication required" })),
        )
            .into_response(),
        AuthDecision::Forbidden => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Forbidden" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(require_auth: bool, roles: &[&str]) -> AuthPolicy {
        AuthPolicy {
            require_auth,
            allowed_roles: Arc::new(roles.iter().map(|s| s.to_string()).collect()),
        }
    }

    // pyfly: test_auth_failure_noop_when_disabled
    #[test]
    fn allows_when_auth_disabled() {
        let p = policy(false, &["ADMIN"]);
        assert_eq!(p.decide(false, &[]), AuthDecision::Allow);
    }

    // pyfly: test_api_blocked_when_auth_required_and_anonymous
    #[test]
    fn unauthenticated_is_401() {
        let p = policy(true, &["ADMIN"]);
        assert_eq!(p.decide(false, &[]), AuthDecision::Unauthenticated);
    }

    // pyfly: test_auth_failure_logic — allowed role passes, other role 403.
    #[test]
    fn role_match_allows_mismatch_forbids() {
        let p = policy(true, &["ADMIN"]);
        assert_eq!(p.decide(true, &["ADMIN".into()]), AuthDecision::Allow);
        assert_eq!(p.decide(true, &["USER".into()]), AuthDecision::Forbidden);
    }

    #[test]
    fn empty_allowed_roles_permits_any_authenticated() {
        let p = policy(true, &[]);
        assert_eq!(p.decide(true, &["USER".into()]), AuthDecision::Allow);
    }
}
