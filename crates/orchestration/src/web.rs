//! REST router for executions, dead letters, and signals — the Rust
//! spelling of pyfly's transactional REST controllers
//! (`pyfly.transactional.rest.controllers`), mirroring the
//! `callbacks::web` axum pattern.
//!
//! [`router`] returns an axum [`Router`] mounting these routes under the
//! `/api/orchestration` prefix, byte-compatible with the pyfly controllers:
//!
//! | Method   | Path                                       | Response                                |
//! |----------|--------------------------------------------|-----------------------------------------|
//! | `GET`    | `/api/orchestration/executions`            | in-flight runs (or `?status=` filter)   |
//! | `GET`    | `/api/orchestration/executions/{cid}`      | one run, or `204` when absent           |
//! | `GET`    | `/api/orchestration/dlq`                    | dead-letter entries (`?execution_name=`/`?correlation_id=`) |
//! | `GET`    | `/api/orchestration/dlq/count`              | `{"count": n}`                          |
//! | `GET`    | `/api/orchestration/dlq/{id}`               | one entry, or `204`                     |
//! | `POST`   | `/api/orchestration/dlq/{id}/retry`         | `{"retried": bool}`                     |
//! | `DELETE` | `/api/orchestration/dlq/{id}`               | `{"deleted": bool}`                     |
//! | `POST`   | `/api/orchestration/workflow/signal`        | `{"delivered": bool}`                   |
//! | `GET`    | `/api/orchestration/definitions`            | registered definitions (admin listing)  |

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http::StatusCode;
use serde::Deserialize;

use crate::dlq::DeadLetterService;
use crate::model::ExecutionStatus;
use crate::persistence::{ExecutionFilter, PersistenceProvider};
use crate::registry::OrchestrationRegistry;
use crate::signal::SignalService;

/// Shared state behind the orchestration REST surface — the persistence
/// provider, dead-letter service, signal service, and (optionally) the
/// definition registry the admin listing renders.
#[derive(Clone)]
pub struct OrchestrationApi {
    persistence: Arc<dyn PersistenceProvider>,
    dlq: DeadLetterService,
    signals: Arc<SignalService>,
    registry: Arc<OrchestrationRegistry>,
}

impl std::fmt::Debug for OrchestrationApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestrationApi").finish_non_exhaustive()
    }
}

impl OrchestrationApi {
    /// Assembles the API state from its collaborators.
    pub fn new(
        persistence: Arc<dyn PersistenceProvider>,
        dlq: DeadLetterService,
        signals: Arc<SignalService>,
        registry: Arc<OrchestrationRegistry>,
    ) -> Self {
        Self {
            persistence,
            dlq,
            signals,
            registry,
        }
    }
}

/// Returns the axum [`Router`] for the orchestration admin surface, mounted
/// under `/api/orchestration` — pyfly's three REST controllers combined.
pub fn router(api: OrchestrationApi) -> Router {
    Router::new()
        .route("/api/orchestration/executions", get(list_executions))
        .route(
            "/api/orchestration/executions/:correlation_id",
            get(get_execution),
        )
        .route("/api/orchestration/dlq", get(list_dlq))
        .route("/api/orchestration/dlq/count", get(dlq_count))
        .route(
            "/api/orchestration/dlq/:entry_id",
            get(get_dlq).delete(delete_dlq),
        )
        .route("/api/orchestration/dlq/:entry_id/retry", post(retry_dlq))
        .route("/api/orchestration/workflow/signal", post(deliver_signal))
        .route("/api/orchestration/definitions", get(list_definitions))
        .with_state(api)
}

/// Query string of `GET /executions`.
#[derive(Debug, Deserialize)]
struct ExecutionsQuery {
    status: Option<String>,
}

/// `GET /executions` — persisted runs. With `?status=`, filters by that
/// status; without it, defaults to non-terminal (in-flight) runs only,
/// matching pyfly audit #169.
async fn list_executions(
    State(api): State<OrchestrationApi>,
    Query(query): Query<ExecutionsQuery>,
) -> Response {
    let states = match query.status.as_deref() {
        Some(raw) => {
            let Some(status) = ExecutionStatus::parse(raw) else {
                return (StatusCode::BAD_REQUEST, format!("unknown status {raw:?}\n"))
                    .into_response();
            };
            api.persistence
                .list(ExecutionFilter::all().status(status))
                .await
        }
        None => api
            .persistence
            .list(ExecutionFilter::all())
            .await
            .map(|all| all.into_iter().filter(|s| !s.is_terminal()).collect()),
    };
    match states {
        Ok(states) => Json(states).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// `GET /executions/{correlation_id}` — one run, or `204` when absent
/// (pyfly maps a `None` return to 204 No Content).
async fn get_execution(
    State(api): State<OrchestrationApi>,
    Path(correlation_id): Path<String>,
) -> Response {
    match api.persistence.load(&correlation_id).await {
        Ok(Some(state)) => Json(state).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// Query string of `GET /dlq`.
#[derive(Debug, Deserialize)]
struct DlqQuery {
    execution_name: Option<String>,
    correlation_id: Option<String>,
}

/// `GET /dlq` — dead-letter entries, optionally filtered.
async fn list_dlq(State(api): State<OrchestrationApi>, Query(query): Query<DlqQuery>) -> Response {
    let mut filter = crate::dlq::DeadLetterFilter::all();
    if let Some(name) = query.execution_name {
        filter = filter.execution_name(name);
    }
    if let Some(cid) = query.correlation_id {
        filter = filter.correlation_id(cid);
    }
    match api.dlq.list(filter).await {
        Ok(entries) => Json(entries).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// `GET /dlq/count` — total dead-letter entries (pyfly audit #167).
async fn dlq_count(State(api): State<OrchestrationApi>) -> Response {
    match api.dlq.count().await {
        Ok(count) => Json(serde_json::json!({ "count": count })).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// `GET /dlq/{entry_id}` — one entry, or `204` when absent.
async fn get_dlq(State(api): State<OrchestrationApi>, Path(entry_id): Path<String>) -> Response {
    match api.dlq.get(&entry_id).await {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// `POST /dlq/{entry_id}/retry` — increments the retry counter.
async fn retry_dlq(State(api): State<OrchestrationApi>, Path(entry_id): Path<String>) -> Response {
    match api.dlq.mark_retried(&entry_id).await {
        Ok(retried) => Json(serde_json::json!({ "retried": retried })).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// `DELETE /dlq/{entry_id}` — removes the entry; `{"deleted": false}` when
/// absent (pyfly returns 200 with the flag, never 404).
async fn delete_dlq(State(api): State<OrchestrationApi>, Path(entry_id): Path<String>) -> Response {
    match api.dlq.delete(&entry_id).await {
        Ok(deleted) => Json(serde_json::json!({ "deleted": deleted })).into_response(),
        Err(err) => internal_error(&err.to_string()),
    }
}

/// Request body of `POST /workflow/signal` — pyfly's `SignalRequest`.
#[derive(Debug, Deserialize)]
struct SignalRequest {
    correlation_id: String,
    signal: String,
    #[serde(default)]
    payload: serde_json::Value,
}

/// `POST /workflow/signal` — delivers a signal to a waiting workflow;
/// `{"delivered": false}` for an unknown correlation id (no error raised),
/// matching pyfly.
async fn deliver_signal(State(api): State<OrchestrationApi>, body: axum::body::Bytes) -> Response {
    let req: SignalRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(err) => return (StatusCode::BAD_REQUEST, format!("{err}\n")).into_response(),
    };
    let delivered = api
        .signals
        .deliver(&req.correlation_id, &req.signal, req.payload);
    Json(serde_json::json!({ "delivered": delivered })).into_response()
}

/// `GET /definitions` — every registered saga / workflow / TCC definition.
async fn list_definitions(State(api): State<OrchestrationApi>) -> Response {
    Json(api.registry.definitions()).into_response()
}

/// 500 with a plain-text body, terminated by a newline.
fn internal_error(message: &str) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{message}\n")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExecutionPattern, ExecutionState};
    use crate::persistence::MemoryPersistence;
    use crate::{DeadLetterCapture, MemoryDeadLetterStore, Saga, Step};
    use axum::body::Body;
    use http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn api() -> (OrchestrationApi, Arc<MemoryPersistence>, Arc<SignalService>) {
        let persistence = Arc::new(MemoryPersistence::new());
        let signals = Arc::new(SignalService::new());
        let registry = Arc::new(OrchestrationRegistry::new());
        registry.register_saga(Saga::new("orderSaga").step(Step::new("a", || async { Ok(()) })));
        let dlq = DeadLetterService::new(Arc::new(MemoryDeadLetterStore::new()));
        let api = OrchestrationApi::new(
            persistence.clone() as Arc<dyn PersistenceProvider>,
            dlq,
            signals.clone(),
            registry,
        );
        (api, persistence, signals)
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        }
    }

    fn running(cid: &str) -> ExecutionState {
        let mut s = ExecutionState::new(cid, "demo", ExecutionPattern::Saga);
        s.transition(ExecutionStatus::Running);
        s
    }

    fn completed(cid: &str) -> ExecutionState {
        let mut s = ExecutionState::new(cid, "demo", ExecutionPattern::Saga);
        s.transition(ExecutionStatus::Completed);
        s
    }

    // Port of pyfly test_list_executions_returns_200 + test_routes_are_mounted.
    #[tokio::test]
    async fn list_executions_empty_returns_200() {
        let (api, _p, _s) = api();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/executions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await, serde_json::json!([]));
    }

    // Port of pyfly test_default_returns_in_flight_only (#169).
    #[tokio::test]
    async fn list_executions_defaults_to_in_flight() {
        let (api, persistence, _s) = api();
        persistence.save(running("run-active")).await.unwrap();
        persistence.save(completed("run-done")).await.unwrap();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/executions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp).await;
        let cids: Vec<&str> = body
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["correlation_id"].as_str().unwrap())
            .collect();
        assert_eq!(cids, ["run-active"]);
    }

    // Port of pyfly test_explicit_status_filter_still_works.
    #[tokio::test]
    async fn list_executions_status_filter() {
        let (api, persistence, _s) = api();
        persistence.save(running("run-active")).await.unwrap();
        persistence.save(completed("run-done")).await.unwrap();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/executions?status=COMPLETED")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["correlation_id"], "run-done");
    }

    // Port of pyfly test_get_unknown_execution_returns_no_content.
    #[tokio::test]
    async fn get_unknown_execution_returns_204() {
        let (api, _p, _s) = api();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/executions/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    // Port of pyfly test_count_endpoint_reflects_captures (#167) +
    // test_list_dlq_returns_200.
    #[tokio::test]
    async fn dlq_count_and_list() {
        let (api, _p, _s) = api();
        api.dlq
            .capture(DeadLetterCapture::new("x", "c1", "boom"))
            .await
            .unwrap();
        api.dlq
            .capture(DeadLetterCapture::new("y", "c2", "boom2"))
            .await
            .unwrap();
        let app = router(api);
        let count = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/dlq/count")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(count).await, serde_json::json!({"count": 2}));
        let list = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/dlq")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(list).await.as_array().unwrap().len(), 2);
    }

    // Port of pyfly test_dlq_delete_unknown_returns_200.
    #[tokio::test]
    async fn delete_unknown_dlq_returns_flag() {
        let (api, _p, _s) = api();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/orchestration/dlq/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await, serde_json::json!({"deleted": false}));
    }

    // Port of pyfly test_workflow_signal_binds_json_body.
    #[tokio::test]
    async fn signal_unknown_correlation_not_delivered() {
        let (api, _p, _s) = api();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/orchestration/workflow/signal")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"correlation_id":"unknown","signal":"approve","payload":{"ok":true}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            body_json(resp).await,
            serde_json::json!({"delivered": false})
        );
    }

    // Signal delivery to a real waiter resumes it.
    #[tokio::test]
    async fn signal_delivers_to_waiting_workflow() {
        let (api, _p, signals) = api();
        let _rx = signals.subscribe("run-9", "approve");
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/orchestration/workflow/signal")
                    .body(Body::from(
                        r#"{"correlation_id":"run-9","signal":"approve","payload":null}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            body_json(resp).await,
            serde_json::json!({"delivered": true})
        );
    }

    // Definitions listing exposes registered names for the admin crate.
    #[tokio::test]
    async fn definitions_listing() {
        let (api, _p, _s) = api();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/definitions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp).await;
        assert_eq!(body[0]["name"], "orderSaga");
        assert_eq!(body[0]["pattern"], "SAGA");
    }

    // A bad status query yields 400.
    #[tokio::test]
    async fn bad_status_query_is_400() {
        let (api, _p, _s) = api();
        let app = router(api);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/orchestration/executions?status=BOGUS")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
