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

//! Port of pyfly's `tests/security/test_oauth2_client.py` and
//! `tests/security/test_authorization_server.py`.

use std::sync::Arc;

use firefly_security::oauth2::{
    github, google, keycloak, AuthorizationServer, ClientRegistration,
    InMemoryClientRegistrationRepository, InMemoryTokenStore, TokenRequest, TokenStore,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// ClientRegistration (pyfly: TestClientRegistration)
// ---------------------------------------------------------------------------

#[test]
fn client_registration_creation_stores_all_fields() {
    let reg = ClientRegistration::new("my-provider", "my-client-id")
        .client_secret("my-secret")
        .authorization_grant_type("authorization_code")
        .redirect_uri("https://app.example.com/callback")
        .scopes(&["openid", "profile"])
        .authorization_uri("https://auth.example.com/authorize")
        .token_uri("https://auth.example.com/token")
        .user_info_uri("https://auth.example.com/userinfo")
        .jwks_uri("https://auth.example.com/.well-known/jwks.json")
        .issuer_uri("https://auth.example.com")
        .provider_name("MyProvider");

    assert_eq!(reg.registration_id, "my-provider");
    assert_eq!(reg.client_id, "my-client-id");
    assert_eq!(reg.client_secret, "my-secret");
    assert_eq!(reg.authorization_grant_type, "authorization_code");
    assert_eq!(reg.redirect_uri, "https://app.example.com/callback");
    assert_eq!(reg.scopes, vec!["openid", "profile"]);
    assert_eq!(reg.authorization_uri, "https://auth.example.com/authorize");
    assert_eq!(reg.token_uri, "https://auth.example.com/token");
    assert_eq!(reg.user_info_uri, "https://auth.example.com/userinfo");
    assert_eq!(
        reg.jwks_uri,
        "https://auth.example.com/.well-known/jwks.json"
    );
    assert_eq!(reg.issuer_uri, "https://auth.example.com");
    assert_eq!(reg.provider_name, "MyProvider");
    assert!(!reg.use_pkce);
}

#[test]
fn client_registration_defaults() {
    let reg = ClientRegistration::new("minimal", "cid");
    assert_eq!(reg.registration_id, "minimal");
    assert_eq!(reg.client_id, "cid");
    assert_eq!(reg.client_secret, "");
    assert_eq!(reg.authorization_grant_type, "authorization_code");
    assert_eq!(reg.redirect_uri, "");
    assert!(reg.scopes.is_empty());
    assert_eq!(reg.authorization_uri, "");
    assert_eq!(reg.token_uri, "");
    assert_eq!(reg.user_info_uri, "");
    assert_eq!(reg.jwks_uri, "");
    assert_eq!(reg.issuer_uri, "");
    assert_eq!(reg.provider_name, "");
}

// ---------------------------------------------------------------------------
// Provider presets (pyfly: TestProviderFactories)
// ---------------------------------------------------------------------------

#[test]
fn google_preset() {
    let reg = google("gid", "gsecret", "");
    assert_eq!(reg.registration_id, "google");
    assert_eq!(reg.client_id, "gid");
    assert_eq!(reg.client_secret, "gsecret");
    assert_eq!(reg.authorization_grant_type, "authorization_code");
    assert_eq!(reg.redirect_uri, "");
    assert_eq!(reg.scopes, vec!["openid", "profile", "email"]);
    assert_eq!(
        reg.authorization_uri,
        "https://accounts.google.com/o/oauth2/v2/auth"
    );
    assert_eq!(reg.token_uri, "https://oauth2.googleapis.com/token");
    assert_eq!(
        reg.user_info_uri,
        "https://www.googleapis.com/oauth2/v3/userinfo"
    );
    assert_eq!(reg.jwks_uri, "https://www.googleapis.com/oauth2/v3/certs");
    assert_eq!(reg.issuer_uri, "https://accounts.google.com");
    assert_eq!(reg.provider_name, "Google");
}

#[test]
fn github_preset() {
    let reg = github("ghid", "ghsecret", "");
    assert_eq!(reg.registration_id, "github");
    assert_eq!(reg.client_id, "ghid");
    assert_eq!(reg.client_secret, "ghsecret");
    assert_eq!(reg.authorization_grant_type, "authorization_code");
    assert_eq!(reg.scopes, vec!["read:user", "user:email"]);
    assert_eq!(
        reg.authorization_uri,
        "https://github.com/login/oauth/authorize"
    );
    assert_eq!(reg.token_uri, "https://github.com/login/oauth/access_token");
    assert_eq!(reg.user_info_uri, "https://api.github.com/user");
    assert_eq!(reg.jwks_uri, "");
    assert_eq!(reg.provider_name, "GitHub");
}

#[test]
fn keycloak_preset_derives_oidc_uris_from_issuer() {
    let reg = keycloak("kcid", "kcsecret", "https://kc.example.com/realms/test", "");
    assert_eq!(reg.registration_id, "keycloak");
    assert_eq!(reg.client_id, "kcid");
    assert_eq!(reg.client_secret, "kcsecret");
    assert_eq!(reg.scopes, vec!["openid", "profile", "email"]);
    assert_eq!(reg.issuer_uri, "https://kc.example.com/realms/test");
    assert_eq!(reg.provider_name, "Keycloak");

    let base = "https://kc.example.com/realms/test";
    assert_eq!(
        reg.authorization_uri,
        format!("{base}/protocol/openid-connect/auth")
    );
    assert_eq!(
        reg.token_uri,
        format!("{base}/protocol/openid-connect/token")
    );
    assert_eq!(
        reg.user_info_uri,
        format!("{base}/protocol/openid-connect/userinfo")
    );
    assert_eq!(
        reg.jwks_uri,
        format!("{base}/protocol/openid-connect/certs")
    );
}

// ---------------------------------------------------------------------------
// InMemoryClientRegistrationRepository (pyfly:
// TestInMemoryClientRegistrationRepository)
// ---------------------------------------------------------------------------

#[test]
fn repository_find_add_and_list() {
    let g = google("gid", "gs", "");
    let gh = github("ghid", "ghs", "");
    let repo = InMemoryClientRegistrationRepository::new([g.clone(), gh.clone()]);

    use firefly_security::oauth2::ClientRegistrationRepository;
    assert_eq!(repo.find_by_registration_id("google"), Some(g));
    assert_eq!(repo.find_by_registration_id("github"), Some(gh));
    assert_eq!(repo.find_by_registration_id("nonexistent"), None);

    let empty = InMemoryClientRegistrationRepository::default();
    assert_eq!(empty.find_by_registration_id("google"), None);
    empty.add(google("gid", "gs", ""));
    assert!(empty.find_by_registration_id("google").is_some());

    let mut ids: Vec<String> = repo
        .registrations()
        .into_iter()
        .map(|r| r.registration_id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["github", "google"]);
}

// ---------------------------------------------------------------------------
// AuthorizationServer fixtures (pyfly: client_repo/token_store/auth_server)
// ---------------------------------------------------------------------------

const SECRET: &str = "test-signing-secret";

fn server() -> (AuthorizationServer, Arc<InMemoryTokenStore>) {
    let reg = ClientRegistration::new("test-client", "test-client")
        .client_secret("test-secret")
        .authorization_grant_type("client_credentials")
        .scopes(&["read", "write"]);
    let repo = InMemoryClientRegistrationRepository::new([reg]);
    let store = Arc::new(InMemoryTokenStore::new());
    let server = AuthorizationServer::new(SECRET, Arc::new(repo), store.clone())
        .issuer("https://auth.example.com");
    (server, store)
}

fn request(grant_type: &str) -> TokenRequest {
    TokenRequest {
        grant_type: grant_type.into(),
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        ..TokenRequest::default()
    }
}

fn decode_access_token(token: &str) -> Map<String, Value> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);
    jsonwebtoken::decode::<Map<String, Value>>(
        token,
        &DecodingKey::from_secret(SECRET.as_bytes()),
        &validation,
    )
    .unwrap()
    .claims
}

// Ported from pyfly: test_client_credentials_grant
#[tokio::test]
async fn client_credentials_grant() {
    let (server, _) = server();
    let result = server.token(&request("client_credentials")).await.unwrap();

    assert!(!result.access_token.is_empty());
    assert_eq!(result.token_type, "Bearer");
    assert_eq!(result.expires_in, 3600);
    assert!(!result.refresh_token.is_empty());
    assert_eq!(result.scope, "read write");
}

// Ported from pyfly: test_client_credentials_decodes_valid_jwt
#[tokio::test]
async fn client_credentials_access_token_decodes_to_valid_jwt() {
    let (server, _) = server();
    let result = server.token(&request("client_credentials")).await.unwrap();

    let payload = decode_access_token(&result.access_token);
    assert_eq!(payload["sub"], "test-client");
    assert_eq!(payload["scope"], "read write");
    assert_eq!(payload["iss"], "https://auth.example.com");
    assert!(payload.contains_key("iat"));
    assert!(payload.contains_key("exp"));
}

// Ported from pyfly: test_client_credentials_custom_scope
#[tokio::test]
async fn client_credentials_custom_scope_overrides_defaults() {
    let (server, _) = server();
    let mut req = request("client_credentials");
    req.scope = "admin superuser".into();
    let result = server.token(&req).await.unwrap();

    let payload = decode_access_token(&result.access_token);
    assert_eq!(payload["scope"], "admin superuser");
    assert_eq!(result.scope, "admin superuser");
}

// Ported from pyfly: test_refresh_token_grant
#[tokio::test]
async fn refresh_token_grant() {
    let (server, _) = server();
    let initial = server.token(&request("client_credentials")).await.unwrap();

    let mut refresh_req = request("refresh_token");
    refresh_req.refresh_token = Some(initial.refresh_token.clone());
    let refreshed = server.token(&refresh_req).await.unwrap();

    assert!(!refreshed.access_token.is_empty());
    assert_eq!(refreshed.token_type, "Bearer");
    assert_eq!(refreshed.expires_in, 3600);
    // Refresh token is always new (random); access token may match if
    // issued within the same second (deterministic claims + same iat).
    assert_ne!(refreshed.refresh_token, initial.refresh_token);
    assert_eq!(refreshed.scope, "read write");
}

// Ported from pyfly: test_refresh_token_rotation
#[tokio::test]
async fn refresh_token_rotation_revokes_old_token() {
    let (server, store) = server();
    let initial = server.token(&request("client_credentials")).await.unwrap();
    let old_refresh = initial.refresh_token;

    let mut refresh_req = request("refresh_token");
    refresh_req.refresh_token = Some(old_refresh.clone());
    server.token(&refresh_req).await.unwrap();

    // Old refresh token should be revoked.
    assert_eq!(store.find(&old_refresh).await.unwrap(), None);

    // Attempting to reuse the old refresh token should fail.
    let err = server.token(&refresh_req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_GRANT");
}

// Ported from pyfly: TestAuthorizationServerErrors
#[tokio::test]
async fn invalid_client_id_is_rejected() {
    let (server, _) = server();
    let mut req = request("client_credentials");
    req.client_id = "unknown-client".into();
    let err = server.token(&req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_CLIENT");
    assert_eq!(err.message, "Invalid client credentials");
}

#[tokio::test]
async fn invalid_client_secret_is_rejected() {
    let (server, _) = server();
    let mut req = request("client_credentials");
    req.client_secret = "wrong-secret".into();
    let err = server.token(&req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_CLIENT");
}

#[tokio::test]
async fn unsupported_grant_type_is_rejected() {
    let (server, _) = server();
    let err = server
        .token(&request("authorization_code"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "UNSUPPORTED_GRANT_TYPE");
    assert_eq!(err.message, "Unsupported grant type: authorization_code");
}

#[tokio::test]
async fn unknown_refresh_token_is_rejected() {
    let (server, _) = server();
    let mut req = request("refresh_token");
    req.refresh_token = Some("nonexistent-token".into());
    let err = server.token(&req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_GRANT");
    assert_eq!(err.message, "Invalid refresh token");
}

#[tokio::test]
async fn refresh_token_grant_requires_refresh_token() {
    let (server, _) = server();
    let err = server.token(&request("refresh_token")).await.unwrap_err();
    assert_eq!(err.code, "INVALID_REQUEST");
    assert_eq!(err.message, "Refresh token required");
}

#[tokio::test]
async fn grant_type_confusion_is_rejected() {
    // A client registered only for authorization_code must not mint
    // client_credentials tokens.
    let reg = ClientRegistration::new("web", "web")
        .client_secret("s")
        .authorization_grant_type("authorization_code");
    let server = AuthorizationServer::new(
        SECRET,
        Arc::new(InMemoryClientRegistrationRepository::new([reg])),
        Arc::new(InMemoryTokenStore::new()),
    );
    let err = server
        .token(&TokenRequest {
            grant_type: "client_credentials".into(),
            client_id: "web".into(),
            client_secret: "s".into(),
            ..TokenRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "UNAUTHORIZED_CLIENT");
}

#[tokio::test]
async fn expired_refresh_token_is_revoked_and_rejected() {
    let (server, store) = server();
    store
        .store(
            "stale",
            json!({"client_id": "test-client", "scope": "read", "exp": 1}),
        )
        .await
        .unwrap();

    let mut req = request("refresh_token");
    req.refresh_token = Some("stale".into());
    let err = server.token(&req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_GRANT");
    assert_eq!(err.message, "Refresh token expired");
    assert_eq!(store.find("stale").await.unwrap(), None, "revoked");
}

#[tokio::test]
async fn refresh_token_client_mismatch_is_rejected() {
    let (server, store) = server();
    store
        .store(
            "foreign",
            json!({"client_id": "someone-else", "scope": "read", "exp": 9999999999u64}),
        )
        .await
        .unwrap();

    let mut req = request("refresh_token");
    req.refresh_token = Some("foreign".into());
    let err = server.token(&req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_GRANT");
    assert_eq!(err.message, "Refresh token client mismatch");
}

// Ported from pyfly: TestTokenRevocation
#[tokio::test]
async fn revoking_a_refresh_token_makes_subsequent_use_fail() {
    let (server, _) = server();
    let result = server.token(&request("client_credentials")).await.unwrap();

    server.revoke(&result.refresh_token).await.unwrap();

    let mut req = request("refresh_token");
    req.refresh_token = Some(result.refresh_token);
    let err = server.token(&req).await.unwrap_err();
    assert_eq!(err.code, "INVALID_GRANT");
}

// ---------------------------------------------------------------------------
// InMemoryTokenStore (pyfly: TestInMemoryTokenStore)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_memory_store_and_find() {
    let store = InMemoryTokenStore::new();
    let data = json!({"client_id": "c1", "scope": "read", "exp": 9999999999u64});
    store.store("tok-1", data.clone()).await.unwrap();
    assert_eq!(store.find("tok-1").await.unwrap(), Some(data));
}

#[tokio::test]
async fn in_memory_find_nonexistent_returns_none() {
    let store = InMemoryTokenStore::new();
    assert_eq!(store.find("missing").await.unwrap(), None);
}

#[tokio::test]
async fn in_memory_revoke_removes_token() {
    let store = InMemoryTokenStore::new();
    store
        .store("tok-2", json!({"client_id": "c1"}))
        .await
        .unwrap();
    store.revoke("tok-2").await.unwrap();
    assert_eq!(store.find("tok-2").await.unwrap(), None);
}

#[tokio::test]
async fn in_memory_revoke_nonexistent_does_not_fail() {
    let store = InMemoryTokenStore::new();
    store.revoke("missing").await.unwrap();
}
