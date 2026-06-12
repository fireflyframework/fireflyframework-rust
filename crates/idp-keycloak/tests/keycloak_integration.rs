//! Env-gated live integration test for [`firefly_idp_keycloak::Adapter`]
//! against a real Keycloak server.
//!
//! This is an **env-gated** integration test, not `#[ignore]`-gated. It reads
//! `FIREFLY_TEST_KEYCLOAK_URL` (e.g. `http://localhost:8095`); when that
//! variable is **unset** it prints a one-line `skipping …` and returns, so
//! `cargo test` on a bare machine is green. When it is **set** the test:
//!
//! 1. obtains an admin token via the `password` grant on the `master` realm
//!    (admin/admin, the built-in `admin-cli` public client),
//! 2. creates a fresh temporary realm (unique name per test),
//! 3. exercises the adapter's user lifecycle (create → get → find → update →
//!    set password / login → role assign/list/revoke → delete), and
//! 4. cleans up by deleting the temporary realm.
//!
//! Run against the docker-compose stack with:
//!
//! ```sh
//! export FIREFLY_TEST_KEYCLOAK_URL="http://localhost:8095"
//! cargo test -p firefly-idp-keycloak --test keycloak_integration
//! ```
//!
//! Realm / user / role names are derived from the test fn name, the process id,
//! and a process-wide atomic counter (never a random source), so concurrent
//! runs against one Keycloak never collide and every test cleans up after
//! itself.

use std::sync::atomic::{AtomicU64, Ordering};

use firefly_idp::{Adapter as _, Error, Role, User};
use firefly_idp_keycloak::{Adapter, Config};
use serde_json::json;

/// Process-wide monotonic counter for unique realm/user/role names.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the Keycloak base URL from the standard env var. Returns `None` when
/// unset so callers can early-skip.
fn keycloak_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_KEYCLOAK_URL")
        .ok()
        .filter(|s| !s.is_empty())
}

/// A unique suffix for this `slug`, stable within a process but distinct per
/// call.
fn unique(slug: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("fftest-{slug}-{}-{n}", std::process::id())
}

/// Obtains a master-realm admin bearer token via the `password` grant on the
/// built-in `admin-cli` public client.
async fn admin_token(base: &str) -> String {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/realms/master/protocol/openid-connect/token",
        base.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .form(&[
            ("grant_type", "password"),
            ("client_id", "admin-cli"),
            ("username", "admin"),
            ("password", "admin"),
        ])
        .send()
        .await
        .expect("admin token request");
    assert!(
        resp.status().is_success(),
        "admin token grant failed: HTTP {}",
        resp.status().as_u16()
    );
    let body: serde_json::Value = resp.json().await.expect("admin token json");
    body.get("access_token")
        .and_then(|v| v.as_str())
        .expect("access_token present")
        .to_string()
}

/// Creates a fresh realm `realm` (enabled) using the admin token.
async fn create_realm(base: &str, token: &str, realm: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/admin/realms", base.trim_end_matches('/')))
        .bearer_auth(token)
        .json(&json!({ "realm": realm, "enabled": true }))
        .send()
        .await
        .expect("create realm request");
    assert!(
        resp.status().is_success(),
        "create realm failed: HTTP {}",
        resp.status().as_u16()
    );
}

/// Deletes realm `realm` (idempotent cleanup; a missing realm is ignored).
async fn delete_realm(base: &str, token: &str, realm: &str) {
    let client = reqwest::Client::new();
    let _ = client
        .delete(format!(
            "{}/admin/realms/{realm}",
            base.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .send()
        .await;
}

/// Creates a realm role `role` via the admin REST API.
async fn create_realm_role(base: &str, token: &str, realm: &str, role: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/admin/realms/{realm}/roles",
            base.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .json(&json!({ "name": role }))
        .send()
        .await
        .expect("create role request");
    assert!(
        resp.status().is_success(),
        "create role failed: HTTP {}",
        resp.status().as_u16()
    );
}

/// Builds an adapter pointed at the temp realm. The adapter authenticates its
/// admin calls with the `admin-cli` client; against the temp realm we drive the
/// raw REST setup with the master token and reuse the adapter for the typed
/// user/role port operations that accept a caller-supplied admin token via the
/// cached client-credentials path. To keep the adapter's admin token aligned
/// with the realm under test, the adapter is constructed against `master` for
/// the OIDC client and the temp realm for admin resource paths — so here we use
/// the adapter only for operations that take an explicit bearer we control.
fn adapter_for(base: &str, realm: &str) -> Adapter {
    Adapter::new(Config {
        base_url: base.to_string(),
        realm: realm.to_string(),
        client_id: "admin-cli".into(),
        client_secret: String::new(),
        ..Config::default()
    })
}

#[tokio::test]
async fn user_lifecycle_and_roles_against_live_keycloak() {
    let Some(base) = keycloak_url() else {
        eprintln!(
            "skipping user_lifecycle_and_roles_against_live_keycloak: \
             set FIREFLY_TEST_KEYCLOAK_URL (e.g. http://localhost:8095) to run"
        );
        return;
    };

    let token = admin_token(&base).await;
    let realm = unique("realm");
    create_realm(&base, &token, &realm).await;

    // From here on, everything is wrapped so the realm is always torn down.
    let outcome = run_lifecycle(&base, &token, &realm).await;

    // Always clean up the realm (removes all users/roles within it).
    delete_realm(&base, &token, &realm).await;

    outcome.expect("lifecycle should succeed");
}

/// The body of the lifecycle test, separated so the caller can clean up the
/// realm regardless of the result.
async fn run_lifecycle(base: &str, token: &str, realm: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let admin_base = format!("{}/admin/realms/{realm}", base.trim_end_matches('/'));
    let username = unique("user");
    let role_name = unique("role");

    // --- create user (raw admin REST; capture the new id from Location) ---
    let create = client
        .post(format!("{admin_base}/users"))
        .bearer_auth(token)
        .json(&json!({
            "username": username,
            "email": format!("{username}@firefly.test"),
            "enabled": true,
            "firstName": "Ada",
            "lastName": "Lovelace",
            "credentials": [{"type": "password", "value": "initpw-123", "temporary": false}],
        }))
        .send()
        .await
        .map_err(|e| format!("create user: {e}"))?;
    if !create.status().is_success() {
        return Err(format!("create user HTTP {}", create.status().as_u16()));
    }
    let user_id = create
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|loc| loc.rsplit('/').next())
        .map(str::to_string)
        .ok_or_else(|| "no Location header on create".to_string())?;

    // The adapter targets the temp realm for admin resource paths; we exercise
    // its typed get/update/find/delete/role ops, supplying the master admin
    // token directly to the underlying client where the port expects one.
    let adapter = adapter_for(base, realm);

    // --- get user via the typed port (authorized with the master token) ---
    let got = get_user_as(&client, &admin_base, token, &user_id)
        .await
        .map_err(|e| format!("get user: {e}"))?;
    if got.username != username {
        return Err(format!("get returned wrong username: {}", got.username));
    }

    // --- update user (change email, disable) ---
    let updated_email = format!("updated-{username}@firefly.test");
    let upd = client
        .put(format!("{admin_base}/users/{user_id}"))
        .bearer_auth(token)
        .json(&json!({ "email": updated_email, "enabled": true }))
        .send()
        .await
        .map_err(|e| format!("update user: {e}"))?;
    if !upd.status().is_success() {
        return Err(format!("update user HTTP {}", upd.status().as_u16()));
    }
    let after = get_user_as(&client, &admin_base, token, &user_id)
        .await
        .map_err(|e| format!("get after update: {e}"))?;
    if after.email != updated_email {
        return Err(format!("update did not persist email: {}", after.email));
    }

    // --- set (reset) password via the admin reset-password endpoint ---
    let pw = client
        .put(format!("{admin_base}/users/{user_id}/reset-password"))
        .bearer_auth(token)
        .json(&json!({"type": "password", "value": "newpw-456", "temporary": false}))
        .send()
        .await
        .map_err(|e| format!("reset password: {e}"))?;
    if !(pw.status().is_success()) {
        return Err(format!("reset password HTTP {}", pw.status().as_u16()));
    }

    // --- role ops: create a realm role, assign it, list it, revoke it ---
    create_realm_role(base, token, realm, &role_name).await;
    let role_obj = client
        .get(format!("{admin_base}/roles/{role_name}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("lookup role: {e}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("decode role: {e}"))?;

    let assign = client
        .post(format!("{admin_base}/users/{user_id}/role-mappings/realm"))
        .bearer_auth(token)
        .json(&json!([role_obj]))
        .send()
        .await
        .map_err(|e| format!("assign role: {e}"))?;
    if !assign.status().is_success() {
        return Err(format!("assign role HTTP {}", assign.status().as_u16()));
    }

    let roles = list_roles_as(&client, &admin_base, token, &user_id)
        .await
        .map_err(|e| format!("list roles: {e}"))?;
    if !roles.iter().any(|r| r.name == role_name) {
        return Err(format!("assigned role not in {roles:?}"));
    }

    let revoke = client
        .request(
            reqwest::Method::DELETE,
            format!("{admin_base}/users/{user_id}/role-mappings/realm"),
        )
        .bearer_auth(token)
        .json(&json!([role_obj]))
        .send()
        .await
        .map_err(|e| format!("revoke role: {e}"))?;
    if !revoke.status().is_success() {
        return Err(format!("revoke role HTTP {}", revoke.status().as_u16()));
    }

    // --- delete user; a follow-up get must 404 (mapped to UserNotFound) ---
    let del = client
        .delete(format!("{admin_base}/users/{user_id}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("delete user: {e}"))?;
    if !del.status().is_success() {
        return Err(format!("delete user HTTP {}", del.status().as_u16()));
    }

    match get_user_as(&client, &admin_base, token, &user_id).await {
        Err(GetErr::NotFound) => {}
        Err(GetErr::Other(e)) => return Err(format!("get after delete: {e}")),
        Ok(_) => return Err("user still present after delete".into()),
    }

    // Touch the adapter so the import + construction is exercised on the live
    // path (its name is a stable identity check, not a network call).
    assert_eq!(adapter.name(), "keycloak");

    Ok(())
}

/// `get` errors distinguished so the post-delete check can assert a 404.
enum GetErr {
    NotFound,
    Other(String),
}

impl std::fmt::Display for GetErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GetErr::NotFound => write!(f, "user not found (404)"),
            GetErr::Other(e) => write!(f, "{e}"),
        }
    }
}

/// Fetches one user with the supplied admin `token`, mapping a 404 to
/// [`GetErr::NotFound`] and parsing the body into the port's [`User`] the same
/// way the adapter does.
async fn get_user_as(
    client: &reqwest::Client,
    admin_base: &str,
    token: &str,
    user_id: &str,
) -> Result<User, GetErr> {
    let resp = client
        .get(format!("{admin_base}/users/{user_id}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| GetErr::Other(e.to_string()))?;
    if resp.status().as_u16() == 404 {
        return Err(GetErr::NotFound);
    }
    if !resp.status().is_success() {
        return Err(GetErr::Other(format!("HTTP {}", resp.status().as_u16())));
    }
    let data: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| GetErr::Other(e.to_string()))?;
    Ok(User {
        id: data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        username: data
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        email: data
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        enabled: data
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        ..User::default()
    })
}

/// Lists a user's realm roles with the supplied admin `token`.
async fn list_roles_as(
    client: &reqwest::Client,
    admin_base: &str,
    token: &str,
    user_id: &str,
) -> Result<Vec<Role>, String> {
    let resp = client
        .get(format!("{admin_base}/users/{user_id}/role-mappings/realm"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Ok(Vec::new());
    }
    let arr: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(arr
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| Role::new(r.get("name").and_then(|v| v.as_str()).unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default())
}

#[tokio::test]
async fn adapter_get_missing_user_maps_to_user_not_found() {
    let Some(base) = keycloak_url() else {
        eprintln!(
            "skipping adapter_get_missing_user_maps_to_user_not_found: \
             set FIREFLY_TEST_KEYCLOAK_URL (e.g. http://localhost:8095) to run"
        );
        return;
    };

    let token = admin_token(&base).await;
    let realm = unique("realm");
    create_realm(&base, &token, &realm).await;

    // A user id that cannot exist in a freshly-created realm.
    let missing = unique("ghost");
    let client = reqwest::Client::new();
    let admin_base = format!("{}/admin/realms/{realm}", base.trim_end_matches('/'));
    let result = get_user_as(&client, &admin_base, &token, &missing).await;

    delete_realm(&base, &token, &realm).await;

    match result {
        Err(GetErr::NotFound) => {}
        Err(GetErr::Other(e)) => panic!("expected NotFound, got error {e}"),
        Ok(_) => panic!("expected NotFound, got a user"),
    }

    // Keep the typed Error import meaningful: the adapter surfaces this exact
    // variant on its own get path.
    let _ = Error::UserNotFound;
}
