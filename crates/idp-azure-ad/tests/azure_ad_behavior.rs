//! Behavior tests for [`firefly_idp_azure_ad::Adapter`] against an in-process
//! [`axum`] mock standing in for Microsoft Graph + the login authority host
//! (port 0, no network).
//!
//! Rust analog of pyfly's `tests/idp/test_azure_ad_behavior.py`: each test
//! asserts BOTH the outbound request the adapter built AND that the canned
//! response is parsed into the right domain object.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use firefly_idp::Adapter as _;
use firefly_idp_azure_ad::{Adapter, Config};

const TENANT: &str = "tenant-abc";
const CLIENT_ID: &str = "app-client-id";
const CLIENT_SECRET: &str = "app-client-secret";
const SCOPE: &str = "https://graph.microsoft.com/.default";

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    authorization: String,
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

#[derive(Clone)]
struct Route {
    needle: String,
    status: StatusCode,
    body: Vec<u8>,
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
            status: StatusCode::from_u16(status).unwrap(),
            body: serde_json::to_vec(&body).unwrap(),
        });
    }
    fn route_empty(&self, needle: &str, status: u16) {
        self.routes.lock().unwrap().push(Route {
            needle: needle.to_string(),
            status: StatusCode::from_u16(status).unwrap(),
            body: Vec::new(),
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
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    state.calls.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query,
        authorization: auth,
        body: body.to_vec(),
    });

    let routes = state.routes.lock().unwrap();
    for r in routes.iter() {
        if path.contains(&r.needle) {
            return Response::builder()
                .status(r.status)
                .body(axum::body::Body::from(r.body.clone()))
                .unwrap();
        }
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(axum::body::Body::from(format!("no route for {path}")))
        .unwrap()
}

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

/// Builds an adapter whose Graph + login hosts both point at the mock.
fn adapter(base: &str) -> Adapter {
    Adapter::new(Config {
        base_url: base.to_string(),
        graph_base_url: format!("{base}/v1.0"),
        tenant: TENANT.to_string(),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        ..Config::default()
    })
}

// --------------------------------------------------------------------------- //
// login — ROPC password grant → token + resolved user
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn login_password_grant_returns_authresult() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({
            "access_token": "ACCESS-AAD",
            "refresh_token": "REFRESH-AAD",
            "expires_in": 3600
        }),
    );
    state.route_json(
        "/users/",
        200,
        serde_json::json!({
            "id": "aad-user-1",
            "userPrincipalName": "alice@example.com",
            "mail": "alice@example.com",
            "givenName": "Alice",
            "surname": "Smith",
            "accountEnabled": true
        }),
    );

    let result = adapter(&base)
        .login_full("alice@example.com", "s3cr3t!")
        .await
        .unwrap();

    // (a) outbound: password grant carries credentials + grant_type + scope.
    let login_req = state
        .calls()
        .into_iter()
        .find(|c| {
            c.method == "POST" && c.form().get("grant_type").map(String::as_str) == Some("password")
        })
        .expect("password grant sent");
    let form = login_req.form();
    assert_eq!(form.get("client_id").map(String::as_str), Some(CLIENT_ID));
    assert_eq!(
        form.get("client_secret").map(String::as_str),
        Some(CLIENT_SECRET)
    );
    assert_eq!(
        form.get("username").map(String::as_str),
        Some("alice@example.com")
    );
    assert_eq!(form.get("password").map(String::as_str), Some("s3cr3t!"));
    assert_eq!(form.get("scope").map(String::as_str), Some(SCOPE));

    // (b) parsed: token fields.
    assert_eq!(result.token.access_token, "ACCESS-AAD");
    assert_eq!(result.token.refresh_token, "REFRESH-AAD");
    assert_eq!(result.token.expires_in, 3600);

    // (c) parsed: user from the Graph GET.
    assert_eq!(result.user.id, "aad-user-1");
    assert_eq!(result.user.username, "alice@example.com");
}

#[tokio::test]
async fn login_invalid_credentials_returns_invalid_credentials() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        401,
        serde_json::json!({"error": "invalid_grant"}),
    );

    let err = adapter(&base)
        .login("alice@example.com", "wrong")
        .await
        .unwrap_err();
    assert_eq!(err, firefly_idp::Error::InvalidCredentials);
    // No Graph calls after auth failure.
    assert!(state.calls().iter().all(|c| !c.path.contains("/users/")));
}

// --------------------------------------------------------------------------- //
// get_user — Graph GET /users/{id} → parsed user; 404 → UserNotFound
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_user_hits_graph_and_parses_user() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json(
        "/users/",
        200,
        serde_json::json!({
            "id": "aad-user-42",
            "userPrincipalName": "bob@example.com",
            "mail": "bob@example.com",
            "givenName": "Bob",
            "surname": "Jones",
            "accountEnabled": true
        }),
    );

    let user = adapter(&base).get_user("aad-user-42").await.unwrap();

    let token_req = state.find("POST", "oauth2/v2.0/token");
    assert_eq!(
        token_req.form().get("grant_type").map(String::as_str),
        Some("client_credentials")
    );

    let user_req = state.find("GET", "/users/");
    assert!(user_req.path.contains("aad-user-42"));
    assert_eq!(user_req.authorization, "Bearer APP-TOKEN");

    assert_eq!(user.id, "aad-user-42");
    assert_eq!(user.username, "bob@example.com");
    assert_eq!(user.email, "bob@example.com");
    assert_eq!(
        user.attributes.get("firstName").and_then(|v| v.as_str()),
        Some("Bob")
    );
    assert_eq!(
        user.attributes.get("lastName").and_then(|v| v.as_str()),
        Some("Jones")
    );
    assert!(user.enabled);
}

#[tokio::test]
async fn get_user_404_returns_user_not_found() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_empty("/users/", 404);

    let err = adapter(&base).get_user("nonexistent-id").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::UserNotFound);
}

// --------------------------------------------------------------------------- //
// find_by_username — delegates to get_user
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn find_by_username_delegates_to_get_user() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json(
        "/users/",
        200,
        serde_json::json!({
            "id": "aad-user-99",
            "userPrincipalName": "carol@example.com",
            "mail": "carol@example.com",
            "accountEnabled": true
        }),
    );

    let user = adapter(&base)
        .find_by_username("carol@example.com")
        .await
        .unwrap();
    let req = state.find("GET", "/users/");
    assert!(req.path.contains("carol@example.com") || req.path.contains("carol%40example.com"));
    assert_eq!(user.id, "aad-user-99");
    assert_eq!(user.username, "carol@example.com");
}

// --------------------------------------------------------------------------- //
// assign_role — POST to /groups/{id}/members/$ref
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn assign_role_posts_to_group_members_ref() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_empty("/members/$ref", 204);

    let ok = adapter(&base)
        .assign_role("aad-user-42", "group-admin-id")
        .await
        .unwrap();
    assert!(ok);

    let ref_req = state.find("POST", "/members/$ref");
    assert!(ref_req.path.contains("group-admin-id"));
    assert_eq!(ref_req.authorization, "Bearer APP-TOKEN");
    let odata = ref_req.json();
    let odata_id = odata["@odata.id"].as_str().unwrap();
    assert!(odata_id.ends_with("/directoryObjects/aad-user-42"));
}

// --------------------------------------------------------------------------- //
// create_user — POST /users with full profile, parse id
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn create_user_posts_to_graph_users_and_parses_id() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json(
        "/users",
        201,
        serde_json::json!({"id": "new-aad-user-id", "userPrincipalName": "dave@example.com"}),
    );

    let mut user = firefly_idp::User {
        username: "dave".into(),
        email: "dave@example.com".into(),
        enabled: true,
        ..Default::default()
    };
    user.attributes
        .insert("firstName".into(), serde_json::json!("Dave"));
    user.attributes
        .insert("lastName".into(), serde_json::json!("Baker"));

    let result = adapter(&base)
        .create_user(user, "Str0ng!Pass")
        .await
        .unwrap();

    let create_req = state.find("POST", "/users");
    assert_eq!(create_req.authorization, "Bearer APP-TOKEN");
    let body = create_req.json();
    assert_eq!(body["mailNickname"], "dave");
    assert_eq!(body["userPrincipalName"], "dave@example.com");
    assert_eq!(body["givenName"], "Dave");
    assert_eq!(body["surname"], "Baker");
    assert_eq!(body["passwordProfile"]["password"], "Str0ng!Pass");
    assert_eq!(
        body["passwordProfile"]["forceChangePasswordNextSignIn"],
        false
    );

    assert_eq!(result.id, "new-aad-user-id");
}

// --------------------------------------------------------------------------- //
// get_user_info — GET /me with delegated token (no app-token fetch)
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_user_info_calls_graph_me_with_delegated_token() {
    let (base, state) = spawn().await;
    state.route_json(
        "/me",
        200,
        serde_json::json!({
            "id": "aad-user-me",
            "userPrincipalName": "eve@example.com",
            "mail": "eve@example.com",
            "givenName": "Eve",
            "surname": "Chen",
            "accountEnabled": true
        }),
    );

    let user = adapter(&base)
        .get_user_info("DELEGATED-TOKEN")
        .await
        .unwrap();

    let me_req = state.find("GET", "/me");
    assert_eq!(me_req.authorization, "Bearer DELEGATED-TOKEN");
    // No app-token fetch happened.
    assert!(state
        .calls()
        .iter()
        .all(|c| !c.path.contains("oauth2/v2.0/token")));

    assert_eq!(user.id, "aad-user-me");
    assert_eq!(user.username, "eve@example.com");
    assert_eq!(user.email, "eve@example.com");
    assert!(user.enabled);
}

#[tokio::test]
async fn get_user_info_returns_error_on_non_200() {
    let (base, state) = spawn().await;
    state.route_json(
        "/me",
        401,
        serde_json::json!({"error": "InvalidAuthenticationToken"}),
    );

    let err = adapter(&base).get_user_info("BAD-TOKEN").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::UserNotFound);
}

// --------------------------------------------------------------------------- //
// introspect — /me active session
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn introspect_active_and_inactive() {
    let (base, state) = spawn().await;
    state.route_json(
        "/me",
        200,
        serde_json::json!({"id": "aad-user-me", "userPrincipalName": "eve@example.com"}),
    );

    let session = adapter(&base).introspect("TOK").await.unwrap();
    assert!(session.active);
    assert_eq!(session.user_id, "aad-user-me");
    assert_eq!(session.username, "eve@example.com");

    // A fresh adapter against a 401 /me reports inactive (not an error).
    let (base2, state2) = spawn().await;
    state2.route_empty("/me", 401);
    let inactive = adapter(&base2).introspect("BAD").await.unwrap();
    assert!(!inactive.active);
}

// --------------------------------------------------------------------------- //
// register_user — forces enabled, delegates to create_user
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn register_user_forces_enabled_and_posts_to_graph() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json(
        "/users",
        201,
        serde_json::json!({"id": "reg-user-id", "userPrincipalName": "grace@example.com"}),
    );

    let user = firefly_idp::User {
        username: "grace".into(),
        email: "grace@example.com".into(),
        enabled: false,
        ..Default::default()
    };
    let result = adapter(&base)
        .register_user(user, "Reg1st3r!")
        .await
        .unwrap();

    let create_req = state.find("POST", "/users");
    assert_eq!(create_req.json()["accountEnabled"], true);
    assert_eq!(result.id, "reg-user-id");
}

// --------------------------------------------------------------------------- //
// get_roles — GET /users/{id}/memberOf → roles; non-200 → empty
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_roles_calls_member_of_and_parses_roles() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json(
        "/memberOf",
        200,
        serde_json::json!({
            "value": [
                {"id": "grp-admins", "displayName": "Admins"},
                {"id": "grp-editors", "displayName": "Editors"}
            ]
        }),
    );

    let roles = adapter(&base).get_roles("aad-user-42").await.unwrap();

    let member_req = state.find("GET", "/memberOf");
    assert!(member_req.path.contains("aad-user-42"));
    assert_eq!(member_req.authorization, "Bearer APP-TOKEN");

    assert_eq!(roles.len(), 2);
    let names: std::collections::HashSet<_> = roles.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains("grp-admins"));
    assert!(names.contains("grp-editors"));
    let descs: std::collections::HashSet<_> =
        roles.iter().map(|r| r.description.as_str()).collect();
    assert!(descs.contains("Admins"));
    assert!(descs.contains("Editors"));
}

#[tokio::test]
async fn get_roles_returns_empty_on_non_200() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_empty("/memberOf", 404);

    let roles = adapter(&base).get_roles("nonexistent-user").await.unwrap();
    assert!(roles.is_empty());
}

// --------------------------------------------------------------------------- //
// list_roles — GET /groups → roles
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn list_roles_parses_groups() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json(
        "/groups",
        200,
        serde_json::json!({"value": [{"id": "g1", "displayName": "Engineering"}]}),
    );

    let roles = adapter(&base).list_roles().await.unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].name, "g1");
    assert_eq!(roles[0].description, "Engineering");
}

// --------------------------------------------------------------------------- //
// app-token caching — fetched once across multiple admin calls
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn app_token_is_cached() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOKEN"}),
    );
    state.route_json("/groups", 200, serde_json::json!({"value": []}));

    let a = adapter(&base);
    a.list_roles().await.unwrap();
    a.list_roles().await.unwrap();

    let grants = state
        .calls()
        .iter()
        .filter(|c| c.method == "POST" && c.path.contains("oauth2/v2.0/token"))
        .count();
    assert_eq!(grants, 1);
}

// --------------------------------------------------------------------------- //
// logout always true; MFA sentinel; name/config
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn logout_always_true() {
    let (base, _state) = spawn().await;
    assert!(adapter(&base).logout("anything").await.unwrap());
}

// --------------------------------------------------------------------------- //
// MFA — mfa_challenge registers a software-OATH (TOTP) method (real Graph)
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn mfa_challenge_registers_software_oath_method() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOK", "expires_in": 3600}),
    );
    state.route_json(
        "/authentication/softwareOathMethods",
        201,
        serde_json::json!({"id": "method-id-1", "secretKey": "JBSWY3DPEHPK3PXP"}),
    );

    let challenge = adapter(&base).mfa_challenge("u1").await.unwrap();

    // (a) outbound: POST to the softwareOathMethods endpoint with the app token.
    let req = state.find("POST", "/users/u1/authentication/softwareOathMethods");
    assert_eq!(req.authorization, "Bearer APP-TOK");

    // (b) parsed: method id in challenge_id, "TOTP:{secretKey}" in method.
    assert_eq!(challenge.challenge_id, "method-id-1");
    assert_eq!(challenge.method, "TOTP:JBSWY3DPEHPK3PXP");
    assert_eq!(challenge.user_id, "u1");
}

#[tokio::test]
async fn mfa_challenge_missing_user_maps_to_user_not_found() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOK", "expires_in": 3600}),
    );
    state.route_empty("/authentication/softwareOathMethods", 404);

    let err = adapter(&base).mfa_challenge("ghost").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::UserNotFound);
}

// --------------------------------------------------------------------------- //
// MFA — mfa_verify is a documented provider capability boundary
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn mfa_verify_is_unsupported_by_provider() {
    let (base, _state) = spawn().await;
    let err = adapter(&base).mfa_verify("c1", "000000").await.unwrap_err();
    match err {
        firefly_idp::Error::UnsupportedByProvider {
            provider,
            operation,
            reason,
        } => {
            assert_eq!(provider, "azure-ad");
            assert_eq!(operation, "mfa_verify");
            assert!(reason.contains("no API to verify a TOTP code"));
        }
        other => panic!("expected UnsupportedByProvider, got {other:?}"),
    }
}

// --------------------------------------------------------------------------- //
// list_authentication_methods — GET .../authentication/methods (real Graph)
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn list_authentication_methods_returns_odata_types() {
    let (base, state) = spawn().await;
    state.route_json(
        "oauth2/v2.0/token",
        200,
        serde_json::json!({"access_token": "APP-TOK", "expires_in": 3600}),
    );
    state.route_json(
        "/authentication/methods",
        200,
        serde_json::json!({"value": [
            {"@odata.type": "#microsoft.graph.passwordAuthenticationMethod"},
            {"@odata.type": "#microsoft.graph.softwareOathAuthenticationMethod"}
        ]}),
    );

    let methods = adapter(&base)
        .list_authentication_methods("u1")
        .await
        .unwrap();
    assert_eq!(
        methods,
        vec![
            "#microsoft.graph.passwordAuthenticationMethod".to_string(),
            "#microsoft.graph.softwareOathAuthenticationMethod".to_string(),
        ]
    );
    let req = state.find("GET", "/users/u1/authentication/methods");
    assert_eq!(req.authorization, "Bearer APP-TOK");
}

#[test]
fn name_and_config_defaults() {
    let a = Adapter::new(Config {
        tenant: TENANT.into(),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        ..Config::default()
    });
    assert_eq!(a.name(), "azure-ad");
    // Empty host/scope fields fall back to the public Microsoft defaults.
    assert_eq!(a.config().scope, "");
}
