//! REST admin surface — the Rust counterpart of the Go
//! `ruleengine/web` package (planned for v26.06 in Go; implemented
//! here).
//!
//! The stateless router exposes pure evaluation endpoints:
//!
//! | Method | Path                        | Body                                  |
//! |--------|-----------------------------|---------------------------------------|
//! | `POST` | `/api/rules/evaluate`       | `{"ruleset": <RuleSet>, "fact": {…}}` |
//! | `POST` | `/api/rules/evaluate/yaml`  | `{"yaml": "<DSL>", "fact": {…}}`      |
//!
//! Both respond `200 OK` with the JSON [`Verdict`]
//! (`{"matched": […], "actions": […]}`) or `400 Bad Request` with
//! `{"error": "<message>"}` when the YAML cannot be parsed or
//! evaluation fails (unknown operator, bad regex, non-numeric
//! comparison).
//!
//! The **service** router ([`rule_engine_service_router`]) adds named
//! ruleset management and action-executing **evaluate-by-name** on top of a
//! [`RuleEngineService`](crate::service::RuleEngineService):
//!
//! | Method | Path                           | Body / Response                                                      |
//! |--------|--------------------------------|----------------------------------------------------------------------|
//! | `PUT`  | `/api/rules/rulesets/{name}`   | body `<RuleSet>` → `200` `{"name": …}`                               |
//! | `GET`  | `/api/rules/rulesets`          | → `200` `{"names": […]}`                                             |
//! | `POST` | `/api/rules/rulesets/{name}/evaluate` | `{"fact": {…}}` → `200` outcome / `404` unknown name / `400` eval error |
//!
//! The evaluate-by-name response is the action-executed
//! [`EvaluationOutcome`](crate::service::EvaluationOutcome) projected to
//! `{"matched": […], "actions": […], "facts": {…}, "actionsExecuted": […],
//! "error": <string|null>}`.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use firefly_rule_engine::rule_engine_router;
//!
//! #[tokio::main]
//! async fn main() {
//!     let app = rule_engine_router();
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
//!     axum::serve(listener, app).await.unwrap();
//! }
//! ```

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::core::AstEvaluator;
use crate::interfaces::{Evaluator, Fact, Verdict};
use crate::models::{Action, RuleSet};
use crate::service::{RuleEngineService, ServiceError};

/// Request body of `POST /api/rules/evaluate`: an inline AST rule set
/// plus the fact to judge it against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateRequest {
    /// The rule set, in its canonical JSON projection.
    pub ruleset: RuleSet,
    /// The fact object; defaults to empty.
    #[serde(default)]
    pub fact: Fact,
}

/// Request body of `POST /api/rules/evaluate/yaml`: a YAML rule
/// document plus the fact to judge it against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateYamlRequest {
    /// The YAML rule DSL document, verbatim.
    pub yaml: String,
    /// The fact object; defaults to empty.
    #[serde(default)]
    pub fact: Fact,
}

/// JSON error envelope returned with `400 Bad Request`:
/// `{"error": "<message>"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Human-readable description of what was rejected.
    pub error: String,
}

/// Builds the rule-engine [`Router`] backed by the default
/// [`AstEvaluator`].
pub fn rule_engine_router() -> Router {
    rule_engine_router_with(Arc::new(AstEvaluator::new()))
}

/// Builds the rule-engine [`Router`] backed by a custom
/// [`Evaluator`] implementation.
pub fn rule_engine_router_with(evaluator: Arc<dyn Evaluator>) -> Router {
    Router::new()
        .route("/api/rules/evaluate", post(evaluate))
        .route("/api/rules/evaluate/yaml", post(evaluate_yaml))
        .with_state(evaluator)
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { error: message })).into_response()
}

async fn run(evaluator: &dyn Evaluator, set: &RuleSet, fact: &Fact) -> Response {
    match evaluator.evaluate(set, fact).await {
        Ok(verdict) => (StatusCode::OK, Json::<Verdict>(verdict)).into_response(),
        Err(e) => bad_request(e.to_string()),
    }
}

async fn evaluate(
    State(evaluator): State<Arc<dyn Evaluator>>,
    Json(req): Json<EvaluateRequest>,
) -> Response {
    run(evaluator.as_ref(), &req.ruleset, &req.fact).await
}

async fn evaluate_yaml(
    State(evaluator): State<Arc<dyn Evaluator>>,
    Json(req): Json<EvaluateYamlRequest>,
) -> Response {
    match RuleSet::from_yaml(&req.yaml) {
        Ok(set) => run(evaluator.as_ref(), &set, &req.fact).await,
        Err(e) => bad_request(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Service router — named ruleset management + evaluate-by-name
// ---------------------------------------------------------------------------

/// Request body of `POST /api/rules/rulesets/{name}/evaluate`: just the fact
/// to evaluate the named ruleset against.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvaluateByNameRequest {
    /// The fact object; defaults to empty.
    #[serde(default)]
    pub fact: Fact,
}

/// Response body of the evaluate-by-name endpoint — the action-executed
/// [`EvaluationOutcome`](crate::service::EvaluationOutcome) projected to the
/// REST wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluateByNameResponse {
    /// Ids of rules whose logic evaluated true, in firing order.
    pub matched: Vec<String>,
    /// The matched actions, in firing order (the pure [`Verdict`] list).
    pub actions: Vec<Action>,
    /// The fact context after every matched action has been applied.
    pub facts: Fact,
    /// The actions that executed without error, in firing order.
    #[serde(rename = "actionsExecuted")]
    pub actions_executed: Vec<Action>,
    /// `"; "`-joined per-action failures, or `null` when all succeeded.
    pub error: Option<String>,
}

/// Response body of `PUT /api/rules/rulesets/{name}`: the registered name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredResponse {
    /// The [`RuleSet::name`] the ruleset was registered under.
    pub name: String,
}

/// Response body of `GET /api/rules/rulesets`: the registered names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSetNamesResponse {
    /// The names of every registered ruleset.
    pub names: Vec<String>,
}

/// Builds the **service** [`Router`] for named ruleset management and
/// action-executing evaluate-by-name, backed by a fresh in-memory
/// [`RuleEngineService`].
pub fn rule_engine_service_router() -> Router {
    rule_engine_service_router_with(Arc::new(RuleEngineService::in_memory()))
}

/// Builds the service [`Router`] backed by a caller-supplied
/// [`RuleEngineService`] (custom repository, evaluator, or action registry).
///
/// Routes:
/// * `PUT  /api/rules/rulesets/{name}` — register a ruleset.
/// * `GET  /api/rules/rulesets` — list registered names.
/// * `POST /api/rules/rulesets/{name}/evaluate` — evaluate by name,
///   executing the matched actions over the request fact.
pub fn rule_engine_service_router_with(service: Arc<RuleEngineService>) -> Router {
    Router::new()
        .route("/api/rules/rulesets", get(list_rulesets))
        .route("/api/rules/rulesets/:name", put(register_ruleset))
        .route("/api/rules/rulesets/:name/evaluate", post(evaluate_by_name))
        .with_state(service)
}

async fn register_ruleset(
    State(service): State<Arc<RuleEngineService>>,
    Path(name): Path<String>,
    Json(mut ruleset): Json<RuleSet>,
) -> Response {
    // The path segment is authoritative for the registration key, so a
    // body whose `name` disagrees is normalised to the URL.
    ruleset.name = name.clone();
    service.register(ruleset).await;
    (StatusCode::OK, Json(RegisteredResponse { name })).into_response()
}

async fn list_rulesets(State(service): State<Arc<RuleEngineService>>) -> Response {
    let mut names: Vec<String> = service.list().await.into_iter().map(|r| r.name).collect();
    names.sort();
    (StatusCode::OK, Json(RuleSetNamesResponse { names })).into_response()
}

async fn evaluate_by_name(
    State(service): State<Arc<RuleEngineService>>,
    Path(name): Path<String>,
    Json(req): Json<EvaluateByNameRequest>,
) -> Response {
    match service.evaluate_by_name(&name, &req.fact).await {
        Ok(outcome) => (
            StatusCode::OK,
            Json(EvaluateByNameResponse {
                matched: outcome.verdict.matched,
                actions: outcome.verdict.actions,
                facts: outcome.facts,
                actions_executed: outcome.actions_executed,
                error: outcome.error,
            }),
        )
            .into_response(),
        Err(ServiceError::RuleSetNotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: format!("ruleset {name:?} not found"),
            }),
        )
            .into_response(),
        Err(ServiceError::Eval(e)) => bad_request(e.to_string()),
    }
}
