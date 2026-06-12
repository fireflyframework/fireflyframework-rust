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

use crate::authentication::{Authentication, ANONYMOUS_ID};
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
            None => path.starts_with(&self.rule.prefix),
        }
    }
}

/// Compiles `pattern` as an fnmatch-style glob (a `*` crosses `/`
/// segments — pyfly's `fnmatch` semantics).
fn compile_glob(pattern: &str) -> GlobMatcher {
    GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .unwrap_or_else(|e| panic!("firefly/security: invalid glob pattern {pattern:?}: {e}"))
        .compile_matcher()
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
/// rejected with 403 — pyfly's [`HttpSecurity`] deny-by-default
/// (Spring Security 6) semantics. To allow unmatched paths, declare a
/// catch-all last via [`any_request_permit`](Self::any_request_permit)
/// (pyfly `any_request().permit_all()`). A chain with **no** rules at
/// all is a no-op and passes every request through, so it never becomes
/// a blanket lockout.
#[derive(Debug, Clone, Default)]
pub struct FilterChain {
    rules: Vec<Rule>,
    hierarchy: Option<Arc<RoleHierarchy>>,
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
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid glob.
    pub fn permit_pattern(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        compile_glob(&pattern); // eager validation
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
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid glob.
    pub fn require_pattern(mut self, pattern: impl Into<String>, roles: &[&str]) -> Self {
        let pattern = pattern.into();
        compile_glob(&pattern);
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
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid glob.
    pub fn require_authority(mut self, pattern: impl Into<String>, authorities: &[&str]) -> Self {
        let pattern = pattern.into();
        compile_glob(&pattern);
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
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid glob.
    pub fn authenticated(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        compile_glob(&pattern);
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
    /// # Panics
    ///
    /// Panics if `pattern` is not a valid glob.
    pub fn deny(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        compile_glob(&pattern);
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

    /// The declared rules, in evaluation order.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Converts the chain into a tower layer (Go: `Middleware()`).
    /// Auth must already have been populated by
    /// [`BearerLayer`](crate::BearerLayer) for non-`allow` rules.
    pub fn layer(self) -> FilterChainLayer {
        let compiled = self
            .rules
            .into_iter()
            .map(|rule| {
                let glob = rule.pattern.as_deref().map(compile_glob);
                CompiledRule { rule, glob }
            })
            .collect();
        FilterChainLayer {
            rules: Arc::new(compiled),
            hierarchy: self.hierarchy,
        }
    }
}

/// The tower layer produced by [`FilterChain::layer`].
#[derive(Clone)]
pub struct FilterChainLayer {
    rules: Arc<Vec<CompiledRule>>,
    hierarchy: Option<Arc<RoleHierarchy>>,
}

impl<S> Layer<S> for FilterChainLayer {
    type Service = FilterChainService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FilterChainService {
            inner,
            rules: Arc::clone(&self.rules),
            hierarchy: self.hierarchy.clone(),
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
}

/// The decision a matched rule produced.
enum Verdict {
    Pass,
    Unauthorized,
    Forbidden(&'static str),
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
        if !rule.roles.is_empty() && !rule.roles.iter().any(|r| effective_roles.contains(r)) {
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
        Box::pin(async move {
            match verdict {
                Verdict::Pass => inner.call(req).await,
                Verdict::Unauthorized => Ok(problem::unauthorized("authentication required")),
                Verdict::Forbidden(detail) => Ok(problem::forbidden(detail)),
            }
        })
    }
}
