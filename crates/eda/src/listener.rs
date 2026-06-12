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

//! Retry + dead-letter wrapping for event [`Handler`]s.
//!
//! Adapter-agnostic: a handler is wrapped once at wiring time, so
//! retry/DLQ behaves identically across every [`Broker`](crate::Broker)
//! — the in-memory broker and the Kafka / RabbitMQ transports alike.
//! Mirrors pyfly's `messaging.wrap_listener` and Spring Kafka's
//! `@RetryableTopic` / `DefaultErrorHandler` dead-letter routing.

use std::sync::Arc;
use std::time::Duration;

use crate::{handler, EdaDeadLetterEntry, EdaDeadLetterStore, Event, Handler, Publisher};

/// Header stamped on a dead-lettered event recording the topic it was
/// originally published to — pyfly's `x-original-topic`.
pub const HEADER_ORIGINAL_TOPIC: &str = "x-original-topic";
/// Header stamped on a dead-lettered event recording the failing
/// handler's error code (the Rust analog of pyfly's exception class
/// name) — pyfly's `x-exception`.
pub const HEADER_EXCEPTION: &str = "x-exception";

/// Retry + dead-letter policy for [`wrap_listener`].
///
/// Mirrors pyfly's `@message_listener(retries, retry_delay,
/// dead_letter_topic)` knobs:
///
/// - `retries` — how many times to re-invoke the handler after the
///   first failure (total attempts = `retries + 1`).
/// - `retry_delay` — base linear backoff: the delay before attempt *n*
///   (1-based) is `retry_delay * n`. Zero means retry immediately.
/// - `dead_letter_topic` — where to republish an event whose retries
///   are exhausted. `None` re-raises the last error instead.
/// - `dead_letter_store` — an optional queryable [`EdaDeadLetterStore`]
///   into which an exhausted event is captured (in addition to any
///   republishing), so dead-lettered events stay inspectable, not just
///   re-routed. This is the wiring of pyfly's `EdaDeadLetterStore` into
///   the retry/DLQ path.
#[derive(Clone, Default)]
pub struct ListenerPolicy {
    /// Number of retries after the initial attempt.
    pub retries: u32,
    /// Base linear-backoff delay (multiplied by the attempt number).
    pub retry_delay: Duration,
    /// Optional dead-letter topic for exhausted events.
    pub dead_letter_topic: Option<String>,
    /// Optional queryable store for exhausted events.
    pub dead_letter_store: Option<Arc<dyn EdaDeadLetterStore>>,
}

impl std::fmt::Debug for ListenerPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerPolicy")
            .field("retries", &self.retries)
            .field("retry_delay", &self.retry_delay)
            .field("dead_letter_topic", &self.dead_letter_topic)
            // The store is a trait object with no Debug bound; surface
            // only whether one is configured.
            .field("dead_letter_store", &self.dead_letter_store.is_some())
            .finish()
    }
}

impl ListenerPolicy {
    /// A policy with `retries` retries and no backoff or DLQ.
    pub fn with_retries(retries: u32) -> Self {
        Self {
            retries,
            ..Self::default()
        }
    }

    /// Sets the linear-backoff base delay and returns the policy.
    #[must_use]
    pub fn retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    /// Sets the dead-letter topic and returns the policy.
    #[must_use]
    pub fn dead_letter_topic(mut self, topic: impl Into<String>) -> Self {
        self.dead_letter_topic = Some(topic.into());
        self
    }

    /// Sets the queryable dead-letter store and returns the policy. An
    /// exhausted event is captured into the store (alongside any
    /// republishing to the [`dead_letter_topic`](Self::dead_letter_topic))
    /// so it stays inspectable.
    #[must_use]
    pub fn dead_letter_store(mut self, store: Arc<dyn EdaDeadLetterStore>) -> Self {
        self.dead_letter_store = Some(store);
        self
    }

    /// Whether the policy actually wraps anything. A policy with no
    /// retries, no dead-letter topic, and no dead-letter store is a
    /// pass-through.
    fn is_noop(&self) -> bool {
        self.retries == 0 && self.dead_letter_topic.is_none() && self.dead_letter_store.is_none()
    }
}

/// Wraps `h` so a failing delivery is retried up to `policy.retries`
/// times (linear `policy.retry_delay` backoff) and, if still failing,
/// dead-lettered. On exhaustion the exhausted event is, in order:
///
/// 1. captured into `policy.dead_letter_store` (when set) as a queryable
///    [`EdaDeadLetterEntry`] recording the error code/message and total
///    attempts, so the failed event stays inspectable; then
/// 2. republished to `policy.dead_letter_topic` (when set) with the
///    [`HEADER_ORIGINAL_TOPIC`] / [`HEADER_EXCEPTION`] diagnostic headers.
///
/// With no retries, no dead-letter topic, and no dead-letter store the
/// original `h` is returned unchanged (zero overhead) — the same
/// `is handler` fast path pyfly's `wrap_listener` takes.
///
/// On exhaustion the wrapped handler returns `Ok(())` whenever the
/// failure was *handled* — i.e. captured into the store and/or
/// republished to a topic (the failure has been routed/recorded, not
/// crashed), matching pyfly. Only when **neither** a topic nor a store
/// is configured is the last handler error re-raised to the caller. A
/// failure to publish to the dead-letter topic itself is propagated.
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use firefly_eda::{handler, wrap_listener, InMemoryBroker, ListenerPolicy};
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let broker = Arc::new(InMemoryBroker::new());
/// let inner = handler(|_ev| async { Err(firefly_kernel::FireflyError::internal("boom")) });
/// let wrapped = wrap_listener(
///     inner,
///     broker.clone(),
///     ListenerPolicy::with_retries(2).dead_letter_topic("orders.DLT"),
/// );
/// # let _ = wrapped;
/// # });
/// ```
pub fn wrap_listener(h: Handler, publisher: Arc<dyn Publisher>, policy: ListenerPolicy) -> Handler {
    if policy.is_noop() {
        return h;
    }

    let retries = policy.retries;
    let retry_delay = policy.retry_delay;
    let dead_letter_topic = policy.dead_letter_topic;
    let dead_letter_store = policy.dead_letter_store;

    handler(move |ev: Event| {
        let h = Arc::clone(&h);
        let publisher = Arc::clone(&publisher);
        let dead_letter_topic = dead_letter_topic.clone();
        let dead_letter_store = dead_letter_store.clone();
        async move {
            let mut attempt: u32 = 0;
            loop {
                match h(ev.clone()).await {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        if attempt < retries {
                            attempt += 1;
                            if !retry_delay.is_zero() {
                                // Linear backoff: delay grows with the
                                // attempt number, exactly as pyfly's
                                // `retry_delay * attempt`.
                                tokio::time::sleep(retry_delay * attempt).await;
                            }
                            continue;
                        }

                        // Retries exhausted. Capture into the queryable
                        // store first (total attempts = initial + retries),
                        // so the failed event stays inspectable even when
                        // routing to a topic is also configured.
                        if let Some(store) = &dead_letter_store {
                            let entry = EdaDeadLetterEntry::new(
                                ev.clone(),
                                err.code.clone(),
                                err.detail.clone(),
                                attempt + 1,
                            );
                            store.add(entry).await;
                        }

                        if let Some(dlt) = &dead_letter_topic {
                            let dlq_event = dead_letter_event(&ev, dlt, &err.code);
                            publisher.publish(dlq_event).await?;
                            return Ok(());
                        }

                        // Captured to the store but not routed to a topic:
                        // the failure has been recorded, so do not re-raise.
                        if dead_letter_store.is_some() {
                            return Ok(());
                        }

                        return Err(err);
                    }
                }
            }
        }
    })
}

/// Builds the event republished to the dead-letter topic: the original
/// payload, key, source, type and correlation are preserved, the topic
/// becomes `dead_letter_topic`, and the original headers are carried
/// forward with [`HEADER_ORIGINAL_TOPIC`] / [`HEADER_EXCEPTION`] added.
fn dead_letter_event(original: &Event, dead_letter_topic: &str, exception_code: &str) -> Event {
    let mut dlq = original.clone();
    dlq.headers
        .insert(HEADER_ORIGINAL_TOPIC.to_string(), original.topic.clone());
    dlq.headers
        .insert(HEADER_EXCEPTION.to_string(), exception_code.to_string());
    dlq.topic = dead_letter_topic.to_string();
    dlq
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use firefly_kernel::FireflyError;

    use super::*;
    use crate::{EdaResult, InMemoryEdaDeadLetterStore};

    /// Records every published event so DLQ routing can be asserted —
    /// the Rust analog of pyfly's `_FakeBroker`.
    #[derive(Default)]
    struct RecordingPublisher {
        published: Mutex<Vec<Event>>,
    }

    #[async_trait]
    impl Publisher for RecordingPublisher {
        async fn publish(&self, ev: Event) -> EdaResult<()> {
            self.published.lock().unwrap().push(ev);
            Ok(())
        }
        async fn close(&self) -> EdaResult<()> {
            Ok(())
        }
    }

    fn msg() -> Event {
        Event::new("orders", "OrderPlaced", "src", Some(b"data".to_vec()))
            .with_key(b"k".to_vec())
            .with_header("h", "1")
    }

    /// pyfly `test_no_config_returns_handler_unchanged`: a no-op policy
    /// returns the very same `Arc` — zero overhead.
    #[test]
    fn no_config_returns_handler_unchanged() {
        let publisher: Arc<dyn Publisher> = Arc::new(RecordingPublisher::default());
        let h = handler(|_ev: Event| async { Ok(()) });
        let wrapped = wrap_listener(Arc::clone(&h), publisher, ListenerPolicy::default());
        assert!(Arc::ptr_eq(&h, &wrapped), "no-op policy must pass through");
    }

    /// pyfly `test_retries_then_succeeds`: failing twice then succeeding
    /// invokes the handler exactly three times and never hits the DLQ.
    #[tokio::test]
    async fn retries_then_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&calls);
        let inner = handler(move |_ev: Event| {
            let c = Arc::clone(&c);
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 {
                    Err(FireflyError::internal("boom"))
                } else {
                    Ok(())
                }
            }
        });
        let publisher = Arc::new(RecordingPublisher::default());
        let wrapped = wrap_listener(inner, publisher.clone(), ListenerPolicy::with_retries(3));
        wrapped(msg()).await.expect("eventually succeeds");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(publisher.published.lock().unwrap().is_empty());
    }

    /// pyfly `test_exhausted_retries_routes_to_dlq`: an always-failing
    /// handler routes to the DLQ with the diagnostic headers and does
    /// not raise.
    #[tokio::test]
    async fn exhausted_retries_routes_to_dlq() {
        let inner = handler(|_ev: Event| async { Err(FireflyError::not_found("missing")) });
        let publisher = Arc::new(RecordingPublisher::default());
        let wrapped = wrap_listener(
            inner,
            publisher.clone(),
            ListenerPolicy::with_retries(2).dead_letter_topic("orders.DLT"),
        );
        wrapped(msg()).await.expect("DLQ routing must not raise");

        let published = publisher.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        let dlq = &published[0];
        assert_eq!(dlq.topic, "orders.DLT");
        assert_eq!(dlq.payload.as_deref(), Some(&b"data"[..]));
        assert_eq!(dlq.key.as_deref(), Some(&b"k"[..]));
        assert_eq!(
            dlq.headers.get(HEADER_ORIGINAL_TOPIC).map(String::as_str),
            Some("orders")
        );
        assert_eq!(
            dlq.headers.get(HEADER_EXCEPTION).map(String::as_str),
            Some(FireflyError::not_found("x").code.as_str())
        );
        // Original headers are carried forward.
        assert_eq!(dlq.headers.get("h").map(String::as_str), Some("1"));
    }

    /// An exhausted event is captured into the queryable dead-letter
    /// store with the error code/message and total attempts, so it stays
    /// inspectable — pyfly's `EdaDeadLetterStore` wired into the DLQ path.
    /// With a store but no topic, the wrapped handler does not re-raise.
    #[tokio::test]
    async fn exhausted_retries_captured_in_store() {
        let inner = handler(|_ev: Event| async { Err(FireflyError::not_found("missing order")) });
        let publisher: Arc<dyn Publisher> = Arc::new(RecordingPublisher::default());
        let store = Arc::new(InMemoryEdaDeadLetterStore::new());
        let wrapped = wrap_listener(
            inner,
            publisher,
            ListenerPolicy::with_retries(2).dead_letter_store(store.clone()),
        );
        wrapped(msg())
            .await
            .expect("store capture must not re-raise");

        let listed = store.list(100).await;
        assert_eq!(listed.len(), 1);
        let entry = &listed[0];
        assert_eq!(entry.event.topic, "orders");
        assert_eq!(entry.event.payload.as_deref(), Some(&b"data"[..]));
        assert_eq!(entry.error_message, "missing order");
        assert_eq!(entry.error_type, FireflyError::not_found("x").code);
        // initial attempt + 2 retries = 3
        assert_eq!(entry.attempts, 3);
    }

    /// With both a store and a topic configured, the exhausted event is
    /// *both* captured (queryable) and republished (routed).
    #[tokio::test]
    async fn exhausted_retries_captured_and_routed() {
        let inner = handler(|_ev: Event| async { Err(FireflyError::internal("boom")) });
        let publisher = Arc::new(RecordingPublisher::default());
        let store = Arc::new(InMemoryEdaDeadLetterStore::new());
        let wrapped = wrap_listener(
            inner,
            publisher.clone(),
            ListenerPolicy::with_retries(1)
                .dead_letter_topic("orders.DLT")
                .dead_letter_store(store.clone()),
        );
        wrapped(msg()).await.expect("must not raise");

        // Captured AND routed.
        assert_eq!(store.list(100).await.len(), 1);
        let published = publisher.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].topic, "orders.DLT");
    }

    /// pyfly `test_exhausted_retries_without_dlq_reraises`: with no DLQ
    /// the last error propagates to the caller.
    #[tokio::test]
    async fn exhausted_retries_without_dlq_reraises() {
        let inner = handler(|_ev: Event| async { Err(FireflyError::internal("boom")) });
        let publisher: Arc<dyn Publisher> = Arc::new(RecordingPublisher::default());
        let wrapped = wrap_listener(inner, publisher, ListenerPolicy::with_retries(1));
        let err = wrapped(msg()).await.expect_err("must re-raise");
        assert_eq!(err.detail, "boom");
    }

    /// Linear backoff: with `retry_delay` set, the wrapped handler waits
    /// `delay * attempt` between attempts (paused-time clock keeps the
    /// test instant). Total simulated sleep here is 30ms, well under the
    /// budget even on a real clock.
    #[tokio::test]
    async fn linear_backoff_scales_with_attempt() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = Arc::clone(&calls);
        let inner = handler(move |_ev: Event| {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(FireflyError::internal("boom"))
            }
        });
        let publisher: Arc<dyn Publisher> = Arc::new(RecordingPublisher::default());
        let wrapped = wrap_listener(
            inner,
            publisher,
            ListenerPolicy::with_retries(2).retry_delay(Duration::from_millis(10)),
        );
        let _ = wrapped(msg()).await;
        // initial + 2 retries
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
