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

//! DI / routing **introspection** endpoints — Spring Boot Actuator's
//! `/actuator/beans`, `/actuator/mappings`, and `/actuator/conditions`.
//!
//! Each renders the framework's **compile-time inventory** (the same
//! `firefly_container::{discovered, routes}` registrations the
//! `ApplicationContext` scans and the OpenAPI generator reads) — so they need no
//! live container and report the exact bean/route table the binary was built
//! with. Register them on an [`EndpointRegistry`](crate::EndpointRegistry) with
//! [`register_introspection`].

use std::collections::HashMap;

use async_trait::async_trait;
use firefly_container::{discovered, routes, Condition};
use serde_json::{json, Value};

use crate::endpoint::Endpoint;
use crate::EndpointRegistry;

/// Registers the `beans`, `mappings`, and `conditions` introspection endpoints
/// on `registry` — the Actuator DI/routing report set, in one call.
///
/// Idempotent and override-respecting: an endpoint id already present on the
/// registry (e.g. a custom `beans` endpoint a user registered) is left in place.
/// [`mount`](crate::mount) calls this automatically, so the three endpoints are
/// available on every actuator surface — served only when the
/// [`ExposureConfig`](crate::ExposureConfig) includes them, exactly as Spring
/// gates `beans`/`mappings`/`conditions` behind `exposure.include`.
pub fn register_introspection(registry: &EndpointRegistry) {
    let existing: std::collections::HashSet<String> =
        registry.all().iter().map(|e| e.id().to_string()).collect();
    if !existing.contains("beans") {
        registry.register(BeansEndpoint);
    }
    if !existing.contains("mappings") {
        registry.register(MappingsEndpoint);
    }
    if !existing.contains("conditions") {
        registry.register(ConditionsEndpoint);
    }
}

/// `GET /actuator/beans` — every DI bean discovered across the crate graph,
/// grouped Spring-style under `contexts.application.beans` keyed by bean name.
pub struct BeansEndpoint;

#[async_trait]
impl Endpoint for BeansEndpoint {
    fn id(&self) -> &str {
        "beans"
    }

    async fn handle(
        &self,
        _selector: Option<&str>,
        _query: &HashMap<String, String>,
    ) -> Option<Value> {
        let mut beans = serde_json::Map::new();
        for reg in discovered() {
            let name = if reg.bean_name.is_empty() {
                reg.type_name
            } else {
                reg.bean_name
            };
            beans.insert(
                name.to_string(),
                json!({
                    "type": reg.type_name,
                    "module": reg.module_path,
                    "scope": reg.scope.name(),
                    "stereotype": reg.stereotype.label(),
                    "primary": reg.primary,
                    "lazy": reg.lazy,
                }),
            );
        }
        Some(json!({
            "contexts": { "application": { "beans": Value::Object(beans) } }
        }))
    }
}

/// `GET /actuator/mappings` — every `#[rest_controller]` route discovered across
/// the crate graph (the Rust analog of Spring's `RequestMappingHandlerMapping`).
pub struct MappingsEndpoint;

#[async_trait]
impl Endpoint for MappingsEndpoint {
    fn id(&self) -> &str {
        "mappings"
    }

    async fn handle(
        &self,
        _selector: Option<&str>,
        _query: &HashMap<String, String>,
    ) -> Option<Value> {
        let mut mappings: Vec<Value> = routes()
            .map(|r| {
                json!({
                    "method": r.method,
                    "path": r.path,
                    "controller": r.controller,
                    "handler": r.handler,
                    "summary": r.summary,
                    "deprecated": r.deprecated,
                })
            })
            .collect();
        // A stable order (method, path) so the report is deterministic.
        mappings.sort_by(|a, b| {
            let key = |v: &Value| {
                (
                    v["method"].as_str().unwrap_or("").to_string(),
                    v["path"].as_str().unwrap_or("").to_string(),
                )
            };
            key(a).cmp(&key(b))
        });
        Some(json!({
            "contexts": { "application": { "mappings": mappings } }
        }))
    }
}

/// `GET /actuator/conditions` — the conditions guarding each conditionally
/// registered bean (`@Profile`, `@ConditionalOnProperty`, `@ConditionalOnBean`,
/// …). Only beans that *declare* a condition are listed.
pub struct ConditionsEndpoint;

#[async_trait]
impl Endpoint for ConditionsEndpoint {
    fn id(&self) -> &str {
        "conditions"
    }

    async fn handle(
        &self,
        _selector: Option<&str>,
        _query: &HashMap<String, String>,
    ) -> Option<Value> {
        let mut report = serde_json::Map::new();
        for reg in discovered() {
            let conditions = (reg.conditions)();
            if conditions.is_empty() {
                continue;
            }
            let rendered: Vec<String> = conditions.iter().map(render_condition).collect();
            report.insert(reg.type_name.to_string(), json!({ "conditions": rendered }));
        }
        Some(json!({
            "contexts": { "application": { "conditionalBeans": Value::Object(report) } }
        }))
    }
}

/// Renders one [`Condition`] as a human-readable `@ConditionalOn…`-style label.
fn render_condition(condition: &Condition) -> String {
    match condition {
        Condition::Profile(expr) => format!("@Profile(\"{expr}\")"),
        Condition::OnProperty {
            key,
            having_value,
            match_if_missing,
        } => match having_value {
            Some(value) => format!("@ConditionalOnProperty({key} = \"{value}\")"),
            None if *match_if_missing => format!("@ConditionalOnProperty({key}, matchIfMissing)"),
            None => format!("@ConditionalOnProperty({key})"),
        },
        Condition::OnClass(label) => format!("@ConditionalOnClass(\"{label}\")"),
        Condition::OnBean(ty) => format!("@ConditionalOnBean({ty})"),
        Condition::OnMissingBean(ty) => format!("@ConditionalOnMissingBean({ty})"),
        Condition::OnSingleCandidate(ty) => format!("@ConditionalOnSingleCandidate({ty})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn beans_endpoint_reports_under_the_spring_contexts_shape() {
        let value = BeansEndpoint
            .handle(None, &HashMap::new())
            .await
            .expect("beans payload");
        // The Spring-style envelope is always present, even with no beans in this
        // (library) test binary's inventory.
        assert!(
            value["contexts"]["application"]["beans"].is_object(),
            "payload: {value}"
        );
    }

    #[tokio::test]
    async fn mappings_endpoint_reports_a_mappings_array() {
        let value = MappingsEndpoint
            .handle(None, &HashMap::new())
            .await
            .expect("mappings payload");
        assert!(value["contexts"]["application"]["mappings"].is_array());
    }

    #[tokio::test]
    async fn conditions_endpoint_reports_conditional_beans() {
        let value = ConditionsEndpoint
            .handle(None, &HashMap::new())
            .await
            .expect("conditions payload");
        assert!(value["contexts"]["application"]["conditionalBeans"].is_object());
    }

    #[test]
    fn conditions_render_with_spring_labels() {
        assert_eq!(
            render_condition(&Condition::Profile("prod".into())),
            "@Profile(\"prod\")"
        );
        assert_eq!(
            render_condition(&Condition::OnProperty {
                key: "feature.cache".into(),
                having_value: Some("true".into()),
                match_if_missing: false,
            }),
            "@ConditionalOnProperty(feature.cache = \"true\")"
        );
        assert_eq!(
            render_condition(&Condition::OnMissingBean("CacheManager".into())),
            "@ConditionalOnMissingBean(CacheManager)"
        );
    }

    fn endpoint_ids() -> Vec<&'static str> {
        vec![
            BeansEndpoint.id(),
            MappingsEndpoint.id(),
            ConditionsEndpoint.id(),
        ]
    }

    #[test]
    fn introspection_ids_are_the_spring_endpoint_names() {
        assert_eq!(endpoint_ids(), vec!["beans", "mappings", "conditions"]);
    }

    // End-to-end: `mount()` auto-registers introspection, so a default actuator
    // surface serves `/actuator/beans` (the default exposure includes everything).
    #[tokio::test]
    async fn beans_is_served_through_the_mounted_actuator() {
        use axum::body::Body;
        use http::Request;
        use tower::ServiceExt;

        let app = crate::mount(crate::ActuatorConfig::default());
        let res = app
            .oneshot(Request::get("/actuator/beans").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), http::StatusCode::OK);
    }
}
