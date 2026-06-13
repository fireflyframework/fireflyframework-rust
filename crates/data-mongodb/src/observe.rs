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

//! Optional actuator integration (feature `actuator`): a MongoDB
//! [`HealthIndicator`](firefly_actuator::HealthIndicator).
//!
//! [`MongoHealthIndicator`] is the document-store counterpart of the
//! relational adapter's `SqlxHealthIndicator` and the Rust port of pyfly's
//! database health probe: it issues the server `ping` admin command and
//! reports `UP` (with `details.database` set to the database name) on
//! success, `DOWN` (with the error) on failure — the `db` component on
//! `GET /actuator/health`.

use std::time::Instant;

use async_trait::async_trait;
use firefly_actuator::{HealthIndicator, HealthResult, HealthStatus};
use mongodb::bson::doc;
use mongodb::Database;

/// A MongoDB [`HealthIndicator`] — `UP` iff the server `ping` succeeds.
///
/// Construct it over a [`mongodb::Database`]; it runs `{ ping: 1 }` and
/// reports the database name on `details.database`. Registered as the `db`
/// component on `GET /actuator/health`.
#[derive(Clone)]
pub struct MongoHealthIndicator {
    database: Database,
    name: String,
}

impl MongoHealthIndicator {
    /// Builds the indicator over `database`, named `db` (the conventional
    /// Spring Boot / pyfly component name).
    pub fn new(database: Database) -> Self {
        MongoHealthIndicator {
            database,
            name: "db".to_string(),
        }
    }

    /// Builds the indicator with a custom component `name` — for services
    /// that wire more than one database and want each probed under a distinct
    /// name (the named-datasource health path).
    pub fn named(database: Database, name: impl Into<String>) -> Self {
        MongoHealthIndicator {
            database,
            name: name.into(),
        }
    }
}

#[async_trait]
impl HealthIndicator for MongoHealthIndicator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        let started = Instant::now();
        match self.database.run_command(doc! { "ping": 1 }).await {
            Ok(_) => {
                let mut details = serde_json::Map::new();
                details.insert(
                    "database".to_string(),
                    serde_json::Value::String(self.database.name().to_string()),
                );
                let mut r = HealthResult::new(HealthStatus::Up).with_details(details);
                r.duration = started.elapsed();
                r
            }
            Err(e) => {
                let mut details = serde_json::Map::new();
                let msg: String = e.to_string().chars().take(200).collect();
                details.insert("error".to_string(), serde_json::Value::String(msg.clone()));
                let mut r = HealthResult::down(msg).with_details(details);
                r.duration = started.elapsed();
                r
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The indicator names itself `db` by default, `<name>` when overridden.
    #[tokio::test]
    async fn names_default_and_custom() {
        // No live MongoDB needed to assert the static name surface.
        let Ok(url) =
            std::env::var("FIREFLY_TEST_MONGODB_URL").or_else(|_| std::env::var("MONGODB_URL"))
        else {
            eprintln!("skipping mongo health name check: set FIREFLY_TEST_MONGODB_URL to run");
            return;
        };
        let client = mongodb::Client::with_uri_str(&url).await.expect("connect");
        let db = client.database("firefly_test");
        assert_eq!(MongoHealthIndicator::new(db.clone()).name(), "db");
        assert_eq!(
            MongoHealthIndicator::named(db, "db-reporting").name(),
            "db-reporting"
        );
    }
}
