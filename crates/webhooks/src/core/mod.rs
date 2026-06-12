//! The ingestion engine — the Rust spelling of the Go `webhooks/core`
//! package: the [`Pipeline`] (validate → enrich → dispatch → DLQ), the
//! in-memory [`MemoryDlq`], and the four canonical signature
//! validators.

mod mime;
mod sha1;
mod util;
mod validators;

pub use validators::{GitHubValidator, HmacValidator, StripeValidator, TwilioValidator};

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::WebhookError;
use crate::interfaces::{Inbound, Processor, Validator};

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
}

/// The validate → enrich → dispatch → DLQ chain that runs on every
/// inbound webhook.
///
/// Registration is thread-safe (`&self`, like Go's `sync.RWMutex`
/// guarded maps), so validators and processors may be installed while
/// the ingestion endpoint is live.
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

    /// Returns a copy of the registered validator map — used by the web
    /// layer to look up the right validator per request.
    pub fn validators(&self) -> HashMap<String, Arc<dyn Validator>> {
        self.read().validators.clone()
    }

    /// Runs the pipeline against `ev`: enrich, then dispatch to every
    /// processor registered for `ev.provider`. The first processor
    /// error aborts downstream processors, pushes the (enriched) event
    /// to the DLQ, and is returned.
    ///
    /// # Errors
    ///
    /// The first processor error, verbatim.
    pub async fn process(&self, mut ev: Inbound) -> Result<(), WebhookError> {
        let (enrich, procs) = {
            let reg = self.read();
            (
                reg.enrich.clone(),
                reg.processors
                    .get(&ev.provider)
                    .cloned()
                    .unwrap_or_default(),
            )
        };
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
