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

//! Request cache — the Rust analog of Spring Security's `RequestCache` /
//! `SavedRequest` (`HttpSessionRequestCache`).
//!
//! When an unauthenticated user hits a protected resource, the entry point
//! redirects them to log in — but the resource they *wanted* must be
//! remembered so they can be sent back there afterwards. A [`RequestCache`]
//! [`save_request`](RequestCache::save_request)s the original request before the
//! redirect; the form-login success path
//! ([`form_login_routes`](crate::form_login_routes)) then prefers the
//! [`SavedRequest`]'s URL over its configured default target — Spring's
//! `SavedRequestAwareAuthenticationSuccessHandler`.
//!
//! * [`HttpSessionRequestCache`] (default) — stores the [`SavedRequest`] as a
//!   `firefly_session::Session` attribute (Spring's `HttpSessionRequestCache`).
//! * [`NullRequestCache`] — never stores, for stateless APIs that have no
//!   post-login redirect (Spring's `NullRequestCache`).

use async_trait::async_trait;
use axum::extract::Request;
use serde::{Deserialize, Serialize};

use firefly_session::Session;

/// Session attribute key under which the [`SavedRequest`] is stored — the
/// Firefly analog of Spring's `SPRING_SECURITY_SAVED_REQUEST`.
pub const SESSION_KEY_SAVED_REQUEST: &str = "firefly:savedRequest";

/// A snapshot of the request a user tried to reach before being sent to log in
/// — Spring's `SavedRequest`. Captures enough to redirect the user back
/// ([`redirect_url`](Self::redirect_url)) and to recognize a replay
/// ([`matches`](Self::matches)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRequest {
    /// The HTTP method (e.g. `"GET"`).
    pub method: String,
    /// The request target — path plus query string (e.g. `"/dashboard?tab=1"`).
    pub uri: String,
}

impl SavedRequest {
    /// Builds a saved request from an explicit method and target.
    #[must_use]
    pub fn new(method: impl Into<String>, uri: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            uri: uri.into(),
        }
    }

    /// Captures the method and path+query of `request`.
    #[must_use]
    pub fn from_request(request: &Request) -> Self {
        Self {
            method: request.method().as_str().to_owned(),
            uri: request_target(request),
        }
    }

    /// The URL to redirect the user back to after login — Spring's
    /// `SavedRequest.getRedirectUrl()`.
    #[must_use]
    pub fn redirect_url(&self) -> &str {
        &self.uri
    }

    /// Whether [`redirect_url`](Self::redirect_url) is a safe **same-origin**
    /// target: a rooted absolute path (`/…`) that is neither protocol-relative
    /// (`//host`) nor backslash-tricked (`/\host`, which some browsers treat as
    /// `//host`), and contains no control characters (which would enable header
    /// injection). A post-login redirect should honour the saved request only
    /// when this holds, so a crafted off-site or header-splitting path can never
    /// turn the login flow into an open redirect.
    #[must_use]
    pub fn is_safe_redirect(&self) -> bool {
        let p = self.uri.as_bytes();
        p.first() == Some(&b'/')
            && p.get(1) != Some(&b'/')
            && p.get(1) != Some(&b'\\')
            && !self.uri.bytes().any(|b| b.is_ascii_control())
    }

    /// Whether `incoming` is the same request that was saved (method + target),
    /// used by [`RequestCache::get_matching_request`] to recognize a replay.
    #[must_use]
    pub fn matches(&self, incoming: &SavedRequest) -> bool {
        self == incoming
    }
}

/// Saves and restores the pre-login [`SavedRequest`] — Spring's `RequestCache`.
///
/// The methods take an owned [`SavedRequest`] (build one from the live request
/// with [`SavedRequest::from_request`]) rather than a `&Request`: an
/// `axum::extract::Request` is not `Sync` (its streaming body), so it cannot be
/// held across the `.await` in an async-trait method.
#[async_trait]
pub trait RequestCache: Send + Sync {
    /// Stores `request` so it can be restored after the user authenticates.
    async fn save_request(&self, session: &Session, request: SavedRequest);

    /// Returns the stored request, if any (without removing it).
    async fn get_request(&self, session: &Session) -> Option<SavedRequest>;

    /// Discards any stored request.
    async fn remove_request(&self, session: &Session);

    /// If the stored request matches `incoming`, removes and returns it (a
    /// replay); otherwise leaves the cache untouched and returns `None`.
    async fn get_matching_request(
        &self,
        session: &Session,
        incoming: &SavedRequest,
    ) -> Option<SavedRequest> {
        let saved = self.get_request(session).await?;
        if saved.matches(incoming) {
            self.remove_request(session).await;
            Some(saved)
        } else {
            None
        }
    }
}

/// Session-backed request cache — Spring's `HttpSessionRequestCache`.
///
/// Stores the [`SavedRequest`] under a session attribute key (default
/// [`SESSION_KEY_SAVED_REQUEST`]).
#[derive(Debug, Clone)]
pub struct HttpSessionRequestCache {
    key: String,
}

impl HttpSessionRequestCache {
    /// Builds the cache keyed on the default [`SESSION_KEY_SAVED_REQUEST`]
    /// attribute.
    #[must_use]
    pub fn new() -> Self {
        Self {
            key: SESSION_KEY_SAVED_REQUEST.to_owned(),
        }
    }

    /// Builds the cache keyed on a custom session attribute.
    #[must_use]
    pub fn with_key(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl Default for HttpSessionRequestCache {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RequestCache for HttpSessionRequestCache {
    async fn save_request(&self, session: &Session, request: SavedRequest) {
        let _ = session.set_attribute(&self.key, request).await;
    }

    async fn get_request(&self, session: &Session) -> Option<SavedRequest> {
        session.attribute::<SavedRequest>(&self.key).await
    }

    async fn remove_request(&self, session: &Session) {
        session.remove_attribute(&self.key).await;
    }
}

/// A request cache that never stores — Spring's `NullRequestCache`, for
/// stateless APIs with no post-login redirect.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullRequestCache;

#[async_trait]
impl RequestCache for NullRequestCache {
    async fn save_request(&self, _session: &Session, _request: SavedRequest) {}
    async fn get_request(&self, _session: &Session) -> Option<SavedRequest> {
        None
    }
    async fn remove_request(&self, _session: &Session) {}
}

/// The request target — path plus query string when present, else just the path.
fn request_target(request: &Request) -> String {
    request.uri().path_and_query().map_or_else(
        || request.uri().path().to_owned(),
        |pq| pq.as_str().to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_session::SessionInner;

    fn get(uri: &str) -> SavedRequest {
        SavedRequest::from_request(
            &Request::builder()
                .method(http::Method::GET)
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn from_request_captures_method_and_path_with_query() {
        let saved = get("/dashboard?tab=1");
        assert_eq!(saved.method, "GET");
        assert_eq!(saved.uri, "/dashboard?tab=1");
        assert_eq!(saved.redirect_url(), "/dashboard?tab=1");
    }

    #[test]
    fn is_safe_redirect_rejects_off_site_targets() {
        // Same-origin rooted paths are safe.
        assert!(SavedRequest::new("GET", "/").is_safe_redirect());
        assert!(SavedRequest::new("GET", "/dashboard?tab=1").is_safe_redirect());
        // Protocol-relative, backslash-tricked, absolute, empty, and
        // control-char (header-injection) targets are not.
        assert!(!SavedRequest::new("GET", "//evil.com").is_safe_redirect());
        assert!(!SavedRequest::new("GET", "/\\evil.com").is_safe_redirect());
        assert!(!SavedRequest::new("GET", "https://evil.com").is_safe_redirect());
        assert!(!SavedRequest::new("GET", "").is_safe_redirect());
        assert!(!SavedRequest::new("GET", "/x\r\nSet-Cookie: evil=1").is_safe_redirect());
    }

    #[tokio::test]
    async fn saves_and_restores_through_the_session() {
        let cache = HttpSessionRequestCache::new();
        let session = Session::new(SessionInner::new("sid"));

        // Empty: nothing saved.
        assert!(cache.get_request(&session).await.is_none());

        cache.save_request(&session, get("/dashboard?tab=1")).await;
        let saved = cache.get_request(&session).await.expect("saved");
        assert_eq!(saved.redirect_url(), "/dashboard?tab=1");

        cache.remove_request(&session).await;
        assert!(cache.get_request(&session).await.is_none());
    }

    #[tokio::test]
    async fn saved_request_survives_session_id_rotation() {
        // The request is saved on the pre-login request; the login POST rotates
        // the session id (anti-fixation) and must still see the saved request.
        let cache = HttpSessionRequestCache::new();
        let session = Session::new(SessionInner::new("sid"));
        cache.save_request(&session, get("/account")).await;
        session.rotate_id().await;
        let saved = cache
            .get_request(&session)
            .await
            .expect("survives rotation");
        assert_eq!(saved.redirect_url(), "/account");
    }

    #[tokio::test]
    async fn get_matching_request_consumes_only_on_match() {
        let cache = HttpSessionRequestCache::new();
        let session = Session::new(SessionInner::new("sid"));
        cache.save_request(&session, get("/reports?y=2026")).await;

        // A non-matching target leaves the cache intact.
        assert!(cache
            .get_matching_request(&session, &get("/other"))
            .await
            .is_none());
        assert!(cache.get_request(&session).await.is_some());

        // The matching request is returned and consumed.
        let matched = cache
            .get_matching_request(&session, &get("/reports?y=2026"))
            .await
            .expect("match");
        assert_eq!(matched.redirect_url(), "/reports?y=2026");
        assert!(cache.get_request(&session).await.is_none());
    }

    #[tokio::test]
    async fn null_cache_never_stores() {
        let cache = NullRequestCache;
        let session = Session::new(SessionInner::new("sid"));
        cache.save_request(&session, get("/x")).await;
        assert!(cache.get_request(&session).await.is_none());
        assert!(cache
            .get_matching_request(&session, &get("/x"))
            .await
            .is_none());
    }
}
