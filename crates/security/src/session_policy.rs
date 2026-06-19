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

//! Session creation policy — the Rust analog of Spring Security's
//! `SessionCreationPolicy` (`sessionManagement().sessionCreationPolicy(...)`).
//!
//! The policy governs whether the security layer reads/writes the
//! [`Authentication`](crate::Authentication) context from the HTTP session. It
//! maps to a [`SecurityContextRepository`]:
//!
//! * [`Always`](SessionCreationPolicy::Always) /
//!   [`IfRequired`](SessionCreationPolicy::IfRequired) (default) /
//!   [`Never`](SessionCreationPolicy::Never) — the session-backed
//!   [`HttpSessionSecurityContextRepository`]: an established context survives
//!   across requests.
//! * [`Stateless`](SessionCreationPolicy::Stateless) — the
//!   [`NullSecurityContextRepository`]: nothing is read from or written to the
//!   session, so each request must re-authenticate (e.g. a bearer token). Pair
//!   with [`SessionAuthenticationLayer::anonymous_fallback(false)`](crate::SessionAuthenticationLayer::anonymous_fallback)
//!   so a following [`BearerLayer`](crate::BearerLayer) governs the request.
//!
//! Whether a *new* session is actually minted is the
//! [`firefly_session::SessionLayer`]'s concern; this policy controls only
//! whether the security tier persists its context there.

use std::sync::Arc;

use crate::security_context::{
    HttpSessionSecurityContextRepository, NullSecurityContextRepository, SecurityContextRepository,
};

/// How the security tier uses the HTTP session to store the authenticated
/// context — Spring's `SessionCreationPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCreationPolicy {
    /// Always use the session to hold the context (Spring `ALWAYS`).
    Always,
    /// Use the session only when needed — the default (Spring `IF_REQUIRED`).
    IfRequired,
    /// Use an existing session but never create one to hold the context
    /// (Spring `NEVER`).
    Never,
    /// Never read or write the session — every request re-authenticates
    /// (Spring `STATELESS`); for token-only APIs.
    Stateless,
}

impl Default for SessionCreationPolicy {
    /// Spring's default is `IF_REQUIRED`.
    fn default() -> Self {
        Self::IfRequired
    }
}

impl SessionCreationPolicy {
    /// Whether the security tier reads/writes its context in the session
    /// (`false` only for [`Stateless`](Self::Stateless)).
    #[must_use]
    pub fn uses_session(self) -> bool {
        !matches!(self, Self::Stateless)
    }

    /// Whether a *new* session may be created to hold the context (`true` for
    /// [`Always`](Self::Always) / [`IfRequired`](Self::IfRequired)).
    #[must_use]
    pub fn allows_session_creation(self) -> bool {
        matches!(self, Self::Always | Self::IfRequired)
    }

    /// Whether this is the [`Stateless`](Self::Stateless) policy.
    #[must_use]
    pub fn is_stateless(self) -> bool {
        matches!(self, Self::Stateless)
    }

    /// The [`SecurityContextRepository`] implied by this policy: the
    /// [`NullSecurityContextRepository`] for [`Stateless`](Self::Stateless)
    /// (nothing persisted), the session-backed
    /// [`HttpSessionSecurityContextRepository`] otherwise.
    #[must_use]
    pub fn security_context_repository(self) -> Arc<dyn SecurityContextRepository> {
        if self.is_stateless() {
            Arc::new(NullSecurityContextRepository)
        } else {
            Arc::new(HttpSessionSecurityContextRepository::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authentication::Authentication;
    use firefly_session::{Session, SessionInner};

    #[test]
    fn default_is_if_required() {
        assert_eq!(
            SessionCreationPolicy::default(),
            SessionCreationPolicy::IfRequired
        );
    }

    #[test]
    fn predicates_match_spring_semantics() {
        use SessionCreationPolicy::{Always, IfRequired, Never, Stateless};

        // uses_session: everything except STATELESS.
        assert!(Always.uses_session());
        assert!(IfRequired.uses_session());
        assert!(Never.uses_session());
        assert!(!Stateless.uses_session());

        // allows_session_creation: ALWAYS / IF_REQUIRED only.
        assert!(Always.allows_session_creation());
        assert!(IfRequired.allows_session_creation());
        assert!(!Never.allows_session_creation());
        assert!(!Stateless.allows_session_creation());

        assert!(Stateless.is_stateless());
        assert!(!Never.is_stateless());
    }

    #[tokio::test]
    async fn stateless_repository_never_persists_but_others_do() {
        let auth = Authentication {
            principal: "u1".into(),
            username: "u1".into(),
            roles: vec!["USER".into()],
            ..Default::default()
        };

        // STATELESS → Null repo: a stored context is not read back.
        let stateless = SessionCreationPolicy::Stateless.security_context_repository();
        let s1 = Session::new(SessionInner::new("sid"));
        stateless.save(&s1, &auth).await;
        assert!(stateless.load(&s1).await.is_none());

        // IF_REQUIRED → session repo: the context round-trips.
        let stateful = SessionCreationPolicy::IfRequired.security_context_repository();
        let s2 = Session::new(SessionInner::new("sid"));
        stateful.save(&s2, &auth).await;
        assert_eq!(stateful.load(&s2).await.expect("loaded").principal, "u1");
    }
}
