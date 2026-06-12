//! HTTP-boundary tests driven with `tower::ServiceExt::oneshot` against the
//! full [`build_router`] composition — asserting the wire shape, the NDJSON
//! / SSE framing of the reactive streaming endpoint, and the JWT/RBAC
//! enforcement, without a network socket.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use firefly_kernel::{ProblemDetail, PROBLEM_CONTENT_TYPE};
use firefly_sample_reactive_banking::build_router;
use firefly_sample_reactive_banking::domain::{AccountEvent, AccountView};
use firefly_sample_reactive_banking::security::mint_token;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &["CUSTOMER"]))
}

fn post(path: &str, body: serde_json::Value, auth: Option<&str>) -> Request<Body> {
    let mut b = Request::post(path).header("Content-Type", "application/json");
    if let Some(token) = auth {
        b = b.header("Authorization", token);
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

async fn body_bytes(res: Response) -> Vec<u8> {
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

fn content_type(res: &Response) -> String {
    res.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn open_account_returns_201_with_location_and_view() {
    let app = build_router().await;
    let res = app
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "alice", "openingBalance": 1000 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(content_type(&res), "application/json");
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let raw = body_bytes(res).await;
    assert_eq!(raw.last(), Some(&b'\n'), "trailing newline");
    let view: AccountView = serde_json::from_slice(&raw).unwrap();
    assert_eq!(view.owner, "alice");
    assert_eq!(view.balance, 1000);
    assert_eq!(location, format!("/api/v1/accounts/{}", view.id));
}

#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let app = build_router().await;
    let res = app
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "alice", "openingBalance": 1000 }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).starts_with(PROBLEM_CONTENT_TYPE));
}

#[tokio::test]
async fn open_account_validation_is_422_problem() {
    let app = build_router().await;
    let res = app
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "", "openingBalance": 1000 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(pd.detail, "owner is required");
}

#[tokio::test]
async fn get_missing_account_is_404_problem() {
    let app = build_router().await;
    let res = app
        .oneshot(get("/api/v1/accounts/acc_ghost"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(content_type(&res).starts_with(PROBLEM_CONTENT_TYPE));
}

#[tokio::test]
async fn withdraw_overdraft_is_422_insufficient_funds() {
    let app = build_router().await;
    // Open with 100 (authed).
    let opened = app
        .clone()
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "alice", "openingBalance": 100 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    let view: AccountView = serde_json::from_slice(&body_bytes(opened).await).unwrap();

    let res = app
        .oneshot(post(
            &format!("/api/v1/accounts/{}/withdraw", view.id),
            serde_json::json!({ "amount": 500 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(pd.detail, "insufficient funds");
}

#[tokio::test]
async fn transfer_compensates_with_422() {
    let app = build_router().await;
    let open = |owner: &str, bal: i64| {
        post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": owner, "openingBalance": bal }),
            Some(&bearer()),
        )
    };
    let a: AccountView = serde_json::from_slice(
        &body_bytes(app.clone().oneshot(open("alice", 100)).await.unwrap()).await,
    )
    .unwrap();
    let b: AccountView = serde_json::from_slice(
        &body_bytes(app.clone().oneshot(open("bob", 0)).await.unwrap()).await,
    )
    .unwrap();

    // Overdraw alice → bob: the saga compensates, the response is a 422.
    let res = app
        .oneshot(post(
            "/api/v1/transfers",
            serde_json::json!({ "from": a.id, "to": b.id, "amount": 5000 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(pd.detail, "insufficient funds");
}

/// A read immediately after a write reflects the new balance — the
/// mutating handlers invalidate the `GetAccount` query cache, so the 30 s
/// read-side cache never serves a stale view within the same request flow.
#[tokio::test]
async fn read_after_write_is_consistent_despite_query_cache() {
    let app = build_router().await;
    let opened = app
        .clone()
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "alice", "openingBalance": 100 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    let view: AccountView = serde_json::from_slice(&body_bytes(opened).await).unwrap();

    // Prime the read cache (balance 100).
    let r1 = app
        .clone()
        .oneshot(get(&format!("/api/v1/accounts/{}", view.id)))
        .await
        .unwrap();
    let v1: AccountView = serde_json::from_slice(&body_bytes(r1).await).unwrap();
    assert_eq!(v1.balance, 100);

    // Deposit, then read again — the cache was invalidated, so the new
    // balance (150) is returned, not the cached 100.
    app.clone()
        .oneshot(post(
            &format!("/api/v1/accounts/{}/deposit", view.id),
            serde_json::json!({ "amount": 50 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    let r2 = app
        .oneshot(get(&format!("/api/v1/accounts/{}", view.id)))
        .await
        .unwrap();
    let v2: AccountView = serde_json::from_slice(&body_bytes(r2).await).unwrap();
    assert_eq!(
        v2.balance, 150,
        "read-after-write must not serve a stale cached view"
    );
}

#[tokio::test]
async fn events_endpoint_streams_ndjson() {
    let app = build_router().await;
    let opened = app
        .clone()
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "alice", "openingBalance": 100 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    let view: AccountView = serde_json::from_slice(&body_bytes(opened).await).unwrap();
    app.clone()
        .oneshot(post(
            &format!("/api/v1/accounts/{}/deposit", view.id),
            serde_json::json!({ "amount": 50 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();

    // The events stream is public (no token) and framed as NDJSON.
    let res = app
        .oneshot(get(&format!("/api/v1/accounts/{}/events", view.id)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type(&res), "application/x-ndjson");
    let raw = body_bytes(res).await;
    let text = String::from_utf8(raw).unwrap();
    // One JSON document per non-empty line, each ending in '\n'.
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "ndjson lines: {text:?}");
    let parsed: Vec<AccountEvent> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(parsed[0].event_type, "AccountOpened");
    assert_eq!(parsed[0].amount, 100);
    assert_eq!(parsed[1].event_type, "MoneyDeposited");
    assert_eq!(parsed[1].amount, 50);
}

#[tokio::test]
async fn events_endpoint_streams_sse_when_requested() {
    let app = build_router().await;
    let opened = app
        .clone()
        .oneshot(post(
            "/api/v1/accounts",
            serde_json::json!({ "owner": "alice", "openingBalance": 100 }),
            Some(&bearer()),
        ))
        .await
        .unwrap();
    let view: AccountView = serde_json::from_slice(&body_bytes(opened).await).unwrap();

    let res = app
        .oneshot(get(&format!(
            "/api/v1/accounts/{}/events?format=sse",
            view.id
        )))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(content_type(&res), "text/event-stream");
    let text = String::from_utf8(body_bytes(res).await).unwrap();
    // One `data:` frame, terminated by a blank line.
    assert!(text.contains("data: "), "sse: {text:?}");
    assert!(text.contains("AccountOpened"), "sse: {text:?}");
}
