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

//! `POST /actuator/refresh` — Spring Cloud's context refresh, via a
//! local [`Refresher`] trait so the actuator stays decoupled from the
//! configuration layer (the starter bridges the two).

use async_trait::async_trait;

/// Rebinds refresh-scoped state and reports what changed. Consulted by
/// `POST /actuator/refresh`, whose response is
/// `{"refreshed": [keys…]}` — pyfly's `RefreshEndpoint` over
/// `ContextRefresher`.
#[async_trait]
pub trait Refresher: Send + Sync {
    /// Performs the refresh and returns the refreshed property/bean keys.
    async fn refresh(&self) -> Vec<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRefresher;

    #[async_trait]
    impl Refresher for FakeRefresher {
        async fn refresh(&self) -> Vec<String> {
            vec!["app.timeout".into(), "app.pool-size".into()]
        }
    }

    #[tokio::test]
    async fn refresher_reports_keys() {
        let refreshed = FakeRefresher.refresh().await;
        assert_eq!(refreshed, vec!["app.timeout", "app.pool-size"]);
    }
}
