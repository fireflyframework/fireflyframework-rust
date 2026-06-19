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

//! Port of pyfly's `tests/security/test_oauth2_pkce.py` plus the
//! login-flow paths of `oauth2/login.py`, against an in-process axum
//! OAuth2 provider mock (token + userinfo + JWKS endpoints).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::Request;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use firefly_security::oauth2::{
    pkce_challenge, ClientRegistration, FixedLoginSessionStore,
    InMemoryClientRegistrationRepository, LoginSession, OAuth2LoginHandler, SESSION_KEY_ID_TOKEN,
    SESSION_KEY_NONCE, SESSION_KEY_PKCE_VERIFIER, SESSION_KEY_REGISTRATION_ID,
    SESSION_KEY_SECURITY_CONTEXT, SESSION_KEY_STATE,
};
use http::{header, Method, StatusCode};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

/// Same static RSA test key as `jwks_test.rs` (tests only).
const TEST_RSA_PEM: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----
MIIEpQIBAAKCAQEArjIEmi7GzGJBUZL9oGQKi0P5BQG9nHUrvHz/A+rg0cbGPyL6
l9uZ3t3RnWHkf8QSaGTAZS1UNJ3aMhByKDudT6VdHaOjqKc6tEBmYDKeG7Mxk1UG
Nsmaz3W9Io4HR2X3G9bzM4Gv48uMNMnl13vYYonl4Saut57Wn5qZjxGmkBxrA9Wx
ee1qZXZrVp3n0ZU+OxylAQJvhszwQdi/6uXyXJUUTtW2mdb9fmpymF6r7svAP5/C
bPXPuI4bal7MlQqWSpWFebTZ9ewvlizTMGY+wCu98UfonQqy/U1wv0UngNDoIfqN
qz+GV1lFwIO9nhq5g2vU7L2+NUO3FFRtDsoLfwIDAQABAoIBAEr8lSaaRFHvahbn
o+7Log5ZcHVLTohvmChH1q+lCKrFWsoLEL0Wd6KM8pNBdM/bY+E0ne3wGXOdEDTF
B59yKkIC+ZasvuL3OjomDuwSXiWmegzmaQpktxPfp0+cvF1r83g0i/T8Ou9gzDZd
Q2gDlB63JhJKSKQa6GFEeB4yhvU50IXSc1Y8gV7TLlMp2vXq5FDP33y+IExS3ndc
TI7ThIRJ/GRotopnM12kkME+IgxXwJeLru5KIJhuz8kC0hg+cxUFsz+iYaa4uDQO
L6ONJSG2ezYbbFGsDtIB8ju8zti+iirb9FrwC3HXUPuDIl/e3KnSnh0MRvaknSiL
gXX6HOECgYEA3S5v25IDgmNjeLWY366uVFVcnsNxfwO+DjWJBRM5aUC7JICYYVMs
h64ss3q3Qsxm9x+YnAkvFuh1nKHwQ72C4xJPQOPiuYr3Z0iIZsG2phfW/Pmaym8D
UysRkMtbEkhK6UWCaLnZbGdQUrh1hAMLFGoPr1PKJFUOTbwWQA6yJEsCgYEAyZ4O
Bgdrq5SP6BTpwPXzMqzDQIb6xV6NIjNE13O/q3vCs2LmhWnSTD8E4mE2R9PwvKU7
5xta28eSRLXAbYTgtJqYS7k7RCjyirNuvl/VX/j8U6xM8jb0UGmoCVoZGY/zClrD
I+aBl7AzUXMX3OB8PVIiZkfzvwRdKHjeFJh4bR0CgYEAjmMso3+WPsRY7waJCcbc
d3IUlChh0lDIc0FHmjrMBNQlJdSbRFxVGGuqX0iq3ZfU2VY/2oOXCvpPbKxbjmBb
+G57Et0hwiySJK1vEie2u6oxPt45JgTdcRcS0dH4KQbdItsanuy16bGA5h/Vl0yW
P2gf/NDGGymecbCZ6lcLm40CgYEAinfkxbs+9V5Y31nNmNrSJlGE38JUZE0lvQFd
HGPAlbOv6qfYDnS5G+iEID4Hm5kx0z3gQD8HTb5o9IunFxCVizRJuGgFDjDZMu08
9761uu4zzfud9RRNAxUtdQ7OAkJc9xWSxAtBob4/4IadMvNyIGNSgNCV1PDYUj2A
uMBmpPkCgYEAss81ftA75DpKa3eKC6Ye8jPrRDkwmvu2oBw9izCuxmO4mAjQ48Z/
FpOZGCoJOe3cEbULWgqCS5Mn043RZRIQmSZ07vmvLZTx9ztnGjuueMv3ZraPMuLZ
ZW0eaw0u6dIaSFDQ7rMiy1eFFLKCDlAa4J3/RXikSbu/zvDHiDVwqkQ=
-----END RSA PRIVATE KEY-----
";

const TEST_RSA_N: &str = "rjIEmi7GzGJBUZL9oGQKi0P5BQG9nHUrvHz_A-rg0cbGPyL6l9uZ3t3RnWHkf8QSaGTAZS1UNJ3aMhByKDudT6VdHaOjqKc6tEBmYDKeG7Mxk1UGNsmaz3W9Io4HR2X3G9bzM4Gv48uMNMnl13vYYonl4Saut57Wn5qZjxGmkBxrA9Wxee1qZXZrVp3n0ZU-OxylAQJvhszwQdi_6uXyXJUUTtW2mdb9fmpymF6r7svAP5_CbPXPuI4bal7MlQqWSpWFebTZ9ewvlizTMGY-wCu98UfonQqy_U1wv0UngNDoIfqNqz-GV1lFwIO9nhq5g2vU7L2-NUO3FFRtDsoLfw";

/// What the mock provider should answer and what it captured.
#[derive(Default)]
struct ProviderState {
    /// Form fields the token endpoint received.
    token_form: Option<HashMap<String, String>>,
    /// Extra members merged into the token response (e.g. id_token).
    token_extra: serde_json::Map<String, Value>,
    /// When true, the token endpoint answers 500.
    fail_token: bool,
    /// The userinfo payload.
    user_info: Value,
}

/// Spawns the OAuth2 provider mock; returns its base URL and shared
/// state.
async fn spawn_provider(user_info: Value) -> (String, Arc<Mutex<ProviderState>>) {
    let state = Arc::new(Mutex::new(ProviderState {
        user_info,
        ..ProviderState::default()
    }));

    let token_state = Arc::clone(&state);
    let userinfo_state = Arc::clone(&state);
    let app = Router::new()
        .route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let state = Arc::clone(&token_state);
                async move {
                    let mut guard = state.lock().unwrap();
                    guard.token_form = Some(form);
                    if guard.fail_token {
                        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
                    }
                    let mut body = serde_json::Map::new();
                    body.insert("access_token".into(), json!("AT-123"));
                    body.insert("token_type".into(), json!("Bearer"));
                    for (k, v) in &guard.token_extra {
                        body.insert(k.clone(), v.clone());
                    }
                    (StatusCode::OK, Json(Value::Object(body)))
                }
            }),
        )
        .route(
            "/userinfo",
            get(move |headers: http::HeaderMap| {
                let state = Arc::clone(&userinfo_state);
                async move {
                    let authorized = headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        == Some("Bearer AT-123");
                    if !authorized {
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    let body = state.lock().unwrap().user_info.clone();
                    (StatusCode::OK, Json(body))
                }
            }),
        )
        .route(
            "/jwks",
            get(|| async {
                Json(json!({
                    "keys": [{"kty": "RSA", "kid": "idp-kid", "use": "sig",
                              "alg": "RS256", "n": TEST_RSA_N, "e": "AQAB"}]
                }))
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn registration(provider_base: &str, use_pkce: bool) -> ClientRegistration {
    ClientRegistration::new("acme", "cid")
        .client_secret("secret")
        .redirect_uri("https://app/cb")
        .scopes(&["openid"])
        .authorization_uri("https://idp/auth")
        .token_uri(format!("{provider_base}/token"))
        .user_info_uri(format!("{provider_base}/userinfo"))
        .use_pkce(use_pkce)
}

/// Builds the login router + the shared session store for assertions.
fn login_app(reg: ClientRegistration) -> (Router, Arc<FixedLoginSessionStore>) {
    let sessions = Arc::new(FixedLoginSessionStore::new());
    let repo = Arc::new(InMemoryClientRegistrationRepository::new([reg]));
    let handler = OAuth2LoginHandler::new(repo, sessions.clone());
    (handler.router(), sessions)
}

fn get_req(uri: &str) -> Request {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Parses `k=v&k2=v2` query strings (percent + plus decoding).
fn parse_query(query: &str) -> HashMap<String, String> {
    serde_urlencoded_lite(query)
}

fn serde_urlencoded_lite(query: &str) -> HashMap<String, String> {
    fn decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                    if let Ok(b) = u8::from_str_radix(hex, 16) {
                        out.push(b);
                        i += 2;
                    } else {
                        out.push(bytes[i]);
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// ---------------------------------------------------------------------------
// Authorization redirect (pyfly: test_authorization_adds_pkce_challenge…)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn authorization_adds_pkce_challenge_when_enabled() {
    let (app, sessions) = login_app(registration("http://unused", true));

    let resp = app
        .oneshot(get_req("/oauth2/authorization/acme"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let (base, query) = location.split_once('?').unwrap();
    assert_eq!(base, "https://idp/auth");
    let query = parse_query(query);

    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "cid");
    assert_eq!(query["redirect_uri"], "https://app/cb");
    assert_eq!(query["scope"], "openid");
    assert_eq!(query["code_challenge_method"], "S256");

    let session = sessions.session();
    // One-time verifier stashed in the session; challenge matches it.
    let verifier = session
        .get_attribute(SESSION_KEY_PKCE_VERIFIER)
        .await
        .expect("verifier stashed");
    assert_eq!(query["code_challenge"], s256(&verifier));
    // state + nonce stashed and sent.
    assert_eq!(
        session.get_attribute(SESSION_KEY_STATE).await.as_deref(),
        Some(query["state"].as_str())
    );
    assert_eq!(
        session.get_attribute(SESSION_KEY_NONCE).await.as_deref(),
        Some(query["nonce"].as_str())
    );
    assert_eq!(query["code_challenge"], pkce_challenge(&verifier));
}

// Ported from pyfly: test_authorization_omits_pkce_when_disabled
#[tokio::test]
async fn authorization_omits_pkce_when_disabled() {
    let (app, sessions) = login_app(registration("http://unused", false));

    let resp = app
        .oneshot(get_req("/oauth2/authorization/acme"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let query = parse_query(location.split_once('?').unwrap().1);

    assert!(!query.contains_key("code_challenge"));
    assert_eq!(
        sessions
            .session()
            .get_attribute(SESSION_KEY_PKCE_VERIFIER)
            .await,
        None
    );
}

#[tokio::test]
async fn authorization_unknown_registration_is_400() {
    let (app, _) = login_app(registration("http://unused", false));
    let resp = app
        .oneshot(get_req("/oauth2/authorization/nope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "unknown_registration");
}

// ---------------------------------------------------------------------------
// Callback flow (pyfly: test_exchange_code_sends_verifier + login flow)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_login_flow_exchanges_code_with_pkce_and_stores_security_context() {
    let (provider, provider_state) =
        spawn_provider(json!({"sub": "user-9", "name": "Alice", "email": "a@example.com"})).await;
    let (app, sessions) = login_app(registration(&provider, true));
    let session = sessions.session();

    // Step 1: authorization redirect (stashes state + verifier).
    let resp = app
        .clone()
        .oneshot(get_req("/oauth2/authorization/acme"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    let state_param = session.get_attribute(SESSION_KEY_STATE).await.unwrap();
    let verifier = session
        .get_attribute(SESSION_KEY_PKCE_VERIFIER)
        .await
        .unwrap();
    let pre_login_id = session.id().await;

    // Step 2: provider callback with code + state.
    let resp = app
        .oneshot(get_req(&format!(
            "/login/oauth2/code/acme?code=the-code&state={state_param}"
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()[header::LOCATION], "/");

    // The token endpoint received the authorization_code exchange with
    // the PKCE verifier (pyfly: test_exchange_code_sends_verifier).
    let form = provider_state.lock().unwrap().token_form.clone().unwrap();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "the-code");
    assert_eq!(form["redirect_uri"], "https://app/cb");
    assert_eq!(form["client_id"], "cid");
    assert_eq!(form["client_secret"], "secret");
    assert_eq!(form["code_verifier"], verifier);

    // Security context stored from userinfo; one-time values consumed;
    // session id rotated against fixation.
    let ctx: Value = serde_json::from_str(
        &session
            .get_attribute(SESSION_KEY_SECURITY_CONTEXT)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["principal"], "user-9");
    assert_eq!(ctx["claims"]["email"], "a@example.com");
    assert_eq!(session.get_attribute(SESSION_KEY_STATE).await, None);
    assert_eq!(session.get_attribute(SESSION_KEY_PKCE_VERIFIER).await, None);
    assert_ne!(session.id().await, pre_login_id, "session id rotated");
}

#[tokio::test]
async fn callback_rejects_state_mismatch() {
    let (provider, _) = spawn_provider(json!({"sub": "u"})).await;
    let (app, sessions) = login_app(registration(&provider, false));
    sessions
        .session()
        .set_attribute(SESSION_KEY_STATE, "expected".into())
        .await;

    let resp = app
        .oneshot(get_req("/login/oauth2/code/acme?code=c&state=tampered"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "invalid_state");
    assert_eq!(body["message"], "OAuth2 state parameter mismatch");
}

#[tokio::test]
async fn callback_without_stored_state_is_rejected() {
    let (provider, _) = spawn_provider(json!({"sub": "u"})).await;
    let (app, _) = login_app(registration(&provider, false));

    let resp = app
        .oneshot(get_req("/login/oauth2/code/acme?code=c&state=anything"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(resp).await["error"], "invalid_state");
}

#[tokio::test]
async fn callback_propagates_provider_error() {
    let (provider, _) = spawn_provider(json!({"sub": "u"})).await;
    let (app, sessions) = login_app(registration(&provider, false));
    sessions
        .session()
        .set_attribute(SESSION_KEY_STATE, "st".into())
        .await;

    let resp = app
        .oneshot(get_req(
            "/login/oauth2/code/acme?state=st&error=access_denied&error_description=user+said+no",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "access_denied");
    assert_eq!(body["message"], "user said no");
}

#[tokio::test]
async fn callback_requires_code() {
    let (provider, _) = spawn_provider(json!({"sub": "u"})).await;
    let (app, sessions) = login_app(registration(&provider, false));
    sessions
        .session()
        .set_attribute(SESSION_KEY_STATE, "st".into())
        .await;

    let resp = app
        .oneshot(get_req("/login/oauth2/code/acme?state=st"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "missing_code");
    assert_eq!(body["message"], "No authorization code in callback");
}

#[tokio::test]
async fn callback_returns_502_when_token_exchange_fails() {
    let (provider, provider_state) = spawn_provider(json!({"sub": "u"})).await;
    provider_state.lock().unwrap().fail_token = true;
    let (app, sessions) = login_app(registration(&provider, false));
    sessions
        .session()
        .set_attribute(SESSION_KEY_STATE, "st".into())
        .await;

    let resp = app
        .oneshot(get_req("/login/oauth2/code/acme?code=c&state=st"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "token_exchange_failed");
}

#[tokio::test]
async fn callback_rejects_login_without_principal() {
    // userinfo without sub/id/login → 401 login_failed (audit #49).
    let (provider, _) = spawn_provider(json!({"name": "Ghost"})).await;
    let (app, sessions) = login_app(registration(&provider, false));
    sessions
        .session()
        .set_attribute(SESSION_KEY_STATE, "st".into())
        .await;

    let resp = app
        .oneshot(get_req("/login/oauth2/code/acme?code=c&state=st"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], "login_failed");
}

// ---------------------------------------------------------------------------
// OIDC id_token path (validated against the provider JWKS)
// ---------------------------------------------------------------------------

fn make_id_token(claims: Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("idp-kid".to_string());
    jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(TEST_RSA_PEM).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn callback_prefers_verified_id_token_claims() {
    let (provider, provider_state) = spawn_provider(json!({"sub": "userinfo-user"})).await;
    let reg = registration(&provider, false).jwks_uri(format!("{provider}/jwks"));
    let (app, sessions) = login_app(reg);
    let session = sessions.session();

    // Authorization leg stashes state + nonce.
    app.clone()
        .oneshot(get_req("/oauth2/authorization/acme"))
        .await
        .unwrap();
    let state_param = session.get_attribute(SESSION_KEY_STATE).await.unwrap();
    let nonce = session.get_attribute(SESSION_KEY_NONCE).await.unwrap();

    // Provider returns an id_token carrying the nonce, aud, and roles.
    let id_token = make_id_token(json!({
        "sub": "oidc-user",
        "aud": "cid",
        "nonce": nonce,
        "exp": 9999999999u64,
        "realm_access": {"roles": ["realm-admin"]},
    }));
    provider_state
        .lock()
        .unwrap()
        .token_extra
        .insert("id_token".into(), json!(id_token));

    let resp = app
        .oneshot(get_req(&format!(
            "/login/oauth2/code/acme?code=c&state={state_param}"
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);

    let ctx: Value = serde_json::from_str(
        &session
            .get_attribute(SESSION_KEY_SECURITY_CONTEXT)
            .await
            .unwrap(),
    )
    .unwrap();
    // Identity comes from the *verified* id_token, not userinfo.
    assert_eq!(ctx["principal"], "oidc-user");
    assert_eq!(ctx["roles"], json!(["realm-admin"]));
}

#[tokio::test]
async fn callback_rejects_id_token_with_wrong_nonce() {
    let (provider, provider_state) = spawn_provider(json!({"sub": "userinfo-user"})).await;
    let reg = registration(&provider, false).jwks_uri(format!("{provider}/jwks"));
    let (app, sessions) = login_app(reg);
    let session = sessions.session();

    app.clone()
        .oneshot(get_req("/oauth2/authorization/acme"))
        .await
        .unwrap();
    let state_param = session.get_attribute(SESSION_KEY_STATE).await.unwrap();

    let id_token = make_id_token(json!({
        "sub": "oidc-user",
        "aud": "cid",
        "nonce": "attacker-nonce",
        "exp": 9999999999u64,
    }));
    provider_state
        .lock()
        .unwrap()
        .token_extra
        .insert("id_token".into(), json!(id_token));

    let resp = app
        .oneshot(get_req(&format!(
            "/login/oauth2/code/acme?code=c&state={state_param}"
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], "invalid_id_token");
}

// H6: an id_token present in the token response must NEVER be trusted without
// validation. With no JWKS configured we cannot validate it, so the login must
// fail rather than silently fall through to the (unverified) userinfo identity.
#[tokio::test]
async fn callback_rejects_id_token_when_no_jwks_configured() {
    let (provider, provider_state) = spawn_provider(json!({"sub": "userinfo-user"})).await;
    let reg = registration(&provider, false); // deliberately no jwks_uri
    let (app, sessions) = login_app(reg);
    let session = sessions.session();

    app.clone()
        .oneshot(get_req("/oauth2/authorization/acme"))
        .await
        .unwrap();
    let state_param = session.get_attribute(SESSION_KEY_STATE).await.unwrap();

    let id_token = make_id_token(json!({
        "sub": "oidc-user",
        "aud": "cid",
        "exp": 9999999999u64,
    }));
    provider_state
        .lock()
        .unwrap()
        .token_extra
        .insert("id_token".into(), json!(id_token));

    let resp = app
        .oneshot(get_req(&format!(
            "/login/oauth2/code/acme?code=c&state={state_param}"
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"], "invalid_id_token");
}

// ---------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logout_invalidates_session_and_redirects() {
    let (app, sessions) = login_app(registration("http://unused", false));
    let session = sessions.session();
    session
        .set_attribute(SESSION_KEY_SECURITY_CONTEXT, "{}".into())
        .await;

    let req = Request::builder()
        .method(Method::POST)
        .uri("/logout")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(resp.headers()[header::LOCATION], "/");
    assert_eq!(
        session.get_attribute(SESSION_KEY_SECURITY_CONTEXT).await,
        None
    );
}

#[tokio::test]
async fn logout_redirects_to_oidc_end_session_endpoint() {
    // A provider that advertises RP-initiated logout.
    let reg = registration("http://unused", false).end_session_endpoint("https://idp/logout");
    let (app, sessions) = login_app(reg);
    let session = sessions.session();
    session
        .set_attribute(SESSION_KEY_SECURITY_CONTEXT, "{}".into())
        .await;
    session
        .set_attribute(SESSION_KEY_REGISTRATION_ID, "acme".into())
        .await;
    session
        .set_attribute(SESSION_KEY_ID_TOKEN, "the-id-token".into())
        .await;

    let req = Request::builder()
        .method(Method::POST)
        .uri("/logout")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    // Redirected to the provider's end_session_endpoint with the id_token hint.
    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp.headers()[header::LOCATION].to_str().unwrap();
    assert!(
        location.starts_with("https://idp/logout?"),
        "location: {location}"
    );
    assert!(
        location.contains("id_token_hint=the-id-token"),
        "{location}"
    );
    assert!(location.contains("client_id=cid"), "{location}");
    // Local session still invalidated.
    assert_eq!(
        session.get_attribute(SESSION_KEY_SECURITY_CONTEXT).await,
        None
    );
}
