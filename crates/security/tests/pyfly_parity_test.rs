//! Integration tests for the pyfly-parity HTTP surfaces:
//!
//! - [`CsrfLayer`] — port of pyfly's `TestCsrfFilter`
//!   (`tests/security/test_csrf.py`): safe-method cookie set, unsafe
//!   double-submit validation, bearer bypass, token rotation.
//! - [`FilterChain`]'s glob / `deny` / `authenticated` /
//!   `require_authority` rules and [`RoleHierarchy`] integration —
//!   the URL-DSL behaviours of pyfly's `HttpSecurity`
//!   (`security/http_security.py`), exercised through the real tower
//!   stack.
//!
//! Everything runs in-process via `tower::ServiceExt::oneshot`; no
//! sleeps, no external servers.

use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use firefly_security::{
    with_authentication, Authentication, CsrfLayer, FilterChain, RoleHierarchy, CSRF_COOKIE_NAME,
    CSRF_HEADER_NAME,
};
use http::{header, Method, StatusCode};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A minimal app guarded by `CsrfLayer`: `GET /r` and `POST /r`.
fn csrf_app() -> Router {
    Router::new()
        .route("/r", get(|| async { "read" }).post(|| async { "wrote" }))
        .layer(CsrfLayer::new())
}

/// Extracts the `XSRF-TOKEN` value from a response's `Set-Cookie`
/// headers, if present.
fn set_cookie_token(resp: &Response) -> Option<String> {
    for value in resp.headers().get_all(header::SET_COOKIE) {
        let raw = value.to_str().ok()?;
        for pair in raw.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=') {
                if k.trim() == CSRF_COOKIE_NAME {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

async fn status_of(app: Router, req: Request) -> StatusCode {
    app.oneshot(req).await.unwrap().status()
}

fn auth(principal: &str, roles: &[&str], authorities: &[&str]) -> Authentication {
    Authentication {
        principal: principal.into(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        authorities: authorities.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// CsrfLayer — port of pyfly TestCsrfFilter
// ---------------------------------------------------------------------------

// Ported from pyfly: test_csrf_filter_safe_method_sets_cookie
#[tokio::test]
async fn csrf_safe_method_sets_cookie() {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/r")
        .body(Body::empty())
        .unwrap();
    let resp = csrf_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(set_cookie_token(&resp).is_some(), "XSRF-TOKEN cookie set");
}

// Ported from pyfly: test_csrf_filter_unsafe_method_missing_cookie
#[tokio::test]
async fn csrf_unsafe_missing_cookie_is_forbidden() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/r")
        .header(CSRF_HEADER_NAME, "some-token")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(csrf_app(), req).await, StatusCode::FORBIDDEN);
}

// Ported from pyfly: test_csrf_filter_unsafe_method_missing_header
#[tokio::test]
async fn csrf_unsafe_missing_header_is_forbidden() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/r")
        .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}=some-token"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(csrf_app(), req).await, StatusCode::FORBIDDEN);
}

// Ported from pyfly: test_csrf_filter_unsafe_method_invalid_token
#[tokio::test]
async fn csrf_unsafe_mismatched_tokens_is_forbidden() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/r")
        .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}=token-a"))
        .header(CSRF_HEADER_NAME, "token-b")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(csrf_app(), req).await, StatusCode::FORBIDDEN);
}

// Ported from pyfly: test_csrf_filter_unsafe_method_valid_token
#[tokio::test]
async fn csrf_unsafe_matching_tokens_pass_and_rotate() {
    let token = "matching-token-value";
    let req = Request::builder()
        .method(Method::POST)
        .uri("/r")
        .header(header::COOKIE, format!("{CSRF_COOKIE_NAME}={token}"))
        .header(CSRF_HEADER_NAME, token)
        .body(Body::empty())
        .unwrap();
    let resp = csrf_app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // A fresh CSRF cookie is set on the response (token rotation).
    let rotated = set_cookie_token(&resp).expect("rotated cookie present");
    assert_ne!(rotated, token, "token rotated after a valid submission");
}

// Ported from pyfly: test_csrf_filter_bearer_bypass
#[tokio::test]
async fn csrf_bearer_requests_bypass_validation() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/r")
        .header(header::AUTHORIZATION, "Bearer eyJhbGciOiJIUzI1NiJ9.x.y")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status_of(csrf_app(), req).await, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// FilterChain pyfly-parity URL DSL: glob / deny / authenticated /
// require_authority / role hierarchy
// ---------------------------------------------------------------------------

/// A handler the chain protects; the chain reads `Authentication` from
/// the request extensions (already populated by `BearerLayer` in
/// production — here injected directly).
async fn ok() -> &'static str {
    "ok"
}

/// Builds an app guarded by `chain`, optionally pre-authenticating the
/// request with `who`.
fn chain_app(chain: FilterChain) -> Router {
    Router::new()
        .route("/api/admin/users", get(ok))
        .route("/api/admin/users", post(ok))
        .route("/api/me", get(ok))
        .route("/files/report.pdf", get(ok))
        .route("/internal/metrics", get(ok))
        .route("/public/docs", get(ok))
        .layer(chain.layer())
}

fn req_as(uri: &str, who: Option<Authentication>) -> Request {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    match who {
        Some(auth) => with_authentication(req, auth),
        None => req,
    }
}

#[tokio::test]
async fn glob_pattern_matches_across_path_segments() {
    // pyfly request_matchers("/api/admin/**").has_any_role("ADMIN")
    let chain = FilterChain::new().require_pattern("/api/admin/**", &["ADMIN"]);

    // ADMIN reaches the globbed path.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(
            app,
            req_as("/api/admin/users", Some(auth("u1", &["ADMIN"], &[])))
        )
        .await,
        StatusCode::OK
    );

    // Non-admin is forbidden.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(
            app,
            req_as("/api/admin/users", Some(auth("u2", &["USER"], &[])))
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // Anonymous (no auth on the request) is unauthorized.
    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/api/admin/users", None)).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn deny_rule_rejects_everyone() {
    // pyfly request_matchers("/internal/**").deny_all()
    let chain = FilterChain::new().deny("/internal/**");

    // Even an authenticated admin is forbidden.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(
            app,
            req_as("/internal/metrics", Some(auth("u1", &["ADMIN"], &[])))
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // Anonymous is forbidden too (deny is unconditional).
    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/internal/metrics", None)).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn authenticated_rule_requires_any_principal() {
    // pyfly request_matchers("/api/**").authenticated()
    let chain = FilterChain::new().authenticated("/api/**");

    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/api/me", Some(auth("u1", &[], &[])))).await,
        StatusCode::OK
    );

    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/api/me", None)).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn require_authority_checks_permissions() {
    // pyfly request_matchers("/files/**").has_permission("files:read")
    let chain = FilterChain::new().require_authority("/files/**", &["files:read"]);

    // Principal carrying the authority passes.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(
            app,
            req_as("/files/report.pdf", Some(auth("u1", &[], &["files:read"])))
        )
        .await,
        StatusCode::OK
    );

    // Principal without it is forbidden.
    let app = chain_app(chain);
    assert_eq!(
        status_of(
            app,
            req_as("/files/report.pdf", Some(auth("u2", &[], &["files:write"])))
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn role_hierarchy_lets_admin_satisfy_lower_role_rules() {
    // pyfly role_hierarchy: ADMIN > USER; a USER-gated path admits ADMIN.
    let chain = FilterChain::new()
        .require_pattern("/api/**", &["USER"])
        .with_role_hierarchy(RoleHierarchy::from_string("ADMIN > USER"));

    // ADMIN reaches a USER-gated path via the hierarchy.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/api/me", Some(auth("u1", &["ADMIN"], &[])))).await,
        StatusCode::OK
    );

    // An unrelated role is still rejected.
    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/api/me", Some(auth("u2", &["GUEST"], &[])))).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn first_matching_rule_wins_with_globs() {
    // permit the docs glob before a broad deny; declaration order decides.
    let chain = FilterChain::new().permit_pattern("/public/**").deny("/**");

    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/public/docs", None)).await,
        StatusCode::OK
    );

    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/api/me", None)).await,
        StatusCode::FORBIDDEN
    );
}

// Regression for Bug 1: FilterChain must be fail-CLOSED (deny-by-default)
// on unmatched paths once any rule is configured — matching pyfly's
// HttpSecurityFilter ("Access to this resource is denied (no matching
// security rule).") and Spring Security 6, NOT fail-open / default-allow.
#[tokio::test]
async fn unmatched_path_is_denied_when_rules_configured() {
    // A pyfly-style config declaring only known paths and relying on the
    // implicit deny-all tail. /api/me is declared; an *undeclared* path
    // (here the existing /public/docs route) must be denied, not served.
    let chain = FilterChain::new()
        .permit("/api/admin")
        .authenticated("/api/**");

    // The declared, authenticated path still works for a principal.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/api/me", Some(auth("u1", &[], &[])))).await,
        StatusCode::OK
    );

    // An undeclared path matches no rule -> 403 (fail-closed), even for an
    // authenticated admin. Pre-fix this returned 200 (fail-open).
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(
            app,
            req_as("/public/docs", Some(auth("admin", &["ADMIN"], &[])))
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // Anonymous on the undeclared path is likewise denied (403, not 200).
    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/public/docs", None)).await,
        StatusCode::FORBIDDEN
    );
}

// Regression for Bug 1: a chain with NO rules is a no-op (never a blanket
// lockout), matching pyfly ("An HttpSecurity with no rules at all is a
// no-op").
#[tokio::test]
async fn empty_chain_passes_everything_through() {
    let app = chain_app(FilterChain::new());
    assert_eq!(
        status_of(app, req_as("/public/docs", None)).await,
        StatusCode::OK
    );
}

// Regression for Bug 1: the any_request_* catch-all (pyfly
// `any_request().permit_all()` / `.authenticated()` / `.deny_all()`)
// re-opens the deny-by-default tail with explicit, declaration-ordered
// semantics.
#[tokio::test]
async fn any_request_permit_reopens_unmatched_tail() {
    // Gate /api/** behind a role, then permit everything else.
    let chain = FilterChain::new()
        .require_pattern("/api/admin/**", &["ADMIN"])
        .any_request_permit();

    // The catch-all serves an otherwise-unmatched path, even anonymous.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/public/docs", None)).await,
        StatusCode::OK
    );

    // The earlier, more-specific rule still wins for its path.
    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/api/admin/users", None)).await,
        StatusCode::UNAUTHORIZED
    );
}

// Regression for Bug 1: any_request_authenticated forces auth on the tail.
#[tokio::test]
async fn any_request_authenticated_gates_unmatched_tail() {
    let chain = FilterChain::new()
        .permit("/public")
        .any_request_authenticated();

    // Unmatched-by-permit path requires a principal.
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/api/me", None)).await,
        StatusCode::UNAUTHORIZED
    );
    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/api/me", Some(auth("u1", &[], &[])))).await,
        StatusCode::OK
    );

    // The permit rule still serves the public path anonymously.
    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/public/docs", None)).await,
        StatusCode::OK
    );
}

// Regression for Bug 1: any_request_deny is the explicit form of the
// implicit deny-by-default tail (pyfly `any_request().deny_all()`).
#[tokio::test]
async fn any_request_deny_rejects_unmatched_tail() {
    let chain = FilterChain::new().permit("/public").any_request_deny();

    let app = chain_app(chain.clone());
    assert_eq!(
        status_of(app, req_as("/api/me", Some(auth("admin", &["ADMIN"], &[])))).await,
        StatusCode::FORBIDDEN
    );

    let app = chain_app(chain);
    assert_eq!(
        status_of(app, req_as("/public/docs", None)).await,
        StatusCode::OK
    );
}
