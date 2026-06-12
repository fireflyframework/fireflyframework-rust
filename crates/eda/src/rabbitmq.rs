//! The RabbitMQ transport scaffold.

use crate::{Broker, EdaError, EdaResult};

/// Captures the wiring needed by a production RabbitMQ adapter. Same
/// scaffolding rationale as [`KafkaConfig`](crate::KafkaConfig).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RabbitMqConfig {
    /// AMQP connection URL, e.g. `amqp://guest:guest@rabbit:5672/`.
    pub url: String,
    /// Exchange events are published to.
    pub exchange: String,
    /// Queue the subscribing side consumes from.
    pub queue: String,
}

/// The placeholder factory invoked by the starter when the EDA
/// configuration selects RabbitMQ. Until a real AMQP-backed crate is
/// registered (planned: `firefly-eda-rabbitmq`), this returns
/// [`EdaError::RabbitMqUnavailable`] — Go's `ErrRabbitMQUnavailable`.
pub fn new_rabbitmq_broker(_config: RabbitMqConfig) -> EdaResult<Box<dyn Broker>> {
    Err(EdaError::RabbitMqUnavailable)
}
