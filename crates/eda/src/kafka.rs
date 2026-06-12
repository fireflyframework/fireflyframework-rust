//! The Kafka transport scaffold.

use crate::{Broker, EdaError, EdaResult};

/// Captures the wiring needed by a production Kafka adapter. The
/// concrete client is supplied by a dedicated transport crate; this
/// crate only carries the contract so services can be tested against
/// [`InMemoryBroker`](crate::InMemoryBroker) and switched to Kafka via
/// configuration — the same scaffolding rationale as the Go module's
/// `KafkaConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KafkaConfig {
    /// Bootstrap broker addresses, e.g. `["kafka:9092"]`.
    pub brokers: Vec<String>,
    /// Client id presented to the cluster.
    pub client_id: String,
    /// Consumer-group id for the subscribing side.
    pub consumer_group: String,
    /// Whether to dial the brokers over TLS.
    pub tls: bool,
    /// Schema-registry endpoint, when Avro/Protobuf schemas are used.
    pub schema_reg_url: String,
}

/// The placeholder factory invoked by the starter when the EDA
/// configuration selects Kafka. Until a real Kafka-backed crate is
/// registered (planned: `firefly-eda-kafka`), this returns
/// [`EdaError::KafkaUnavailable`] — Go's `ErrKafkaUnavailable`.
///
/// This explicit failure is deliberate — it surfaces a missing
/// dependency at startup rather than silently falling back to
/// in-memory.
pub fn new_kafka_broker(_config: KafkaConfig) -> EdaResult<Box<dyn Broker>> {
    Err(EdaError::KafkaUnavailable)
}
