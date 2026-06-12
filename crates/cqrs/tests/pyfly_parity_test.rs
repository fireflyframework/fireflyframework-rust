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

//! Port of pyfly's CQRS test suites for the parity layer:
//! `tests/cqrs/test_authorization.py`, `test_context.py`,
//! `test_eda_cache_invalidation.py`, `test_fluent_builders.py`, plus
//! coverage for `Bus::handler_names` (pyfly's `HandlerRegistry`
//! listing) and `ExecutionContext` threading through dispatch.
//!
//! Python idioms are adapted per the porting contract: decorators and
//! kwargs-reflection become builders/closures, `AuthorizationException`
//! becomes `CqrsError::Authorization`, and the `"*"` wildcard EDA
//! subscription becomes a per-topic subscription on the in-memory
//! broker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use firefly_cqrs::{
    resolve_pattern, AuthorizationError, AuthorizationMiddleware, AuthorizationResult,
    AuthorizationSeverity, Bus, CacheInvalidationEvent, CommandBuilder, CqrsError,
    EdaCacheInvalidationBridge, ExecutionContext, Message, QueryBuilder, QueryCache,
    AUTHORIZATION_ERROR_CODE,
};
use firefly_eda::{Event, InMemoryBroker};
use serde::Serialize;

// ---- fixtures ----------------------------------------------------------

/// pyfly: `AllowedCommand` — `authorize()` returns success.
#[derive(Clone, Serialize)]
struct AllowedCommand {
    name: String,
}

impl Message for AllowedCommand {}

/// pyfly: `DeniedCommand` — `authorize()` returns a failure with a
/// denied action.
#[derive(Clone, Serialize)]
struct DeniedCommand;

impl Message for DeniedCommand {
    fn authorize(&self, _ctx: Option<&ExecutionContext>) -> AuthorizationResult {
        AuthorizationResult::failure_with(
            AuthorizationError::new("orders", "not authorized to create orders")
                .with_denied_action("CREATE"),
        )
    }
}

/// pyfly: `ContextDeniedCommand` — authorized without a context,
/// denied when one is attached (`authorize_with_context`).
#[derive(Clone, Serialize)]
struct ContextDeniedCommand;

impl Message for ContextDeniedCommand {
    fn authorize(&self, ctx: Option<&ExecutionContext>) -> AuthorizationResult {
        match ctx {
            Some(_) => AuthorizationResult::failure("orders", "context says no"),
            None => AuthorizationResult::success(),
        }
    }
}

/// pyfly: `DeniedQuery`.
#[derive(Clone, Serialize)]
struct DeniedQuery;

impl Message for DeniedQuery {
    fn authorize(&self, _ctx: Option<&ExecutionContext>) -> AuthorizationResult {
        AuthorizationResult::failure("reports", "not authorized to view reports")
    }
}

fn register_ok_handlers(bus: &Bus) {
    bus.register(|c: AllowedCommand| async move { Ok::<_, CqrsError>(c.name) });
    bus.register(|_: DeniedCommand| async move { Ok::<_, CqrsError>("created".to_string()) });
    bus.register(|_: ContextDeniedCommand| async move { Ok::<_, CqrsError>("ok".to_string()) });
    bus.register(|_: DeniedQuery| async move { Ok::<_, CqrsError>("report".to_string()) });
}

// ---- AuthorizationSeverity (pyfly TestAuthorizationSeverity) -----------

#[test]
fn severity_wire_values() {
    assert_eq!(AuthorizationSeverity::Warning.as_str(), "WARNING");
    assert_eq!(AuthorizationSeverity::Error.as_str(), "ERROR");
    assert_eq!(AuthorizationSeverity::Critical.as_str(), "CRITICAL");
    // StrEnum parity: Display and serde produce the same strings.
    assert_eq!(AuthorizationSeverity::Error.to_string(), "ERROR");
    assert_eq!(
        serde_json::to_string(&AuthorizationSeverity::Warning).unwrap(),
        "\"WARNING\""
    );
    let back: AuthorizationSeverity = serde_json::from_str("\"CRITICAL\"").unwrap();
    assert_eq!(back, AuthorizationSeverity::Critical);
}

#[test]
fn severity_default_is_error() {
    assert_eq!(
        AuthorizationSeverity::default(),
        AuthorizationSeverity::Error
    );
}

// ---- AuthorizationError (pyfly TestAuthorizationError) ------------------

#[test]
fn authorization_error_required_fields_and_defaults() {
    let error = AuthorizationError::new("orders", "access denied");
    assert_eq!(error.resource, "orders");
    assert_eq!(error.message, "access denied");
    assert_eq!(error.error_code, AUTHORIZATION_ERROR_CODE);
    assert_eq!(error.error_code, "AUTHORIZATION_ERROR");
    assert_eq!(error.severity, AuthorizationSeverity::Error);
    assert_eq!(error.denied_action, None);
}

#[test]
fn authorization_error_customisation() {
    let error = AuthorizationError::new("orders", "cannot delete")
        .with_error_code("ROLE_MISMATCH")
        .with_severity(AuthorizationSeverity::Warning)
        .with_denied_action("DELETE");
    assert_eq!(error.error_code, "ROLE_MISMATCH");
    assert_eq!(error.severity, AuthorizationSeverity::Warning);
    assert_eq!(error.denied_action.as_deref(), Some("DELETE"));
}

// ---- AuthorizationResult (pyfly TestAuthorizationResult) ----------------

#[test]
fn result_success_is_authorized() {
    let result = AuthorizationResult::success();
    assert!(result.is_authorized());
    assert!(result.errors().is_empty());
    assert_eq!(result.summary(), None);
}

#[test]
fn result_failure_is_not_authorized() {
    let result = AuthorizationResult::failure("orders", "access denied");
    assert!(!result.is_authorized());
    assert_eq!(result.errors().len(), 1);
    assert_eq!(result.errors()[0].resource, "orders");
    assert_eq!(result.errors()[0].message, "access denied");
    assert_eq!(result.errors()[0].error_code, "AUTHORIZATION_ERROR");
}

#[test]
fn result_failure_with_custom_error() {
    let result = AuthorizationResult::failure_with(
        AuthorizationError::new("orders", "denied")
            .with_error_code("INSUFFICIENT_ROLE")
            .with_denied_action("DELETE"),
    );
    assert_eq!(result.errors()[0].error_code, "INSUFFICIENT_ROLE");
    assert_eq!(result.errors()[0].denied_action.as_deref(), Some("DELETE"));
}

#[test]
fn result_combine_both_authorized() {
    let combined = AuthorizationResult::success().combine(AuthorizationResult::success());
    assert!(combined.is_authorized());
    assert!(combined.errors().is_empty());
}

#[test]
fn result_combine_either_unauthorized() {
    let combined =
        AuthorizationResult::failure("orders", "denied").combine(AuthorizationResult::success());
    assert!(!combined.is_authorized());
    assert_eq!(combined.errors().len(), 1);

    let combined =
        AuthorizationResult::success().combine(AuthorizationResult::failure("users", "denied"));
    assert!(!combined.is_authorized());
    assert_eq!(combined.errors().len(), 1);
}

#[test]
fn result_combine_both_unauthorized_merges_errors() {
    let combined = AuthorizationResult::failure("orders", "cannot read")
        .combine(AuthorizationResult::failure("users", "cannot write"));
    assert!(!combined.is_authorized());
    assert_eq!(combined.errors().len(), 2);
    let resources: Vec<&str> = combined
        .errors()
        .iter()
        .map(|e| e.resource.as_str())
        .collect();
    assert!(resources.contains(&"orders"));
    assert!(resources.contains(&"users"));
}

#[test]
fn result_error_messages() {
    let result = AuthorizationResult::failure("orders", "access denied");
    assert_eq!(result.error_messages(), vec!["orders: access denied"]);

    let combined = AuthorizationResult::failure("orders", "read denied")
        .combine(AuthorizationResult::failure("users", "write denied"));
    let messages = combined.error_messages();
    assert_eq!(messages.len(), 2);
    assert!(messages.contains(&"orders: read denied".to_string()));
    assert!(messages.contains(&"users: write denied".to_string()));

    assert!(AuthorizationResult::success().error_messages().is_empty());
}

// ---- CqrsError::Authorization (pyfly TestAuthorizationException) --------

#[test]
fn authorization_error_carries_result() {
    let err = CqrsError::authorization(AuthorizationResult::failure("orders", "denied"));
    assert!(err.is_authorization());
    let result = err.authorization_result().unwrap();
    assert!(!result.is_authorized());
    // pyfly: message derived from error_messages.
    assert!(err.to_string().contains("orders: denied"));
}

#[test]
fn authorization_error_custom_summary() {
    let err = CqrsError::authorization(
        AuthorizationResult::failure("orders", "denied").with_summary("Custom auth message"),
    );
    assert_eq!(err.to_string(), "Custom auth message");
}

#[test]
fn authorization_error_empty_result_displays_denied() {
    // pyfly: a denial with no explicit summary renders as its joined
    // error messages (the "Authorization denied" string is the fallback
    // only when there are no errors at all).
    let joined =
        AuthorizationResult::failure("a", "b").combine(AuthorizationResult::failure("c", "d"));
    assert_eq!(joined.to_string(), "a: b; c: d");
}

// ---- AuthorizationMiddleware (pyfly TestAuthorizationService) -----------

/// pyfly: `test_enabled_allows_authorized_command`.
#[tokio::test]
async fn middleware_allows_authorized_command() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    register_ok_handlers(&bus);
    let out: String = bus
        .send(AllowedCommand {
            name: "allowed".into(),
        })
        .await
        .unwrap();
    assert_eq!(out, "allowed");
}

/// pyfly: `test_enabled_denies_unauthorized_command`.
#[tokio::test]
async fn middleware_denies_unauthorized_command() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    register_ok_handlers(&bus);
    let err = bus
        .send::<DeniedCommand, String>(DeniedCommand)
        .await
        .unwrap_err();
    let result = err.authorization_result().expect("authorization error");
    assert!(!result.is_authorized());
    assert_eq!(result.errors()[0].resource, "orders");
    assert_eq!(result.errors()[0].denied_action.as_deref(), Some("CREATE"));
}

/// pyfly: `test_disabled_skips_authorization_for_command`.
#[tokio::test]
async fn middleware_disabled_skips_authorization() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::disabled());
    register_ok_handlers(&bus);
    let out: String = bus.send(DeniedCommand).await.unwrap();
    assert_eq!(out, "created");
}

/// pyfly: `test_enabled_denies_unauthorized_query`.
#[tokio::test]
async fn middleware_denies_unauthorized_query() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    register_ok_handlers(&bus);
    let err = bus
        .query::<DeniedQuery, String>(DeniedQuery)
        .await
        .unwrap_err();
    let result = err.authorization_result().unwrap();
    assert_eq!(result.errors()[0].resource, "reports");
}

/// pyfly: `test_is_enabled_property_*` / `test_default_enabled_is_true`.
#[test]
fn middleware_enablement_flags() {
    assert!(AuthorizationMiddleware::new().is_enabled());
    assert!(!AuthorizationMiddleware::disabled().is_enabled());
    assert!(AuthorizationMiddleware::with_enabled(true).is_enabled());
    assert!(!AuthorizationMiddleware::with_enabled(false).is_enabled());
    assert!(AuthorizationMiddleware::default().is_enabled());
}

/// pyfly: `test_authorize_command_with_context`.
#[tokio::test]
async fn middleware_authorizes_with_context() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    register_ok_handlers(&bus);
    let ctx = ExecutionContext::builder().with_user_id("user-1").build();
    let err = bus
        .send_with_context::<ContextDeniedCommand, String>(ContextDeniedCommand, ctx)
        .await
        .unwrap_err();
    let result = err.authorization_result().unwrap();
    assert!(result.errors()[0].message.contains("context says no"));
}

/// pyfly: `test_authorize_command_without_context_uses_authorize`.
#[tokio::test]
async fn middleware_without_context_falls_back_to_plain_authorize() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    register_ok_handlers(&bus);
    let out: String = bus.send(ContextDeniedCommand).await.unwrap();
    assert_eq!(out, "ok");
}

/// pyfly: `test_authorize_object_without_authorize_method_succeeds` —
/// the default `Message::authorize` hook authorizes everything.
#[tokio::test]
async fn middleware_default_hook_authorizes_plain_messages() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    register_ok_handlers(&bus);
    let out: String = bus.send(AllowedCommand { name: "x".into() }).await.unwrap();
    assert_eq!(out, "x");
}

// ---- ExecutionContext (pyfly TestDefaultExecutionContext) ---------------

/// pyfly: `test_default_values`.
#[test]
fn context_default_values() {
    let ctx = ExecutionContext::new();
    assert_eq!(ctx.user_id, None);
    assert_eq!(ctx.tenant_id, None);
    assert_eq!(ctx.organization_id, None);
    assert_eq!(ctx.session_id, None);
    assert_eq!(ctx.request_id, None);
    assert_eq!(ctx.source, None);
    assert_eq!(ctx.client_ip, None);
    assert_eq!(ctx.user_agent, None);
    assert!(ctx.properties.is_empty());
    assert!(ctx.feature_flags.is_empty());
}

/// pyfly: `test_get_feature_flag_*`.
#[test]
fn context_feature_flags() {
    let ctx = ExecutionContext::builder()
        .with_feature_flag("dark_mode", true)
        .with_feature_flag("beta", false)
        .build();
    assert!(ctx.get_feature_flag("dark_mode", false));
    assert!(!ctx.get_feature_flag("beta", true));
    assert!(!ctx.get_feature_flag("unknown", false));
    assert!(ctx.get_feature_flag("unknown", true));
}

/// pyfly: `test_get_property_*`.
#[test]
fn context_properties() {
    let ctx = ExecutionContext::builder()
        .with_property("region", "us-east-1")
        .build();
    assert_eq!(
        ctx.get_property("region"),
        Some(&serde_json::json!("us-east-1"))
    );
    assert_eq!(ctx.get_property("nonexistent"), None);
}

/// pyfly: `test_builder_sets_all_fields`.
#[test]
fn context_builder_sets_all_fields() {
    let ctx = ExecutionContext::builder()
        .with_user_id("user-42")
        .with_tenant_id("tenant-1")
        .with_organization_id("org-99")
        .with_session_id("sess-abc")
        .with_request_id("req-xyz")
        .with_source("web")
        .with_client_ip("192.168.1.1")
        .with_user_agent("Mozilla/5.0")
        .with_property("locale", "en_US")
        .with_feature_flag("dark_mode", true)
        .build();

    assert_eq!(ctx.user_id.as_deref(), Some("user-42"));
    assert_eq!(ctx.tenant_id.as_deref(), Some("tenant-1"));
    assert_eq!(ctx.organization_id.as_deref(), Some("org-99"));
    assert_eq!(ctx.session_id.as_deref(), Some("sess-abc"));
    assert_eq!(ctx.request_id.as_deref(), Some("req-xyz"));
    assert_eq!(ctx.source.as_deref(), Some("web"));
    assert_eq!(ctx.client_ip.as_deref(), Some("192.168.1.1"));
    assert_eq!(ctx.user_agent.as_deref(), Some("Mozilla/5.0"));
    assert_eq!(
        ctx.get_property("locale"),
        Some(&serde_json::json!("en_US"))
    );
    assert!(ctx.get_feature_flag("dark_mode", false));
}

/// pyfly: `test_builder_default_created_at`.
#[test]
fn context_builder_default_created_at() {
    let before = Utc::now();
    let ctx = ExecutionContext::builder().build();
    let after = Utc::now();
    assert!(before <= ctx.created_at && ctx.created_at <= after);
}

// ---- ExecutionContext threading through dispatch -------------------------

/// The context attached via `send_with_context` reaches handlers
/// registered with `register_with_context` (pyfly's context-aware
/// `do_handle(command, context)`).
#[tokio::test]
async fn context_threads_through_dispatch_to_handler() {
    let bus = Bus::new();
    bus.register_with_context(
        |c: AllowedCommand, ctx: Option<ExecutionContext>| async move {
            let tenant = ctx
                .and_then(|ctx| ctx.tenant_id)
                .unwrap_or_else(|| "none".into());
            Ok::<_, CqrsError>(format!("{}@{tenant}", c.name))
        },
    );

    let ctx = ExecutionContext::builder().with_tenant_id("acme").build();
    let out: String = bus
        .send_with_context(
            AllowedCommand {
                name: "alice".into(),
            },
            ctx,
        )
        .await
        .unwrap();
    assert_eq!(out, "alice@acme");

    // A plain send passes no context.
    let out: String = bus
        .send(AllowedCommand { name: "bob".into() })
        .await
        .unwrap();
    assert_eq!(out, "bob@none");
}

/// `query_with_context` is the read-side synonym.
#[tokio::test]
async fn query_with_context_threads_context() {
    let bus = Bus::new();
    bus.register_with_context(|_: DeniedQuery, ctx: Option<ExecutionContext>| async move {
        Ok::<_, CqrsError>(ctx.and_then(|c| c.user_id).unwrap_or_default())
    });
    let ctx = ExecutionContext::builder().with_user_id("user-7").build();
    let out: String = bus.query_with_context(DeniedQuery, ctx).await.unwrap();
    assert_eq!(out, "user-7");
}

// ---- handler names (pyfly HandlerRegistry listing) -----------------------

#[tokio::test]
async fn handler_names_lists_registered_types_sorted() {
    let bus = Bus::new();
    assert!(bus.handler_names().is_empty());

    bus.register(|_: DeniedQuery| async move { Ok::<_, CqrsError>(()) });
    bus.register(|c: AllowedCommand| async move { Ok::<_, CqrsError>(c.name) });
    bus.register_with_context(|_: DeniedCommand, _ctx| async move { Ok::<_, CqrsError>(()) });

    let names = bus.handler_names();
    assert_eq!(names.len(), 3);
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "names must be sorted");
    assert!(names.iter().any(|n| n.ends_with("AllowedCommand")));
    assert!(names.iter().any(|n| n.ends_with("DeniedCommand")));
    assert!(names.iter().any(|n| n.ends_with("DeniedQuery")));

    // Re-registering the same type does not duplicate the listing.
    bus.register(|c: AllowedCommand| async move { Ok::<_, CqrsError>(c.name) });
    assert_eq!(bus.handler_names().len(), 3);
}

// ---- EDA cache-invalidation bridge (pyfly test_eda_cache_invalidation) --

#[derive(Clone, Serialize)]
struct GetWidget {
    widget_id: String,
}

impl Message for GetWidget {
    fn cache_ttl(&self) -> Option<Duration> {
        Some(Duration::from_secs(300))
    }
}

fn json_event(topic: &str, event_type: &str, payload: serde_json::Value) -> Event {
    Event::new(
        topic,
        event_type,
        "test",
        Some(serde_json::to_vec(&payload).unwrap()),
    )
}

/// Seeds the cache through the bus with an explicit cache key and
/// returns the handler-call counter.
async fn seed_cached_query(bus: &Bus, cache_key: &str, widget_id: &str) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetWidget| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(format!("widget-{}", q.widget_id))
        }
    });
    let _: String = QueryBuilder::create(GetWidget {
        widget_id: widget_id.to_string(),
    })
    .with_cache_key(cache_key)
    .execute_with(bus)
    .await
    .unwrap();
    calls
}

/// pyfly: `test_matching_event_evicts_cache_key` — end-to-end through a
/// real in-memory EDA broker.
#[tokio::test]
async fn bridge_matching_event_evicts_cache_key() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = seed_cached_query(&bus, "order:42", "42").await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    bridge.register("order.updated", "order:{order_id}");
    let broker = InMemoryBroker::new();
    bridge.subscribe(&broker, "cqrs.events").await.unwrap();

    broker
        .publish(json_event(
            "cqrs.events",
            "order.updated",
            serde_json::json!({"order_id": "42"}),
        ))
        .await
        .unwrap();

    // Evicted → the next query re-runs the handler.
    let _: String = QueryBuilder::create(GetWidget {
        widget_id: "42".into(),
    })
    .with_cache_key("order:42")
    .execute_with(&bus)
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// pyfly: `test_non_matching_event_does_not_evict`.
#[tokio::test]
async fn bridge_non_matching_event_does_not_evict() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = seed_cached_query(&bus, "order:99", "99").await;

    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    bridge.register("order.updated", "order:{order_id}");
    let broker = InMemoryBroker::new();
    bridge.subscribe(&broker, "cqrs.events").await.unwrap();

    broker
        .publish(json_event(
            "cqrs.events",
            "order.deleted",
            serde_json::json!({"order_id": "99"}),
        ))
        .await
        .unwrap();

    let _: String = QueryBuilder::create(GetWidget {
        widget_id: "99".into(),
    })
    .with_cache_key("order:99")
    .execute_with(&bus)
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "entry must survive");
}

/// pyfly: `test_multiple_rules_same_event_type_all_evicted`.
#[tokio::test]
async fn bridge_multiple_rules_same_event_type_all_evicted() {
    let cache = QueryCache::new();
    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    bridge.register("order.updated", "order:{order_id}");
    bridge.register("order.updated", "customer-orders:{order_id}");

    // Track evictions through two independently cached queries.
    let bus = Bus::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetWidget| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(q.widget_id)
        }
    });
    for key in ["order:7", "customer-orders:7"] {
        let _: String = QueryBuilder::create(GetWidget {
            widget_id: "7".into(),
        })
        .with_cache_key(key)
        .execute_with(&bus)
        .await
        .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let broker = InMemoryBroker::new();
    bridge.subscribe(&broker, "cqrs.events").await.unwrap();
    broker
        .publish(json_event(
            "cqrs.events",
            "order.updated",
            serde_json::json!({"order_id": "7"}),
        ))
        .await
        .unwrap();

    for key in ["order:7", "customer-orders:7"] {
        let _: String = QueryBuilder::create(GetWidget {
            widget_id: "7".into(),
        })
        .with_cache_key(key)
        .execute_with(&bus)
        .await
        .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4, "both keys must be evicted");
}

/// pyfly: `test_missing_payload_field_leaves_placeholder_intact` — no
/// panic, the placeholder stays literal.
#[tokio::test]
async fn bridge_missing_payload_field_is_harmless() {
    let cache = QueryCache::new();
    let bridge = EdaCacheInvalidationBridge::new(cache);
    bridge.register("order.updated", "order:{order_id}");
    let broker = InMemoryBroker::new();
    bridge.subscribe(&broker, "cqrs.events").await.unwrap();

    // Publish without the expected field — must not fail.
    broker
        .publish(json_event(
            "cqrs.events",
            "order.updated",
            serde_json::json!({"customer_id": "99"}),
        ))
        .await
        .unwrap();
}

/// pyfly: `test_bridge_noop_when_no_rules_registered`.
#[tokio::test]
async fn bridge_noop_when_no_rules_registered() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = seed_cached_query(&bus, "order:1", "1").await;

    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    let broker = InMemoryBroker::new();
    bridge.subscribe(&broker, "cqrs.events").await.unwrap();
    broker
        .publish(json_event(
            "cqrs.events",
            "order.updated",
            serde_json::json!({"order_id": "1"}),
        ))
        .await
        .unwrap();

    let _: String = QueryBuilder::create(GetWidget {
        widget_id: "1".into(),
    })
    .with_cache_key("order:1")
    .execute_with(&bus)
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "nothing must be evicted");
}

/// An explicit `CacheInvalidationEvent` evicts its prefixes with no
/// registered rules — the dedicated invalidation topic.
#[tokio::test]
async fn bridge_cache_invalidation_event_evicts_prefixes_directly() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = seed_cached_query(&bus, "order:5", "5").await;

    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    let broker = InMemoryBroker::new();
    bridge.subscribe_default(&broker).await.unwrap();

    let payload = serde_json::to_value(CacheInvalidationEvent::of(["order:5"])).unwrap();
    broker
        .publish(json_event(
            firefly_cqrs::CACHE_INVALIDATION_TOPIC,
            CacheInvalidationEvent::EVENT_TYPE,
            payload,
        ))
        .await
        .unwrap();

    let _: String = QueryBuilder::create(GetWidget {
        widget_id: "5".into(),
    })
    .with_cache_key("order:5")
    .execute_with(&bus)
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "prefix must be evicted");
}

// ---- pattern resolution (pyfly TestEdaCacheInvalidationBridgeResolution)

#[test]
fn resolve_simple_placeholder() {
    assert_eq!(
        resolve_pattern("order:{order_id}", &serde_json::json!({"order_id": "42"})),
        "order:42"
    );
}

#[test]
fn resolve_multiple_placeholders() {
    assert_eq!(
        resolve_pattern(
            "tenant:{tenant_id}:order:{order_id}",
            &serde_json::json!({"tenant_id": "acme", "order_id": "7"})
        ),
        "tenant:acme:order:7"
    );
}

#[test]
fn resolve_missing_field_leaves_placeholder() {
    assert_eq!(
        resolve_pattern("order:{order_id}", &serde_json::json!({})),
        "order:{order_id}"
    );
}

#[test]
fn resolve_no_placeholders_unchanged() {
    assert_eq!(
        resolve_pattern("orders:all", &serde_json::json!({"order_id": "1"})),
        "orders:all"
    );
}

#[test]
fn resolve_numeric_field_stringifies() {
    // pyfly: str(value) — numbers resolve to their decimal rendering.
    assert_eq!(
        resolve_pattern("order:{order_id}", &serde_json::json!({"order_id": 42})),
        "order:42"
    );
}

// ---- end-to-end: bridge evicts entries cached through the bus -----------

/// pyfly: `TestEdaBridgeEvictsBusCachedEntries::test_eda_event_evicts_bus_cached_entry`.
#[tokio::test]
async fn bridge_evicts_bus_cached_entry_end_to_end() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetWidget| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(format!("Sprocket:{}", q.widget_id))
        }
    });

    let broker = InMemoryBroker::new();
    let bridge = EdaCacheInvalidationBridge::new(cache.clone());
    bridge.register("widget.updated", "widget:{widget_id}");
    bridge.subscribe(&broker, "cqrs.events").await.unwrap();

    let get = |id: &str| {
        QueryBuilder::create(GetWidget {
            widget_id: id.to_string(),
        })
        .with_cache_key(format!("widget:{id}"))
    };

    // Step 1 — run the query so the result is cached through the bus.
    let result1: String = get("w-99").execute_with(&bus).await.unwrap();
    assert_eq!(result1, "Sprocket:w-99");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Step 2 — confirm the cache hit (handler NOT called again).
    let result2: String = get("w-99").execute_with(&bus).await.unwrap();
    assert_eq!(result2, "Sprocket:w-99");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "served from cache");

    // Step 3 — fire the invalidating EDA event.
    broker
        .publish(json_event(
            "cqrs.events",
            "widget.updated",
            serde_json::json!({"widget_id": "w-99"}),
        ))
        .await
        .unwrap();

    // Step 4 — the entry is gone; the next query re-runs the handler.
    let result3: String = get("w-99").execute_with(&bus).await.unwrap();
    assert_eq!(result3, "Sprocket:w-99");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "handler must run again after EDA invalidation"
    );
}

// ---- fluent builders (pyfly test_fluent_builders) ------------------------

#[derive(Clone, Serialize, Default)]
struct CreateOrder {
    customer_id: String,
    amount: f64,
}

impl Message for CreateOrder {}

#[derive(Clone, Serialize, Default)]
struct GetOrder {
    order_id: String,
    include_details: bool,
}

impl Message for GetOrder {}

/// pyfly: `test_create_and_build_with_field` / `test_with_fields_kwargs`
/// — Rust constructs the struct and mutates via the `with` closure.
#[test]
fn command_builder_with_mutates_fields() {
    let builder = CommandBuilder::create(CreateOrder::default())
        .with(|c| c.customer_id = "cust-1".into())
        .with(|c| c.amount = 99.99);
    assert_eq!(builder.message().customer_id, "cust-1");
    assert_eq!(builder.message().amount, 99.99);
}

/// pyfly: `test_correlated_by_sets_correlation_id` /
/// `test_initiated_by_sets_user` / `test_with_metadata_adds_entries`.
#[test]
fn command_builder_metadata_setters() {
    let builder = CommandBuilder::create(CreateOrder::default())
        .correlated_by("corr-abc")
        .initiated_by("user-42")
        .with_metadata("source", "api")
        .with_metadata("version", "v2");
    let meta = builder.metadata();
    assert_eq!(meta.correlation_id.as_deref(), Some("corr-abc"));
    assert_eq!(meta.initiated_by.as_deref(), Some("user-42"));
    assert_eq!(meta.get("source"), Some(&serde_json::json!("api")));
    assert_eq!(meta.get("version"), Some(&serde_json::json!("v2")));
}

/// pyfly: `test_build_assigns_command_id` — a fresh 36-char UUID.
#[test]
fn command_builder_assigns_message_id() {
    let builder = CommandBuilder::create(CreateOrder::default());
    assert_eq!(builder.metadata().message_id.len(), 36);
    let other = CommandBuilder::create(CreateOrder::default());
    assert_ne!(builder.metadata().message_id, other.metadata().message_id);
}

/// pyfly: `test_build_assigns_default_timestamp` / `test_at_sets_timestamp`.
#[test]
fn command_builder_timestamps() {
    let builder = CommandBuilder::create(CreateOrder::default());
    let now = Utc::now();
    assert!((now - builder.metadata().timestamp).num_seconds() < 2);

    let ts = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
    let builder = CommandBuilder::create(CreateOrder::default()).at(ts);
    assert_eq!(builder.metadata().timestamp, ts);
}

/// pyfly: `test_no_correlation_id_when_not_set` /
/// `test_no_initiated_by_when_not_set`.
#[test]
fn command_builder_defaults_unset() {
    let builder = CommandBuilder::create(CreateOrder::default());
    assert_eq!(builder.metadata().correlation_id, None);
    assert_eq!(builder.metadata().initiated_by, None);
    assert!(builder.context().is_none());
}

/// pyfly: `test_execute_with_builds_and_sends` — the handler observes
/// the built command; the envelope carries the metadata.
#[tokio::test]
async fn command_builder_execute_with_dispatches() {
    let bus = Bus::new();
    let seen = Arc::new(Mutex::new(None::<String>));
    let sink = Arc::clone(&seen);
    bus.register(move |c: CreateOrder| {
        let sink = Arc::clone(&sink);
        async move {
            *sink.lock().unwrap() = Some(c.customer_id.clone());
            Ok::<_, CqrsError>("order-123".to_string())
        }
    });

    let result: String = CommandBuilder::create(CreateOrder::default())
        .with(|c| c.customer_id = "cust-exec".into())
        .correlated_by("corr-exec")
        .execute_with(&bus)
        .await
        .unwrap();

    assert_eq!(result, "order-123");
    assert_eq!(seen.lock().unwrap().as_deref(), Some("cust-exec"));
}

/// The built envelope exposes metadata and context for middleware.
#[test]
fn command_builder_build_attaches_envelope_extensions() {
    let ctx = ExecutionContext::builder().with_user_id("u1").build();
    let env = CommandBuilder::create(CreateOrder::default())
        .correlated_by("corr-1")
        .with_context(ctx)
        .build();
    assert_eq!(
        env.metadata().unwrap().correlation_id.as_deref(),
        Some("corr-1")
    );
    assert_eq!(env.context().unwrap().user_id.as_deref(), Some("u1"));
}

/// pyfly: `QueryBuilder` field + metadata setters.
#[test]
fn query_builder_setters() {
    let ts = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
    let builder = QueryBuilder::create(GetOrder::default())
        .with(|q| q.order_id = "ord-2".into())
        .with(|q| q.include_details = true)
        .correlated_by("corr-q1")
        .with_metadata("source", "dashboard")
        .at(ts);
    assert_eq!(builder.message().order_id, "ord-2");
    assert!(builder.message().include_details);
    assert_eq!(
        builder.metadata().correlation_id.as_deref(),
        Some("corr-q1")
    );
    assert_eq!(
        builder.metadata().get("source"),
        Some(&serde_json::json!("dashboard"))
    );
    assert_eq!(builder.metadata().timestamp, ts);
    assert_eq!(builder.metadata().message_id.len(), 36);
}

/// pyfly: `test_cached_sets_cacheable` — `cached_for` opts a non-cacheable
/// message into caching for this dispatch.
#[tokio::test]
async fn query_builder_cached_for_enables_caching() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    // GetOrder has no cache_ttl — normally never cached.
    bus.register(move |q: GetOrder| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(q.order_id)
        }
    });

    for _ in 0..2 {
        let _: String = QueryBuilder::create(GetOrder {
            order_id: "o1".into(),
            include_details: false,
        })
        .cached_for(Duration::from_secs(60))
        .execute_with(&bus)
        .await
        .unwrap();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second call must hit cache"
    );
}

/// pyfly: `test_cached_false_disables_caching` — `uncached` bypasses the
/// cache for a message type that opts in.
#[tokio::test]
async fn query_builder_uncached_disables_caching() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    // GetWidget declares cache_ttl = 300s.
    bus.register(move |q: GetWidget| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(q.widget_id)
        }
    });

    for _ in 0..2 {
        let _: String = QueryBuilder::create(GetWidget {
            widget_id: "w1".into(),
        })
        .uncached()
        .execute_with(&bus)
        .await
        .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "uncached must bypass");
}

/// pyfly: `test_with_cache_key_overrides_get_cache_key` — the explicit
/// key replaces the derived `<type>:<sha>` key, so `invalidate` with the
/// explicit key evicts.
#[tokio::test]
async fn query_builder_cache_key_override_is_honoured() {
    let bus = Bus::new();
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    bus.register(move |q: GetWidget| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, CqrsError>(q.widget_id)
        }
    });

    let run = || {
        QueryBuilder::create(GetWidget {
            widget_id: "w1".into(),
        })
        .with_cache_key("custom-key")
        .execute_with::<String>(&bus)
    };
    let _ = run().await.unwrap();
    let _ = run().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "cached under custom-key");

    cache.invalidate("custom-key");
    let _ = run().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "explicit key evicts");
}

/// pyfly: `test_execute_with_builds_and_queries`.
#[tokio::test]
async fn query_builder_execute_with_dispatches() {
    let bus = Bus::new();
    bus.register(
        |q: GetOrder| async move { Ok::<_, CqrsError>(format!("{}:pending", q.order_id)) },
    );
    let result: String = QueryBuilder::create(GetOrder::default())
        .with(|q| q.order_id = "ord-exec".into())
        .cached_for(Duration::from_secs(60))
        .execute_with(&bus)
        .await
        .unwrap();
    assert_eq!(result, "ord-exec:pending");
}

/// Builders thread the execution context to authorization — fluent +
/// authorization integration.
#[tokio::test]
async fn builders_thread_context_to_authorization() {
    let bus = Bus::new();
    bus.use_middleware(AuthorizationMiddleware::new());
    bus.register(|_: ContextDeniedCommand| async move { Ok::<_, CqrsError>("ok".to_string()) });

    let ctx = ExecutionContext::builder().with_user_id("user-1").build();
    let err = CommandBuilder::create(ContextDeniedCommand)
        .with_context(ctx)
        .execute_with::<String>(&bus)
        .await
        .unwrap_err();
    assert!(err.is_authorization());

    // Without a context the same command authorizes.
    let out = CommandBuilder::create(ContextDeniedCommand)
        .execute_with::<String>(&bus)
        .await
        .unwrap();
    assert_eq!(out, "ok");
}

// ---- Send + Sync bounds for the new public surface -----------------------

#[test]
fn new_public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AuthorizationMiddleware>();
    assert_send_sync::<AuthorizationResult>();
    assert_send_sync::<AuthorizationError>();
    assert_send_sync::<ExecutionContext>();
    assert_send_sync::<EdaCacheInvalidationBridge>();
    assert_send_sync::<CacheInvalidationEvent>();
    assert_send_sync::<CommandBuilder<CreateOrder>>();
    assert_send_sync::<QueryBuilder<GetOrder>>();
}
