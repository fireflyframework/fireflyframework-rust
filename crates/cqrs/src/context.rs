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

//! The execution context threaded through command/query dispatch.
//!
//! The Rust port of pyfly's `pyfly.cqrs.context.execution_context` —
//! Java's `ExecutionContext` interface and `DefaultExecutionContext`.
//! Python uses a Protocol plus a frozen dataclass; Rust needs neither:
//! a plain owned struct with a fluent [`ExecutionContextBuilder`] covers
//! both halves.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Context propagated across the command/query pipeline.
///
/// Carries user identity, tenant info, request metadata, feature flags,
/// and arbitrary properties. Attach one to a dispatch with
/// [`Bus::send_with_context`](crate::Bus::send_with_context) /
/// [`Bus::query_with_context`](crate::Bus::query_with_context) (or a
/// fluent builder's `with_context`), read it back inside
/// [`Message::authorize`](crate::Message::authorize) or a handler
/// registered via
/// [`Bus::register_with_context`](crate::Bus::register_with_context).
///
/// Mirrors pyfly's frozen `DefaultExecutionContext`: build instances via
/// [`ExecutionContext::builder`] and treat them as immutable snapshots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionContext {
    /// Authenticated user id, if any.
    pub user_id: Option<String>,
    /// Tenant the request executes under.
    pub tenant_id: Option<String>,
    /// Organization the request executes under.
    pub organization_id: Option<String>,
    /// Session identifier.
    pub session_id: Option<String>,
    /// Request identifier (usually the inbound HTTP request id).
    pub request_id: Option<String>,
    /// Logical origin of the request, e.g. `"web"` or `"batch"`.
    pub source: Option<String>,
    /// Client IP address as reported by the edge.
    pub client_ip: Option<String>,
    /// Client user agent string.
    pub user_agent: Option<String>,
    /// Creation timestamp (UTC) — pyfly's `created_at`.
    pub created_at: DateTime<Utc>,
    /// Arbitrary extension properties.
    pub properties: HashMap<String, serde_json::Value>,
    /// Feature-flag snapshot for the request.
    pub feature_flags: HashMap<String, bool>,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self {
            user_id: None,
            tenant_id: None,
            organization_id: None,
            session_id: None,
            request_id: None,
            source: None,
            client_ip: None,
            user_agent: None,
            created_at: Utc::now(),
            properties: HashMap::new(),
            feature_flags: HashMap::new(),
        }
    }
}

impl ExecutionContext {
    /// Returns an empty context stamped with the current time — pyfly's
    /// `DefaultExecutionContext()` with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a fluent [`ExecutionContextBuilder`] — pyfly's
    /// `ExecutionContextBuilder()`.
    pub fn builder() -> ExecutionContextBuilder {
        ExecutionContextBuilder::default()
    }

    /// Looks up a feature flag, falling back to `default` when the flag
    /// is absent — pyfly's `get_feature_flag(name, default)`.
    pub fn get_feature_flag(&self, name: &str, default: bool) -> bool {
        self.feature_flags.get(name).copied().unwrap_or(default)
    }

    /// Looks up an extension property — pyfly's `get_property(key)`,
    /// `None` when absent.
    pub fn get_property(&self, key: &str) -> Option<&serde_json::Value> {
        self.properties.get(key)
    }
}

/// Fluent builder for [`ExecutionContext`] — the Rust spelling of
/// pyfly's `ExecutionContextBuilder`.
///
/// ```
/// use firefly_cqrs::ExecutionContext;
///
/// let ctx = ExecutionContext::builder()
///     .with_user_id("user-42")
///     .with_tenant_id("tenant-1")
///     .with_feature_flag("dark_mode", true)
///     .build();
/// assert_eq!(ctx.user_id.as_deref(), Some("user-42"));
/// assert!(ctx.get_feature_flag("dark_mode", false));
/// ```
#[derive(Clone, Debug, Default)]
pub struct ExecutionContextBuilder {
    user_id: Option<String>,
    tenant_id: Option<String>,
    organization_id: Option<String>,
    session_id: Option<String>,
    request_id: Option<String>,
    source: Option<String>,
    client_ip: Option<String>,
    user_agent: Option<String>,
    created_at: Option<DateTime<Utc>>,
    properties: HashMap<String, serde_json::Value>,
    feature_flags: HashMap<String, bool>,
}

impl ExecutionContextBuilder {
    /// Returns an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the authenticated user id.
    #[must_use]
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Sets the tenant id.
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Sets the organization id.
    #[must_use]
    pub fn with_organization_id(mut self, organization_id: impl Into<String>) -> Self {
        self.organization_id = Some(organization_id.into());
        self
    }

    /// Sets the session id.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Sets the request id.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Sets the logical source of the request.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Sets the client IP address.
    #[must_use]
    pub fn with_client_ip(mut self, client_ip: impl Into<String>) -> Self {
        self.client_ip = Some(client_ip.into());
        self
    }

    /// Sets the client user agent.
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Pins the creation timestamp instead of stamping `now` at build.
    #[must_use]
    pub fn with_created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = Some(created_at);
        self
    }

    /// Adds an extension property.
    #[must_use]
    pub fn with_property(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Sets a feature flag.
    #[must_use]
    pub fn with_feature_flag(mut self, name: impl Into<String>, enabled: bool) -> Self {
        self.feature_flags.insert(name.into(), enabled);
        self
    }

    /// Assembles the [`ExecutionContext`], stamping `created_at` with the
    /// current time unless [`ExecutionContextBuilder::with_created_at`]
    /// pinned one — pyfly's `build()`.
    pub fn build(self) -> ExecutionContext {
        ExecutionContext {
            user_id: self.user_id,
            tenant_id: self.tenant_id,
            organization_id: self.organization_id,
            session_id: self.session_id,
            request_id: self.request_id,
            source: self.source,
            client_ip: self.client_ip,
            user_agent: self.user_agent,
            created_at: self.created_at.unwrap_or_else(Utc::now),
            properties: self.properties,
            feature_flags: self.feature_flags,
        }
    }
}
