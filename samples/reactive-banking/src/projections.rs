//! The read-model **projection** — an EDA subscriber that folds the
//! published account events into the
//! [`AccountRepository`](crate::repository::AccountRepository).
//!
//! This closes the CQRS loop: the [`Bank`](crate::commands::Bank) publishes
//! a domain event to the [`Broker`](firefly_eda::Broker); the projection
//! consumes it, rebuilds the affected account's
//! [`AccountView`](crate::domain::AccountView) from its full stream, and
//! upserts it into the reactive read-model repository. The
//! `GET /api/v1/accounts/:id` query then serves that projected row.
//!
//! Rebuilding from the full stream (rather than mutating the row from the
//! single event) keeps the projection **idempotent**: replaying the same
//! event — at-least-once delivery on a real Kafka topic — converges on the
//! same view, never double-applying a deposit.

use std::sync::Arc;

use firefly_eda::{handler, Broker, Event};
use firefly_eventsourcing::EventStore;

use crate::commands::EVENTS_TOPIC;
use crate::domain::Account;
use crate::repository::AccountRepository;

/// Subscribes the read-model projection to the account-events topic.
///
/// For every published [`Event`] on [`EVENTS_TOPIC`], the handler reloads
/// the affected aggregate's stream from `store`, folds it into an
/// [`AccountView`](crate::domain::AccountView), and upserts it into `repo`.
/// Returns once the subscription is registered; delivery then happens on the
/// broker's fan-out.
///
/// # Errors
///
/// Propagates an [`EdaError`](firefly_eda::EdaError) if the broker rejects
/// the subscription (e.g. a closed broker).
pub async fn start(
    broker: &Arc<dyn Broker>,
    store: Arc<dyn EventStore>,
    repo: AccountRepository,
) -> firefly_eda::EdaResult<()> {
    broker
        .subscribe(
            EVENTS_TOPIC,
            handler(move |ev: Event| {
                let store = Arc::clone(&store);
                let repo = Arc::clone(&repo);
                async move {
                    project(&store, &repo, &ev).await;
                    Ok(())
                }
            }),
        )
        .await
}

/// Projects one delivered event by rebuilding the aggregate view from its
/// stream and upserting it. A malformed event or a transient store miss is
/// swallowed (logged in a real service) so one poison message never stalls
/// the projection — the EDA at-least-once contract.
async fn project(store: &Arc<dyn EventStore>, repo: &AccountRepository, ev: &Event) {
    let Some(account_id) = ev.headers.get("aggregateId") else {
        return;
    };
    let Ok(events) = store.load(account_id).await else {
        return;
    };
    let view = Account::rehydrate(account_id, &events).view();
    // The reactive repo's save is a Mono; drive it to completion.
    let _ = repo.save(view).into_future().await;
}

#[cfg(test)]
mod tests {
    use firefly_eda::InMemoryBroker;
    use firefly_eventsourcing::MemoryEventStore;

    use super::*;
    use crate::commands::Bank;
    use crate::repository::new_in_memory;

    /// A published open/deposit converges the read model on the folded
    /// balance.
    #[tokio::test]
    async fn projection_updates_read_model_from_published_events() {
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
        let broker: Arc<dyn Broker> = Arc::new(InMemoryBroker::new());
        let repo = new_in_memory();

        start(&broker, Arc::clone(&store), Arc::clone(&repo))
            .await
            .unwrap();

        let bank = Bank::new(Arc::clone(&store), Arc::clone(&broker));
        let opened = bank.open("alice", 100).await.unwrap();
        bank.deposit(&opened.id, 50).await.unwrap();

        // In-memory delivery is synchronous within publish, so the read
        // model has already converged.
        let view = repo
            .find_by_id(opened.id.clone())
            .into_future()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.balance, 150);
        assert_eq!(view.version, 2);
        assert_eq!(view.owner, "alice");
    }

    /// Re-projecting the same event is idempotent — the balance does not
    /// double-apply.
    #[tokio::test]
    async fn projection_is_idempotent_on_replay() {
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
        let broker: Arc<dyn Broker> = Arc::new(InMemoryBroker::new());
        let repo = new_in_memory();

        let bank = Bank::new(Arc::clone(&store), Arc::clone(&broker));
        let opened = bank.open("alice", 100).await.unwrap();

        // Project the same (already-persisted) event twice by hand.
        let events = store.load(&opened.id).await.unwrap();
        let env = crate::commands::to_event_envelope(&events[0]);
        project(&store, &repo, &env).await;
        project(&store, &repo, &env).await;

        let view = repo
            .find_by_id(opened.id.clone())
            .into_future()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.balance, 100, "replay must not double-apply");
    }
}
