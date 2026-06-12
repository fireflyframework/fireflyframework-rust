//! [`BroadcastHub`] — topic-based fan-out to many WebSocket sessions.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

/// A message pushed to a subscriber, distinguishing text from binary frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubMessage {
    /// A UTF-8 text payload.
    Text(String),
    /// A binary payload.
    Binary(Vec<u8>),
}

/// The receiving end handed to a subscriber when it [`join`](BroadcastHub::join)s
/// a topic. Pull [`HubMessage`]s off it and forward them to the client socket.
pub type Subscription = mpsc::UnboundedReceiver<HubMessage>;

/// Per-topic map of session id -> sender, the inner shared state of the hub.
type Topics = HashMap<String, HashMap<String, mpsc::UnboundedSender<HubMessage>>>;

/// A topic-based publish/subscribe hub for fanning a message out to every
/// session currently joined to a topic.
///
/// `BroadcastHub` has no direct pyfly analog — pyfly's websocket package stops
/// at single-connection handling — but it covers the common chat/presence
/// pattern the Go and Java ports expose. A session [`join`](BroadcastHub::join)s
/// a topic (identified by the session id) and receives a [`Subscription`];
/// [`broadcast`](BroadcastHub::broadcast) pushes a [`HubMessage`] to every live
/// subscriber of that topic. Dropped/closed receivers are pruned lazily on the
/// next broadcast.
///
/// The hub is cheap to [`clone`](Clone) — all clones share the same state
/// behind an [`Arc`] — so hand a clone to each handler.
#[derive(Clone, Default)]
pub struct BroadcastHub {
    inner: Arc<Mutex<Topics>>,
}

impl BroadcastHub {
    /// Create an empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe `session_id` to `topic` and return the receiving end of its
    /// per-session channel. Re-joining with the same id replaces the prior
    /// sender (the old [`Subscription`] then yields `None`).
    pub async fn join(
        &self,
        topic: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Subscription {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut guard = self.inner.lock().await;
        guard
            .entry(topic.into())
            .or_default()
            .insert(session_id.into(), tx);
        rx
    }

    /// Remove `session_id` from `topic`. No-op if it was not joined. Empties
    /// the topic entry entirely once its last subscriber leaves.
    pub async fn leave(&self, topic: &str, session_id: &str) {
        let mut guard = self.inner.lock().await;
        if let Some(members) = guard.get_mut(topic) {
            members.remove(session_id);
            if members.is_empty() {
                guard.remove(topic);
            }
        }
    }

    /// Push `message` to every live subscriber of `topic` and return how many
    /// received it. Subscribers whose receiver has been dropped are pruned.
    pub async fn broadcast(&self, topic: &str, message: HubMessage) -> usize {
        let mut guard = self.inner.lock().await;
        let Some(members) = guard.get_mut(topic) else {
            return 0;
        };
        let mut delivered = 0;
        members.retain(|_, tx| {
            if tx.send(message.clone()).is_ok() {
                delivered += 1;
                true
            } else {
                false
            }
        });
        if members.is_empty() {
            guard.remove(topic);
        }
        delivered
    }

    /// Number of live subscribers currently joined to `topic`.
    pub async fn subscriber_count(&self, topic: &str) -> usize {
        let guard = self.inner.lock().await;
        guard.get(topic).map_or(0, HashMap::len)
    }

    /// Number of topics that currently have at least one subscriber.
    pub async fn topic_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn join_broadcast_delivers_to_all_members() {
        let hub = BroadcastHub::new();
        let mut a = hub.join("room", "a").await;
        let mut b = hub.join("room", "b").await;

        let delivered = hub
            .broadcast("room", HubMessage::Text("hello".into()))
            .await;

        assert_eq!(delivered, 2);
        assert_eq!(a.recv().await, Some(HubMessage::Text("hello".into())));
        assert_eq!(b.recv().await, Some(HubMessage::Text("hello".into())));
    }

    #[tokio::test]
    async fn leave_removes_member_and_empties_topic() {
        let hub = BroadcastHub::new();
        let _a = hub.join("room", "a").await;
        assert_eq!(hub.subscriber_count("room").await, 1);
        assert_eq!(hub.topic_count().await, 1);

        hub.leave("room", "a").await;
        assert_eq!(hub.subscriber_count("room").await, 0);
        assert_eq!(hub.topic_count().await, 0); // empty topic pruned
    }

    #[tokio::test]
    async fn leave_is_noop_for_unknown_member() {
        let hub = BroadcastHub::new();
        hub.leave("nope", "ghost").await; // must not panic
        assert_eq!(hub.topic_count().await, 0);
    }

    #[tokio::test]
    async fn broadcast_prunes_dropped_subscribers() {
        let hub = BroadcastHub::new();
        let a = hub.join("room", "a").await;
        let mut b = hub.join("room", "b").await;
        drop(a); // a's receiver is gone

        let delivered = hub.broadcast("room", HubMessage::Text("x".into())).await;
        assert_eq!(delivered, 1); // only b
        assert_eq!(hub.subscriber_count("room").await, 1); // a pruned
        assert_eq!(b.recv().await, Some(HubMessage::Text("x".into())));
    }

    #[tokio::test]
    async fn broadcast_to_unknown_topic_delivers_zero() {
        let hub = BroadcastHub::new();
        assert_eq!(
            hub.broadcast("ghost", HubMessage::Text("x".into())).await,
            0
        );
    }

    #[tokio::test]
    async fn rejoin_replaces_prior_sender() {
        let hub = BroadcastHub::new();
        let mut first = hub.join("room", "a").await;
        let mut second = hub.join("room", "a").await; // same id replaces

        assert_eq!(hub.subscriber_count("room").await, 1);
        hub.broadcast("room", HubMessage::Binary(vec![9])).await;
        // The first subscription's sender was dropped on re-join.
        assert_eq!(first.recv().await, None);
        assert_eq!(second.recv().await, Some(HubMessage::Binary(vec![9])));
    }
}
