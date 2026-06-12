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

//! The ingestion engine — the Rust spelling of the Go `webhooks/core`
//! package: the [`Pipeline`] (validate → dedupe → enrich → dispatch →
//! DLQ), the in-memory [`MemoryDlq`], the idempotency
//! [`EventStore`]/[`MemoryEventStore`] (pyfly parity), and the four
//! canonical signature validators.

mod event_store;
mod mime;
mod sha1;
mod util;
mod validators;

pub use event_store::{EventStore, MemoryEventStore};
pub use validators::{GitHubValidator, HmacValidator, StripeValidator, TwilioValidator};

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::WebhookError;
use crate::interfaces::{Inbound, Processor, Validator};

/// The default request header carrying a webhook's idempotency key —
/// the pyfly `WebhookProcessor` default (`idempotency_header`). When a
/// [`Pipeline`] has an [`EventStore`] registered, [`Pipeline::process`]
/// reads this header (with Go's canonical MIME casing, as stored on
/// [`Inbound::headers`](crate::Inbound::headers)) to dedupe deliveries.
pub const DEFAULT_IDEMPOTENCY_HEADER: &str = "X-Idempotency-Key";

/// The dead-letter queue contract. The default in-memory implementation
/// ([`MemoryDlq`]) buffers events for inspection; production
/// deployments supply a database- or broker-backed implementation.
#[async_trait]
pub trait Dlq: Send + Sync {
    /// Records a failed event together with the error that killed it.
    ///
    /// # Errors
    ///
    /// Implementation-specific persistence failures. The [`Pipeline`]
    /// ignores push errors, exactly as the Go port does.
    async fn push(&self, ev: Inbound, error: &WebhookError) -> Result<(), WebhookError>;
}

/// One dead-letter record: the event, the stringified processor error,
/// and the UTC instant it was dead-lettered.
#[derive(Debug, Clone)]
pub struct DlqEntry {
    /// The event that failed processing.
    pub event: Inbound,
    /// The processor error's message (Go stores `err.Error()`).
    pub err: String,
    /// When the entry was pushed (UTC).
    pub time: DateTime<Utc>,
}

/// An in-process [`Dlq`] backed by a mutex-guarded vector — the analog
/// of Go's `MemoryDLQ`.
#[derive(Debug, Default)]
pub struct MemoryDlq {
    events: std::sync::Mutex<Vec<DlqEntry>>,
}

impl MemoryDlq {
    /// Returns an empty in-memory DLQ.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of the buffered entries (Go exposes the
    /// `Events` slice directly; the Rust port hands out a copy so the
    /// internal lock never escapes).
    pub fn entries(&self) -> Vec<DlqEntry> {
        self.lock().clone()
    }

    /// Returns the number of buffered entries.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Reports whether the DLQ is empty.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<DlqEntry>> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl Dlq for MemoryDlq {
    async fn push(&self, ev: Inbound, error: &WebhookError) -> Result<(), WebhookError> {
        self.lock().push(DlqEntry {
            event: ev,
            err: error.to_string(),
            time: Utc::now(),
        });
        Ok(())
    }
}

/// The enrichment hook type: mutates every event after validation and
/// before dispatch.
type EnrichFn = Arc<dyn Fn(&mut Inbound) + Send + Sync>;

#[derive(Default)]
struct Registry {
    validators: HashMap<String, Arc<dyn Validator>>,
    processors: HashMap<String, Vec<Arc<dyn Processor>>>,
    enrich: Option<EnrichFn>,
    event_store: Option<Arc<dyn EventStore>>,
    idempotency_header: Option<String>,
}

/// The validate → dedupe → enrich → dispatch → DLQ chain that runs on
/// every inbound webhook.
///
/// Registration is thread-safe (`&self`, like Go's `sync.RWMutex`
/// guarded maps), so validators, processors, and the idempotency
/// [`EventStore`] may be installed while the ingestion endpoint is live.
/// The optional [`EventStore`] (pyfly parity) deduplicates redeliveries
/// before dispatch — see
/// [`register_event_store`](Pipeline::register_event_store).
///
/// # Example
///
/// ```
/// use std::sync::Arc;
///
/// use firefly_webhooks::{HmacValidator, MemoryDlq, Pipeline};
///
/// let dlq = Arc::new(MemoryDlq::new());
/// let pipeline = Pipeline::new(dlq);
/// pipeline.register_validator(HmacValidator::new("generic", b"s3cret"));
/// assert!(pipeline.validators().contains_key("generic"));
/// ```
pub struct Pipeline {
    registry: RwLock<Registry>,
    dlq: Option<Arc<dyn Dlq>>,
}

impl Pipeline {
    /// Returns an empty pipeline backed by `dlq` — Go's
    /// `NewPipeline(dlq)`.
    pub fn new(dlq: Arc<dyn Dlq>) -> Self {
        Self {
            registry: RwLock::new(Registry::default()),
            dlq: Some(dlq),
        }
    }

    /// Returns an empty pipeline with no DLQ (failed events are
    /// dropped after the error is returned) — Go's `NewPipeline(nil)`.
    pub fn without_dlq() -> Self {
        Self {
            registry: RwLock::new(Registry::default()),
            dlq: None,
        }
    }

    /// Installs a validator (one per provider; a later registration for
    /// the same provider replaces the earlier one).
    pub fn register_validator(&self, v: impl Validator + 'static) {
        let v: Arc<dyn Validator> = Arc::new(v);
        self.write().validators.insert(v.provider().to_owned(), v);
    }

    /// Installs a processor (multiple per provider allowed; they run in
    /// registration order).
    pub fn register_processor(&self, p: impl Processor + 'static) {
        let p: Arc<dyn Processor> = Arc::new(p);
        self.write()
            .processors
            .entry(p.provider().to_owned())
            .or_default()
            .push(p);
    }

    /// Registers an enrichment hook applied to every event after
    /// validation and before dispatch.
    pub fn enrich(&self, hook: impl Fn(&mut Inbound) + Send + Sync + 'static) {
        self.write().enrich = Some(Arc::new(hook));
    }

    /// Installs the idempotency [`EventStore`] consulted before dispatch
    /// — the Rust spelling of passing pyfly's `event_store` to
    /// `WebhookProcessor`.
    ///
    /// Once registered, [`process`](Pipeline::process) reads the
    /// idempotency key from the event's
    /// [`DEFAULT_IDEMPOTENCY_HEADER`] header (override with
    /// [`with_idempotency_header`](Pipeline::with_idempotency_header)); a
    /// key already recorded skips dispatch (the redelivery is treated as
    /// a success), and a fresh key is recorded before the processors run.
    /// Events without the header are never deduped, exactly as in pyfly.
    pub fn register_event_store(&self, store: impl EventStore + 'static) {
        self.write().event_store = Some(Arc::new(store));
    }

    /// Installs the idempotency [`EventStore`] from an existing `Arc`,
    /// so a store shared with other components (e.g. metrics) can be
    /// reused without re-wrapping.
    pub fn register_event_store_arc(&self, store: Arc<dyn EventStore>) {
        self.write().event_store = Some(store);
    }

    /// Overrides the request header [`process`](Pipeline::process) reads
    /// the idempotency key from (default
    /// [`DEFAULT_IDEMPOTENCY_HEADER`]) — the analog of pyfly's
    /// `idempotency_header` keyword argument.
    ///
    /// The name is matched against [`Inbound::headers`](crate::Inbound),
    /// which the web layer stores with Go's canonical MIME casing, so
    /// pass the canonical form (e.g. `"X-Idempotency-Key"`).
    pub fn with_idempotency_header(&self, header: impl Into<String>) {
        self.write().idempotency_header = Some(header.into());
    }

    /// Returns a copy of the registered validator map — used by the web
    /// layer to look up the right validator per request.
    pub fn validators(&self) -> HashMap<String, Arc<dyn Validator>> {
        self.read().validators.clone()
    }

    /// Runs the pipeline against `ev`: dedupe (when an
    /// [`EventStore`] is registered), enrich, then dispatch to every
    /// processor registered for `ev.provider`. The first processor
    /// error aborts downstream processors, pushes the (enriched) event
    /// to the DLQ, and is returned.
    ///
    /// ## Idempotency
    ///
    /// When an [`EventStore`] is registered (see
    /// [`register_event_store`](Pipeline::register_event_store)) and the
    /// event carries the idempotency header, the pipeline checks the
    /// store **before** dispatch: a duplicate (a key already recorded)
    /// returns `Ok(())` without invoking any processor — the redelivery
    /// is treated as already accepted, exactly as pyfly's
    /// `WebhookProcessor.process` returns the event and the web layer
    /// answers `202 Accepted`. A fresh key is recorded before the
    /// processors run. Events without the header are dispatched
    /// unconditionally.
    ///
    /// # Errors
    ///
    /// The first processor error, verbatim. A failed
    /// [`EventStore::already_processed`] lookup is surfaced verbatim
    /// (and is fail-closed: dispatch does not happen).
    pub async fn process(&self, mut ev: Inbound) -> Result<(), WebhookError> {
        let (enrich, procs, event_store, idempotency_header) = {
            let reg = self.read();
            (
                reg.enrich.clone(),
                reg.processors
                    .get(&ev.provider)
                    .cloned()
                    .unwrap_or_default(),
                reg.event_store.clone(),
                reg.idempotency_header
                    .clone()
                    .unwrap_or_else(|| DEFAULT_IDEMPOTENCY_HEADER.to_owned()),
            )
        };
        // Dedup before dispatch, mirroring pyfly's WebhookProcessor: a key
        // already seen short-circuits (the redelivery is a no-op success),
        // a fresh key is recorded so the next delivery is recognised.
        if let Some(store) = &event_store {
            if let Some(key) = ev.headers.get(&idempotency_header) {
                if !key.is_empty() {
                    if store.already_processed(key).await? {
                        return Ok(());
                    }
                    store.remember(key).await?;
                }
            }
        }
        if let Some(enrich) = enrich {
            enrich(&mut ev);
        }
        for p in procs {
            if let Err(err) = p.process(&ev).await {
                if let Some(dlq) = &self.dlq {
                    let _ = dlq.push(ev, &err).await;
                }
                return Err(err);
            }
        }
        Ok(())
    }

    fn read(&self) -> RwLockReadGuard<'_, Registry> {
        self.registry
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Registry> {
        self.registry
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
