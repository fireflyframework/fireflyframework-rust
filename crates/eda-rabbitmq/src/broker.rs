//! The RabbitMQ [`Broker`] over `lapin`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions,
    BasicRejectOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions,
};
use lapin::types::FieldTable;
use lapin::{
    BasicProperties, Channel, ChannelState, Connection, ConnectionProperties, ConnectionState,
    ExchangeKind, Result as LapinResult,
};
use tokio::task::JoinHandle;

use firefly_eda::{Broker, EdaError, EdaResult, Event, Handler, Publisher, Subscriber};

use crate::config::RabbitMqBrokerConfig;
use crate::dispatch::{dispatch, Ack, Subscription};

/// Decides the acknowledgement for a raw delivery `body`: undeserializable
/// bodies are dropped ([`Ack::RejectDrop`]), otherwise the decision is
/// delegated to [`dispatch`]. This is the whole of pyfly's `on_message`,
/// extracted so the policy is unit-testable without a live broker.
async fn decide(subscriptions: &[Subscription], body: &[u8]) -> Ack {
    match serde_json::from_slice::<Event>(body) {
        Ok(event) => dispatch(subscriptions, &event).await,
        Err(err) => {
            tracing::error!(error = %err, "firefly/eda-rabbitmq: failed to deserialize message body; dropping");
            Ack::RejectDrop
        }
    }
}

/// An [`EventPublisher`](firefly_eda::Publisher)/[`Subscriber`] backed by
/// RabbitMQ via `lapin` — the Rust port of pyfly's `RabbitMqEventBus`.
///
/// Topology (see [`RabbitMqBrokerConfig`]): a single durable `direct`
/// exchange, plus one durable queue `<group>.<destination>` per
/// destination, each bound with `<destination>` as its routing key and
/// consumed with **publisher confirms** on the publishing channel and
/// **manual ack** on the consuming side.
///
/// Delivery semantics (at-least-once, parity with pyfly and the brief):
///
/// * handler success (or no matching pattern) → `basic_ack`,
/// * a matching handler returns `Err` → `basic_nack(requeue = true)` so
///   the broker redelivers,
/// * an undeserializable body → `basic_reject(requeue = false)` so the
///   poison message is dropped rather than looping.
///
/// [`subscribe`](Subscriber::subscribe) registers an `fnmatch`-style
/// pattern on the event's `type` (not on the AMQP routing key): every
/// running consumer reads the shared subscription list on each delivery,
/// so a pattern added after [`start`](Self::start) takes effect
/// immediately — exactly like pyfly's live `_handlers` list.
pub struct RabbitMqBroker {
    config: RabbitMqBrokerConfig,
    subscriptions: Arc<Mutex<Vec<Subscription>>>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    connection: Option<Connection>,
    publish_channel: Option<Channel>,
    consumers: Vec<JoinHandle<()>>,
    started: bool,
    closed: bool,
}

impl RabbitMqBroker {
    /// Creates a broker from `config` without connecting; call
    /// [`start`](Self::start) to open the AMQP connection, declare the
    /// topology, and begin consuming.
    pub fn new(config: RabbitMqBrokerConfig) -> Self {
        Self {
            config,
            subscriptions: Arc::new(Mutex::new(Vec::new())),
            state: Mutex::new(State::default()),
        }
    }

    /// Returns the broker's configuration.
    pub fn config(&self) -> &RabbitMqBrokerConfig {
        &self.config
    }

    /// Connects to RabbitMQ, declares the durable `direct` exchange and
    /// the per-destination queues from
    /// [`declaration_plan`](RabbitMqBrokerConfig::declaration_plan), and
    /// spawns one consumer task per destination. Enables publisher
    /// confirms on the publishing channel. Idempotent: a second call
    /// while started is a no-op (parity with pyfly's `start`).
    ///
    /// On any declaration error the half-open connection is closed so no
    /// resources leak, then the error is returned.
    pub async fn start(&self) -> LapinResult<()> {
        {
            let state = self
                .state
                .lock()
                .expect("firefly/eda-rabbitmq: lock poisoned");
            if state.started {
                return Ok(());
            }
        }

        let connection =
            Connection::connect(&self.config.url, ConnectionProperties::default()).await?;

        match self.declare_and_consume(&connection).await {
            Ok((publish_channel, consumers)) => {
                let mut state = self
                    .state
                    .lock()
                    .expect("firefly/eda-rabbitmq: lock poisoned");
                state.connection = Some(connection);
                state.publish_channel = Some(publish_channel);
                state.consumers = consumers;
                state.started = true;
                state.closed = false;
                Ok(())
            }
            Err(err) => {
                // Don't leak a half-open connection if declare/consume fails.
                let _ = connection.close(0, "declare failed").await;
                Err(err)
            }
        }
    }

    /// Declares the exchange + queues and starts the consumers, returning
    /// the confirm-enabled publishing channel and the consumer tasks.
    async fn declare_and_consume(
        &self,
        connection: &Connection,
    ) -> LapinResult<(Channel, Vec<JoinHandle<()>>)> {
        let plan = self.config.declaration_plan();

        // Publishing channel with publisher confirms enabled.
        let publish_channel = connection.create_channel().await?;
        publish_channel.confirm_select(Default::default()).await?;
        publish_channel
            .exchange_declare(
                &plan.exchange.name,
                ExchangeKind::Direct,
                ExchangeDeclareOptions {
                    durable: plan.exchange.durable,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await?;

        let mut consumers = Vec::with_capacity(plan.queues.len());
        for queue in &plan.queues {
            let channel = connection.create_channel().await?;
            channel
                .queue_declare(
                    &queue.name,
                    QueueDeclareOptions {
                        durable: queue.durable,
                        ..Default::default()
                    },
                    FieldTable::default(),
                )
                .await?;
            channel
                .queue_bind(
                    &queue.name,
                    &queue.exchange,
                    &queue.routing_key,
                    QueueBindOptions::default(),
                    FieldTable::default(),
                )
                .await?;
            let consumer = channel
                .basic_consume(
                    &queue.name,
                    &format!("{}-consumer", queue.name),
                    BasicConsumeOptions::default(),
                    FieldTable::default(),
                )
                .await?;
            let subscriptions = self.subscriptions.clone();
            // Keep the channel alive for the lifetime of the consumer task.
            consumers.push(tokio::spawn(consume_loop(channel, consumer, subscriptions)));
        }

        Ok((publish_channel, consumers))
    }

    /// Publishes `ev` to the exchange with `ev.topic` as the routing key,
    /// then awaits the publisher confirm. Auto-starts the broker if it
    /// has not been started yet (parity with pyfly's `publish`).
    pub async fn publish_event(&self, ev: Event) -> EdaResult<()> {
        let channel = {
            let state = self
                .state
                .lock()
                .expect("firefly/eda-rabbitmq: lock poisoned");
            if state.closed {
                return Err(EdaError::Closed);
            }
            state.publish_channel.clone()
        };
        let channel = match channel {
            Some(channel) => channel,
            None => {
                self.start().await.map_err(|e| {
                    EdaError::from(firefly_kernel::FireflyError::internal(e.to_string()))
                })?;
                self.state
                    .lock()
                    .expect("firefly/eda-rabbitmq: lock poisoned")
                    .publish_channel
                    .clone()
                    .ok_or(EdaError::RabbitMqUnavailable)?
            }
        };

        let body = serde_json::to_vec(&ev)
            .map_err(|e| EdaError::from(firefly_kernel::FireflyError::internal(e.to_string())))?;
        let confirm = channel
            .basic_publish(
                &self.config.exchange,
                &ev.topic,
                BasicPublishOptions::default(),
                &body,
                BasicProperties::default(),
            )
            .await
            .map_err(map_lapin)?;
        // Await the publisher confirm so a publish only resolves once the
        // broker has accepted the message.
        confirm.await.map_err(map_lapin)?;
        Ok(())
    }

    /// Registers an `fnmatch`-style `pattern` on the event `type`.
    /// Subscriptions may be added before or after [`start`](Self::start);
    /// running consumers pick them up on the next delivery.
    pub fn subscribe_pattern(&self, pattern: impl Into<String>, h: Handler) {
        self.subscriptions
            .lock()
            .expect("firefly/eda-rabbitmq: lock poisoned")
            .push(Subscription {
                pattern: pattern.into(),
                handler: h,
            });
    }

    /// Aborts the consumers and closes the AMQP connection. Idempotent
    /// and safe to call when never started (parity with pyfly's `stop`).
    ///
    /// The close path is tolerant of an **already-closing or already-closed**
    /// connection or channel: a consumer task can race ahead and begin closing
    /// its channel (and, with it, the underlying connection) before `stop` runs,
    /// in which case `lapin` reports `InvalidChannelState(Closing|Closed)` /
    /// `InvalidConnectionState(Closing|Closed)`. Those mean the resource is
    /// already (being) torn down — exactly the outcome `stop` wants — so they are
    /// treated as success rather than propagated as a 500.
    pub async fn stop(&self) -> EdaResult<()> {
        let (connection, consumers) = {
            let mut state = self
                .state
                .lock()
                .expect("firefly/eda-rabbitmq: lock poisoned");
            state.started = false;
            state.closed = true;
            state.publish_channel = None;
            (
                state.connection.take(),
                std::mem::take(&mut state.consumers),
            )
        };
        for c in consumers {
            c.abort();
        }
        if let Some(connection) = connection {
            match connection.close(0, "stopped").await {
                Ok(()) => {}
                Err(e) if is_already_closing(&e) => {
                    // The connection/channel was already (being) closed — for a
                    // `stop()` that is the desired end state, so swallow it.
                    tracing::debug!(
                        error = %e,
                        "firefly/eda-rabbitmq: connection already closing/closed on stop; treating as stopped"
                    );
                }
                Err(e) => return Err(map_lapin(e)),
            }
        }
        Ok(())
    }
}

/// Returns `true` for the `lapin` errors that mean a channel or connection is
/// already closing or fully closed — the benign outcomes a `stop()`/`close()`
/// should treat as "already stopped" rather than propagate. Covers both the
/// channel-level (`InvalidChannelState`) and connection-level
/// (`InvalidConnectionState`) `Closing`/`Closed` states.
fn is_already_closing(err: &lapin::Error) -> bool {
    matches!(
        err,
        lapin::Error::InvalidChannelState(ChannelState::Closing | ChannelState::Closed)
            | lapin::Error::InvalidConnectionState(
                ConnectionState::Closing | ConnectionState::Closed
            )
    )
}

/// The per-queue consume loop: for each delivery, [`decide`] the
/// acknowledgement, then apply it (`ack` / `nack(requeue)` / `reject`).
async fn consume_loop(
    _channel: Channel,
    mut consumer: lapin::Consumer,
    subscriptions: Arc<Mutex<Vec<Subscription>>>,
) {
    while let Some(delivery) = consumer.next().await {
        let delivery = match delivery {
            Ok(delivery) => delivery,
            Err(err) => {
                tracing::error!(error = %err, "firefly/eda-rabbitmq: consumer stream error");
                continue;
            }
        };
        let snapshot = {
            subscriptions
                .lock()
                .expect("firefly/eda-rabbitmq: lock poisoned")
                .clone()
        };
        let result = match decide(&snapshot, &delivery.data).await {
            Ack::Ack => delivery.acker.ack(BasicAckOptions::default()).await,
            Ack::NackRequeue => {
                delivery
                    .acker
                    .nack(BasicNackOptions {
                        requeue: true,
                        ..Default::default()
                    })
                    .await
            }
            Ack::RejectDrop => {
                delivery
                    .acker
                    .reject(BasicRejectOptions { requeue: false })
                    .await
            }
        };
        if let Err(err) = result {
            tracing::error!(error = %err, "firefly/eda-rabbitmq: ack/nack/reject failed");
        }
    }
}

/// Maps a `lapin` error into the kernel error family.
fn map_lapin(err: lapin::Error) -> EdaError {
    EdaError::from(firefly_kernel::FireflyError::internal(err.to_string()))
}

#[async_trait]
impl Publisher for RabbitMqBroker {
    async fn publish(&self, ev: Event) -> EdaResult<()> {
        self.publish_event(ev).await
    }

    async fn close(&self) -> EdaResult<()> {
        self.stop().await
    }
}

#[async_trait]
impl Subscriber for RabbitMqBroker {
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()> {
        // `topic` is treated as the fnmatch pattern over the event type,
        // matching pyfly's subscribe(event_type_pattern, handler).
        self.subscribe_pattern(topic, h);
        Ok(())
    }

    async fn close(&self) -> EdaResult<()> {
        self.stop().await
    }
}

/// Constructs a RabbitMQ-backed [`Broker`] from `config`, returning it
/// boxed behind the [`Broker`] port — the registered factory the EDA
/// starter calls in place of `firefly_eda::new_rabbitmq_broker`'s
/// sentinel when the configuration selects RabbitMQ.
///
/// The connection is opened lazily on the first
/// [`start`](RabbitMqBroker::start) / publish, so this constructor never
/// performs I/O and cannot fail.
pub fn new_rabbitmq_broker(config: RabbitMqBrokerConfig) -> EdaResult<Box<dyn Broker>> {
    Ok(Box::new(RabbitMqBroker::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_eda::handler;
    use firefly_kernel::FireflyError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn subs_collecting(pattern: &str, counter: Arc<AtomicUsize>) -> Vec<Subscription> {
        vec![Subscription {
            pattern: pattern.into(),
            handler: handler(move |_ev| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        }]
    }

    fn good_body(event_type: &str, destination: &str) -> Vec<u8> {
        let ev = Event::new(destination, event_type, "test", Some(b"{}".to_vec()));
        serde_json::to_vec(&ev).unwrap()
    }

    #[tokio::test]
    async fn decide_dispatches_matching_and_acks() {
        let counter = Arc::new(AtomicUsize::new(0));
        let subs = subs_collecting("order.*", counter.clone());
        let body = good_body("order.created", "orders");
        assert_eq!(decide(&subs, &body).await, Ack::Ack);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn decide_non_matching_acks_without_dispatch() {
        let counter = Arc::new(AtomicUsize::new(0));
        let subs = subs_collecting("payment.*", counter.clone());
        let body = good_body("order.created", "events");
        assert_eq!(decide(&subs, &body).await, Ack::Ack);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn decide_undeserializable_body_is_dropped() {
        let subs: Vec<Subscription> = vec![];
        assert_eq!(decide(&subs, b"not-valid-json").await, Ack::RejectDrop);
    }

    #[tokio::test]
    async fn decide_handler_failure_nacks_with_requeue() {
        let subs = vec![Subscription {
            pattern: "order.*".into(),
            handler: handler(|_ev| async { Err(FireflyError::internal("boom")) }),
        }];
        let body = good_body("order.created", "orders");
        assert_eq!(decide(&subs, &body).await, Ack::NackRequeue);
    }

    #[test]
    fn factory_returns_boxed_broker_without_io() {
        let broker = new_rabbitmq_broker(RabbitMqBrokerConfig::default());
        assert!(broker.is_ok());
    }

    #[tokio::test]
    async fn subscribe_pattern_is_picked_up_by_decide() {
        // A subscription added via the Subscriber port is visible to the
        // dispatch policy that the consume loop runs.
        let broker = RabbitMqBroker::new(RabbitMqBrokerConfig::default());
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = counter.clone();
        broker
            .subscribe(
                "order.*",
                handler(move |_ev| {
                    let counter2 = counter2.clone();
                    async move {
                        counter2.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            )
            .await
            .unwrap();

        let snapshot = broker.subscriptions.lock().unwrap().clone();
        let body = good_body("order.created", "orders");
        assert_eq!(decide(&snapshot, &body).await, Ack::Ack);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn publish_after_close_is_rejected() {
        let broker = RabbitMqBroker::new(RabbitMqBrokerConfig::default());
        broker.stop().await.unwrap();
        let ev = Event::new("orders", "order.created", "test", None);
        assert!(matches!(
            broker.publish_event(ev).await,
            Err(EdaError::Closed)
        ));
    }

    #[tokio::test]
    async fn stop_when_never_started_is_safe() {
        let broker = RabbitMqBroker::new(RabbitMqBrokerConfig::default());
        assert!(broker.stop().await.is_ok());
    }

    #[test]
    fn already_closing_states_are_treated_as_stopped() {
        // Channel-level closing/closed and connection-level closing/closed are
        // the benign outcomes a `stop()` swallows.
        assert!(is_already_closing(&lapin::Error::InvalidChannelState(
            ChannelState::Closing
        )));
        assert!(is_already_closing(&lapin::Error::InvalidChannelState(
            ChannelState::Closed
        )));
        assert!(is_already_closing(&lapin::Error::InvalidConnectionState(
            ConnectionState::Closing
        )));
        assert!(is_already_closing(&lapin::Error::InvalidConnectionState(
            ConnectionState::Closed
        )));
    }

    #[test]
    fn other_states_and_errors_are_not_swallowed() {
        // A channel in `Error` / `Initial` / `Connected` is a genuine failure,
        // as is any non-state error — those must still propagate.
        assert!(!is_already_closing(&lapin::Error::InvalidChannelState(
            ChannelState::Error
        )));
        assert!(!is_already_closing(&lapin::Error::InvalidChannelState(
            ChannelState::Connected
        )));
        assert!(!is_already_closing(&lapin::Error::InvalidConnectionState(
            ConnectionState::Connected
        )));
        assert!(!is_already_closing(&lapin::Error::ChannelsLimitReached));
    }
}
