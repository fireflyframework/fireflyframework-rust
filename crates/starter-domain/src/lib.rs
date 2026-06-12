//! # firefly-starter-domain
//!
//! The **domain-tier starter**: [`firefly_starter_core`] plus the
//! in-memory event-sourcing stores from [`firefly_eventsourcing`] —
//! the port of the Go `starterdomain` module (Java original:
//! `firefly-starter-domain`, .NET: `FireflyFramework.Starter.Domain`).
//!
//! [`Domain::new`] wires a full [`Core`] and adds:
//!
//! * [`Domain::events`] — an [`EventStore`] (default
//!   [`MemoryEventStore`]).
//! * [`Domain::snapshots`] — a [`SnapshotStore`] (default
//!   [`MemorySnapshotStore`]).
//! * [`Domain::projections`] — a [`ProjectionRunner`].
//!
//! This is the canonical wiring for domain-tier services that source
//! aggregates from events. A Postgres-backed [`EventStore`] is on the
//! roadmap; until then, services that need persistent event storage
//! register their own implementation by overriding `domain.events`
//! after [`Domain::new`] — the fields are public trait objects for
//! exactly that reason.
//!
//! [`Domain`] dereferences to [`Core`] (the Rust analog of Go's struct
//! embedding), so every core field and convenience method —
//! `apply_middleware`, `actuator_router`, `new_application`,
//! `print_banner`, … — is available directly on the domain value.
//! `starter_name` defaults to `"starter-domain"`.
//!
//! ## Quick start
//!
//! ```
//! use firefly_starter_domain::{AggregateRoot, CoreConfig, Domain};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let domain = Domain::new(CoreConfig {
//!     app_name: "billing".into(),
//!     ..CoreConfig::default()
//! });
//!
//! // domain.projections.register(billing_projection);
//!
//! let mut invoice = AggregateRoot::new("i1", "Invoice");
//! invoice.raise("InvoiceCreated", br#"{"amount":100}"#);
//! let batch = invoice.take_uncommitted();
//! domain.events.append(&invoice.id, 0, batch).await.unwrap();
//! domain.projections.replay(&*domain.events, "i1").await.unwrap();
//! # });
//! ```

#![warn(missing_docs)]

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub use firefly_eventsourcing::{
    AggregateRoot, DomainEvent, EventSourcingError, EventStore, MemoryEventStore,
    MemorySnapshotStore, Projection, ProjectionRunner, Snapshot, SnapshotStore,
};
pub use firefly_starter_core::{Core, CoreConfig};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = firefly_starter_core::VERSION;

/// Domain is [`Core`] + [`EventStore`] + [`SnapshotStore`] +
/// projections — the Rust spelling of the Go `starterdomain.Domain`
/// struct.
///
/// All fields are public so services can override any default after
/// [`Domain::new`] (e.g. swap [`Domain::events`] for a persistent
/// store), exactly as the Go module documents for `domain.Events`.
pub struct Domain {
    /// The wired infrastructure core. [`Domain`] also [`Deref`]s to
    /// this field, mirroring Go's embedded `*startercore.Core`.
    pub core: Core,
    /// The wired event store; default [`MemoryEventStore`].
    pub events: Arc<dyn EventStore>,
    /// The wired snapshot store; default [`MemorySnapshotStore`].
    pub snapshots: Arc<dyn SnapshotStore>,
    /// The wired projection runner, empty until the service registers
    /// its read-side projections.
    pub projections: Arc<ProjectionRunner>,
}

impl Domain {
    /// Wires the domain starter with in-memory event stores — Go's
    /// `starterdomain.New(cfg)`.
    ///
    /// Delegates to [`Core::new`] for the infrastructure tier, then —
    /// exactly like Go — replaces a `starter_name` that resolved to
    /// the `"starter-core"` default with `"starter-domain"`. A custom
    /// starter name passed in `cfg` is preserved untouched.
    pub fn new(cfg: CoreConfig) -> Self {
        let mut core = Core::new(cfg);
        if core.starter_name == "starter-core" {
            core.starter_name = "starter-domain".to_string();
        }
        Domain {
            core,
            events: Arc::new(MemoryEventStore::new()),
            snapshots: Arc::new(MemorySnapshotStore::new()),
            projections: Arc::new(ProjectionRunner::new()),
        }
    }
}

impl Deref for Domain {
    type Target = Core;

    fn deref(&self) -> &Core {
        &self.core
    }
}

impl DerefMut for Domain {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;

    fn domain_for(app_name: &str) -> Domain {
        Domain::new(CoreConfig {
            app_name: app_name.into(),
            ..CoreConfig::default()
        })
    }

    /// A projection that counts the events folded into it.
    struct CountingProjection {
        seen: AtomicUsize,
    }

    #[async_trait]
    impl Projection for CountingProjection {
        fn name(&self) -> &str {
            "counting"
        }

        async fn apply(&self, _event: &DomainEvent) -> Result<(), EventSourcingError> {
            self.seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// An [`EventStore`] decorator that counts appends — stands in for
    /// the persistent store a real service would swap in.
    struct CountingStore {
        inner: MemoryEventStore,
        appends: AtomicUsize,
    }

    #[async_trait]
    impl EventStore for CountingStore {
        async fn append(
            &self,
            aggregate_id: &str,
            expected_version: i64,
            events: Vec<DomainEvent>,
        ) -> Result<(), EventSourcingError> {
            self.appends.fetch_add(1, Ordering::SeqCst);
            self.inner
                .append(aggregate_id, expected_version, events)
                .await
        }

        async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError> {
            self.inner.load(aggregate_id).await
        }

        async fn load_after(
            &self,
            aggregate_id: &str,
            since_version: i64,
        ) -> Result<Vec<DomainEvent>, EventSourcingError> {
            self.inner.load_after(aggregate_id, since_version).await
        }
    }

    // ---- ports of the Go test suite ----------------------------------------

    /// Go: TestDomainWiring — the event-sourcing dependencies are wired
    /// (here proven by behavior rather than nil-checks: the defaults
    /// accept work), the starter name is `"starter-domain"`, and an
    /// event round-trips through the wired store.
    #[tokio::test]
    async fn domain_wiring() {
        let d = domain_for("svc");
        assert_eq!(d.starter_name, "starter-domain");
        assert_eq!(d.app_name, "svc");

        // Round-trip an event through the wired store.
        let mut a = AggregateRoot::new("u1", "User");
        a.raise("UserCreated", b"{}".as_slice());
        let batch = a.take_uncommitted();
        d.events
            .append(&a.id, 0, batch)
            .await
            .expect("append through wired store");

        let events = d.events.load("u1").await.expect("load round-trip");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "UserCreated");
        assert_eq!(events[0].aggregate_type, "User");
        assert_eq!(events[0].version, 1);

        // The wired snapshot store and projection runner accept work too.
        d.snapshots
            .save(Snapshot {
                aggregate_id: "u1".into(),
                aggregate_type: "User".into(),
                version: 1,
                payload: b"{}".to_vec(),
            })
            .await
            .expect("save through wired snapshot store");
        d.projections
            .replay(&*d.events, "u1")
            .await
            .expect("replay through wired runner");
    }

    // ---- Rust-specific coverage ---------------------------------------------

    /// New() only renames the resolved "starter-core" default; a custom
    /// starter name passes through untouched — the Go `if` branch.
    #[test]
    fn custom_starter_name_preserved() {
        let d = Domain::new(CoreConfig {
            app_name: "svc".into(),
            starter_name: "starter-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(d.starter_name, "starter-custom");
    }

    /// An explicit "starter-core" is indistinguishable from the default
    /// after Core::new, so it is renamed too — exactly like Go.
    #[test]
    fn explicit_starter_core_is_renamed() {
        let d = Domain::new(CoreConfig {
            app_name: "svc".into(),
            starter_name: "starter-core".into(),
            ..CoreConfig::default()
        });
        assert_eq!(d.starter_name, "starter-domain");
    }

    /// Core defaults flow through the domain starter unchanged.
    #[test]
    fn core_defaults_flow_through() {
        let d = Domain::new(CoreConfig::default());
        assert_eq!(d.app_name, "firefly-app");
        assert_eq!(d.starter_name, "starter-domain");
        assert_eq!(d.cache.name(), "memory");
        assert_eq!(d.log.service, "firefly-app");
    }

    /// Loading an unknown aggregate reports AggregateNotFound; a stale
    /// expected version reports Concurrency.
    #[tokio::test]
    async fn wired_store_keeps_event_store_semantics() {
        let d = domain_for("svc");
        assert_eq!(
            d.events.load("missing").await.unwrap_err(),
            EventSourcingError::AggregateNotFound
        );

        let mut a = AggregateRoot::new("u1", "User");
        a.raise("UserCreated", b"{}".as_slice());
        let batch = a.take_uncommitted();
        d.events.append(&a.id, 0, batch).await.unwrap();

        let mut stale = AggregateRoot::new("u1", "User");
        stale.raise("UserRenamed", b"{}".as_slice());
        let stale_batch = stale.take_uncommitted();
        assert_eq!(
            d.events
                .append(&stale.id, 0, stale_batch)
                .await
                .unwrap_err(),
            EventSourcingError::Concurrency
        );
    }

    /// The wired snapshot store keeps the latest capture per aggregate
    /// and reports a soft miss as Ok(None).
    #[tokio::test]
    async fn wired_snapshot_store_round_trip() {
        let d = domain_for("svc");
        assert_eq!(d.snapshots.latest("u1").await.unwrap(), None);

        let snapshot = Snapshot {
            aggregate_id: "u1".into(),
            aggregate_type: "User".into(),
            version: 3,
            payload: serde_json::to_vec(&serde_json::json!({ "name": "alice" })).unwrap(),
        };
        d.snapshots.save(snapshot.clone()).await.unwrap();
        assert_eq!(d.snapshots.latest("u1").await.unwrap(), Some(snapshot));
    }

    /// The README quick-start flow: register a projection, append
    /// events, replay the stream through the wired store and runner.
    #[tokio::test]
    async fn projection_replay_through_wired_stores() {
        let d = domain_for("billing");
        let projection = Arc::new(CountingProjection {
            seen: AtomicUsize::new(0),
        });
        d.projections.register(projection.clone());

        let mut invoice = AggregateRoot::new("i1", "Invoice");
        invoice.raise("InvoiceCreated", br#"{"amount":100}"#.as_slice());
        invoice.raise("InvoicePaid", br#"{"amount":100}"#.as_slice());
        let batch = invoice.take_uncommitted();
        d.events.append(&invoice.id, 0, batch).await.unwrap();

        d.projections.replay(&*d.events, "i1").await.unwrap();
        assert_eq!(projection.seen.load(Ordering::SeqCst), 2);
    }

    /// Services override `domain.events` after New() to swap in a
    /// persistent store — the documented Go extension point.
    #[tokio::test]
    async fn event_store_overridable_after_new() {
        let mut d = domain_for("svc");
        let store = Arc::new(CountingStore {
            inner: MemoryEventStore::new(),
            appends: AtomicUsize::new(0),
        });
        d.events = store.clone();

        let mut a = AggregateRoot::new("u1", "User");
        a.raise("UserCreated", b"{}".as_slice());
        let batch = a.take_uncommitted();
        d.events.append(&a.id, 0, batch).await.unwrap();
        assert_eq!(store.appends.load(Ordering::SeqCst), 1);
        assert_eq!(d.events.load("u1").await.unwrap().len(), 1);
    }

    /// Two writers racing on the same fresh stream: exactly one append
    /// wins, the other observes the optimistic-concurrency conflict.
    #[tokio::test]
    async fn concurrent_appends_one_wins() {
        let d = domain_for("svc");
        let write = |writer: &str| {
            let events = Arc::clone(&d.events);
            let mut a = AggregateRoot::new("u1", "User");
            a.raise(
                "UserCreated",
                format!(r#"{{"by":"{writer}"}}"#).into_bytes(),
            );
            let batch = a.take_uncommitted();
            async move { events.append("u1", 0, batch).await }
        };

        let (left, right) = futures::join!(write("left"), write("right"));
        let wins = [&left, &right].iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "exactly one writer wins: {left:?} / {right:?}");
        assert_eq!(
            [left, right].into_iter().find_map(Result::err).unwrap(),
            EventSourcingError::Concurrency
        );
        assert_eq!(d.events.load("u1").await.unwrap().len(), 1);
    }

    /// Deref promotes the embedded core's surface, mirroring Go's field
    /// and method promotion through the embedded `*startercore.Core`.
    #[test]
    fn deref_promotes_core_surface() {
        let d = domain_for("billing");
        let banner = d.banner();
        assert!(banner.contains("billing"));
        assert!(banner.contains("starter-domain"));
        assert_eq!(d.new_application().name(), "billing");

        // DerefMut allows post-construction tweaks on core fields.
        let mut d = d;
        d.app_version = "2.0.0".into();
        assert_eq!(d.core.app_version, "2.0.0");
    }

    #[test]
    fn version_matches_workspace() {
        assert_eq!(VERSION, firefly_starter_core::VERSION);
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn domain_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Domain>();
    }
}
