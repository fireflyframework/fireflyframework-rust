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

//! Client-mode self-registration — the Rust rendering of pyfly's
//! `AdminClientRegistration`.
//!
//! [`AdminClient`] POSTs this application's `{name, url}` to a remote admin
//! server's `/admin/api/instances` on start, and DELETEs it on stop. Both
//! operations swallow their own errors (a down admin server never aborts
//! application startup, matching pyfly), and the two
//! [`register_hook`](AdminClient::register_hook) /
//! [`deregister_hook`](AdminClient::deregister_hook) helpers adapt them to
//! `firefly-lifecycle`'s [`on_start`](firefly_lifecycle::Application::on_start)
//! / [`on_stop`](firefly_lifecycle::Application::on_stop) signatures.

use std::sync::Arc;
use std::time::Duration;

use firefly_lifecycle::HookResult;

use crate::config::AdminClientConfig;

/// Registers (and deregisters) this application with a remote admin server —
/// pyfly's `AdminClientRegistration`.
#[derive(Clone)]
pub struct AdminClient {
    server_url: String,
    app_name: String,
    app_url: String,
    auto_register: bool,
    client: reqwest::Client,
}

impl AdminClient {
    /// Builds a client targeting `server_url`, registering this application
    /// under `app_name` at `app_url`. `auto_register` mirrors pyfly: when
    /// `false`, [`start`](Self::start) / [`stop`](Self::stop) are no-ops.
    pub fn new(
        server_url: impl Into<String>,
        app_name: impl Into<String>,
        app_url: impl Into<String>,
        auto_register: bool,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            server_url: server_url.into().trim_end_matches('/').to_string(),
            app_name: app_name.into(),
            app_url: app_url.into(),
            auto_register,
            client,
        }
    }

    /// Builds a client from an [`AdminClientConfig`] plus this application's
    /// identity.
    pub fn from_config(
        config: &AdminClientConfig,
        app_name: impl Into<String>,
        app_url: impl Into<String>,
    ) -> Self {
        Self::new(config.url.clone(), app_name, app_url, config.auto_register)
    }

    /// Self-registers when `auto_register` is on (pyfly's lifecycle `start`).
    pub async fn start(&self) {
        if self.auto_register {
            let _ = self.register().await;
        }
    }

    /// Deregisters when `auto_register` is on (pyfly's lifecycle `stop`).
    pub async fn stop(&self) {
        if self.auto_register {
            let _ = self.deregister().await;
        }
    }

    /// The instances endpoint of the configured admin server.
    fn instances_endpoint(&self) -> String {
        format!("{}/admin/api/instances", self.server_url)
    }

    /// POSTs `{name, url}` to the admin server's instance registry. Returns
    /// `true` on a 2xx response, `false` on any error or non-2xx (logged).
    pub async fn register(&self) -> bool {
        let payload = serde_json::json!({ "name": self.app_name, "url": self.app_url });
        match self
            .client
            .post(self.instances_endpoint())
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(
                    server = %self.server_url,
                    name = %self.app_name,
                    "registered with admin server",
                );
                true
            }
            Ok(resp) => {
                tracing::warn!(
                    status = resp.status().as_u16(),
                    "admin server registration failed",
                );
                false
            }
            Err(err) => {
                tracing::warn!(error = %err, server = %self.server_url, "failed to register with admin server");
                false
            }
        }
    }

    /// DELETEs this instance from the admin server's registry. Returns `true`
    /// on a 2xx response, `false` on any error or non-2xx (logged).
    pub async fn deregister(&self) -> bool {
        let endpoint = format!("{}/{}", self.instances_endpoint(), self.app_name);
        match self.client.delete(endpoint).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(server = %self.server_url, "deregistered from admin server");
                true
            }
            Ok(resp) => {
                tracing::warn!(
                    status = resp.status().as_u16(),
                    "admin server deregistration failed",
                );
                false
            }
            Err(err) => {
                tracing::warn!(error = %err, server = %self.server_url, "failed to deregister from admin server");
                false
            }
        }
    }

    /// An `on_start` hook (for
    /// [`Application::on_start`](firefly_lifecycle::Application::on_start))
    /// that self-registers. Always reports success — a down admin server
    /// never blocks startup (pyfly parity).
    pub fn register_hook(
        self: &Arc<Self>,
    ) -> impl FnOnce() -> futures::future::BoxFuture<'static, HookResult> {
        let client = Arc::clone(self);
        move || {
            Box::pin(async move {
                client.start().await;
                Ok(())
            })
        }
    }

    /// An `on_stop` hook (for
    /// [`Application::on_stop`](firefly_lifecycle::Application::on_stop)) that
    /// deregisters. Always reports success.
    pub fn deregister_hook(
        self: &Arc<Self>,
    ) -> impl FnOnce() -> futures::future::BoxFuture<'static, HookResult> {
        let client = Arc::clone(self);
        move || {
            Box::pin(async move {
                client.stop().await;
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_slash_on_server_url() {
        let client = AdminClient::new("http://admin:9000/", "orders", "http://orders:8080", true);
        assert_eq!(
            client.instances_endpoint(),
            "http://admin:9000/admin/api/instances"
        );
    }

    #[test]
    fn from_config_carries_auto_register() {
        let cfg = AdminClientConfig {
            url: "http://admin:9000".into(),
            auto_register: true,
        };
        let client = AdminClient::from_config(&cfg, "orders", "http://orders:8080");
        assert!(client.auto_register);
        assert_eq!(client.server_url, "http://admin:9000");
    }

    #[tokio::test]
    async fn start_is_noop_when_auto_register_off() {
        // url is unreachable; with auto_register off, start() must not even
        // try (and so never errors / hangs).
        let client = AdminClient::new("http://127.0.0.1:1", "orders", "http://orders:8080", false);
        client.start().await;
        client.stop().await;
    }

    #[tokio::test]
    async fn register_swallows_connection_errors() {
        // Port 1 is unbound — register() must return false, not panic.
        let client = AdminClient::new("http://127.0.0.1:1", "orders", "http://orders:8080", true);
        assert!(!client.register().await);
        assert!(!client.deregister().await);
    }
}
