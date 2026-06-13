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

//! MySQL integration test — **env-gated**: it reads `FIREFLY_TEST_MYSQL_URL`
//! (fallback `MYSQL_URL`) and runs the full suite against live infra; it
//! **skips cleanly** when no URL is set, so `cargo test` stays green on a
//! bare machine. The orchestrator runs it against a real MySQL later.

#![cfg(feature = "mysql")]

mod common;

use firefly_data_sqlx::Db;

#[tokio::test]
async fn mysql_full_suite() {
    let Ok(url) = std::env::var("FIREFLY_TEST_MYSQL_URL").or_else(|_| std::env::var("MYSQL_URL"))
    else {
        eprintln!("skipping mysql_full_suite: set FIREFLY_TEST_MYSQL_URL to run");
        return;
    };

    let pool = sqlx::MySqlPool::connect(&url).await.expect("connect mysql");
    common::run_full_suite(Db::MySql(pool)).await;
}
