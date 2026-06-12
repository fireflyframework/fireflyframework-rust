//! Path-prefix RBAC filter chain — the Rust analog of the Go port's
//! `FilterChain`.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::Response;
use tower::{Layer, Service};

use crate::authentication::{Authentication, ANONYMOUS_ID};
use crate::problem;

/// `Rule` maps an HTTP path prefix to a set of required roles. The
/// empty roles vec means "authentication required, any role".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rule {
    /// `None` matches any method (Go: `""`); comparison is
    /// case-insensitive, as with Go's `strings.EqualFold`.
    pub method: Option<String>,
    /// Path prefix the rule applies to.
    pub prefix: String,
    /// Roles of which at least one is required; empty means any
    /// authenticated principal.
    pub roles: Vec<String>,
    /// When true, no auth required (skip guard).
    pub allow: bool,
}

impl Rule {
    /// Reports whether this rule applies to `method` + `path`.
    fn matches(&self, method: &str, path: &str) -> bool {
        if let Some(m) = &self.method {
            if !m.eq_ignore_ascii_case(method) {
                return false;
            }
        }
        path.starts_with(&self.prefix)
    }
}

/// `FilterChain` is an ordered list of [`Rule`]s evaluated in
/// declaration order; the first matching rule decides. Use it to
/// express coarse RBAC like
///
/// ```rust
/// use firefly_security::FilterChain;
///
/// let chain = FilterChain::new()
///     .permit("/actuator/health")
///     .permit("/actuator/info")
///     .require("/admin/", &["ADMIN"])
///     .require("/api/", &["USER"]);
/// ```
#[derive(Debug, Clone, Default)]
pub struct FilterChain {
    rules: Vec<Rule>,
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

    /// The declared rules, in evaluation order.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Converts the chain into a tower layer (Go: `Middleware()`).
    /// Auth must already have been populated by
    /// [`BearerLayer`](crate::BearerLayer) for non-`allow` rules.
    pub fn layer(self) -> FilterChainLayer {
        FilterChainLayer {
            rules: Arc::new(self.rules),
        }
    }
}

/// The tower layer produced by [`FilterChain::layer`].
#[derive(Clone)]
pub struct FilterChainLayer {
    rules: Arc<Vec<Rule>>,
}

impl<S> Layer<S> for FilterChainLayer {
    type Service = FilterChainService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FilterChainService {
            inner,
            rules: Arc::clone(&self.rules),
        }
    }
}

/// The tower service produced by [`FilterChainLayer`]. Evaluates the
/// rules in declaration order; the first match decides. When no rule
/// matches, the request is allowed through — default-allow keeps the
/// chain composable with any upstream auth middleware.
#[derive(Clone)]
pub struct FilterChainService<S> {
    inner: S,
    rules: Arc<Vec<Rule>>,
}

/// The decision a matched rule produced.
enum Verdict {
    Pass,
    Unauthorized,
    Forbidden,
}

fn decide(rules: &[Rule], req: &Request) -> Verdict {
    let method = req.method().as_str();
    let path = req.uri().path();
    for rule in rules {
        if !rule.matches(method, path) {
            continue;
        }
        if rule.allow {
            return Verdict::Pass;
        }
        let auth = req.extensions().get::<Authentication>();
        let Some(auth) = auth else {
            return Verdict::Unauthorized;
        };
        if auth.principal.is_empty() || auth.principal == ANONYMOUS_ID {
            return Verdict::Unauthorized;
        }
        if !rule.roles.is_empty() {
            let wanted: Vec<&str> = rule.roles.iter().map(String::as_str).collect();
            if !auth.has_any_role(&wanted) {
                return Verdict::Forbidden;
            }
        }
        return Verdict::Pass;
    }
    // No rule matched — default-allow.
    Verdict::Pass
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
        let verdict = decide(&self.rules, &req);
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            match verdict {
                Verdict::Pass => inner.call(req).await,
                Verdict::Unauthorized => Ok(problem::unauthorized("authentication required")),
                Verdict::Forbidden => Ok(problem::forbidden("required role missing")),
            }
        })
    }
}
