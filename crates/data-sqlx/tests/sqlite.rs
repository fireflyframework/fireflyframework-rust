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

//! SQLite integration tests — these run on a **bare machine** (no external
//! server, no `#[ignore]`), exercising the full CRUD / specification /
//! pageable / auditing / soft-delete suite against both an in-memory and a
//! file-backed SQLite database.

#![cfg(feature = "sqlite")]

mod common;

use firefly_data_sqlx::Db;

/// The full suite against an in-memory SQLite database.
#[tokio::test]
async fn sqlite_in_memory_full_suite() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    common::run_full_suite(Db::Sqlite(pool)).await;
}

/// The full suite against a file-backed SQLite database in a temp dir, so
/// the persistence path (not just `:memory:`) is exercised.
#[tokio::test]
async fn sqlite_file_backed_full_suite() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("firefly_data_sqlx_{}.db", std::process::id()));
    // Start clean.
    let _ = std::fs::remove_file(&path);

    let url = format!("sqlite://{}?mode=rwc", path.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .expect("open file-backed sqlite");
    common::run_full_suite(Db::Sqlite(pool)).await;

    // Cleanup.
    let _ = std::fs::remove_file(&path);
}
