//! Behavior tests for [`firefly_idp_keycloak::Adapter`] against an in-process
//! [`axum`] mock that stands in for a Keycloak server (port 0, no network, no
//! Docker).
//!
//! These are the Rust analog of pyfly's `tests/idp/test_keycloak_behavior.py`:
//! each test asserts BOTH the outbound request the adapter built (URL, verb,
//! form/JSON body, auth headers) AND that the adapter parsed the canned
//! response into the right domain object. The pyfly fakes record `httpx`
//! requests; here the real reqwest path is exercised and the mock records the
//! wire contract.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use firefly_idp::Adapter as _;
use firefly_idp_keycloak::{Adapter, Config};

const REALM: &str = "demo";

/// One request the mock saw.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    /// Raw path, percent-encoded as it went over the wire.
    path: String,
    query: String,
    authorization: String,
    content_type: String,
    body: Vec<u8>,
}

impl Recorded {
    fn form(&self) -> std::collections::HashMap<String, String> {
        url_decode_form(&String::from_utf8_lossy(&self.body))
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// A canned response keyed by a path/query substring; first match wins.
#[derive(Clone)]
struct Route {
    needle: String,
    method: Option<String>,
    status: StatusCode,
    body: Vec<u8>,
    location: Option<String>,
}

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    routes: Arc<Mutex<Vec<Route>>>,
}

impl MockState {
    fn route_json(&self, needle: &str, status: u16, body: serde_json::Value) {
        self.routes.lock().unwrap().push(Route {
            needle: needle.to_string(),
            method: None,
            status: StatusCode::from_u16(status).unwrap(),
            body: serde_json::to_vec(&body).unwrap(),
            location: None,
        });
    }
    fn route_empty(&self, needle: &str, status: u16) {
        self.routes.lock().unwrap().push(Route {
            needle: needle.to_string(),
            method: None,
            status: StatusCode::from_u16(status).unwrap(),
            body: Vec::new(),
            location: None,
        });
    }
    fn route_location(&self, needle: &str, status: u16, location: &str) {
        self.routes.lock().unwrap().push(Route {
            needle: needle.to_string(),
            method: None,
            status: StatusCode::from_u16(status).unwrap(),
            body: Vec::new(),
            location: Some(location.to_string()),
        });
    }
    fn calls(&self) -> Vec<Recorded> {
        self.calls.lock().unwrap().clone()
    }
    fn find(&self, method: &str, needle: &str) -> Recorded {
        self.calls()
            .into_iter()
            .find(|c| c.method == method && (c.path.contains(needle) || c.query.contains(needle)))
            .unwrap_or_else(|| {
                panic!(
                    "no {method} request matching {needle:?}; got {:?}",
                    self.calls()
                )
            })
    }
}

async fn handler(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    method: axum::http::Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    state.calls.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        authorization: get("authorization"),
        content_type: get("content-type"),
        body: body.to_vec(),
    });

    let target = format!("{path}?{query}");
    let routes = state.routes.lock().unwrap();
    for r in routes.iter() {
        if target.contains(&r.needle) {
            if let Some(m) = &r.method {
                if m != method.as_str() {
                    continue;
                }
            }
            let mut builder = Response::builder().status(r.status);
            if let Some(loc) = &r.location {
                builder = builder.header("location", loc);
            }
            return builder
                .body(axum::body::Body::from(r.body.clone()))
                .unwrap();
        }
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(axum::body::Body::from(format!("no route for {target}")))
        .unwrap()
}

/// Spawns the mock on port 0 and returns its base URL + shared state.
async fn spawn() -> (String, MockState) {
    let state = MockState::default();
    let app = Router::new()
        .route("/", any(handler))
        .route("/*rest", any(handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn url_decode_form(s: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for pair in s.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = pct_decode(it.next().unwrap_or(""));
        let v = pct_decode(it.next().unwrap_or(""));
        out.insert(k, v);
    }
    out
}

fn pct_decode(s: &str) -> String {
    let bytes = s.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn adapter(base_url: &str) -> Adapter {
    Adapter::new(Config {
        base_url: base_url.to_string(),
        realm: REALM.to_string(),
        client_id: "admin-cli".into(),
        client_secret: "s3cr3t".into(),
        ..Config::default()
    })
}

// --------------------------------------------------------------------------- //
// create_user — admin token grant + user POST + Location id parsing
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn create_user_builds_admin_request_and_parses_location_id() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "ADMIN-TOK", "expires_in": 300}),
    );
    state.route_location(
        "/admin/realms/demo/users",
        201,
        &format!("{base}/admin/realms/demo/users/abc-123-uuid"),
    );

    let mut user = firefly_idp::User {
        username: "alice".into(),
        email: "alice@example.com".into(),
        enabled: false,
        ..Default::default()
    };
    user.attributes
        .insert("firstName".into(), serde_json::json!("Al"));
    user.attributes
        .insert("lastName".into(), serde_json::json!("Ice"));
    let result = adapter(&base).create_user(user, "p@ss-w0rd").await.unwrap();

    // (a) outbound: the client_credentials admin token grant came first.
    let token_req = state.find("POST", "openid-connect/token");
    let form = token_req.form();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("client_credentials")
    );
    assert_eq!(form.get("client_id").map(String::as_str), Some("admin-cli"));
    assert_eq!(
        form.get("client_secret").map(String::as_str),
        Some("s3cr3t")
    );

    // (a) outbound: the user-creation POST carries the bearer header + payload.
    let create_req = state.find("POST", "/admin/realms/demo/users");
    assert_eq!(create_req.authorization, "Bearer ADMIN-TOK");
    let body = create_req.json();
    assert_eq!(body["username"], "alice");
    assert_eq!(body["email"], "alice@example.com");
    assert_eq!(body["credentials"][0]["type"], "password");
    assert_eq!(body["credentials"][0]["value"], "p@ss-w0rd");
    assert_eq!(body["credentials"][0]["temporary"], false);

    // (b) parsed: id extracted from the Location header tail.
    assert_eq!(result.id, "abc-123-uuid");
    assert_eq!(result.username, "alice");
}

// --------------------------------------------------------------------------- //
// login — password grant, token parsing, find_by_username follow-up
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn login_password_grant_returns_authresult() {
    let (base, state) = spawn().await;
    // The password grant fires first; the admin client_credentials grant fires
    // during the find_by_username follow-up. Both hit openid-connect/token, so
    // the same canned response serves both (we assert the password-grant body).
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({
            "access_token": "ACCESS-XYZ",
            "refresh_token": "REFRESH-ABC",
            "expires_in": 1800
        }),
    );
    state.route_json(
        "/admin/realms/demo/users?username=bob",
        200,
        serde_json::json!([{"id": "user-9", "username": "bob", "email": "bob@x.io"}]),
    );

    let result = adapter(&base).login_full("bob", "hunter2").await.unwrap();

    // (a) outbound: ROPC password grant with credentials in the form body.
    let token_reqs: Vec<_> = state
        .calls()
        .into_iter()
        .filter(|c| c.method == "POST" && c.path.contains("openid-connect/token"))
        .collect();
    let pw = token_reqs
        .iter()
        .find(|c| c.form().get("grant_type").map(String::as_str) == Some("password"))
        .expect("a password grant was sent");
    let form = pw.form();
    assert_eq!(form.get("username").map(String::as_str), Some("bob"));
    assert_eq!(form.get("password").map(String::as_str), Some("hunter2"));
    assert_eq!(form.get("client_id").map(String::as_str), Some("admin-cli"));

    // (a) outbound: the username lookup uses exact match query params.
    let lookup = state.find("GET", "/admin/realms/demo/users");
    assert!(lookup.query.contains("username=bob"));
    assert!(lookup.query.contains("exact=true"));

    // (b) parsed: tokens + resolved user mapped into AuthResult.
    assert_eq!(result.token.access_token, "ACCESS-XYZ");
    assert_eq!(result.token.refresh_token, "REFRESH-ABC");
    assert_eq!(result.token.expires_in, 1800);
    assert_eq!(result.user.id, "user-9");
    assert_eq!(result.user.username, "bob");
}

#[tokio::test]
async fn login_invalid_credentials_returns_invalid_credentials() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        401,
        serde_json::json!({"error": "invalid_grant"}),
    );

    let err = adapter(&base).login("bob", "wrong").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::InvalidCredentials);

    // The adapter must NOT attempt the find_by_username follow-up.
    assert!(state
        .calls()
        .iter()
        .all(|c| !c.path.contains("/admin/realms")));
}

// --------------------------------------------------------------------------- //
// introspect — token introspection mapped into SessionIntrospection
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn introspect_maps_active_session() {
    let (base, state) = spawn().await;
    state.route_json(
        "token/introspect",
        200,
        serde_json::json!({
            "active": true,
            "sub": "user-9",
            "preferred_username": "bob",
            "scope": "openid profile email"
        }),
    );

    let result = adapter(&base).introspect("ACCESS-XYZ").await.unwrap();

    // (a) outbound: introspect endpoint with client creds + token in form body.
    let req = state.find("POST", "token/introspect");
    let form = req.form();
    assert_eq!(form.get("client_id").map(String::as_str), Some("admin-cli"));
    assert_eq!(
        form.get("client_secret").map(String::as_str),
        Some("s3cr3t")
    );
    assert_eq!(form.get("token").map(String::as_str), Some("ACCESS-XYZ"));

    // (b) parsed: SessionIntrospection with split scopes.
    assert!(result.active);
    assert_eq!(result.user_id, "user-9");
    assert_eq!(result.username, "bob");
    assert_eq!(result.scopes, vec!["openid", "profile", "email"]);
}

// --------------------------------------------------------------------------- //
// admin token caching — a second admin op reuses the cached grant
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn admin_token_is_cached_across_calls() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "ADMIN-TOK", "expires_in": 300}),
    );
    state.route_json(
        "/admin/realms/demo/users/u1",
        200,
        serde_json::json!({"id": "u1", "username": "u1"}),
    );
    state.route_json(
        "/admin/realms/demo/users/u2",
        200,
        serde_json::json!({"id": "u2", "username": "u2"}),
    );

    let a = adapter(&base);
    a.get_user("u1").await.unwrap();
    a.get_user("u2").await.unwrap();

    // Exactly one client_credentials grant for two admin calls.
    let grants = state
        .calls()
        .iter()
        .filter(|c| c.method == "POST" && c.path.contains("openid-connect/token"))
        .count();
    assert_eq!(grants, 1, "admin token should be fetched once and cached");
}

// --------------------------------------------------------------------------- //
// get_user 404 → UserNotFound; find_by_username empty → UserNotFound
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_user_404_maps_to_user_not_found() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "T", "expires_in": 300}),
    );
    state.route_empty("/admin/realms/demo/users/missing", 404);

    let err = adapter(&base).get_user("missing").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::UserNotFound);
}

// --------------------------------------------------------------------------- //
// role-mappings — assign / revoke / list / get
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn assign_role_looks_up_role_then_posts_mapping() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "T", "expires_in": 300}),
    );
    state.route_json(
        "/roles/admin",
        200,
        serde_json::json!({"id": "r1", "name": "admin"}),
    );
    state.route_empty("/users/u1/role-mappings/realm", 204);

    let ok = adapter(&base).assign_role("u1", "admin").await.unwrap();
    assert!(ok);

    let post = state.find("POST", "/users/u1/role-mappings/realm");
    let body = post.json();
    assert_eq!(body[0]["name"], "admin");
}

#[tokio::test]
async fn get_roles_returns_empty_on_non_200() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "T", "expires_in": 300}),
    );
    state.route_empty("/users/ghost/role-mappings/realm", 404);

    let roles = adapter(&base).get_roles("ghost").await.unwrap();
    assert!(roles.is_empty());
}

#[tokio::test]
async fn list_roles_parses_catalogue() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "T", "expires_in": 300}),
    );
    state.route_json(
        "/admin/realms/demo/roles",
        200,
        serde_json::json!([
            {"name": "admin", "description": "Administrators"},
            {"name": "user", "description": ""}
        ]),
    );

    let roles = adapter(&base).list_roles().await.unwrap();
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0].name, "admin");
    assert_eq!(roles[0].description, "Administrators");
}

// --------------------------------------------------------------------------- //
// userinfo (validate / get_user_info) and logout
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_user_info_maps_userinfo_response() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/userinfo",
        200,
        serde_json::json!({
            "sub": "u9",
            "preferred_username": "bob",
            "email": "bob@x.io"
        }),
    );

    let req_token = "DELEGATED";
    let user = adapter(&base).get_user_info(req_token).await.unwrap();
    let info = state.find("GET", "openid-connect/userinfo");
    assert_eq!(info.authorization, "Bearer DELEGATED");
    assert_eq!(user.id, "u9");
    assert_eq!(user.username, "bob");
    assert_eq!(user.email, "bob@x.io");
}

#[tokio::test]
async fn logout_returns_true_on_204() {
    let (base, state) = spawn().await;
    state.route_empty("openid-connect/logout", 204);
    assert!(adapter(&base).logout("ACCESS").await.unwrap());
    let req = state.find("POST", "openid-connect/logout");
    assert_eq!(req.authorization, "Bearer ACCESS");
}

// --------------------------------------------------------------------------- //
// refresh — keeps the supplied refresh token when not rotated
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn refresh_keeps_supplied_token_when_not_rotated() {
    let (base, state) = spawn().await;
    state.route_json(
        "openid-connect/token",
        200,
        serde_json::json!({"access_token": "NEW-ACCESS", "expires_in": 900}),
    );

    let token = adapter(&base).refresh("OLD-REFRESH").await.unwrap();
    assert_eq!(token.access_token, "NEW-ACCESS");
    assert_eq!(token.refresh_token, "OLD-REFRESH");

    let req = state.find("POST", "openid-connect/token");
    let form = req.form();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        form.get("refresh_token").map(String::as_str),
        Some("OLD-REFRESH")
    );
    assert_eq!(req.content_type, "application/x-www-form-urlencoded");
}

// --------------------------------------------------------------------------- //
// MFA stays sentinel (Keycloak runs MFA server-side)
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn mfa_methods_return_sentinel() {
    let (base, _state) = spawn().await;
    let a = adapter(&base);
    assert_eq!(
        a.mfa_challenge("u1").await.unwrap_err(),
        firefly_idp_keycloak::not_implemented()
    );
    assert_eq!(
        a.mfa_verify("c1", "000000").await.unwrap_err(),
        firefly_idp_keycloak::not_implemented()
    );
    assert_eq!(
        firefly_idp_keycloak::ERR_NOT_IMPLEMENTED,
        "firefly/idpkeycloak: not yet implemented"
    );
}

#[test]
fn name_and_config_round_trip() {
    let cfg = Config {
        base_url: "https://keycloak.example.com/".into(),
        realm: "firefly".into(),
        client_id: "app".into(),
        client_secret: "secret".into(),
        ..Config::default()
    };
    let a = Adapter::new(cfg);
    assert_eq!(a.name(), "keycloak");
    // trailing slash trimmed
    assert_eq!(a.config().base_url, "https://keycloak.example.com");
    assert!(a.config().verify_ssl);
}
