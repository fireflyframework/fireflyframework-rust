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

//! Path-prefix and glob-pattern RBAC filter chain — the Rust analog of
//! the Go port's `FilterChain`, upgraded with pyfly's `HttpSecurity`
//! URL DSL capabilities: glob patterns (`/api/admin/**`), `deny`,
//! `authenticated`, `require_authority`, and an optional
//! [`RoleHierarchy`] consulted by role checks.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::Response;
use globset::{GlobBuilder, GlobMatcher};
use tower::{Layer, Service};

use crate::authentication::{Authentication, SecurityError, ANONYMOUS_ID, ROLE_PREFIX};
use crate::exception::{AccessDeniedHandler, AuthenticationEntryPoint};
use crate::problem;
use crate::role_hierarchy::RoleHierarchy;

/// `Rule` maps an HTTP path matcher to an access decision. The path is
/// matched either by `prefix` (Go parity) or — when `pattern` is set —
/// by an fnmatch-style glob where `*` crosses `/` boundaries (pyfly
/// parity). The empty roles + authorities vecs mean "authentication
/// required, any role".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    /// `None` matches any method (Go: `""`); comparison is
    /// case-insensitive, as with Go's `strings.EqualFold`.
    pub method: Option<String>,
    /// Path prefix the rule applies to (ignored when `pattern` is set).
    pub prefix: String,
    /// Glob pattern the rule applies to (pyfly `HttpSecurity`
    /// `request_matchers("/api/**")`); takes precedence over `prefix`.
    pub pattern: Option<String>,
    /// Roles of which at least one is required; empty means any
    /// authenticated principal (unless `authorities` is non-empty).
    pub roles: Vec<String>,
    /// Authorities of which at least one is required (pyfly
    /// `has_permission`); checked against
    /// [`Authentication::authorities`] and hierarchy-expanded roles.
    pub authorities: Vec<String>,
    /// When true, no auth required (skip guard).
    pub allow: bool,
    /// When true, every matching request is rejected with 403 (pyfly
    /// `deny_all`).
    pub deny: bool,
    /// When true, the rule matches every path regardless of `prefix`
    /// or `pattern` (pyfly `any_request()`). Used to build explicit
    /// catch-all tails.
    pub catch_all: bool,
}

impl Rule {
    /// Reports whether this rule's method constraint applies.
    fn method_matches(&self, method: &str) -> bool {
        match &self.method {
            Some(m) => m.eq_ignore_ascii_case(method),
            None => true,
        }
    }
}

/// A [`Rule`] plus its compiled glob matcher (when pattern-based).
#[derive(Clone)]
struct CompiledRule {
    rule: Rule,
    glob: Option<GlobMatcher>,
}

impl CompiledRule {
    fn matches(&self, method: &str, path: &str) -> bool {
        if !self.rule.method_matches(method) {
            return false;
        }
        if self.rule.catch_all {
            return true;
        }
        match &self.glob {
            Some(glob) => glob.is_match(path),
            None => prefix_matches(path, &self.rule.prefix),
        }
    }
}

/// Path-segment-aware prefix match — the Rust analog of Spring's
/// `AntPathRequestMatcher`. A non-empty `prefix` matches `path` only when it
/// ends at a path-segment boundary, so `/api` matches `/api` and `/api/...`
/// but **not** `/api-internal` or `/apixyz` (where a raw `starts_with` leaks).
/// An empty prefix matches every path (Go parity for the `""` prefix).
fn prefix_matches(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/') || prefix.ends_with('/'),
        None => false,
    }
}

/// Compiles `pattern` as an fnmatch-style glob (a `*` crosses `/`
/// segments — pyfly's `fnmatch` semantics), returning a recoverable
/// [`SecurityError`] on an invalid pattern instead of panicking.
fn compile_glob(pattern: &str) -> Result<GlobMatcher, SecurityError> {
    Ok(GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| SecurityError::verification(format!("invalid glob pattern {pattern:?}: {e}")))?
        .compile_matcher())
}

/// `FilterChain` is an ordered list of [`Rule`]s evaluated in
/// declaration order; the first matching rule decides. Use it to
/// express coarse RBAC like
///
/// ```rust
/// use firefly_security::{FilterChain, RoleHierarchy};
///
/// let chain = FilterChain::new()
///     .permit("/actuator/health")
///     .require_pattern("/api/admin/**", &["ADMIN"])
///     .authenticated("/api/**")
///     .require_authority("/files/**", &["files:read"])
///     .deny("/internal/**")
///     .with_role_hierarchy(RoleHierarchy::from_string("ADMIN > USER"));
/// ```
///
/// # Deny-by-default (fail-closed)
///
/// Once **any** rule is declared, requests that match **no** rule are
/// rejected with 403 — pyfly's `HttpSecurity` deny-by-default
/// (Spring Security 6) semantics. To allow unmatched paths, declare a
/// catch-all last via [`any_request_permit`](Self::any_request_permit)
/// (pyfly `any_request().permit_all()`). A chain with **no** rules at
/// all is a no-op and passes every request through, so it never becomes
/// a blanket lockout.
#[derive(Clone, Default)]
pub struct FilterChain {
    rules: Vec<Rule>,
    hierarchy: Option<Arc<RoleHierarchy>>,
    entry_point: Option<Arc<dyn AuthenticationEntryPoint>>,
    access_denied: Option<Arc<dyn AccessDeniedHandler>>,
}

impl std::fmt::Debug for FilterChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterChain")
            .field("rules", &self.rules.len())
            .field("hierarchy", &self.hierarchy.is_some())
            .field("custom_entry_point", &self.entry_point.is_some())
            .field("custom_access_denied", &self.access_denied.is_some())
            .finish()
    }
}

impl FilterChain {
    /// Returns an empty chain (Go: `NewFilterChain`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a public-path rule (no auth required).
    pub fn permit(mut self, prefix: impl Into<String>) -> Self {
        self.rules.push(Rule {
            prefix: prefix.into(),
            allow: true,
            ..Rule::default()
        });
        self
    }

    /// Appends a public-path rule restricted to a specific method.
    pub fn permit_method(mut self, method: impl Into<String>, prefix: impl Into<String>) -> Self {
        self.rules.push(Rule {
            method: Some(method.into()),
            prefix: prefix.into(),
            allow: true,
            ..Rule::default()
        });
        self
    }

    /// Appends a glob-pattern public rule (pyfly:
    /// `request_matchers(pattern).permit_all()`).
    ///
    /// # Errors / Panics
    ///
    /// An invalid glob is surfaced when the chain is converted to a layer:
    /// [`layer`](Self::layer) panics, while [`try_layer`](Self::try_layer)
    /// returns a recoverable [`SecurityError`].
    pub fn permit_pattern(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        self.rules.push(Rule {
            pattern: Some(pattern),
            allow: true,
            ..Rule::default()
        });
        self
    }

    /// Appends an auth-required rule with optional role gating; pass
    /// `&[]` for "any authenticated principal" (Go: `Require(prefix)`).
    pub fn require(mut self, prefix: impl Into<String>, roles: &[&str]) -> Self {
        self.rules.push(Rule {
            prefix: prefix.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            ..Rule::default()
        });
        self
    }

    /// Appends a glob-pattern role rule (pyfly:
    /// `request_matchers(pattern).has_any_role(...)`); pass `&[]` for
    /// "any authenticated principal".
    ///
    /// # Errors / Panics
    ///
    /// An invalid glob is surfaced when the chain is converted to a layer:
    /// [`layer`](Self::layer) panics, while [`try_layer`](Self::try_layer)
    /// returns a recoverable [`SecurityError`].
    pub fn require_pattern(mut self, pattern: impl Into<String>, roles: &[&str]) -> Self {
        let pattern = pattern.into();
        self.rules.push(Rule {
            pattern: Some(pattern),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            ..Rule::default()
        });
        self
    }

    /// Appends a glob-pattern authority rule (pyfly:
    /// `request_matchers(pattern).has_permission(...)`). At least one
    /// of `authorities` must be carried by the principal — either in
    /// [`Authentication::authorities`] or as a (hierarchy-expanded)
    /// role.
    ///
    /// # Errors / Panics
    ///
    /// An invalid glob is surfaced when the chain is converted to a layer:
    /// [`layer`](Self::layer) panics, while [`try_layer`](Self::try_layer)
    /// returns a recoverable [`SecurityError`].
    pub fn require_authority(mut self, pattern: impl Into<String>, authorities: &[&str]) -> Self {
        let pattern = pattern.into();
        self.rules.push(Rule {
            pattern: Some(pattern),
            authorities: authorities.iter().map(|a| a.to_string()).collect(),
            ..Rule::default()
        });
        self
    }

    /// Appends a glob-pattern "any authenticated principal" rule
    /// (pyfly: `request_matchers(pattern).authenticated()`).
    ///
    /// # Errors / Panics
    ///
    /// An invalid glob is surfaced when the chain is converted to a layer:
    /// [`layer`](Self::layer) panics, while [`try_layer`](Self::try_layer)
    /// returns a recoverable [`SecurityError`].
    pub fn authenticated(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        self.rules.push(Rule {
            pattern: Some(pattern),
            ..Rule::default()
        });
        self
    }

    /// Appends a glob-pattern deny-all rule (pyfly:
    /// `request_matchers(pattern).deny_all()`); every matching request
    /// is rejected with 403, authenticated or not.
    ///
    /// # Errors / Panics
    ///
    /// An invalid glob is surfaced when the chain is converted to a layer:
    /// [`layer`](Self::layer) panics, while [`try_layer`](Self::try_layer)
    /// returns a recoverable [`SecurityError`].
    pub fn deny(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        self.rules.push(Rule {
            pattern: Some(pattern),
            deny: true,
            ..Rule::default()
        });
        self
    }

    /// Appends a catch-all public rule that matches every path (pyfly:
    /// `any_request().permit_all()`). Declare it **last** to re-open
    /// the deny-by-default tail so unmatched requests are served.
    pub fn any_request_permit(mut self) -> Self {
        self.rules.push(Rule {
            allow: true,
            catch_all: true,
            ..Rule::default()
        });
        self
    }

    /// Appends a catch-all "any authenticated principal" rule matching
    /// every path (pyfly: `any_request().authenticated()`). Declare it
    /// last so any unmatched path requires authentication.
    pub fn any_request_authenticated(mut self) -> Self {
        self.rules.push(Rule {
            catch_all: true,
            ..Rule::default()
        });
        self
    }

    /// Appends a catch-all deny rule matching every path (pyfly:
    /// `any_request().deny_all()`). This is the explicit form of the
    /// implicit deny-by-default tail; rejects every unmatched request
    /// with 403, authenticated or not.
    pub fn any_request_deny(mut self) -> Self {
        self.rules.push(Rule {
            deny: true,
            catch_all: true,
            ..Rule::default()
        });
        self
    }

    /// Installs a [`RoleHierarchy`] consulted by role and authority
    /// checks, so `require(..., &["USER"])` is satisfied for an
    /// `ADMIN` under `ADMIN > USER`.
    pub fn with_role_hierarchy(mut self, hierarchy: RoleHierarchy) -> Self {
        self.hierarchy = Some(Arc::new(hierarchy));
        self
    }

    /// Sets the [`AuthenticationEntryPoint`] that renders the `401` for an
    /// unauthenticated request (default: the canonical problem+json envelope).
    pub fn with_authentication_entry_point(
        mut self,
        entry_point: Arc<dyn AuthenticationEntryPoint>,
    ) -> Self {
        self.entry_point = Some(entry_point);
        self
    }

    /// Sets the [`AccessDeniedHandler`] that renders the `403` for an
    /// authenticated-but-forbidden request (default: the canonical problem+json
    /// envelope).
    pub fn with_access_denied_handler(mut self, handler: Arc<dyn AccessDeniedHandler>) -> Self {
        self.access_denied = Some(handler);
        self
    }

    /// The declared rules, in evaluation order.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Converts the chain into a tower layer (Go: `Middleware()`).
    /// Auth must already have been populated by
    /// [`BearerLayer`](crate::BearerLayer) for non-`allow` rules.
    ///
    /// # Panics
    ///
    /// Panics if any pattern rule has an invalid glob. Use
    /// [`try_layer`](Self::try_layer) to surface that as a recoverable error.
    pub fn layer(self) -> FilterChainLayer {
        self.try_layer()
            .expect("firefly/security: invalid glob pattern in FilterChain")
    }

    /// Converts the chain into a tower layer, returning a recoverable
    /// [`SecurityError`] if any pattern rule has an invalid glob — the
    /// fail-at-startup-gracefully analog of Spring rejecting bad matcher
    /// config with an exception rather than aborting the process.
    pub fn try_layer(self) -> Result<FilterChainLayer, SecurityError> {
        let mut compiled = Vec::with_capacity(self.rules.len());
        for rule in self.rules {
            let glob = match rule.pattern.as_deref() {
                Some(p) => Some(compile_glob(p)?),
                None => None,
            };
            compiled.push(CompiledRule { rule, glob });
        }
        Ok(FilterChainLayer {
            rules: Arc::new(compiled),
            hierarchy: self.hierarchy,
            entry_point: self.entry_point,
            access_denied: self.access_denied,
        })
    }
}

/// The tower layer produced by [`FilterChain::layer`].
#[derive(Clone)]
pub struct FilterChainLayer {
    rules: Arc<Vec<CompiledRule>>,
    hierarchy: Option<Arc<RoleHierarchy>>,
    entry_point: Option<Arc<dyn AuthenticationEntryPoint>>,
    access_denied: Option<Arc<dyn AccessDeniedHandler>>,
}

impl<S> Layer<S> for FilterChainLayer {
    type Service = FilterChainService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FilterChainService {
            inner,
            rules: Arc::clone(&self.rules),
            hierarchy: self.hierarchy.clone(),
            entry_point: self.entry_point.clone(),
            access_denied: self.access_denied.clone(),
        }
    }
}

/// The tower service produced by [`FilterChainLayer`]. Evaluates the
/// rules in declaration order; the first match decides. When at least
/// one rule is configured but none match, the request is rejected with
/// 403 — deny-by-default (fail-closed), matching pyfly's `HttpSecurity`
/// / Spring Security 6 semantics. A chain with no rules at all is a
/// no-op and passes every request through.
#[derive(Clone)]
pub struct FilterChainService<S> {
    inner: S,
    rules: Arc<Vec<CompiledRule>>,
    hierarchy: Option<Arc<RoleHierarchy>>,
    entry_point: Option<Arc<dyn AuthenticationEntryPoint>>,
    access_denied: Option<Arc<dyn AccessDeniedHandler>>,
}

/// The decision a matched rule produced.
enum Verdict {
    Pass,
    Unauthorized,
    Forbidden(&'static str),
}

/// Whether a rule's required role `rule_role` is satisfied — `ROLE_`-prefix
/// aware and consistent with [`Authentication::has_role`]: a bare or
/// `ROLE_`-prefixed role in the (hierarchy-expanded) role set matches, as does a
/// `ROLE_`-prefixed *authority* (how Spring stores roles). So a principal
/// carrying `ROLE_ADMIN` satisfies a `require(..., &["ADMIN"])` rule on the URL
/// chain just as it does `#[pre_authorize(role = "ADMIN")]`.
fn role_matches(
    rule_role: &str,
    effective_roles: &BTreeSet<String>,
    authorities: &[String],
) -> bool {
    let prefixed = format!("{ROLE_PREFIX}{rule_role}");
    effective_roles.contains(rule_role)
        || effective_roles.contains(&prefixed)
        || authorities.iter().any(|a| a == &prefixed)
}

fn decide(rules: &[CompiledRule], hierarchy: Option<&RoleHierarchy>, req: &Request) -> Verdict {
    let method = req.method().as_str();
    let path = req.uri().path();
    for compiled in rules {
        if !compiled.matches(method, path) {
            continue;
        }
        let rule = &compiled.rule;
        if rule.allow {
            return Verdict::Pass;
        }
        if rule.deny {
            return Verdict::Forbidden("access denied");
        }
        let auth = req.extensions().get::<Authentication>();
        let Some(auth) = auth else {
            return Verdict::Unauthorized;
        };
        if auth.principal.is_empty() || auth.principal == ANONYMOUS_ID {
            return Verdict::Unauthorized;
        }
        // Roles, expanded through the hierarchy when one is installed.
        let effective_roles: BTreeSet<String> = match hierarchy {
            Some(h) => h.expand(auth.roles.iter().cloned()),
            None => auth.roles.iter().cloned().collect(),
        };
        if !rule.roles.is_empty()
            && !rule
                .roles
                .iter()
                .any(|r| role_matches(r, &effective_roles, &auth.authorities))
        {
            return Verdict::Forbidden("required role missing");
        }
        if !rule.authorities.is_empty()
            && !rule.authorities.iter().any(|a| {
                effective_roles.contains(a) || auth.authorities.iter().any(|have| have == a)
            })
        {
            return Verdict::Forbidden("required authority missing");
        }
        return Verdict::Pass;
    }
    // No rule matched. Deny by default when rules are configured
    // (fail-closed, pyfly `HttpSecurity` / Spring Security 6 parity);
    // an empty chain (no rules) is a no-op and passes through. Declare
    // a catch-all (`any_request_permit`) to re-open the unmatched tail.
    if rules.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Forbidden("Access to this resource is denied (no matching security rule).")
    }
}

impl<S> Service<Request> for FilterChainService<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let verdict = decide(&self.rules, self.hierarchy.as_deref(), &req);
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let entry_point = self.entry_point.clone();
        let access_denied = self.access_denied.clone();
        Box::pin(async move {
            match verdict {
                Verdict::Pass => inner.call(req).await,
                // Render rejections through the configured handlers, falling
                // back to the canonical RFC 7807 problem+json envelopes.
                Verdict::Unauthorized => Ok(match &entry_point {
                    Some(ep) => ep.commence(&req, "authentication required"),
                    None => problem::unauthorized("authentication required"),
                }),
                Verdict::Forbidden(detail) => Ok(match &access_denied {
                    Some(handler) => handler.handle(&req, detail),
                    None => problem::forbidden(detail),
                }),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn status_for(chain: FilterChain, method: &str, path: &str) -> http::StatusCode {
        let inner = tower::service_fn(|_req: Request| async {
            Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
        });
        let svc = chain.layer().layer(inner);
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap();
        svc.oneshot(req).await.unwrap().status()
    }

    // H3: a prefix rule must be path-segment aware. `permit("/api")` must match
    // `/api` and `/api/...` but NOT `/api-internal` / `/apixyz` (Spring's
    // AntPathRequestMatcher), where the old raw `starts_with` leaked.
    #[tokio::test]
    async fn prefix_rule_is_path_segment_aware() {
        let chain = || FilterChain::new().permit("/api").any_request_deny();
        assert_eq!(
            status_for(chain(), "GET", "/api").await,
            http::StatusCode::OK
        );
        assert_eq!(
            status_for(chain(), "GET", "/api/accounts").await,
            http::StatusCode::OK
        );
        assert_eq!(
            status_for(chain(), "GET", "/api-internal").await,
            http::StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for(chain(), "GET", "/apixyz").await,
            http::StatusCode::FORBIDDEN
        );
    }

    // Review fix (H2 consistency): the URL-authorization role check must be
    // ROLE_-prefix aware like `has_role`, so a `ROLE_ADMIN` principal satisfies
    // a `require(..., ["ADMIN"])` rule (not only `#[pre_authorize]`).
    #[tokio::test]
    async fn role_rules_are_role_prefix_aware() {
        use crate::Authentication;
        async fn status(chain: FilterChain, auth: Authentication, path: &str) -> http::StatusCode {
            let inner = tower::service_fn(|_r: Request| async {
                Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
            });
            let svc = chain.layer().layer(inner);
            let mut req = Request::builder()
                .method("GET")
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap();
            req.extensions_mut().insert(auth);
            svc.oneshot(req).await.unwrap().status()
        }
        let role = |p: &str, r: &str| Authentication {
            principal: p.into(),
            roles: vec![r.into()],
            ..Default::default()
        };
        let chain = || {
            FilterChain::new()
                .require_pattern("/admin/**", &["ADMIN"])
                .any_request_permit()
        };
        // ROLE_-prefixed principal satisfies a bare "ADMIN" rule (Spring parity).
        assert_eq!(
            status(chain(), role("u1", "ROLE_ADMIN"), "/admin/x").await,
            http::StatusCode::OK
        );
        // A bare role still works.
        assert_eq!(
            status(chain(), role("u2", "ADMIN"), "/admin/x").await,
            http::StatusCode::OK
        );
        // A non-admin is still forbidden.
        assert_eq!(
            status(chain(), role("u3", "USER"), "/admin/x").await,
            http::StatusCode::FORBIDDEN
        );
    }

    // T1.5: a custom AccessDeniedHandler / AuthenticationEntryPoint renders the
    // rejection instead of the default problem+json (Spring's
    // ExceptionTranslationFilter seam).
    #[tokio::test]
    async fn custom_exception_handlers_render_rejections() {
        use crate::exception::{AccessDeniedHandler, AuthenticationEntryPoint};

        struct Teapot;
        impl AccessDeniedHandler for Teapot {
            fn handle(&self, _req: &Request, _detail: &str) -> Response {
                Response::builder()
                    .status(http::StatusCode::IM_A_TEAPOT)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        }
        impl AuthenticationEntryPoint for Teapot {
            fn commence(&self, _req: &Request, _detail: &str) -> Response {
                Response::builder()
                    .status(http::StatusCode::PAYMENT_REQUIRED)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        }

        async fn status(chain: FilterChain, auth: Option<Authentication>) -> http::StatusCode {
            let inner = tower::service_fn(|_r: Request| async {
                Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
            });
            let svc = chain.layer().layer(inner);
            let mut req = Request::builder()
                .method("GET")
                .uri("/admin/x")
                .body(axum::body::Body::empty())
                .unwrap();
            if let Some(a) = auth {
                req.extensions_mut().insert(a);
            }
            svc.oneshot(req).await.unwrap().status()
        }

        let chain = || {
            FilterChain::new()
                .require_pattern("/admin/**", &["ADMIN"])
                .with_access_denied_handler(Arc::new(Teapot))
                .with_authentication_entry_point(Arc::new(Teapot))
        };
        // Authenticated but missing role -> custom AccessDeniedHandler (418).
        let user = Authentication {
            principal: "u1".into(),
            roles: vec!["USER".into()],
            ..Default::default()
        };
        assert_eq!(
            status(chain(), Some(user)).await,
            http::StatusCode::IM_A_TEAPOT
        );
        // Unauthenticated -> custom AuthenticationEntryPoint (402).
        assert_eq!(
            status(chain(), None).await,
            http::StatusCode::PAYMENT_REQUIRED
        );
    }

    // H10: an invalid glob must surface as a recoverable error via try_layer,
    // not abort the process. A valid pattern still yields Ok (discriminating).
    #[test]
    fn try_layer_surfaces_invalid_glob_as_error() {
        let ok = FilterChain::new()
            .require_pattern("/api/**", &["ADMIN"])
            .try_layer();
        assert!(ok.is_ok());

        let bad = FilterChain::new()
            .require_pattern("/admin/[", &["ADMIN"])
            .try_layer();
        assert!(bad.is_err());
    }
}
