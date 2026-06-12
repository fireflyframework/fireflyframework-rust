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

//! Port of the pyfly configuration test contract:
//! `tests/core/test_placeholder_resolution.py`, `test_config_reload.py`,
//! `test_config.py` (effective/sources/masking), `test_wave_config_relaxed.py`
//! and the `config_server` client behavior, adapted to the Rust surface
//! (flat-map sources + serde binding instead of dict access; explicit
//! `ReloadableConfig` instead of `ContextRefresher` DI).
//!
//! Tests that mutate process environment variables serialize on a shared
//! mutex and clean up via drop guards, because the test harness runs tests
//! in parallel threads within this binary.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use firefly_config::{
    active_profiles, from_yaml, load, load_from_profile, mask, ConfigClient, Layered, Refresher,
    ReloadableConfig, Source, StaticSource, SYSTEM_ENVIRONMENT_SOURCE,
};
use serde::Deserialize;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sets an environment variable for the test's lifetime; removes on drop.
struct EnvVar {
    key: &'static str,
}

impl EnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        std::env::set_var(key, value);
        EnvVar { key }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        std::env::remove_var(self.key);
    }
}

fn entries(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn static_sources(pairs: &[(&str, &str)]) -> Vec<Box<dyn Source>> {
    vec![Box::new(StaticSource::new("test", entries(pairs)))]
}

// ---------------------------------------------------------------------------
// Placeholder resolution (pyfly test_placeholder_resolution.py)
// ---------------------------------------------------------------------------

// pyfly: test_resolve_env_var
#[test]
fn placeholder_resolves_env_var() {
    let _guard = env_lock();
    let _secret = EnvVar::set("MY_SECRET_PYFLY_PARITY", "s3cret");

    #[derive(Debug, Deserialize)]
    struct Db {
        password: String,
    }
    #[derive(Debug, Deserialize)]
    struct Cfg {
        db: Db,
    }
    let cfg: Cfg = load(&static_sources(&[(
        "db.password",
        "${MY_SECRET_PYFLY_PARITY}",
    )]))
    .unwrap();
    assert_eq!(cfg.db.password, "s3cret");
}

// pyfly: test_env_var_in_placeholder + test_multiple_placeholders_in_one_value
#[test]
fn placeholder_resolves_multiple_env_vars_in_one_value() {
    let _guard = env_lock();
    let _user = EnvVar::set("PARITY_DB_USER", "admin");
    let _pass = EnvVar::set("PARITY_DB_PASS", "secret");

    #[derive(Debug, Deserialize)]
    struct Cfg {
        dsn: String,
    }
    let cfg: Cfg = load(&static_sources(&[(
        "dsn",
        "${PARITY_DB_USER}:${PARITY_DB_PASS}@localhost",
    )]))
    .unwrap();
    assert_eq!(cfg.dsn, "admin:secret@localhost");
}

// pyfly audit #87/#89: env overrides win over raw config data inside
// placeholders — ${app.name} honors FIREFLY_APP_NAME.
#[test]
fn placeholder_env_beats_config_reference() {
    let _guard = env_lock();
    let _name = EnvVar::set("FIREFLY_APP_NAME", "envname");

    #[derive(Debug, Deserialize)]
    struct Cfg {
        greeting: String,
    }
    let cfg: Cfg = load(&static_sources(&[
        ("app.name", "filename"),
        ("greeting", "Hello from ${app.name}"),
    ]))
    .unwrap();
    assert_eq!(cfg.greeting, "Hello from envname");
}

// pyfly: test_resolve_config_reference / test_resolve_with_default /
// test_resolve_nested, end-to-end through load() with a YAML file.
#[test]
fn placeholders_resolve_through_yaml_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.yaml");
    std::fs::write(
        &path,
        "base: localhost\nhost: ${base}\nurl: http://${host}:8080\nkey: ${MISSING_VAR_PARITY:fallback_value}\n",
    )
    .unwrap();

    #[derive(Debug, Deserialize)]
    struct Cfg {
        url: String,
        key: String,
    }
    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml(path))];
    let cfg: Cfg = load(&sources).unwrap();
    assert_eq!(cfg.url, "http://localhost:8080");
    assert_eq!(cfg.key, "fallback_value");
}

// pyfly: test_max_recursion_guard
#[test]
fn placeholder_circular_reference_errors() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Cfg {
        a: String,
    }
    let err = load::<Cfg>(&static_sources(&[("a", "${b}"), ("b", "${a}")])).unwrap_err();
    assert!(
        err.to_string().contains("max recursion depth"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Relaxed (kebab <-> snake) binding (pyfly test_wave_config_relaxed.py)
// ---------------------------------------------------------------------------

#[test]
fn kebab_yaml_keys_bind_snake_serde_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.yaml");
    std::fs::write(&path, "server:\n  graceful-timeout: 30\n").unwrap();

    #[derive(Debug, Deserialize)]
    struct Server {
        graceful_timeout: u64,
    }
    #[derive(Debug, Deserialize)]
    struct Cfg {
        server: Server,
    }
    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml(path))];
    let cfg: Cfg = load(&sources).unwrap();
    assert_eq!(cfg.server.graceful_timeout, 30);
}

// pyfly #92: kebab and snake forms are interchangeable in references too.
#[test]
fn placeholder_reference_is_relaxed_between_kebab_and_snake() {
    #[derive(Debug, Deserialize)]
    struct Cfg {
        msg: String,
    }
    let cfg: Cfg = load(&static_sources(&[
        ("my_prop.sub_key", "V"),
        ("msg", "${my-prop.sub-key}"),
    ]))
    .unwrap();
    assert_eq!(cfg.msg, "V");
}

// ---------------------------------------------------------------------------
// Runtime reload (pyfly test_config_reload.py)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AppOnly {
    app: AppSection,
}

#[derive(Debug, Deserialize)]
struct AppSection {
    name: String,
}

// pyfly: test_reload_picks_up_file_changes
#[test]
fn reload_picks_up_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.yaml");
    std::fs::write(&path, "app:\n  name: alpha\n").unwrap();

    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml(&path))];
    let cfg: ReloadableConfig<AppOnly> = ReloadableConfig::load(sources).unwrap();
    assert_eq!(cfg.get().app.name, "alpha");

    std::fs::write(&path, "app:\n  name: beta\n").unwrap();
    let changed = cfg.reload().unwrap();
    assert_eq!(changed, vec!["app".to_string()]);
    assert_eq!(cfg.get().app.name, "beta");

    // No further change -> empty diff (pyfly's "reload happened" boolean
    // maps onto the changed-keys list being empty or not).
    assert!(cfg.reload().unwrap().is_empty());
}

// The Refresher hook the actuator /actuator/refresh endpoint calls.
#[test]
fn refresher_hook_reports_changed_top_level_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.yaml");
    std::fs::write(&path, "app:\n  name: alpha\n").unwrap();

    let sources: Vec<Box<dyn Source>> = vec![Box::new(from_yaml(&path))];
    let cfg: std::sync::Arc<ReloadableConfig<AppOnly>> =
        std::sync::Arc::new(ReloadableConfig::load(sources).unwrap());
    let refresher: std::sync::Arc<dyn Refresher> = cfg.clone();

    std::fs::write(&path, "app:\n  name: beta\nfeature:\n  flag: on\n").unwrap();
    let changed = refresher.refresh().unwrap();
    assert_eq!(changed, vec!["app".to_string(), "feature".to_string()]);
    assert_eq!(cfg.get().app.name, "beta");
}

// ---------------------------------------------------------------------------
// Property sources + masking (pyfly test_config.py TestEffectiveAndSources
// and TestSecretMasking)
// ---------------------------------------------------------------------------

// pyfly: test_property_sources_ordered_env_first
#[test]
fn property_sources_ordered_env_first() {
    let _guard = env_lock();
    let _name = EnvVar::set("FIREFLY_APP_NAME", "envname");

    let layered = Layered::new(vec![Box::new(StaticSource::new(
        "applicationConfig",
        entries(&[("app.name", "filename")]),
    ))]);
    let sources = layered.property_sources().unwrap();
    let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&SYSTEM_ENVIRONMENT_SOURCE), "got: {names:?}");
    let env_idx = names
        .iter()
        .position(|n| *n == SYSTEM_ENVIRONMENT_SOURCE)
        .unwrap();
    let cfg_idx = names
        .iter()
        .position(|n| *n != SYSTEM_ENVIRONMENT_SOURCE)
        .unwrap();
    assert!(env_idx < cfg_idx, "systemEnvironment must outrank config");

    let env_view = &sources[env_idx];
    assert_eq!(env_view.properties["FIREFLY_APP_NAME"].value, "envname");
    assert_eq!(
        env_view.properties["FIREFLY_APP_NAME"].origin,
        "System Environment Property"
    );
}

// pyfly: test_property_sources_attribute_value_and_origin
#[test]
fn property_sources_attribute_value_and_origin() {
    let layered = Layered::new(vec![Box::new(StaticSource::new(
        "applicationConfig",
        entries(&[("app.name", "filename")]),
    ))]);
    let sources = layered.property_sources().unwrap();
    let flat: HashMap<String, _> = sources
        .into_iter()
        .flat_map(|s| s.properties.into_iter())
        .collect();
    assert_eq!(flat["app.name"].value, "filename");
    assert_eq!(flat["app.name"].origin, "applicationConfig");
}

// pyfly: TestSecretMasking — env values are masked in the view too.
#[test]
fn property_sources_mask_sensitive_env_values() {
    let _guard = env_lock();
    let _secret = EnvVar::set("FIREFLY_JWT_SECRET", "abc");

    let layered = Layered::new(vec![]);
    let sources = layered.property_sources().unwrap();
    let env_view = sources
        .iter()
        .find(|s| s.name == SYSTEM_ENVIRONMENT_SOURCE)
        .expect("systemEnvironment source");
    assert_eq!(env_view.properties["FIREFLY_JWT_SECRET"].value, mask::MASK);
}

// pyfly: test_masks_sensitive_keys / test_does_not_mask_normal_keys /
// test_redacts_password_in_uri_value
#[test]
fn mask_value_matches_pyfly_sanitizer() {
    assert_eq!(
        mask::mask_value("firefly.security.jwt.secret", "abc"),
        "******"
    );
    assert_eq!(mask::mask_value("db.password", "hunter2"), "******");
    assert_eq!(mask::mask_value("api.token", "xyz"), "******");
    assert_eq!(mask::mask_value("firefly.web.port", "8080"), "8080");
    assert_eq!(mask::mask_value("app.name", "svc"), "svc");
    assert_eq!(
        mask::mask_value("firefly.data.url", "postgresql://user:hunter2@localhost/db"),
        "postgresql://user:******@localhost/db"
    );
    assert_eq!(
        mask::mask_value("firefly.data.url", "sqlite:///firefly.db"),
        "sqlite:///firefly.db"
    );
}

// ---------------------------------------------------------------------------
// Multi-profile (pyfly comma-separated active profiles)
// ---------------------------------------------------------------------------

#[test]
fn active_profiles_splits_trims_and_lowercases() {
    let _guard = env_lock();
    {
        let _profile = EnvVar::set("FIREFLY_PROFILE", " DEV , cloud ,, Prod ");
        assert_eq!(active_profiles("dev"), vec!["dev", "cloud", "prod"]);
    }
    // Removed by the drop guard: fallback applies.
    assert_eq!(active_profiles("staging"), vec!["staging"]);
    // Blank value also falls back.
    let _profile = EnvVar::set("FIREFLY_PROFILE", "  ,  ");
    assert_eq!(active_profiles("dev"), vec!["dev"]);
}

#[test]
fn load_from_profile_overlays_every_active_profile_in_order() {
    let _guard = env_lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("application.yaml"),
        "web:\n  port: 8080\napp:\n  name: base\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("application-dev.yaml"),
        "web:\n  port: 3000\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("application-cloud.yaml"),
        "app:\n  name: cloudy\n",
    )
    .unwrap();

    #[derive(Debug, Deserialize)]
    struct Web {
        port: u16,
    }
    #[derive(Debug, Deserialize)]
    struct App {
        name: String,
    }
    #[derive(Debug, Deserialize)]
    struct Cfg {
        web: Web,
        app: App,
    }

    let _profile = EnvVar::set("FIREFLY_PROFILE", "dev,cloud");
    let cfg: Cfg = load_from_profile(dir.path(), "application", "dev").unwrap();
    assert_eq!(cfg.web.port, 3000, "dev overlay must apply");
    assert_eq!(cfg.app.name, "cloudy", "cloud overlay must apply after dev");
}

// ---------------------------------------------------------------------------
// ConfigClient (pyfly config_server/client.py) — in-process axum mock.
// ---------------------------------------------------------------------------

/// Spins up an in-process config server on port 0 returning `body` for any
/// `/{app}/{profile}/{label}` GET, capturing the Authorization header.
async fn mock_server(
    body: serde_json::Value,
    status: axum::http::StatusCode,
) -> (
    String,
    std::sync::Arc<Mutex<Option<String>>>,
    tokio::task::JoinHandle<()>,
) {
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::get;

    type Captured = std::sync::Arc<Mutex<Option<String>>>;
    let captured: Captured = std::sync::Arc::new(Mutex::new(None));
    let state = (captured.clone(), body, status);

    let app = axum::Router::new().route(
        "/:app/:profile/:label",
        get(
            |State((captured, body, status)): State<(
                Captured,
                serde_json::Value,
                axum::http::StatusCode,
            )>,
             headers: HeaderMap| async move {
                let auth = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                *captured.lock().unwrap() = auth;
                (status, axum::Json(body))
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app.with_state(state)).await.unwrap();
    });
    (format!("http://{addr}"), captured, handle)
}

fn spring_document() -> serde_json::Value {
    serde_json::json!({
        "name": "orders",
        "profiles": ["prod"],
        "label": "main",
        "propertySources": [
            {"name": "high", "source": {"web.port": 9999, "flag": true}},
            {"name": "low",  "source": {"web.port": 1111, "app.name": "orders"}}
        ]
    })
}

#[tokio::test]
async fn config_client_fetch_flattens_property_sources_highest_wins() {
    let (url, _auth, server) = mock_server(spring_document(), axum::http::StatusCode::OK).await;
    let client = ConfigClient::new(&url, "orders").with_profile("prod");
    let flat = client.fetch().await.unwrap();
    assert_eq!(flat["web.port"], "9999", "highest-priority source must win");
    assert_eq!(flat["app.name"], "orders");
    assert_eq!(flat["flag"], "true");
    server.abort();
}

#[tokio::test]
async fn config_client_sends_basic_auth() {
    let (url, auth, server) = mock_server(spring_document(), axum::http::StatusCode::OK).await;
    let client = ConfigClient::new(&url, "orders")
        .with_profile("prod")
        .with_basic_auth("user", "pass");
    client.fetch().await.unwrap();
    // base64("user:pass") == "dXNlcjpwYXNz"
    assert_eq!(auth.lock().unwrap().as_deref(), Some("Basic dXNlcjpwYXNz"));
    server.abort();
}

// pyfly: non-200 logs a warning and returns an empty map (soft miss).
#[tokio::test]
async fn config_client_non_success_status_yields_empty_map() {
    let (url, _auth, server) =
        mock_server(serde_json::json!({}), axum::http::StatusCode::NOT_FOUND).await;
    let client = ConfigClient::new(&url, "missing");
    let flat = client.fetch().await.unwrap();
    assert!(flat.is_empty());
    server.abort();
}

#[tokio::test]
async fn config_client_transport_error_is_remote_error_and_soft_variant_falls_back() {
    // Bind then drop a listener so the port is closed: connection refused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = ConfigClient::new(format!("http://{addr}"), "orders");
    let err = client.fetch().await.unwrap_err();
    assert!(
        matches!(err, firefly_config::ConfigError::Remote { .. }),
        "got: {err:?}"
    );

    // pyfly _import_remote_config fallback: warn + continue on local config.
    let source = client.fetch_source_or_empty().await;
    assert!(source.load().unwrap().is_empty());
    assert!(source.name().starts_with("configserver(http://"));
}

#[tokio::test]
async fn config_client_fetched_source_slots_into_layered_chain() {
    let (url, _auth, server) = mock_server(spring_document(), axum::http::StatusCode::OK).await;
    let remote = ConfigClient::new(&url, "orders")
        .with_profile("prod")
        .fetch_source()
        .await
        .unwrap();

    #[derive(Debug, Deserialize)]
    struct Web {
        port: u16,
    }
    #[derive(Debug, Deserialize)]
    struct Cfg {
        web: Web,
    }
    // Remote config sits above local defaults, below explicit overrides.
    let sources: Vec<Box<dyn Source>> = vec![
        Box::new(StaticSource::new(
            "defaults",
            entries(&[("web.port", "8080")]),
        )),
        Box::new(remote),
    ];
    let cfg: Cfg = load(&sources).unwrap();
    assert_eq!(cfg.web.port, 9999);
    server.abort();
}
