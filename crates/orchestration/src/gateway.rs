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

//! Event gateway — routes inbound domain events to saga / workflow / TCC
//! starts. The Rust spelling of pyfly's `EventGateway`
//! (`pyfly.transactional.core.event_gateway`), with a
//! [`firefly_eda`] binding so broker-delivered events trigger
//! orchestrations.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use futures::future::BoxFuture;

use crate::model::TriggerMode;
use crate::registry::OrchestrationRegistry;
use crate::BoxError;

/// Async handler an [`EventTrigger`] invokes; receives the event payload
/// as JSON and answers a JSON result.
pub type TriggerHandler = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, BoxError>>
        + Send
        + Sync,
>;

/// Wraps an async closure as a [`TriggerHandler`], boxing the future.
pub fn trigger_handler<F, Fut>(f: F) -> TriggerHandler
where
    F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, BoxError>> + Send + 'static,
{
    Arc::new(move |payload| Box::pin(f(payload)))
}

/// One subscription entry inside the gateway — pyfly's `EventTrigger`.
#[derive(Clone)]
pub struct EventTrigger {
    /// The event-type string the trigger listens for.
    pub event_type: String,
    /// The orchestration the trigger starts (used by `unregister`).
    pub target: String,
    handler: TriggerHandler,
}

impl std::fmt::Debug for EventTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventTrigger")
            .field("event_type", &self.event_type)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// Maps event-type strings to orchestration entry points.
///
/// The gateway decouples external event consumers (Kafka, RabbitMQ, the
/// in-memory broker) from the engines — adapters just call
/// [`EventGateway::dispatch`], or bind a whole broker topic with
/// [`EventGateway::bind`].
#[derive(Default)]
pub struct EventGateway {
    subscriptions: RwLock<HashMap<String, Vec<EventTrigger>>>,
}

impl std::fmt::Debug for EventGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventGateway")
            .field("subscriptions", &self.list_subscriptions())
            .finish()
    }
}

impl EventGateway {
    /// Returns an empty gateway.
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Vec<EventTrigger>>> {
        self.subscriptions
            .read()
            .expect("firefly/orchestration: lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Vec<EventTrigger>>> {
        self.subscriptions
            .write()
            .expect("firefly/orchestration: lock poisoned")
    }

    /// Registers `handler` to fire whenever an event of `event_type` is
    /// dispatched; `target` names the orchestration for
    /// [`Self::unregister`] and listings.
    pub fn register(
        &self,
        event_type: impl Into<String>,
        target: impl Into<String>,
        handler: TriggerHandler,
    ) {
        let event_type = event_type.into();
        let trigger = EventTrigger {
            event_type: event_type.clone(),
            target: target.into(),
            handler,
        };
        self.write().entry(event_type).or_default().push(trigger);
    }

    /// Registers a trigger that starts the named saga from `registry` —
    /// the "broker-driven saga start". [`TriggerMode::Sync`] runs the saga
    /// inline and answers its serialized [`Outcome`](crate::Outcome)
    /// (the failure outcome on error); [`TriggerMode::Async`] spawns the
    /// run and answers `{"saga": …, "status": "PENDING"}` immediately.
    pub fn register_saga_trigger(
        &self,
        event_type: impl Into<String>,
        registry: &Arc<OrchestrationRegistry>,
        saga_name: impl Into<String>,
        mode: TriggerMode,
    ) {
        let registry = Arc::clone(registry);
        let saga_name = saga_name.into();
        let target = saga_name.clone();
        let handler = trigger_handler(move |_payload| {
            let registry = Arc::clone(&registry);
            let saga_name = saga_name.clone();
            async move {
                let Some(saga) = registry.saga(&saga_name) else {
                    return Err(format!("unknown saga {saga_name:?}").into());
                };
                match mode {
                    TriggerMode::Sync => {
                        let outcome = match saga.run().await {
                            Ok(outcome) => outcome,
                            Err(failure) => failure.outcome().clone(),
                        };
                        Ok(serde_json::to_value(outcome)?)
                    }
                    TriggerMode::Async => {
                        tokio::spawn(async move {
                            let _ = saga.run().await;
                        });
                        Ok(serde_json::json!({"saga": saga_name, "status": "PENDING"}))
                    }
                }
            }
        });
        self.register(event_type, target, handler);
    }

    /// Removes every trigger whose target is `target` — pyfly's
    /// `unregister`.
    pub fn unregister(&self, target: &str) {
        let mut subs = self.write();
        subs.retain(|_, triggers| {
            triggers.retain(|t| t.target != target);
            !triggers.is_empty()
        });
    }

    /// Invokes every trigger registered for `event_type` sequentially and
    /// collects the successful results; a failing handler is logged and
    /// skipped, never breaking dispatch to its siblings — pyfly's
    /// `dispatch`.
    pub async fn dispatch(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Vec<serde_json::Value> {
        let triggers: Vec<EventTrigger> = self.read().get(event_type).cloned().unwrap_or_default();
        let mut results = Vec::new();
        for trigger in triggers {
            match (trigger.handler)(payload.clone()).await {
                Ok(result) => results.push(result),
                Err(err) => {
                    tracing::error!(
                        event_type,
                        target = %trigger.target,
                        error = %err,
                        "event gateway: handler failed"
                    );
                }
            }
        }
        results
    }

    /// `event_type -> [target, …]` view of the current subscriptions —
    /// pyfly's `list_subscriptions`. A sorted map for deterministic
    /// output.
    pub fn list_subscriptions(&self) -> BTreeMap<String, Vec<String>> {
        self.read()
            .iter()
            .map(|(event_type, triggers)| {
                (
                    event_type.clone(),
                    triggers.iter().map(|t| t.target.clone()).collect(),
                )
            })
            .collect()
    }

    /// Subscribes the gateway to `topic` on a [`firefly_eda`] broker: every
    /// delivered [`firefly_eda::Event`] is dispatched by its `type` field,
    /// with the payload decoded as JSON (`null` when absent or not JSON).
    ///
    /// This is the wiring pyfly performs in its EDA auto-configuration —
    /// the broker adapter "just calls dispatch".
    pub async fn bind(
        self: &Arc<Self>,
        subscriber: &dyn firefly_eda::Subscriber,
        topic: &str,
    ) -> firefly_eda::EdaResult<()> {
        let gateway = Arc::clone(self);
        subscriber
            .subscribe(
                topic,
                firefly_eda::handler(move |ev: firefly_eda::Event| {
                    let gateway = Arc::clone(&gateway);
                    async move {
                        let payload = ev
                            .payload
                            .as_deref()
                            .and_then(|bytes| serde_json::from_slice(bytes).ok())
                            .unwrap_or(serde_json::Value::Null);
                        gateway.dispatch(&ev.event_type, payload).await;
                        Ok(())
                    }
                }),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Saga, Step};
    use std::sync::Mutex;
    use std::time::Duration;

    // Port of pyfly test_dispatch_invokes_handlers.
    #[tokio::test]
    async fn dispatch_invokes_handlers() {
        let gateway = EventGateway::new();
        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let log = captured.clone();
        gateway.register(
            "OrderPlaced",
            "orderSaga",
            trigger_handler(move |payload| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(payload);
                    Ok(serde_json::json!("ok"))
                }
            }),
        );
        let results = gateway
            .dispatch("OrderPlaced", serde_json::json!({"id": 1}))
            .await;
        assert_eq!(*captured.lock().unwrap(), [serde_json::json!({"id": 1})]);
        assert_eq!(results, [serde_json::json!("ok")]);
    }

    // Port of pyfly test_unregister.
    #[tokio::test]
    async fn unregister_removes_target() {
        let gateway = EventGateway::new();
        gateway.register(
            "e",
            "t",
            trigger_handler(|_| async { Ok(serde_json::Value::Null) }),
        );
        gateway.unregister("t");
        let results = gateway.dispatch("e", serde_json::Value::Null).await;
        assert!(results.is_empty());
        assert!(gateway.list_subscriptions().is_empty());
    }

    // Port of pyfly test_handler_exception_does_not_break_dispatch.
    #[tokio::test]
    async fn handler_failure_does_not_break_dispatch() {
        let gateway = EventGateway::new();
        gateway.register("e", "a", trigger_handler(|_| async { Err("boom".into()) }));
        gateway.register(
            "e",
            "b",
            trigger_handler(|_| async { Ok(serde_json::json!("ok")) }),
        );
        let results = gateway.dispatch("e", serde_json::Value::Null).await;
        assert_eq!(results, [serde_json::json!("ok")]);
    }

    #[tokio::test]
    async fn list_subscriptions_groups_targets_by_event() {
        let gateway = EventGateway::new();
        gateway.register(
            "OrderPlaced",
            "orderSaga",
            trigger_handler(|_| async { Ok(serde_json::Value::Null) }),
        );
        gateway.register(
            "OrderPlaced",
            "auditWorkflow",
            trigger_handler(|_| async { Ok(serde_json::Value::Null) }),
        );
        let subs = gateway.list_subscriptions();
        assert_eq!(subs["OrderPlaced"], ["orderSaga", "auditWorkflow"]);
    }

    // Broker-driven saga start over the in-memory firefly-eda broker.
    #[tokio::test]
    async fn broker_event_starts_registered_saga_sync() {
        let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(OrchestrationRegistry::new());
        let log = executed.clone();
        registry.register_saga(Saga::new("orderSaga").step(Step::new("reserve", move || {
            let log = log.clone();
            async move {
                log.lock().unwrap().push("reserve".to_string());
                Ok(())
            }
        })));

        let gateway = Arc::new(EventGateway::new());
        gateway.register_saga_trigger("OrderPlaced", &registry, "orderSaga", TriggerMode::Sync);

        let broker = firefly_eda::InMemoryBroker::new();
        gateway.bind(&broker, "orders").await.expect("bind");

        broker
            .publish(firefly_eda::Event::new(
                "orders",
                "OrderPlaced",
                "test",
                Some(br#"{"id": 7}"#.to_vec()),
            ))
            .await
            .expect("publish");

        assert_eq!(*executed.lock().unwrap(), ["reserve"]);
    }

    // Async trigger mode answers immediately and runs in the background.
    #[tokio::test]
    async fn saga_trigger_async_mode_answers_pending() {
        let executed = Arc::new(Mutex::new(Vec::new()));
        let registry = Arc::new(OrchestrationRegistry::new());
        let log = executed.clone();
        registry.register_saga(Saga::new("bg").step(Step::new("slow", move || {
            let log = log.clone();
            async move {
                log.lock().unwrap().push("slow".to_string());
                Ok(())
            }
        })));
        let gateway = EventGateway::new();
        gateway.register_saga_trigger("Tick", &registry, "bg", TriggerMode::Async);
        let results = gateway.dispatch("Tick", serde_json::Value::Null).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["status"], "PENDING");
        // Give the spawned run a moment to finish.
        for _ in 0..50 {
            if !executed.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(*executed.lock().unwrap(), ["slow"]);
    }

    // Unknown saga names surface as dropped (logged) dispatch results.
    #[tokio::test]
    async fn unknown_saga_yields_no_result() {
        let registry = Arc::new(OrchestrationRegistry::new());
        let gateway = EventGateway::new();
        gateway.register_saga_trigger("E", &registry, "ghost", TriggerMode::Sync);
        let results = gateway.dispatch("E", serde_json::Value::Null).await;
        assert!(results.is_empty());
    }
}
