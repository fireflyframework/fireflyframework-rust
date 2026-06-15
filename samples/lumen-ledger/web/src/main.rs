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

//! # Lumen Ledger — `-web` (`@SpringBootApplication`)
//!
//! The deployable binary: the `@RestController` (in [`controllers`]) plus the
//! one-line `FireflyApplication` boot. The whole layered service —
//! `-interfaces` DTOs, the `-models` sqlx repository (an **async bean** with
//! `@Version` optimistic locking + an `Auditor`), the `-core` `@Service` /
//! `@Mapper` / `@Component` — is assembled by `container.scan()` +
//! `init_async_beans()`; there is no composition root.
//!
//! The one piece of required wiring is [`firefly::link!`]: it force-links each
//! layer crate so the linker keeps their `inventory` registrations (the
//! repository bean, the service/mapper/component beans, the DTO schemas) instead
//! of dead-stripping them. [`firefly::assert_discovered`] guards against a
//! forgotten crate at startup.

// LINK-TIME WIRING — DO NOT REMOVE. Force-links each layer crate so its beans,
// controllers, and schemas survive dead-code elimination into the binary.
firefly::link!(
    lumen_ledger_core,
    lumen_ledger_models,
    lumen_ledger_interfaces
);

mod controllers;

#[tokio::main]
async fn main() -> Result<(), firefly::BoxError> {
    firefly::FireflyApplication::new("lumen-ledger")
        .version(firefly::VERSION)
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    /// Serialises bootstrap across tests: each test points the repository bean at
    /// its own in-memory database via the process-global `DATABASE_URL`, so the
    /// set-env → bootstrap window must not overlap another test's.
    fn boot_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// Boots the full layered service in-process against an isolated in-memory
    /// database (`cache`), asserts the cross-crate discovery wired up, and
    /// returns the public router. The router's controller state keeps the
    /// repository (and its pool) alive after the `Bootstrapped` is dropped.
    async fn router(cache: &str) -> axum::Router {
        let app = {
            let _guard = boot_lock().lock().await;
            std::env::set_var(
                "DATABASE_URL",
                format!("sqlite:file:{cache}?mode=memory&cache=shared"),
            );
            let app = firefly::FireflyApplication::new("lumen-ledger")
                .bootstrap()
                .await
                .expect("bootstrap");
            std::env::remove_var("DATABASE_URL");
            app
        };
        // The async sqlx repository, @Service/@Mapper/@Component, @Configuration,
        // and the @RestController are all discovered cross-crate.
        firefly::assert_discovered(&app.container, 8, 1);
        app.api_router
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post(uri: &str, body: Value) -> Request<Body> {
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn wallet_lifecycle_round_trips_through_every_layer() {
        let app = router("lumen_ledger_web_lifecycle").await;

        // POST create — controller → service → mapper → repository (async sqlx).
        let res = app
            .clone()
            .oneshot(post(
                "/api/v1/wallets",
                json!({"owner": "ada", "currency": "EUR", "openingBalance": 1000}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let created = body_json(res).await;
        let id = created["id"].as_str().unwrap().to_string();
        assert!(created["accountNumber"]
            .as_str()
            .unwrap()
            .starts_with("WAL-"));
        assert_eq!(created["balance"], 1000);
        assert_eq!(created["status"], "active");

        // GET by id.
        let res = app
            .clone()
            .oneshot(get(&format!("/api/v1/wallets/{id}")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // POST deposit → 1500 (transactional + auditor bumps version).
        let res = app
            .clone()
            .oneshot(post(
                &format!("/api/v1/wallets/{id}/deposit"),
                json!({"amount": 500}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let after = body_json(res).await;
        assert_eq!(after["balance"], 1500);
        assert_eq!(after["version"], 2, "the store bumped @Version on update");

        // Overdraft withdraw → 422.
        let res = app
            .clone()
            .oneshot(post(
                &format!("/api/v1/wallets/{id}/withdraw"),
                json!({"amount": 100_000}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // List by owner — derived query.
        let res = app
            .clone()
            .oneshot(get("/api/v1/wallets?owner=ada"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await.as_array().unwrap().len(), 1);

        // Paged by status — the Pageable + Page<T> machinery.
        let res = app
            .clone()
            .oneshot(get("/api/v1/wallets/page?status=active&page=1&size=10"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let page = body_json(res).await;
        assert_eq!(page["totalElements"], 1);
        assert_eq!(page["content"].as_array().unwrap().len(), 1);

        // PATCH status → frozen; a frozen wallet then rejects a deposit (422).
        let res = app
            .clone()
            .oneshot(
                Request::patch(format!("/api/v1/wallets/{id}/status"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"status": "frozen"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["status"], "frozen");
        let res = app
            .clone()
            .oneshot(post(
                &format!("/api/v1/wallets/{id}/deposit"),
                json!({"amount": 1}),
            ))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "a frozen wallet cannot transact"
        );

        // DELETE → 204, then GET → 404.
        let res = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/wallets/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let res = app
            .clone()
            .oneshot(get(&format!("/api/v1/wallets/{id}")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn edge_inputs_render_rfc9457_problems() {
        let app = router("lumen_ledger_web_edges").await;

        // Unknown but well-formed id → 404 problem.
        let res = app
            .clone()
            .oneshot(get(&format!("/api/v1/wallets/{}", uuid::Uuid::new_v4())))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Malformed UUID path → 400 problem (the firefly::web::Path extractor).
        let res = app
            .clone()
            .oneshot(get("/api/v1/wallets/not-a-uuid"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // Missing required ?owner= → 400 problem (the firefly::web::Query extractor).
        let res = app.clone().oneshot(get("/api/v1/wallets")).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // Blank owner / bad currency → 422 (bean validation via Valid<…>).
        let res = app
            .clone()
            .oneshot(post(
                "/api/v1/wallets",
                json!({"owner": "", "currency": "eur"}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Non-positive deposit amount → 422 before the service runs.
        let created = body_json(
            app.clone()
                .oneshot(post(
                    "/api/v1/wallets",
                    json!({"owner": "bob", "currency": "USD"}),
                ))
                .await
                .unwrap(),
        )
        .await;
        let id = created["id"].as_str().unwrap();
        let res = app
            .clone()
            .oneshot(post(
                &format!("/api/v1/wallets/{id}/deposit"),
                json!({"amount": 0}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn openapi_document_lists_every_dto_schema() {
        let app = router("lumen_ledger_web_oas").await;
        let res = app.oneshot(get("/v3/api-docs")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let spec = body_json(res).await;
        let schemas = &spec["components"]["schemas"];
        assert!(schemas["WalletResponse"].is_object(), "response schema");
        assert!(schemas["CreateWalletRequest"].is_object(), "request schema");
        assert!(schemas["AmountRequest"].is_object(), "amount schema");
        assert_eq!(schemas["WalletStatus"]["type"], "string");
        assert!(schemas["WalletStatus"]["enum"].is_array());
    }
}
