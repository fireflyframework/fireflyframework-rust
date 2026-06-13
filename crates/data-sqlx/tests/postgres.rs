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

//! PostgreSQL integration test — **env-gated**: it reads
//! `FIREFLY_TEST_POSTGRES_URL` (fallbacks `DATABASE_URL` / `POSTGRES_URL`)
//! and runs the full suite against live infra; it **skips cleanly** when no
//! URL is set, so `cargo test` stays green on a bare machine. The
//! orchestrator runs it against a real Postgres later.

#![cfg(feature = "postgres")]

mod common;

use firefly_data_sqlx::Db;

#[tokio::test]
async fn postgres_full_suite() {
    let Ok(url) = std::env::var("FIREFLY_TEST_POSTGRES_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .or_else(|_| std::env::var("POSTGRES_URL"))
    else {
        eprintln!("skipping postgres_full_suite: set FIREFLY_TEST_POSTGRES_URL to run");
        return;
    };

    let pool = sqlx::PgPool::connect(&url).await.expect("connect postgres");
    common::run_full_suite(Db::Postgres(pool)).await;
}
