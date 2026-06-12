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

//! The actuator/management surface inherited from `firefly-starter-web`,
//! exercised in-process via `tower::ServiceExt::oneshot`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use firefly_sample_reactive_banking::build_app;
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn actuator_endpoints_are_mounted() {
    let app = build_app().await;
    let admin = app.web.actuator_router(Vec::new());

    for path in [
        "/actuator/health",
        "/actuator/info",
        "/actuator/metrics",
        "/actuator/version",
    ] {
        let res = admin
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn health_reports_up() {
    let app = build_app().await;
    let admin = app.web.actuator_router(Vec::new());
    let res = admin
        .oneshot(
            Request::get("/actuator/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let health: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(health["status"], "UP");
}

#[tokio::test]
async fn version_carries_app_meta() {
    let app = build_app().await;
    let admin = app.web.actuator_router(Vec::new());
    let res = admin
        .oneshot(
            Request::get("/actuator/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let out = String::from_utf8_lossy(&bytes);
    for want in ["reactive-banking", "firefly"] {
        assert!(out.contains(want), "missing {want:?} in {out}");
    }
}
