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

//! Transaction-aware, in-process domain events — the Rust port of Spring's
//! `@EventListener` and `@TransactionalEventListener` (and pyfly's
//! `@event_listener` / `@transactional_event_listener`).
//!
//! This is a **thread-safe, async** in-process publish/subscribe keyed on the
//! concrete event [`TypeId`]. It is the async sibling of
//! `firefly_config::ApplicationEventBus` (which is single-threaded and used for
//! framework lifecycle events): use this one for *domain* events published from
//! `async` service code running on the tokio runtime, and especially for
//! listeners that must run relative to a surrounding transaction's commit.
//!
//! # Immediate vs. transaction-bound listeners
//!
//! A listener registered with `phase = None` is **immediate**: it runs
//! synchronously (awaited) at [`publish_event`] time, exactly like Spring's
//! `@EventListener`. A listener registered with a [`TransactionPhase`] is
//! **transaction-bound** (`@TransactionalEventListener`): when the event is
//! published inside an active [`transactional`](crate::transactional)
//! transaction, it is buffered and dispatched at the requested phase —
//! [`BeforeCommit`](TransactionPhase::BeforeCommit) (still inside the
//! transaction, just before commit), [`AfterCommit`](TransactionPhase::AfterCommit)
//! / [`AfterRollback`](TransactionPhase::AfterRollback) (once the outcome is
//! known), or [`AfterCompletion`](TransactionPhase::AfterCompletion) (always,
//! after either).
//!
//! When a transaction-bound event is published with **no** active transaction
//! (for example a service running without a registered transaction manager —
//! the same graceful-degradation path [`transactional`](crate::transactional)
//! itself takes), the listener falls back to running immediately, as if the
//! work had already committed (`AfterRollback` listeners do not fire on this
//! path). This keeps a `@TransactionalEventListener` useful in unit tests and
//! datasource-less setups instead of silently dropping the event.
//!
//! # Discovery
//!
//! The `#[event_listener]` / `#[transactional_event_listener]` macros emit an
//! [`inventory::submit!`] thunk per listener; the first [`publish_event`] (or an
//! explicit [`register_discovered_listeners`]) drains them once, so listeners
//! defined anywhere in the crate graph are registered without manual wiring.
//! [`register_event_listener`] is the programmatic counterpart used by the
//! macros and by tests.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Once, OnceLock, RwLock};

/// The phase of a surrounding transaction at which a transaction-bound listener
/// runs — Spring's `TransactionPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransactionPhase {
    /// Just before the transaction commits, still inside it. Listeners do work
    /// (they cannot veto the commit — they return `()`), e.g. a last-chance
    /// projection write that should share the transaction.
    BeforeCommit,
    /// After the transaction has committed successfully. The canonical phase:
    /// publish integration events, send notifications, evict caches.
    AfterCommit,
    /// After the transaction has rolled back.
    AfterRollback,
    /// After the transaction completes, whether it committed or rolled back.
    AfterCompletion,
}

/// The erased async dispatcher the macros build per listener: it downcasts the
/// shared event to the listener's concrete type and awaits the handler.
pub type EventDispatcher = Arc<
    dyn Fn(Arc<dyn Any + Send + Sync>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// A registered listener: the optional transaction phase (`None` = immediate)
/// plus its erased dispatcher.
#[derive(Clone)]
struct Listener {
    phase: Option<TransactionPhase>,
    dispatcher: EventDispatcher,
}

/// One transaction-buffered event: its concrete [`TypeId`] (to find the
/// matching listeners at flush time) and the shared, type-erased value.
#[derive(Clone)]
struct BufferedEvent {
    type_id: TypeId,
    event: Arc<dyn Any + Send + Sync>,
}

/// The process-wide listener registry, keyed on the concrete event [`TypeId`].
fn registry() -> &'static RwLock<HashMap<TypeId, Vec<Listener>>> {
    static REGISTRY: OnceLock<RwLock<HashMap<TypeId, Vec<Listener>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

tokio::task_local! {
    /// The current transaction's event buffer, installed by the
    /// [`transactional`](crate::transactional) orchestrator around the
    /// outermost transaction. Absent when no transaction is active.
    static TX_EVENT_BUFFER: Arc<Mutex<Vec<BufferedEvent>>>;
}

/// One registration thunk per `#[event_listener]` / `#[transactional_event_listener]`,
/// collected across the whole crate graph via [`inventory`].
pub struct EventListenerRegistration {
    /// Calls [`register_event_listener`] for this listener.
    pub register: fn(),
}

inventory::collect!(EventListenerRegistration);

/// Drains the discovered (`inventory`-submitted) listener registrations exactly
/// once. Idempotent and safe to call from any thread; [`publish_event`] calls it
/// lazily so listeners are live without explicit startup wiring.
pub fn register_discovered_listeners() {
    static DISCOVER: Once = Once::new();
    DISCOVER.call_once(|| {
        for reg in inventory::iter::<EventListenerRegistration> {
            (reg.register)();
        }
    });
}

/// Registers `dispatcher` for events of type `E`. `phase = None` is an
/// immediate (`@EventListener`) listener; `phase = Some(..)` is a
/// transaction-bound (`@TransactionalEventListener`) listener. The macros call
/// this from their `inventory` thunks; tests call it directly.
pub fn register_event_listener<E: 'static>(
    phase: Option<TransactionPhase>,
    dispatcher: EventDispatcher,
) {
    let mut reg = registry()
        .write()
        .expect("event listener registry poisoned");
    reg.entry(TypeId::of::<E>())
        .or_default()
        .push(Listener { phase, dispatcher });
}

/// Snapshot the listeners for one event type (cloning the cheap `Arc`
/// dispatchers) so dispatch never holds the registry lock across an `.await`.
fn listeners_for(type_id: TypeId) -> Vec<Listener> {
    registry()
        .read()
        .expect("event listener registry poisoned")
        .get(&type_id)
        .cloned()
        .unwrap_or_default()
}

/// Publishes `event` to every listener registered for its concrete type.
///
/// Immediate listeners run now (awaited, in registration order). Transaction-bound
/// listeners are buffered onto the active transaction (and dispatched later at
/// their [`TransactionPhase`]); with no active transaction they fall back to
/// running immediately as if already committed (see the [module docs](self)).
pub async fn publish_event<E: Any + Send + Sync + 'static>(event: E) {
    register_discovered_listeners();
    let type_id = TypeId::of::<E>();
    let listeners = listeners_for(type_id);
    if listeners.is_empty() {
        return;
    }
    let shared: Arc<dyn Any + Send + Sync> = Arc::new(event);

    // Immediate listeners always run at publish time.
    for listener in listeners.iter().filter(|l| l.phase.is_none()) {
        (listener.dispatcher)(shared.clone()).await;
    }

    if !listeners.iter().any(|l| l.phase.is_some()) {
        return;
    }

    // Transaction-bound listeners: buffer onto the active transaction, or fall
    // back to immediate dispatch when there is none.
    let buffered = TX_EVENT_BUFFER
        .try_with(|buf| {
            buf.lock()
                .expect("tx event buffer poisoned")
                .push(BufferedEvent {
                    type_id,
                    event: shared.clone(),
                });
        })
        .is_ok();

    if !buffered {
        for listener in listeners.iter() {
            match listener.phase {
                Some(TransactionPhase::BeforeCommit)
                | Some(TransactionPhase::AfterCommit)
                | Some(TransactionPhase::AfterCompletion) => {
                    (listener.dispatcher)(shared.clone()).await;
                }
                // No rollback happened on the no-transaction path.
                Some(TransactionPhase::AfterRollback) | None => {}
            }
        }
    }
}

/// Whether a transaction event scope is active on the current task — used by the
/// orchestrator to bind synchronization to the *outermost* transaction.
pub(crate) fn tx_scope_active() -> bool {
    TX_EVENT_BUFFER.try_with(|_| ()).is_ok()
}

/// Runs `f` with a fresh, empty transaction event buffer installed on the
/// current task. The orchestrator wraps the outermost transaction in this so
/// events published during it are captured for phased dispatch.
pub(crate) async fn with_tx_scope<F>(f: F) -> F::Output
where
    F: Future,
{
    TX_EVENT_BUFFER
        .scope(Arc::new(Mutex::new(Vec::new())), f)
        .await
}

/// Dispatches the buffered events whose listeners match `phase`. Called by the
/// orchestrator at each transaction phase. The registry lock is released before
/// any handler runs, so handlers may publish further events / open transactions.
pub(crate) async fn dispatch_phase(phase: TransactionPhase) {
    let events: Vec<BufferedEvent> = match TX_EVENT_BUFFER
        .try_with(|buf| buf.lock().expect("tx event buffer poisoned").clone())
    {
        Ok(events) => events,
        Err(_) => return,
    };
    if events.is_empty() {
        return;
    }
    // Collect (event, dispatcher) pairs under the lock, then run them without it.
    let to_run: Vec<(Arc<dyn Any + Send + Sync>, EventDispatcher)> = {
        let reg = registry().read().expect("event listener registry poisoned");
        let mut pairs = Vec::new();
        for be in &events {
            if let Some(listeners) = reg.get(&be.type_id) {
                for listener in listeners {
                    if listener.phase == Some(phase) {
                        pairs.push((be.event.clone(), listener.dispatcher.clone()));
                    }
                }
            }
        }
        pairs
    };
    for (event, dispatcher) in to_run {
        dispatcher(event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::manager::{
        register_transaction_manager, transactional, BoxedTxOp, TransactionManager, TxOptions,
        TxOutcome,
    };
    use crate::TxError;

    // A minimal in-memory manager: it runs the op and reports the outcome,
    // committing on Ok/!rolled_back and rolling back otherwise — enough to drive
    // the phase dispatch without a real datasource.
    struct MemManager;

    #[async_trait::async_trait]
    impl TransactionManager for MemManager {
        async fn execute<'a>(
            &self,
            _opts: TxOptions,
            op: BoxedTxOp<'a>,
        ) -> Result<TxOutcome, TxError> {
            op.await
        }
        fn is_active(&self) -> bool {
            true
        }
    }

    // A dispatcher that records `tag` regardless of the event payload (each test
    // uses a distinct event type so the global registry never crosses tests).
    fn dispatcher_pushing(
        log: Arc<Mutex<Vec<&'static str>>>,
        tag: &'static str,
    ) -> EventDispatcher {
        Arc::new(move |_ev| {
            let log = log.clone();
            Box::pin(async move {
                log.lock().unwrap().push(tag);
            })
        })
    }

    #[tokio::test]
    async fn immediate_listener_runs_at_publish() {
        #[derive(Clone)]
        struct ImmEvt;
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        register_event_listener::<ImmEvt>(
            None,
            Arc::new(move |_| {
                let h = h.clone();
                Box::pin(async move {
                    h.fetch_add(1, Ordering::SeqCst);
                })
            }),
        );
        publish_event(ImmEvt).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn after_commit_listener_runs_after_a_committed_tx() {
        struct CommitEvt;
        let log = Arc::new(Mutex::new(Vec::new()));
        register_event_listener::<CommitEvt>(
            Some(TransactionPhase::AfterCommit),
            dispatcher_pushing(log.clone(), "after_commit"),
        );
        register_transaction_manager(Arc::new(MemManager));

        let out: Result<(), TxError> = transactional(TxOptions::required(), || async {
            // Published mid-transaction: must NOT have fired yet.
            publish_event(CommitEvt).await;
            assert!(
                log.lock().unwrap().is_empty(),
                "after_commit fired too early"
            );
            Ok(())
        })
        .await;
        assert!(out.is_ok());
        assert_eq!(*log.lock().unwrap(), vec!["after_commit"]);
    }

    #[tokio::test]
    async fn after_rollback_listener_runs_when_the_tx_rolls_back() {
        struct RollbackEvt;
        let log = Arc::new(Mutex::new(Vec::new()));
        register_event_listener::<RollbackEvt>(
            Some(TransactionPhase::AfterCommit),
            dispatcher_pushing(log.clone(), "after_commit"),
        );
        register_event_listener::<RollbackEvt>(
            Some(TransactionPhase::AfterRollback),
            dispatcher_pushing(log.clone(), "after_rollback"),
        );
        register_transaction_manager(Arc::new(MemManager));

        let out: Result<(), TxError> = transactional(TxOptions::required(), || async {
            publish_event(RollbackEvt).await;
            Err(TxError::application("boom"))
        })
        .await;
        assert!(out.is_err());
        // Only the rollback listener fired; the commit listener did not.
        assert_eq!(*log.lock().unwrap(), vec!["after_rollback"]);
    }

    #[tokio::test]
    async fn before_commit_runs_inside_the_tx() {
        struct BeforeEvt;
        let log = Arc::new(Mutex::new(Vec::new()));
        register_event_listener::<BeforeEvt>(
            Some(TransactionPhase::BeforeCommit),
            dispatcher_pushing(log.clone(), "before_commit"),
        );
        register_transaction_manager(Arc::new(MemManager));

        let out: Result<(), TxError> = transactional(TxOptions::required(), || async {
            publish_event(BeforeEvt).await;
            Ok(())
        })
        .await;
        assert!(out.is_ok());
        assert_eq!(*log.lock().unwrap(), vec!["before_commit"]);
    }

    #[tokio::test]
    async fn transactional_listener_falls_back_with_no_transaction() {
        // No manager registered for this event's flow: a transaction-bound
        // listener still runs immediately (as if already committed).
        struct FallbackEvt;
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        register_event_listener::<FallbackEvt>(
            Some(TransactionPhase::AfterCommit),
            Arc::new(move |_| {
                let h = h.clone();
                Box::pin(async move {
                    h.fetch_add(1, Ordering::SeqCst);
                })
            }),
        );
        // Publish outside any transactional() scope.
        publish_event(FallbackEvt).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
