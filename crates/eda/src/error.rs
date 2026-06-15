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

//! The error family of the EDA port.

use firefly_kernel::FireflyError;

/// Crate-local result alias: every fallible broker operation returns
/// [`EdaError`].
pub type EdaResult<T> = Result<T, EdaError>;

/// Errors produced by EDA brokers and the transport scaffolds.
///
/// Mirrors the Go module's error surface: the two `*Unavailable`
/// variants are the Rust spelling of Go's `ErrKafkaUnavailable` /
/// `ErrRabbitMQUnavailable` sentinels (test for them with
/// [`matches!`] or the [`From`] conversion below), [`EdaError::Closed`]
/// stands in for the `context.Canceled` a closed Go broker returns, and
/// [`EdaError::Handler`] carries a subscriber's
/// [`FireflyError`] back to the publisher *unchanged* — just like Go's
/// in-memory `Publish` returns the first handler error verbatim.
#[derive(Debug, thiserror::Error)]
pub enum EdaError {
    /// The placeholder Kafka factory was invoked but no real
    /// Kafka-backed [`Broker`](crate::Broker) is registered.
    ///
    /// The Rust spelling of Go's `ErrKafkaUnavailable` sentinel. The
    /// explicit failure is deliberate — it surfaces a missing
    /// dependency at startup rather than silently falling back to
    /// in-memory.
    #[error(
        "firefly/eda: kafka adapter not registered (use new_kafka_broker from an eda-kafka crate)"
    )]
    KafkaUnavailable,

    /// The placeholder RabbitMQ factory was invoked but no real
    /// AMQP-backed [`Broker`](crate::Broker) is registered.
    ///
    /// Mirrors [`EdaError::KafkaUnavailable`] for AMQP — Go's
    /// `ErrRabbitMQUnavailable`.
    #[error("firefly/eda: rabbitmq adapter not registered (use new_rabbitmq_broker from an eda-rabbitmq crate)")]
    RabbitMqUnavailable,

    /// The broker was closed; subsequent publishes and subscriptions
    /// are rejected. The Go in-memory broker returns
    /// `context.Canceled` here.
    #[error("firefly/eda: broker closed")]
    Closed,

    /// No process-wide [`Broker`](crate::Broker) was registered, so a
    /// [`publish_to_broker`](crate::publish_to_broker) call or an externalized
    /// event had nowhere to go. Register one at startup with
    /// [`register_broker`](crate::register_broker).
    #[error("firefly/eda: no broker registered (call register_broker at startup)")]
    BrokerUnavailable,

    /// A subscriber handler failed during delivery. The wrapped
    /// [`FireflyError`] is the handler's error, returned to the
    /// publisher unchanged (display and source chain pass through
    /// transparently).
    #[error(transparent)]
    Handler(#[from] FireflyError),

    /// An [`EventSerializer`](crate::EventSerializer) failed to encode or
    /// decode an [`Event`](crate::Event), or was a not-yet-implemented
    /// codec (Avro / Protobuf). The message names the serializer and the
    /// cause, mirroring pyfly's `NotImplementedError` / `json` decode
    /// failures.
    #[error("firefly/eda: serialization ({serializer}): {message}")]
    Serialization {
        /// The serializer [`name`](crate::EventSerializer::name) that
        /// produced the failure.
        serializer: String,
        /// Human-readable cause.
        message: String,
    },
}

impl From<EdaError> for FireflyError {
    /// Renders an [`EdaError`] in the kernel's canonical error family:
    /// handler failures pass through unchanged, a closed broker maps to
    /// `409 Conflict`, and the missing-transport sentinels map to
    /// `500 Internal Server Error` with the sentinel message as detail.
    fn from(err: EdaError) -> Self {
        match err {
            EdaError::Handler(e) => e,
            e @ EdaError::Closed => FireflyError::conflict(e.to_string()),
            e => FireflyError::internal(e.to_string()),
        }
    }
}
