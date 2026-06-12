//! Behavior tests for [`firefly_idp_aws_cognito::Adapter`] against an
//! in-process [`axum`] mock standing in for the Cognito Identity Provider JSON
//! API (port 0, no network, no AWS credentials).
//!
//! Rust analog of pyfly's `tests/idp/test_cognito_behavior.py`. pyfly injects a
//! fake boto3 client and asserts the call name + kwargs; here the brief
//! mandates the raw JSON API, so the mock asserts the wire contract instead:
//! the `X-Amz-Target` action header and the JSON request body. Admin calls
//! additionally carry a SigV4 `Authorization` header (asserted present).

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use firefly_idp::Adapter as _;
use firefly_idp_aws_cognito::{Adapter, Config};

const USER_POOL_ID: &str = "us-east-1_TestPool";
const CLIENT_ID: &str = "test-client-id";
const REGION: &str = "us-east-1";

#[derive(Debug, Clone)]
struct Recorded {
    target: String,
    authorization: String,
    content_sha256: String,
    body: serde_json::Value,
}

#[derive(Clone, Default)]
struct MockState {
    calls: Arc<Mutex<Vec<Recorded>>>,
    /// Canned responses keyed by short action name (the part after the dot).
    routes: Arc<Mutex<std::collections::HashMap<String, (u16, serde_json::Value)>>>,
}

impl MockState {
    fn register(&self, action: &str, status: u16, body: serde_json::Value) {
        self.routes
            .lock()
            .unwrap()
            .insert(action.to_string(), (status, body));
    }
    fn calls(&self) -> Vec<Recorded> {
        self.calls.lock().unwrap().clone()
    }
    fn find(&self, action: &str) -> Recorded {
        self.calls()
            .into_iter()
            .find(|c| c.target.ends_with(action))
            .unwrap_or_else(|| {
                panic!(
                    "no call to {action}; got {:?}",
                    self.calls()
                        .iter()
                        .map(|c| c.target.clone())
                        .collect::<Vec<_>>()
                )
            })
    }
}

async fn handler(State(state): State<MockState>, headers: HeaderMap, body: Bytes) -> Response {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let target = get("x-amz-target");
    let action = target.rsplit('.').next().unwrap_or("").to_string();
    let body_json: serde_json::Value =
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    state.calls.lock().unwrap().push(Recorded {
        target: target.clone(),
        authorization: get("authorization"),
        content_sha256: get("x-amz-content-sha256"),
        body: body_json,
    });

    let routes = state.routes.lock().unwrap();
    if let Some((status, resp)) = routes.get(&action) {
        Response::builder()
            .status(StatusCode::from_u16(*status).unwrap())
            .header("content-type", "application/x-amz-json-1.1")
            .body(axum::body::Body::from(serde_json::to_vec(resp).unwrap()))
            .unwrap()
    } else {
        // Mimic Cognito error envelope on an unregistered action.
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::from(
                serde_json::json!({"__type": "InternalFailure"}).to_string(),
            ))
            .unwrap()
    }
}

async fn spawn() -> (String, MockState) {
    let state = MockState::default();
    let app = Router::new()
        .route("/", post(handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn adapter(base: &str) -> Adapter {
    Adapter::new(Config {
        base_url: base.to_string(),
        client_id: CLIENT_ID.into(),
        region: REGION.into(),
        user_pool_id: USER_POOL_ID.into(),
        access_key: "AKIDEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        ..Config::default()
    })
}

// --------------------------------------------------------------------------- //
// login — InitiateAuth USER_PASSWORD_AUTH → token + resolved user
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn login_initiate_auth_returns_token() {
    let (base, state) = spawn().await;
    state.register(
        "InitiateAuth",
        200,
        serde_json::json!({
            "AuthenticationResult": {
                "AccessToken": "ACCESS-CG",
                "RefreshToken": "REFRESH-CG",
                "ExpiresIn": 3600,
                "TokenType": "Bearer"
            }
        }),
    );
    state.register(
        "AdminGetUser",
        200,
        serde_json::json!({
            "Username": "alice",
            "UserAttributes": [
                {"Name": "email", "Value": "alice@example.com"},
                {"Name": "given_name", "Value": "Alice"},
                {"Name": "family_name", "Value": "Smith"}
            ],
            "Enabled": true
        }),
    );

    let result = adapter(&base).login_full("alice", "hunter2").await.unwrap();

    // (a) outbound: InitiateAuth with the right flow + params + target header.
    let auth_call = state.find("InitiateAuth");
    assert_eq!(
        auth_call.target,
        "AWSCognitoIdentityProviderService.InitiateAuth"
    );
    assert_eq!(auth_call.body["ClientId"], CLIENT_ID);
    assert_eq!(auth_call.body["AuthFlow"], "USER_PASSWORD_AUTH");
    assert_eq!(auth_call.body["AuthParameters"]["USERNAME"], "alice");
    assert_eq!(auth_call.body["AuthParameters"]["PASSWORD"], "hunter2");
    // No secret configured here, so no SECRET_HASH; InitiateAuth is unsigned.
    assert!(auth_call.body["AuthParameters"]
        .get("SECRET_HASH")
        .is_none());
    assert!(auth_call.authorization.is_empty());

    // (b) parsed: tokens.
    assert_eq!(result.token.access_token, "ACCESS-CG");
    assert_eq!(result.token.refresh_token, "REFRESH-CG");
    assert_eq!(result.token.expires_in, 3600);

    // (c) parsed: user resolved via AdminGetUser (which is SigV4-signed).
    assert_eq!(result.user.username, "alice");
    assert_eq!(result.user.email, "alice@example.com");
    let admin_call = state.find("AdminGetUser");
    assert!(
        admin_call
            .authorization
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"),
        "AdminGetUser must be SigV4-signed; got {:?}",
        admin_call.authorization
    );
    assert!(!admin_call.content_sha256.is_empty());
}

#[tokio::test]
async fn login_secret_hash_included_when_secret_configured() {
    let (base, state) = spawn().await;
    state.register(
        "InitiateAuth",
        200,
        serde_json::json!({"AuthenticationResult": {"AccessToken": "A", "ExpiresIn": 1}}),
    );
    state.register("AdminGetUser", 400, serde_json::json!({}));

    let a = Adapter::new(Config {
        base_url: base.clone(),
        client_id: CLIENT_ID.into(),
        client_secret: "test-secret".into(),
        region: REGION.into(),
        user_pool_id: USER_POOL_ID.into(),
        ..Config::default()
    });
    a.login_full("alice", "pw").await.unwrap();

    let call = state.find("InitiateAuth");
    let expected = a.secret_hash("alice").unwrap();
    assert_eq!(call.body["AuthParameters"]["SECRET_HASH"], expected);
}

#[tokio::test]
async fn login_error_maps_to_invalid_credentials() {
    let (base, state) = spawn().await;
    state.register(
        "InitiateAuth",
        400,
        serde_json::json!({"__type": "NotAuthorizedException"}),
    );

    let err = adapter(&base).login("alice", "wrong").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::InvalidCredentials);
}

#[tokio::test]
async fn login_missing_authentication_result_maps_to_invalid_credentials() {
    let (base, state) = spawn().await;
    state.register(
        "InitiateAuth",
        200,
        serde_json::json!({"ChallengeName": "NEW_PASSWORD_REQUIRED", "Session": "tok"}),
    );

    let err = adapter(&base).login("alice", "tmp").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::InvalidCredentials);
}

// --------------------------------------------------------------------------- //
// create_user — AdminCreateUser + AdminSetUserPassword (Permanent)
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn create_user_calls_admin_create_and_set_password() {
    let (base, state) = spawn().await;
    state.register(
        "AdminCreateUser",
        200,
        serde_json::json!({"User": {"Username": "bob", "Enabled": true}}),
    );
    state.register("AdminSetUserPassword", 200, serde_json::json!({}));

    let mut user = firefly_idp::User {
        username: "bob".into(),
        email: "bob@example.com".into(),
        ..Default::default()
    };
    user.attributes
        .insert("firstName".into(), serde_json::json!("Bob"));
    user.attributes
        .insert("lastName".into(), serde_json::json!("Jones"));

    let result = adapter(&base)
        .create_user(user, "Str0ng!Pass")
        .await
        .unwrap();

    let create = state.find("AdminCreateUser");
    assert_eq!(create.body["UserPoolId"], USER_POOL_ID);
    assert_eq!(create.body["Username"], "bob");
    assert_eq!(create.body["MessageAction"], "SUPPRESS");
    let attrs: std::collections::HashMap<String, String> = create.body["UserAttributes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["Name"].as_str().unwrap().to_string(),
                a["Value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        attrs.get("email").map(String::as_str),
        Some("bob@example.com")
    );
    assert_eq!(attrs.get("given_name").map(String::as_str), Some("Bob"));
    assert_eq!(attrs.get("family_name").map(String::as_str), Some("Jones"));
    // Both admin calls are SigV4-signed.
    assert!(create.authorization.starts_with("AWS4-HMAC-SHA256"));

    let pwd = state.find("AdminSetUserPassword");
    assert_eq!(pwd.body["UserPoolId"], USER_POOL_ID);
    assert_eq!(pwd.body["Username"], "bob");
    assert_eq!(pwd.body["Password"], "Str0ng!Pass");
    assert_eq!(pwd.body["Permanent"], true);

    assert_eq!(result.id, "bob");
    assert_eq!(result.username, "bob");
}

// --------------------------------------------------------------------------- //
// get_user / find_by_username — AdminGetUser → user
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_user_parses_cognito_attributes() {
    let (base, state) = spawn().await;
    state.register(
        "AdminGetUser",
        200,
        serde_json::json!({
            "Username": "carol",
            "UserAttributes": [
                {"Name": "email", "Value": "carol@example.com"},
                {"Name": "given_name", "Value": "Carol"},
                {"Name": "family_name", "Value": "Lee"},
                {"Name": "email_verified", "Value": "true"}
            ],
            "Enabled": true
        }),
    );

    let user = adapter(&base).get_user("carol").await.unwrap();
    let call = state.find("AdminGetUser");
    assert_eq!(call.body["UserPoolId"], USER_POOL_ID);
    assert_eq!(call.body["Username"], "carol");

    assert_eq!(user.id, "carol");
    assert_eq!(user.username, "carol");
    assert_eq!(user.email, "carol@example.com");
    assert_eq!(
        user.attributes.get("given_name").and_then(|v| v.as_str()),
        Some("Carol")
    );
    assert!(user.enabled);
}

#[tokio::test]
async fn get_user_error_maps_to_user_not_found() {
    let (base, state) = spawn().await;
    state.register(
        "AdminGetUser",
        400,
        serde_json::json!({"__type": "UserNotFoundException"}),
    );

    let err = adapter(&base).get_user("nonexistent").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::UserNotFound);
}

#[tokio::test]
async fn find_by_username_delegates_to_get_user() {
    let (base, state) = spawn().await;
    state.register(
        "AdminGetUser",
        200,
        serde_json::json!({
            "Username": "dave",
            "UserAttributes": [{"Name": "email", "Value": "dave@example.com"}],
            "Enabled": true
        }),
    );

    let user = adapter(&base).find_by_username("dave").await.unwrap();
    let call = state.find("AdminGetUser");
    assert_eq!(call.body["Username"], "dave");
    assert_eq!(user.username, "dave");
}

// --------------------------------------------------------------------------- //
// assign_role / revoke_role — group membership
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn assign_role_calls_admin_add_user_to_group() {
    let (base, state) = spawn().await;
    state.register("AdminAddUserToGroup", 200, serde_json::json!({}));

    let ok = adapter(&base).assign_role("alice", "admins").await.unwrap();
    assert!(ok);
    let call = state.find("AdminAddUserToGroup");
    assert_eq!(call.body["UserPoolId"], USER_POOL_ID);
    assert_eq!(call.body["Username"], "alice");
    assert_eq!(call.body["GroupName"], "admins");
}

#[tokio::test]
async fn revoke_role_calls_admin_remove_user_from_group() {
    let (base, state) = spawn().await;
    state.register("AdminRemoveUserFromGroup", 200, serde_json::json!({}));

    let ok = adapter(&base).revoke_role("alice", "admins").await.unwrap();
    assert!(ok);
    let call = state.find("AdminRemoveUserFromGroup");
    assert_eq!(call.body["GroupName"], "admins");
}

// --------------------------------------------------------------------------- //
// get_user_info — GetUser(AccessToken) → user; error → UserNotFound
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_user_info_calls_get_user_with_access_token() {
    let (base, state) = spawn().await;
    state.register(
        "GetUser",
        200,
        serde_json::json!({
            "Username": "frank",
            "UserAttributes": [
                {"Name": "email", "Value": "frank@example.com"},
                {"Name": "given_name", "Value": "Frank"}
            ],
            "Enabled": true
        }),
    );

    let user = adapter(&base)
        .get_user_info("ACCESS-TOKEN-XYZ")
        .await
        .unwrap();
    let call = state.find("GetUser");
    assert_eq!(call.body["AccessToken"], "ACCESS-TOKEN-XYZ");
    // GetUser is a client-flow call: unsigned.
    assert!(call.authorization.is_empty());

    assert_eq!(user.username, "frank");
    assert_eq!(user.email, "frank@example.com");
}

#[tokio::test]
async fn get_user_info_error_maps_to_user_not_found() {
    let (base, state) = spawn().await;
    state.register(
        "GetUser",
        400,
        serde_json::json!({"__type": "NotAuthorizedException"}),
    );

    let err = adapter(&base).get_user_info("BAD-TOKEN").await.unwrap_err();
    assert_eq!(err, firefly_idp::Error::UserNotFound);
}

#[tokio::test]
async fn introspect_active_and_inactive() {
    let (base, state) = spawn().await;
    state.register("GetUser", 200, serde_json::json!({"Username": "frank"}));
    let active = adapter(&base).introspect("TOK").await.unwrap();
    assert!(active.active);
    assert_eq!(active.username, "frank");
    assert_eq!(active.user_id, "frank");

    let (base2, state2) = spawn().await;
    state2.register("GetUser", 400, serde_json::json!({}));
    let inactive = adapter(&base2).introspect("BAD").await.unwrap();
    assert!(!inactive.active);
}

#[tokio::test]
async fn logout_returns_true_on_success() {
    let (base, state) = spawn().await;
    state.register("GlobalSignOut", 200, serde_json::json!({}));
    assert!(adapter(&base).logout("ACCESS").await.unwrap());
    let call = state.find("GlobalSignOut");
    assert_eq!(call.body["AccessToken"], "ACCESS");
    assert!(call.authorization.is_empty());
}

// --------------------------------------------------------------------------- //
// refresh — REFRESH_TOKEN_AUTH; keeps supplied token when not rotated
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn refresh_keeps_supplied_token_when_not_rotated() {
    let (base, state) = spawn().await;
    state.register(
        "InitiateAuth",
        200,
        serde_json::json!({"AuthenticationResult": {"AccessToken": "NEW-ACCESS", "ExpiresIn": 900}}),
    );

    let token = adapter(&base).refresh("OLD-REFRESH").await.unwrap();
    assert_eq!(token.access_token, "NEW-ACCESS");
    assert_eq!(token.refresh_token, "OLD-REFRESH");

    let call = state.find("InitiateAuth");
    assert_eq!(call.body["AuthFlow"], "REFRESH_TOKEN_AUTH");
    assert_eq!(call.body["AuthParameters"]["REFRESH_TOKEN"], "OLD-REFRESH");
}

// --------------------------------------------------------------------------- //
// register_user — forces enabled, delegates to create_user
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn register_user_forces_enabled_and_creates() {
    let (base, state) = spawn().await;
    state.register(
        "AdminCreateUser",
        200,
        serde_json::json!({"User": {"Username": "grace"}}),
    );
    state.register("AdminSetUserPassword", 200, serde_json::json!({}));

    let user = firefly_idp::User {
        username: "grace".into(),
        email: "grace@example.com".into(),
        enabled: false,
        ..Default::default()
    };
    let result = adapter(&base)
        .register_user(user, "S3cur3!Pass")
        .await
        .unwrap();
    let create = state.find("AdminCreateUser");
    assert_eq!(create.body["Username"], "grace");
    assert_eq!(result.id, "grace");
}

// --------------------------------------------------------------------------- //
// get_roles — AdminListGroupsForUser → roles; error → empty
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn get_roles_calls_admin_list_groups_for_user() {
    let (base, state) = spawn().await;
    state.register(
        "AdminListGroupsForUser",
        200,
        serde_json::json!({
            "Groups": [
                {"GroupName": "admins", "Description": "Admin group"},
                {"GroupName": "editors", "Description": ""}
            ]
        }),
    );

    let roles = adapter(&base).get_roles("alice").await.unwrap();
    let call = state.find("AdminListGroupsForUser");
    assert_eq!(call.body["UserPoolId"], USER_POOL_ID);
    assert_eq!(call.body["Username"], "alice");

    assert_eq!(roles.len(), 2);
    let names: std::collections::HashSet<_> = roles.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains("admins"));
    assert!(names.contains("editors"));
}

#[tokio::test]
async fn get_roles_returns_empty_on_error() {
    let (base, state) = spawn().await;
    state.register(
        "AdminListGroupsForUser",
        400,
        serde_json::json!({"__type": "UserNotFoundException"}),
    );
    let roles = adapter(&base).get_roles("nonexistent").await.unwrap();
    assert!(roles.is_empty());
}

#[tokio::test]
async fn list_roles_parses_groups() {
    let (base, state) = spawn().await;
    state.register(
        "ListGroups",
        200,
        serde_json::json!({"Groups": [{"GroupName": "admins", "Description": "Admins"}]}),
    );
    let roles = adapter(&base).list_roles().await.unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].name, "admins");
    assert_eq!(roles[0].description, "Admins");
    // ListGroups is an admin call, SigV4-signed.
    let call = state.find("ListGroups");
    assert!(call.authorization.starts_with("AWS4-HMAC-SHA256"));
}

// --------------------------------------------------------------------------- //
// list_users — ListUsers → users
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn list_users_parses_users() {
    let (base, state) = spawn().await;
    state.register(
        "ListUsers",
        200,
        serde_json::json!({
            "Users": [
                {"Username": "u1", "Attributes": [{"Name": "email", "Value": "u1@x.io"}], "Enabled": true}
            ]
        }),
    );
    let users = adapter(&base).list_users(10).await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].username, "u1");
    assert_eq!(users[0].email, "u1@x.io");
    let call = state.find("ListUsers");
    assert_eq!(call.body["Limit"], 10);
}

// --------------------------------------------------------------------------- //
// MFA sentinel; name
// --------------------------------------------------------------------------- //
#[tokio::test]
async fn mfa_methods_return_sentinel() {
    let (base, _state) = spawn().await;
    let a = adapter(&base);
    assert_eq!(
        a.mfa_challenge("u1").await.unwrap_err(),
        firefly_idp_aws_cognito::not_implemented()
    );
    assert_eq!(
        a.mfa_verify("c1", "000000").await.unwrap_err(),
        firefly_idp_aws_cognito::not_implemented()
    );
    assert_eq!(
        firefly_idp_aws_cognito::ERR_NOT_IMPLEMENTED,
        "firefly/idpawscognito: not yet implemented"
    );
}

#[test]
fn name_is_aws_cognito() {
    let a = Adapter::new(Config::default());
    assert_eq!(a.name(), "aws-cognito");
}
