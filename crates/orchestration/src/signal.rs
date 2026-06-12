//! Signal delivery between an external caller and a waiting workflow —
//! pyfly's `SignalService` (`pyfly.transactional.workflow.signal_service`),
//! re-expressed over keyed `tokio::sync::oneshot` channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::workflow::Node;

/// Error produced while waiting for a signal.
#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    /// The waiter was discarded before the signal arrived (the execution
    /// was unregistered or the service dropped).
    #[error("signal {signal:?} wait cancelled for {correlation_id:?}")]
    Cancelled {
        /// The execution that was waiting.
        correlation_id: String,
        /// The signal it was waiting for.
        signal: String,
    },
}

type Waiters = HashMap<String, HashMap<String, Vec<oneshot::Sender<serde_json::Value>>>>;

/// Routes named signals to specific workflow executions.
///
/// A waiting step calls [`SignalService::wait_for`] (or embeds a
/// [`Node::wait_for_signal`] node); external callers hand a payload to
/// [`SignalService::deliver`] and every waiter registered under
/// `(correlation_id, signal)` resumes with it. The Python port parks the
/// execution context on an `asyncio.Event`; the Rust spelling parks the
/// task on a `oneshot` receiver.
#[derive(Debug, Default)]
pub struct SignalService {
    waiters: Mutex<Waiters>,
}

impl SignalService {
    /// Returns an empty signal service.
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Waiters> {
        self.waiters
            .lock()
            .expect("firefly/orchestration: lock poisoned")
    }

    /// Registers a waiter for `(correlation_id, signal)` and returns the
    /// receiving half. Prefer [`Self::wait_for`] unless you need to select
    /// over the receiver yourself.
    pub fn subscribe(
        &self,
        correlation_id: impl Into<String>,
        signal: impl Into<String>,
    ) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        self.locked()
            .entry(correlation_id.into())
            .or_default()
            .entry(signal.into())
            .or_default()
            .push(tx);
        rx
    }

    /// Parks the caller until `signal` is delivered to `correlation_id`,
    /// resolving to the delivered payload.
    pub async fn wait_for(
        &self,
        correlation_id: &str,
        signal: &str,
    ) -> Result<serde_json::Value, SignalError> {
        let rx = self.subscribe(correlation_id, signal);
        rx.await.map_err(|_| SignalError::Cancelled {
            correlation_id: correlation_id.to_string(),
            signal: signal.to_string(),
        })
    }

    /// Delivers `payload` to every waiter registered under
    /// `(correlation_id, signal)`. Returns `false` when nothing was
    /// waiting — pyfly's `deliver` returning `False` for unknown
    /// correlation ids.
    pub fn deliver(&self, correlation_id: &str, signal: &str, payload: serde_json::Value) -> bool {
        let senders = {
            let mut waiters = self.locked();
            let Some(by_signal) = waiters.get_mut(correlation_id) else {
                return false;
            };
            let Some(senders) = by_signal.remove(signal) else {
                return false;
            };
            if by_signal.is_empty() {
                waiters.remove(correlation_id);
            }
            senders
        };
        if senders.is_empty() {
            return false;
        }
        for tx in senders {
            // A waiter that timed out between registration and delivery just
            // drops its receiver; ignore it.
            let _ = tx.send(payload.clone());
        }
        true
    }

    /// Correlation ids that currently have at least one waiter — pyfly's
    /// `list_active`. Sorted for deterministic output.
    pub fn list_active(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.locked().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Discards every waiter of one execution; their `wait_for` calls
    /// resolve to [`SignalError::Cancelled`] — pyfly's `unregister`.
    pub fn unregister(&self, correlation_id: &str) {
        self.locked().remove(correlation_id);
    }
}

impl Node {
    /// Builds a workflow node that parks until `signal` is delivered to
    /// `correlation_id` through `signals` — the engine spelling of pyfly's
    /// `@wait_for_signal("approved")` step decorator. The delivered
    /// payload is discarded; use [`SignalService::wait_for`] directly when
    /// the payload matters.
    pub fn wait_for_signal(
        name: impl Into<String>,
        signals: &Arc<SignalService>,
        correlation_id: impl Into<String>,
        signal: impl Into<String>,
    ) -> Node {
        let signals = Arc::clone(signals);
        let correlation_id = correlation_id.into();
        let signal = signal.into();
        Node::new(name, move || {
            let signals = Arc::clone(&signals);
            let correlation_id = correlation_id.clone();
            let signal = signal.clone();
            async move {
                signals
                    .wait_for(&correlation_id, &signal)
                    .await
                    .map(|_| ())
                    .map_err(|e| -> crate::BoxError { Box::new(e) })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workflow;
    use std::time::Duration;

    // Port of pyfly TestSignalDriven::test_wait_for_signal_resumes_workflow:
    // a workflow with a wait_for_signal node completes once the signal is
    // delivered by a concurrent task.
    #[tokio::test]
    async fn wait_for_signal_resumes_workflow() {
        let signals = Arc::new(SignalService::new());
        let workflow = Workflow::new("approval")
            .node(crate::Node::new("submit", || async { Ok(()) }))
            .node(
                Node::wait_for_signal("approve", &signals, "run-1", "approved")
                    .depends_on(["submit"]),
            );

        let deliverer = {
            let signals = Arc::clone(&signals);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let active = signals.list_active();
                assert_eq!(active, ["run-1"]);
                assert!(signals.deliver("run-1", "approved", serde_json::json!("manager-A")));
            })
        };

        tokio::time::timeout(Duration::from_millis(200), workflow.run())
            .await
            .expect("workflow must resume on signal")
            .expect("workflow should complete");
        deliverer.await.expect("deliverer");
        assert!(signals.list_active().is_empty());
    }

    // Port of pyfly SignalService.deliver returning False for unknown ids.
    #[tokio::test]
    async fn deliver_to_unknown_returns_false() {
        let signals = SignalService::new();
        assert!(!signals.deliver("nope", "approved", serde_json::Value::Null));
    }

    // Rust-specific: wait_for resolves with the delivered payload.
    #[tokio::test]
    async fn wait_for_resolves_with_payload() {
        let signals = Arc::new(SignalService::new());
        let waiter = {
            let signals = Arc::clone(&signals);
            tokio::spawn(async move { signals.wait_for("run-9", "go").await })
        };
        // Let the waiter register before delivering.
        tokio::task::yield_now().await;
        for _ in 0..50 {
            if signals.deliver("run-9", "go", serde_json::json!({"ok": true})) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let payload = waiter.await.expect("join").expect("signal");
        assert_eq!(payload, serde_json::json!({"ok": true}));
    }

    // Rust-specific: a delivered signal only wakes its own key.
    #[tokio::test]
    async fn deliver_is_keyed_by_signal_name() {
        let signals = SignalService::new();
        let _rx = signals.subscribe("run-1", "approved");
        assert!(!signals.deliver("run-1", "rejected", serde_json::Value::Null));
        assert!(signals.deliver("run-1", "approved", serde_json::Value::Null));
    }

    // Port of pyfly unregister: waiters resolve with a cancellation error.
    #[tokio::test]
    async fn unregister_cancels_waiters() {
        let signals = Arc::new(SignalService::new());
        let waiter = {
            let signals = Arc::clone(&signals);
            tokio::spawn(async move { signals.wait_for("run-2", "go").await })
        };
        tokio::task::yield_now().await;
        while signals.list_active().is_empty() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        signals.unregister("run-2");
        let err = waiter.await.expect("join").expect_err("cancelled");
        assert_eq!(
            err.to_string(),
            "signal \"go\" wait cancelled for \"run-2\""
        );
        assert!(signals.list_active().is_empty());
    }
}
