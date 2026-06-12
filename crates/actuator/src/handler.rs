//! The `/actuator/*` HTTP surface: [`ActuatorConfig`] plus [`mount`],
//! which returns an axum [`Router`] exposing health, info, metrics, env,
//! tasks, and version endpoints.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{SecondsFormat, Utc};
use http::{header, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};

use crate::health::{HealthComposite, HealthStatus};
use crate::metrics::MetricRegistry;
use crate::VERSION;

/// A function contributing extra top-level entries to `/actuator/info` —
/// the counterpart of Go's `func() map[string]any` contributors.
pub type InfoContributor = Box<dyn Fn() -> serde_json::Map<String, Value> + Send + Sync>;

/// Tunes [`mount`].
#[derive(Default)]
pub struct ActuatorConfig {
    /// Reflected on `/actuator/info` and `/actuator/version`.
    pub app_name: String,

    /// Reflected on `/actuator/info` and `/actuator/version`; defaults
    /// to the framework [`VERSION`] when empty.
    pub app_version: String,

    /// Health composite checked on `/actuator/health`; defaults to an
    /// empty composite (overall status `UP`).
    pub health: Arc<HealthComposite>,

    /// Contributors enriching `/actuator/info` with arbitrary
    /// structured data, merged into the top-level object.
    pub info_contributors: Vec<InfoContributor>,

    /// Prefix list of env var names whose values are returned by
    /// `/actuator/env` (everything else is redacted to `"***"`).
    /// Defaults to `["FIREFLY_"]`. Matching is case-insensitive.
    pub env_allow_prefixes: Vec<String>,

    /// Registry consulted by `/actuator/metrics`; defaults to an empty
    /// registry.
    pub metric_registry: Arc<MetricRegistry>,
}

/// Shared per-router state.
struct ActuatorState {
    app_name: String,
    app_version: String,
    health: Arc<HealthComposite>,
    info_contributors: Vec<InfoContributor>,
    env_allow_prefixes: Vec<String>,
    metric_registry: Arc<MetricRegistry>,
}

type SharedState = Arc<ActuatorState>;

/// Returns an axum [`Router`] exposing `/actuator/*` under the given
/// config. Merge it into the router used for the application's public
/// traffic, or — preferred — serve it from a dedicated admin port so the
/// management surface never leaks onto the public network.
///
/// Routes:
///
/// - `GET /actuator/health` — composite status; 200 UP/DEGRADED, 503 DOWN
/// - `GET /actuator/info` — app + runtime + build info, merged contributors
/// - `GET /actuator/metrics` — Prometheus exposition format
/// - `GET /actuator/env` — redacted environment view
/// - `GET /actuator/tasks` — `{"count": N}` alive tokio tasks; `?dump=true`
///   returns a plain-text runtime report (the async analog of Go's
///   `/actuator/goroutines`)
/// - `GET /actuator/version` — framework / app / language version stamp
pub fn mount(cfg: ActuatorConfig) -> Router {
    let app_version = if cfg.app_version.is_empty() {
        VERSION.to_string()
    } else {
        cfg.app_version
    };
    let env_allow_prefixes = if cfg.env_allow_prefixes.is_empty() {
        vec!["FIREFLY_".to_string()]
    } else {
        cfg.env_allow_prefixes
    };

    let state: SharedState = Arc::new(ActuatorState {
        app_name: cfg.app_name,
        app_version,
        health: cfg.health,
        info_contributors: cfg.info_contributors,
        env_allow_prefixes,
        metric_registry: cfg.metric_registry,
    });

    Router::new()
        .route("/actuator/health", get(health_handler))
        .route("/actuator/info", get(info_handler))
        .route("/actuator/metrics", get(metrics_handler))
        .route("/actuator/env", get(env_handler))
        .route("/actuator/tasks", get(tasks_handler))
        .route("/actuator/version", get(version_handler))
        .with_state(state)
}

/// Renders `value` as a JSON response body terminated by a single `\n` —
/// the counterpart of Go's `writeJSON`, whose `json.NewEncoder(w).Encode(v)`
/// always appends a trailing newline. Emitting the same final byte keeps
/// the wire format identical across the ports.
fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(mut body) => {
            body.push(b'\n');
            (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `GET /actuator/health` — runs every registered indicator and answers
/// `{"status": overall, "details": {name: result}}`; 503 when DOWN,
/// 200 otherwise.
async fn health_handler(State(st): State<SharedState>) -> Response {
    let (overall, details) = st.health.check_all().await;
    let code = if overall == HealthStatus::Down {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    json_response(code, &json!({ "status": overall, "details": details }))
}

/// `GET /actuator/info` — build info + app metadata, with every
/// configured contributor merged into the top-level object.
async fn info_handler(State(st): State<SharedState>) -> Response {
    let mut info = serde_json::Map::new();
    info.insert(
        "app".into(),
        json!({ "name": st.app_name, "version": st.app_version }),
    );
    info.insert(
        "runtime".into(),
        json!({
            "rustVersion": env!("CARGO_PKG_RUST_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "numCPU": std::thread::available_parallelism().map(usize::from).unwrap_or(1),
        }),
    );
    info.insert(
        "build".into(),
        json!({
            "crate": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
        }),
    );
    for contributor in &st.info_contributors {
        for (k, v) in contributor() {
            info.insert(k, v);
        }
    }
    json_response(StatusCode::OK, &Value::Object(info))
}

/// `GET /actuator/metrics` — Prometheus exposition format.
async fn metrics_handler(State(st): State<SharedState>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        st.metric_registry.render(),
    )
        .into_response()
}

/// `GET /actuator/env` — every process environment variable, with values
/// outside the allow-prefix list redacted to `"***"`.
async fn env_handler(State(st): State<SharedState>) -> Response {
    let mut out = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        let value = value.to_string_lossy().into_owned();
        let redacted = redact(&key, value, &st.env_allow_prefixes);
        out.insert(key, redacted);
    }
    json_response(StatusCode::OK, &out)
}

/// Returns `value` when `key` matches an allow prefix (case-insensitive),
/// `"***"` otherwise.
fn redact(key: &str, value: String, allow_prefixes: &[String]) -> String {
    let upper = key.to_uppercase();
    for prefix in allow_prefixes {
        if upper.starts_with(&prefix.to_uppercase()) {
            return value;
        }
    }
    "***".to_string()
}

/// `GET /actuator/tasks` — `{"count": N}` where N is the number of alive
/// tokio tasks, the async-Rust analog of Go's goroutine count. With
/// `?dump=true`, returns a plain-text tokio runtime report (Rust cannot
/// capture per-task stacks the way `runtime.Stack` dumps goroutines, so
/// the dump reports runtime metrics instead).
async fn tasks_handler(Query(params): Query<HashMap<String, String>>) -> Response {
    let metrics = tokio::runtime::Handle::current().metrics();
    if params.get("dump").map(String::as_str) == Some("true") {
        use std::fmt::Write as _;
        let mut dump = String::new();
        let _ = writeln!(dump, "=== tokio runtime ===");
        let _ = writeln!(dump, "workers: {}", metrics.num_workers());
        let _ = writeln!(dump, "alive_tasks: {}", metrics.num_alive_tasks());
        return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], dump).into_response();
    }
    json_response(
        StatusCode::OK,
        &json!({ "count": metrics.num_alive_tasks() }),
    )
}

/// `GET /actuator/version` — framework, app, and language version stamp.
/// The Go port reports its toolchain under `"go"`; this port reports the
/// minimum supported Rust version under `"rust"`.
async fn version_handler(State(st): State<SharedState>) -> Response {
    json_response(
        StatusCode::OK,
        &json!({
            "firefly": VERSION,
            "app": st.app_name,
            "appVersion": st.app_version,
            "rust": env!("CARGO_PKG_RUST_VERSION"),
            "buildTime": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_passes_allow_listed_prefixes() {
        let allow = vec!["FIREFLY_".to_string()];
        assert_eq!(redact("FIREFLY_WEB_PORT", "8080".into(), &allow), "8080");
        assert_eq!(
            redact("DATABASE_PASSWORD", "supersecret".into(), &allow),
            "***"
        );
    }

    #[test]
    fn redact_is_case_insensitive() {
        let allow = vec!["firefly_".to_string()];
        assert_eq!(redact("FIREFLY_X", "1".into(), &allow), "1");
        assert_eq!(redact("firefly_y", "2".into(), &allow), "2");
        assert_eq!(redact("other", "3".into(), &allow), "***");
    }
}
