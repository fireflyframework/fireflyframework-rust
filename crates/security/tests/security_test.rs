//! In-process port of the Go module's `security_test.go`, plus
//! Rust-specific wire-shape and bounds tests. All HTTP assertions run
//! through `tower::ServiceExt::oneshot` — no sockets.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use http_body_util::BodyExt;
use tower::ServiceExt;

use firefly_security::{
    Authentication, BearerConfig, BearerLayer, FilterChain, SecurityError, Verifier, VerifierFn,
    ANONYMOUS_ID,
};

/// A verifier that accepts the token `"good"` as alice — the Rust twin
/// of the Go tests' `VerifierFunc`.
struct GoodVerifier;

#[async_trait::async_trait]
impl Verifier for GoodVerifier {
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError> {
        if token == "good" {
            Ok(Authentication {
                principal: "u1".into(),
                username: "alice".into(),
                roles: vec!["USER".into()],
                ..Default::default()
            })
        } else {
            Err(SecurityError::verification("nope"))
        }
    }
}

fn good_verifier() -> GoodVerifier {
    GoodVerifier
}

/// Handler that returns 200 unconditionally (the Go tests' `ok`).
async fn ok() -> StatusCode {
    StatusCode::OK
}

/// Router whose every path echoes the request's authentication (if any)
/// as JSON.
fn echo_app() -> Router {
    Router::new().fallback(|auth: Option<Extension<Authentication>>| async move {
        match auth {
            Some(Extension(a)) => Json(a).into_response(),
            None => StatusCode::OK.into_response(),
        }
    })
}

fn get_req(uri: &str) -> Request {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn get_req_with_auth(uri: &str, auth: Authentication) -> Request {
    Request::builder()
        .uri(uri)
        .extension(auth)
        .body(Body::empty())
        .unwrap()
}

async fn body_string(resp: Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn body_json(resp: Response) -> serde_json::Value {
    serde_json::from_str(&body_string(resp).await).unwrap()
}

// ---------------------------------------------------------------------------
// Ported from Go: TestBearerMiddlewareSuccess
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_middleware_success() {
    let app = echo_app().layer(BearerLayer::new(BearerConfig::new(good_verifier())));

    let req = Request::builder()
        .uri("/x")
        .header("Authorization", "Bearer good")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let seen = body_json(resp).await;
    assert_eq!(seen["username"], "alice", "auth not propagated: {seen}");
    assert_eq!(seen["principal"], "u1");
    assert_eq!(seen["roles"], serde_json::json!(["USER"]));
}

// ---------------------------------------------------------------------------
// Ported from Go: TestBearerMiddlewareUnauthorized
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_middleware_unauthorized() {
    let verifier = VerifierFn(|_token: String| async move {
        Err::<Authentication, _>(SecurityError::verification("invalid"))
    });
    let app = Router::new()
        .route("/x", get(ok))
        .layer(BearerLayer::new(BearerConfig::new(verifier)));

    let req = Request::builder()
        .uri("/x")
        .header("Authorization", "Bearer bad")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let pd = body_json(resp).await;
    assert_eq!(pd["detail"], "invalid");
}

// ---------------------------------------------------------------------------
// Ported from Go: TestBearerMiddlewareAnonymous
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_middleware_anonymous() {
    let verifier = VerifierFn(|_token: String| async move {
        Err::<Authentication, _>(SecurityError::verification("nope"))
    });
    let app = echo_app().layer(BearerLayer::new(
        BearerConfig::new(verifier).allow_anonymous(true),
    ));

    let resp = app.oneshot(get_req("/x")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let seen = body_json(resp).await;
    assert_eq!(seen["principal"], ANONYMOUS_ID, "anon: {seen}");
}

// ---------------------------------------------------------------------------
// Ported from Go: TestFilterChainEnforcesRoles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filter_chain_enforces_roles() {
    let chain = FilterChain::new()
        .permit("/actuator/")
        .require("/admin/", &["ADMIN"])
        .require("/api/", &[]);
    let app = Router::new().fallback(ok).layer(chain.layer());

    // Public endpoint passes without auth.
    let resp = app
        .clone()
        .oneshot(get_req("/actuator/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "permit blocked");

    // Admin endpoint without auth → 401.
    let resp = app.clone().oneshot(get_req("/admin/users")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "admin no-auth");

    // Admin endpoint with USER role → 403.
    let auth = Authentication {
        principal: "u1".into(),
        roles: vec!["USER".into()],
        ..Default::default()
    };
    let resp = app
        .clone()
        .oneshot(get_req_with_auth("/admin/users", auth))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "admin user-role");

    // Admin endpoint with ADMIN role → OK.
    let auth = Authentication {
        principal: "u1".into(),
        roles: vec!["ADMIN".into()],
        ..Default::default()
    };
    let resp = app
        .clone()
        .oneshot(get_req_with_auth("/admin/users", auth))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "admin admin-role");

    // API with any auth.
    let auth = Authentication {
        principal: "u2".into(),
        ..Default::default()
    };
    let resp = app
        .oneshot(get_req_with_auth("/api/orders", auth))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "api any-auth");
}

// ---------------------------------------------------------------------------
// Rust-specific coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_missing_header_is_unauthenticated() {
    let app = Router::new()
        .route("/x", get(ok))
        .layer(BearerLayer::new(BearerConfig::new(good_verifier())));

    let resp = app.oneshot(get_req("/x")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let pd = body_json(resp).await;
    assert_eq!(pd["detail"], "firefly/security: unauthenticated");
}

#[tokio::test]
async fn bearer_malformed_header_is_rejected() {
    let app = Router::new()
        .route("/x", get(ok))
        .layer(BearerLayer::new(BearerConfig::new(good_verifier())));

    for value in ["Basic dXNlcjpwYXNz", "bearer good", "Bearer", "Token x"] {
        let req = Request::builder()
            .uri("/x")
            .header("Authorization", value)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "value {value:?}");
        let pd = body_json(resp).await;
        assert_eq!(pd["detail"], "malformed Authorization header");
    }
}

/// The 401 envelope must be byte-identical to the Go port's
/// `web.WriteProblem(w, kernel.ProblemUnauthorized(...))` (modulo the
/// trailing newline Go's `json.Encoder` appends): members in
/// alphabetical order, canonical type URI, problem content type.
#[tokio::test]
async fn unauthorized_problem_wire_shape_matches_go() {
    let app = Router::new()
        .route("/x", get(ok))
        .layer(BearerLayer::new(BearerConfig::new(good_verifier())));

    let resp = app.oneshot(get_req("/x")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(
        body_string(resp).await,
        "{\"detail\":\"firefly/security: unauthenticated\",\"status\":401,\
         \"title\":\"Unauthorized\",\
         \"type\":\"https://fireflyframework.org/problems/unauthorized\"}"
    );
}

/// Same byte-level check for the filter chain's 403 envelope.
#[tokio::test]
async fn forbidden_problem_wire_shape_matches_go() {
    let chain = FilterChain::new().require("/admin/", &["ADMIN"]);
    let app = Router::new().fallback(ok).layer(chain.layer());

    let auth = Authentication {
        principal: "u1".into(),
        roles: vec!["USER".into()],
        ..Default::default()
    };
    let resp = app
        .oneshot(get_req_with_auth("/admin/users", auth))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(
        body_string(resp).await,
        "{\"detail\":\"required role missing\",\"status\":403,\
         \"title\":\"Forbidden\",\
         \"type\":\"https://fireflyframework.org/problems/forbidden\"}"
    );
}

#[tokio::test]
async fn bearer_custom_header_name() {
    let app = echo_app().layer(BearerLayer::new(
        BearerConfig::new(good_verifier()).header_name("X-Auth-Token"),
    ));

    let req = Request::builder()
        .uri("/x")
        .header("X-Auth-Token", "Bearer good")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let seen = body_json(resp).await;
    assert_eq!(seen["username"], "alice");

    // The default Authorization header is ignored once renamed.
    let req = Request::builder()
        .uri("/x")
        .header("Authorization", "Bearer good")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bearer_custom_unauthorized_handler() {
    let cfg = BearerConfig::new(good_verifier()).unauthorized(|req, err| {
        (
            StatusCode::IM_A_TEAPOT,
            format!("{} {} -> {err}", req.method(), req.uri().path()),
        )
            .into_response()
    });
    let app = Router::new()
        .route("/x", get(ok))
        .layer(BearerLayer::new(cfg));

    let resp = app.oneshot(get_req("/x")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::IM_A_TEAPOT);
    assert_eq!(
        body_string(resp).await,
        "GET /x -> firefly/security: unauthenticated"
    );
}

#[tokio::test]
async fn filter_chain_permit_method_is_method_scoped_and_case_insensitive() {
    let chain = FilterChain::new()
        .permit_method("get", "/public/")
        .require("/public/", &["ADMIN"]);
    let app = Router::new().fallback(ok).layer(chain.layer());

    // GET matches the (lowercase-declared) permit rule.
    let resp = app.clone().oneshot(get_req("/public/info")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // POST skips the permit rule and falls through to the require rule.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/public/info")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn filter_chain_default_allows_unmatched_paths() {
    let chain = FilterChain::new().require("/admin/", &["ADMIN"]);
    let app = Router::new().fallback(ok).layer(chain.layer());

    let resp = app.oneshot(get_req("/totally/elsewhere")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn filter_chain_first_match_wins() {
    // Declaration order decides: the broad permit shadows the narrower
    // require that follows it.
    let chain = FilterChain::new()
        .permit("/api/")
        .require("/api/private/", &["ADMIN"]);
    let app = Router::new().fallback(ok).layer(chain.layer());

    let resp = app.oneshot(get_req("/api/private/x")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn filter_chain_rejects_anonymous_and_empty_principals() {
    let chain = FilterChain::new().require("/api/", &[]);
    let app = Router::new().fallback(ok).layer(chain.layer());

    // The anonymous principal does not count as authenticated.
    let resp = app
        .clone()
        .oneshot(get_req_with_auth("/api/x", Authentication::anonymous()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Nor does an empty principal.
    let resp = app
        .oneshot(get_req_with_auth("/api/x", Authentication::default()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// The README quick-start composition: bearer (anonymous allowed)
/// outermost, then the chain — exactly Go's
/// `bearer(chain.Middleware()(mux))`.
#[tokio::test]
async fn bearer_and_chain_compose_full_stack() {
    let chain = FilterChain::new()
        .permit("/actuator/health")
        .require("/admin/", &["ADMIN"]);
    let app = echo_app().layer(chain.layer()).layer(BearerLayer::new(
        BearerConfig::new(good_verifier()).allow_anonymous(true),
    ));

    // Anonymous request reaches the public endpoint.
    let resp = app
        .clone()
        .oneshot(get_req("/actuator/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Anonymous request is stopped by the chain.
    let resp = app.clone().oneshot(get_req("/admin/users")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // A USER token reaches the chain but lacks ADMIN.
    let req = Request::builder()
        .uri("/admin/users")
        .header("Authorization", "Bearer good")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[test]
fn public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Authentication>();
    assert_send_sync::<SecurityError>();
    assert_send_sync::<BearerConfig>();
    assert_send_sync::<BearerLayer>();
    assert_send_sync::<FilterChain>();
    assert_send_sync::<firefly_security::FilterChainLayer>();
    assert_send_sync::<firefly_security::Rule>();
}
