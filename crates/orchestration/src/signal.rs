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

/// Payloads delivered before any waiter parked, keyed by
/// `(correlation_id, signal)` — pyfly's per-context `_delivered_signals`
/// buffer. A subsequent [`SignalService::wait_for`]/[`SignalService::subscribe`]
/// drains the buffer before parking, so a deliver-before-wait is not lost.
type Buffered = HashMap<String, HashMap<String, serde_json::Value>>;

#[derive(Debug, Default)]
struct Inner {
    waiters: Waiters,
    /// Buffered payloads for `(correlation_id, signal)` pairs that had no
    /// waiter when [`SignalService::deliver`] was called.
    delivered: Buffered,
}

/// Routes named signals to specific workflow executions.
///
/// A waiting step calls [`SignalService::wait_for`] (or embeds a
/// [`Node::wait_for_signal`] node); external callers hand a payload to
/// [`SignalService::deliver`] and every waiter registered under
/// `(correlation_id, signal)` resumes with it. The Python port parks the
/// execution context on an `asyncio.Event`; the Rust spelling parks the
/// task on a `oneshot` receiver.
///
/// When [`SignalService::deliver`] runs before anything is waiting, the
/// payload is buffered (mirroring pyfly's `_delivered_signals`) and the next
/// `wait_for`/`subscribe` for that `(correlation_id, signal)` consumes it
/// immediately rather than parking — avoiding a lost-wakeup when a signal
/// (e.g. an approval) arrives before the workflow step that awaits it.
#[derive(Debug, Default)]
pub struct SignalService {
    inner: Mutex<Inner>,
}

impl SignalService {
    /// Returns an empty signal service.
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .expect("firefly/orchestration: lock poisoned")
    }

    /// Registers a waiter for `(correlation_id, signal)` and returns the
    /// receiving half. Prefer [`Self::wait_for`] unless you need to select
    /// over the receiver yourself.
    ///
    /// If a payload was already buffered for `(correlation_id, signal)` by an
    /// earlier [`Self::deliver`] (deliver-before-wait), the returned receiver
    /// is pre-resolved with that buffered payload — pyfly's
    /// `wait_for_signal` draining `_delivered_signals` first.
    pub fn subscribe(
        &self,
        correlation_id: impl Into<String>,
        signal: impl Into<String>,
    ) -> oneshot::Receiver<serde_json::Value> {
        let correlation_id = correlation_id.into();
        let signal = signal.into();
        let (tx, rx) = oneshot::channel();
        let mut inner = self.locked();
        // Drain a previously buffered payload (deliver-before-wait) before
        // parking, mirroring pyfly's `if name in self._delivered_signals`.
        if let Some(by_signal) = inner.delivered.get_mut(&correlation_id) {
            if let Some(payload) = by_signal.remove(&signal) {
                if by_signal.is_empty() {
                    inner.delivered.remove(&correlation_id);
                }
                let _ = tx.send(payload);
                return rx;
            }
        }
        inner
            .waiters
            .entry(correlation_id)
            .or_default()
            .entry(signal)
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
    /// `(correlation_id, signal)`. Returns `true` when at least one live
    /// waiter consumed it.
    ///
    /// When nothing is waiting (or every waiter has already dropped its
    /// receiver), the payload is **buffered** for `(correlation_id, signal)`
    /// and `false` is returned — pyfly's `deliver_signal` storing into
    /// `_delivered_signals` and returning `False`. The next
    /// [`Self::wait_for`]/[`Self::subscribe`] for that pair then resolves
    /// immediately with the buffered payload, so a signal that arrives before
    /// the workflow step parks is not lost.
    pub fn deliver(&self, correlation_id: &str, signal: &str, payload: serde_json::Value) -> bool {
        let mut inner = self.locked();
        let mut senders = Vec::new();
        if let Some(by_signal) = inner.waiters.get_mut(correlation_id) {
            if let Some(found) = by_signal.remove(signal) {
                senders = found;
            }
            if by_signal.is_empty() {
                inner.waiters.remove(correlation_id);
            }
        }

        // Deliver to live waiters; a waiter that timed out between
        // registration and delivery just dropped its receiver.
        let mut consumed = false;
        for tx in senders {
            if tx.send(payload.clone()).is_ok() {
                consumed = true;
            }
        }
        if consumed {
            return true;
        }

        // No live waiter: buffer the payload (last write wins per pair) so a
        // later wait_for can consume it, then report `false`.
        inner
            .delivered
            .entry(correlation_id.to_string())
            .or_default()
            .insert(signal.to_string(), payload);
        false
    }

    /// Correlation ids that currently have at least one waiter **or** a
    /// buffered payload — pyfly's `list_active`. Sorted for deterministic
    /// output.
    pub fn list_active(&self) -> Vec<String> {
        let inner = self.locked();
        let mut ids: std::collections::BTreeSet<String> = inner.waiters.keys().cloned().collect();
        ids.extend(inner.delivered.keys().cloned());
        ids.into_iter().collect()
    }

    /// Discards every waiter and buffered payload of one execution; pending
    /// `wait_for` calls resolve to [`SignalError::Cancelled`] — pyfly's
    /// `unregister`.
    pub fn unregister(&self, correlation_id: &str) {
        let mut inner = self.locked();
        inner.waiters.remove(correlation_id);
        inner.delivered.remove(correlation_id);
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

    // Port of pyfly SignalService.deliver returning False when no waiter is
    // present (the payload is buffered, not consumed).
    #[tokio::test]
    async fn deliver_to_unknown_returns_false() {
        let signals = SignalService::new();
        assert!(!signals.deliver("nope", "approved", serde_json::Value::Null));
    }

    // Regression for Bug 1: a signal delivered *before* any waiter parks must
    // be buffered (pyfly's `_delivered_signals`), not dropped. Port of pyfly
    // tests/transactional/core/test_context.py::test_signal_buffered_when_no_waiter:
    // deliver returns False yet a subsequent wait_for resolves with the
    // buffered payload immediately rather than hanging until timeout.
    #[tokio::test]
    async fn signal_buffered_when_delivered_before_waiter() {
        let signals = SignalService::new();
        // Deliver-before-wait: no waiter exists yet.
        let consumed = signals.deliver("run-1", "approved", serde_json::json!({"by": "boss"}));
        assert!(!consumed, "deliver with no waiter returns false");
        // The buffered execution shows up as active until drained.
        assert_eq!(signals.list_active(), ["run-1"]);

        // A wait issued *after* the deliver resolves immediately with the
        // buffered payload instead of blocking until timeout/unregister.
        let payload = tokio::time::timeout(
            Duration::from_millis(200),
            signals.wait_for("run-1", "approved"),
        )
        .await
        .expect("wait_for must resolve from the buffer, not hang")
        .expect("buffered payload");
        assert_eq!(payload, serde_json::json!({"by": "boss"}));

        // The buffer is consumed: nothing left active and a second wait would
        // park (so a fresh deliver is needed again).
        assert!(signals.list_active().is_empty());
    }

    // Regression for Bug 1 at the engine layer: an approval delivered before
    // the wait_for_signal node's wave runs must still let the workflow
    // complete (no lost-wakeup), since the node drains the buffer on park.
    #[tokio::test]
    async fn wait_for_signal_node_consumes_pre_delivered_signal() {
        let signals = Arc::new(SignalService::new());
        // Signal arrives before the workflow is even started.
        assert!(!signals.deliver("run-7", "approved", serde_json::json!("manager-A")));

        let workflow = Workflow::new("approval")
            .node(crate::Node::new("submit", || async { Ok(()) }))
            .node(
                Node::wait_for_signal("approve", &signals, "run-7", "approved")
                    .depends_on(["submit"]),
            );

        tokio::time::timeout(Duration::from_millis(200), workflow.run())
            .await
            .expect("workflow must complete from the buffered signal")
            .expect("workflow should complete");
        assert!(signals.list_active().is_empty());
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
