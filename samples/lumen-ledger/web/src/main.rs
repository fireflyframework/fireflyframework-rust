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
//! `-interfaces` DTOs, the `-models` sqlx repository (an **async bean**), the
//! `-core` `@Service` / `@Mapper` / `@Component` — is assembled by
//! `container.scan()` + `init_async_beans()`; there is no composition root.
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

    /// Boots the full layered service in-process (no socket) — exactly as
    /// `main()` does — and returns the public router after asserting the
    /// cross-crate discovery wired up.
    async fn router() -> axum::Router {
        let app = firefly::FireflyApplication::new("lumen-ledger")
            .bootstrap()
            .await
            .expect("bootstrap");
        // Guard: the force-linked layer crates contributed their beans (the async
        // sqlx repository, the @Service/@Mapper/@Component, the @Configuration)
        // and the @RestController mounted.
        firefly::assert_discovered(&app.container, 8, 1);
        app.api_router
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn wallet_lifecycle_round_trips_through_every_layer() {
        std::env::set_var(
            "DATABASE_URL",
            "sqlite:file:lumen_ledger_web_it?mode=memory&cache=shared",
        );
        let app = router().await;
        std::env::remove_var("DATABASE_URL");

        // POST create — controller → service → mapper → repository (async sqlx).
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/v1/wallets")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"owner": "ada", "currency": "EUR", "openingBalance": 1000})
                            .to_string(),
                    ))
                    .unwrap(),
            )
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
            .oneshot(
                Request::get(format!("/api/v1/wallets/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // POST deposit → 1500.
        let res = app
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/wallets/{id}/deposit"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"amount": 500}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["balance"], 1500);

        // Overdraft withdraw → 422 problem.
        let res = app
            .clone()
            .oneshot(
                Request::post(format!("/api/v1/wallets/{id}/withdraw"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"amount": 100_000}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // List by owner — the derived query.
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/v1/wallets?owner=ada")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await.as_array().unwrap().len(), 1);

        // Unknown id → 404 problem (default RFC 9457).
        let res = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/wallets/{}", uuid::Uuid::new_v4()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // Blank owner → 422 (bean validation via Valid<…>).
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/v1/wallets")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"owner": "", "currency": "EUR"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn openapi_document_lists_every_dto_schema() {
        std::env::set_var(
            "DATABASE_URL",
            "sqlite:file:lumen_ledger_web_oas?mode=memory&cache=shared",
        );
        let app = router().await;
        std::env::remove_var("DATABASE_URL");

        let res = app
            .oneshot(Request::get("/v3/api-docs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let spec = body_json(res).await;
        let schemas = &spec["components"]["schemas"];
        assert!(schemas["WalletResponse"].is_object(), "response schema");
        assert!(schemas["CreateWalletRequest"].is_object(), "request schema");
        assert!(schemas["AmountRequest"].is_object(), "amount schema");
        // The enum schema (from the extended #[derive(Schema)]).
        assert_eq!(schemas["WalletStatus"]["type"], "string");
        assert!(schemas["WalletStatus"]["enum"].is_array());
    }
}
