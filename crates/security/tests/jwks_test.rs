//! Port of pyfly's `tests/security/test_oauth2_resource_server.py`
//! against an in-process axum JWKS server (pyfly mocks `PyJWKClient`;
//! here the verifier's real HTTP fetch path is exercised on port 0).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::{Json, Router};
use firefly_security::{JwksVerifier, Verifier};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

/// Static 2048-bit RSA test key (generated once for this test module —
/// never use outside tests).
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

/// URL-safe base64 modulus of [`TEST_RSA_PEM`]'s public key.
const TEST_RSA_N: &str = "rjIEmi7GzGJBUZL9oGQKi0P5BQG9nHUrvHz_A-rg0cbGPyL6l9uZ3t3RnWHkf8QSaGTAZS1UNJ3aMhByKDudT6VdHaOjqKc6tEBmYDKeG7Mxk1UGNsmaz3W9Io4HR2X3G9bzM4Gv48uMNMnl13vYYonl4Saut57Wn5qZjxGmkBxrA9Wxee1qZXZrVp3n0ZU-OxylAQJvhszwQdi_6uXyXJUUTtW2mdb9fmpymF6r7svAP5_CbPXPuI4bal7MlQqWSpWFebTZ9ewvlizTMGY-wCu98UfonQqy_U1wv0UngNDoIfqNqz-GV1lFwIO9nhq5g2vU7L2-NUO3FFRtDsoLfw";

const TEST_RSA_E: &str = "AQAB";
const TEST_KID: &str = "test-kid";

/// Spawns an in-process JWKS server on port 0; returns its JWKS URI
/// and the fetch counter.
async fn spawn_jwks_server() -> (String, Arc<AtomicUsize>) {
    let fetches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fetches);
    let body = json!({
        "keys": [
            {"kty": "RSA", "kid": TEST_KID, "use": "sig", "alg": "RS256",
             "n": TEST_RSA_N, "e": TEST_RSA_E},
            {"kty": "EC", "kid": "ignored-ec", "crv": "P-256"},
        ]
    });
    let app = Router::new().route(
        "/.well-known/jwks.json",
        get(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            async move { Json(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/.well-known/jwks.json"), fetches)
}

/// Creates an RS256-signed JWT (adds an `exp` claim by default) —
/// pyfly's `_create_test_token`.
fn make_token(mut claims: Value, kid: &str) -> String {
    if claims.get("exp").is_none() {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        claims["exp"] = json!(exp);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    jsonwebtoken::encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(TEST_RSA_PEM).unwrap(),
    )
    .unwrap()
}

// Ported from pyfly: test_validate_valid_token
#[tokio::test]
async fn validate_valid_token() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    let token = make_token(json!({"sub": "user-1", "name": "Alice"}), TEST_KID);

    let payload = verifier.validate(&token).await.unwrap();
    assert_eq!(payload["sub"], "user-1");
    assert_eq!(payload["name"], "Alice");
}

// Ported from pyfly: test_validate_invalid_token
#[tokio::test]
async fn validate_invalid_token() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);

    let err = verifier.validate("garbage.token.value").await.unwrap_err();
    assert!(err.to_string().contains("Token validation failed"), "{err}");
}

#[tokio::test]
async fn validate_rejects_unknown_kid() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    let token = make_token(json!({"sub": "user-1"}), "other-kid");

    let err = verifier.validate(&token).await.unwrap_err();
    assert!(err.to_string().contains("no signing key"), "{err}");
}

// Ported from pyfly: test_to_security_context_basic
#[tokio::test]
async fn verify_maps_basic_claims_to_authentication() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    let token = make_token(
        json!({
            "sub": "user-42",
            "roles": ["admin", "editor"],
            "permissions": ["read", "write"],
        }),
        TEST_KID,
    );

    let auth = verifier.verify(&token).await.unwrap();
    assert_eq!(auth.principal, "user-42");
    assert_eq!(auth.roles, vec!["admin", "editor"]);
    assert_eq!(auth.authorities, vec!["read", "write"]);
}

// Ported from pyfly: test_to_security_context_keycloak_roles
#[tokio::test]
async fn verify_maps_keycloak_realm_access_roles() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    let token = make_token(
        json!({
            "sub": "kc-user",
            "realm_access": {"roles": ["realm-admin", "realm-viewer"]},
        }),
        TEST_KID,
    );

    let auth = verifier.verify(&token).await.unwrap();
    assert_eq!(auth.principal, "kc-user");
    assert_eq!(auth.roles, vec!["realm-admin", "realm-viewer"]);
}

// Ported from pyfly: test_to_security_context_scope_as_permissions
#[tokio::test]
async fn verify_splits_scope_into_authorities() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    let token = make_token(
        json!({"sub": "scope-user", "scope": "read write delete"}),
        TEST_KID,
    );

    let auth = verifier.verify(&token).await.unwrap();
    assert_eq!(auth.authorities, vec!["read", "write", "delete"]);
}

// Ported from pyfly: test_validate_with_issuer
#[tokio::test]
async fn validate_with_issuer() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri).issuer("https://auth.example.com");

    let valid = make_token(
        json!({"sub": "user-1", "iss": "https://auth.example.com"}),
        TEST_KID,
    );
    assert_eq!(verifier.validate(&valid).await.unwrap()["sub"], "user-1");

    let bad = make_token(
        json!({"sub": "user-1", "iss": "https://evil.example.com"}),
        TEST_KID,
    );
    let err = verifier.validate(&bad).await.unwrap_err();
    assert!(err.to_string().contains("Token validation failed"), "{err}");
}

// Ported from pyfly: test_validate_with_audience
#[tokio::test]
async fn validate_with_audience() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri).audience("my-api");

    let valid = make_token(json!({"sub": "user-1", "aud": "my-api"}), TEST_KID);
    assert_eq!(verifier.validate(&valid).await.unwrap()["sub"], "user-1");

    let bad = make_token(json!({"sub": "user-1", "aud": "other-api"}), TEST_KID);
    let err = verifier.validate(&bad).await.unwrap_err();
    assert!(err.to_string().contains("Token validation failed"), "{err}");
}

#[tokio::test]
async fn validate_requires_exp_claim() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    // Build a token with no exp at all.
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(TEST_KID.to_string());
    let token = jsonwebtoken::encode(
        &header,
        &json!({"sub": "u1"}),
        &EncodingKey::from_rsa_pem(TEST_RSA_PEM).unwrap(),
    )
    .unwrap();

    let err = verifier.validate(&token).await.unwrap_err();
    assert!(err.to_string().contains("Token validation failed"), "{err}");
}

#[tokio::test]
async fn kid_cache_fetches_jwks_once() {
    let (uri, fetches) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);

    for i in 0..3 {
        let token = make_token(json!({"sub": format!("user-{i}")}), TEST_KID);
        verifier.validate(&token).await.unwrap();
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "kid cache must hold");
}

#[tokio::test]
async fn rejects_disallowed_algorithm() {
    let (uri, _) = spawn_jwks_server().await;
    let verifier = JwksVerifier::new(&uri);
    // HS256-signed token must be rejected before any key lookup.
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(TEST_KID.to_string());
    let token = jsonwebtoken::encode(
        &header,
        &json!({"sub": "u1", "exp": 9999999999u64}),
        &EncodingKey::from_secret(b"hmac-secret"),
    )
    .unwrap();

    let err = verifier.validate(&token).await.unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
}
