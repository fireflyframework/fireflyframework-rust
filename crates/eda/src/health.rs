//! Broker liveness as a [`firefly_observability::Indicator`].
//!
//! Mirrors pyfly's `eda.health.EventPublisherHealthIndicator`: a generic
//! health indicator over the broker port that surfaces broker liveness on
//! `/actuator/health`. pyfly probes the publisher with a duck-typed
//! `ping()` (falling back to a `_started` flag); the Rust spelling is the
//! explicit [`BrokerHealth`] trait — a broker that can report liveness
//! implements `ping()`, and [`EventPublisherHealthIndicator`] adapts any
//! such broker to the framework's [`Indicator`] trait.
//!
//! [`InMemoryBroker`](crate::InMemoryBroker) implements [`BrokerHealth`]
//! (live unless closed); the Kafka / RabbitMQ transport crates can
//! implement it with a real connection probe and register their own
//! indicator alongside this one.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_eda::{EventPublisherHealthIndicator, InMemoryBroker};
//! use firefly_observability::{Indicator, Status};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let broker = Arc::new(InMemoryBroker::new());
//! let indicator = EventPublisherHealthIndicator::new(broker.clone());
//! assert_eq!(indicator.check().await.status, Status::Up);
//!
//! broker.close().unwrap();
//! assert_eq!(indicator.check().await.status, Status::Down);
//! # });
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use firefly_observability::{HealthResult, Indicator};

use crate::EdaResult;

/// A broker that can report its own liveness. The Rust spelling of the
/// `ping()` capability pyfly's `EventPublisherHealthIndicator` duck-types
/// on the publisher.
///
/// `ping` returns `Ok(())` when the broker is live and an
/// [`EdaError`](crate::EdaError) (e.g. [`Closed`](crate::EdaError::Closed))
/// when it is not — the same contract a transport's connection probe
/// would honour.
#[async_trait]
pub trait BrokerHealth: Send + Sync {
    /// Probes broker liveness.
    async fn ping(&self) -> EdaResult<()>;
}

/// A [`firefly_observability::Indicator`] over any [`BrokerHealth`]
/// broker — pyfly's `EventPublisherHealthIndicator`.
///
/// Reports `UP` when [`BrokerHealth::ping`] succeeds and `DOWN` (with the
/// error code as the message and an `error` detail) when it fails. The
/// indicator is reported under the name `"eventPublisher"` — the
/// camelCase id Spring Boot / pyfly surface on `/actuator/health`.
pub struct EventPublisherHealthIndicator {
    name: String,
    broker: Arc<dyn BrokerHealth>,
}

impl EventPublisherHealthIndicator {
    /// The default indicator id, matching pyfly / Spring Boot's
    /// `eventPublisher` health key.
    pub const DEFAULT_NAME: &'static str = "eventPublisher";

    /// Wraps `broker` as an indicator reported under
    /// [`DEFAULT_NAME`](Self::DEFAULT_NAME).
    pub fn new(broker: Arc<dyn BrokerHealth>) -> Self {
        Self {
            name: Self::DEFAULT_NAME.to_string(),
            broker,
        }
    }

    /// Wraps `broker` as an indicator reported under a custom `name`.
    pub fn with_name(name: impl Into<String>, broker: Arc<dyn BrokerHealth>) -> Self {
        Self {
            name: name.into(),
            broker,
        }
    }
}

#[async_trait]
impl Indicator for EventPublisherHealthIndicator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        match self.broker.ping().await {
            Ok(()) => HealthResult::up(),
            Err(err) => {
                let fe: firefly_kernel::FireflyError = err.into();
                HealthResult::down(fe.detail.clone()).with_detail("error", fe.code)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use firefly_observability::Status;

    use super::*;
    use crate::{EdaError, InMemoryBroker};

    /// A live in-memory broker reports `UP`.
    #[tokio::test]
    async fn live_broker_is_up() {
        let broker = Arc::new(InMemoryBroker::new());
        let indicator = EventPublisherHealthIndicator::new(broker);
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Up);
        assert_eq!(indicator.name(), "eventPublisher");
    }

    /// A closed broker reports `DOWN` with the error code in `details`.
    #[tokio::test]
    async fn closed_broker_is_down() {
        let broker = Arc::new(InMemoryBroker::new());
        broker.close().unwrap();
        let indicator = EventPublisherHealthIndicator::new(broker);
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Down);
        assert!(result.details.contains_key("error"));
    }

    /// A custom broker whose `ping` fails surfaces `DOWN` — the path a
    /// transport's connection-probe failure would take.
    #[tokio::test]
    async fn failing_ping_is_down() {
        struct DeadBroker;
        #[async_trait]
        impl BrokerHealth for DeadBroker {
            async fn ping(&self) -> EdaResult<()> {
                Err(EdaError::KafkaUnavailable)
            }
        }
        let indicator = EventPublisherHealthIndicator::with_name("kafka", Arc::new(DeadBroker));
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Down);
        assert_eq!(indicator.name(), "kafka");
        assert!(result.message.contains("kafka adapter not registered"));
    }
}
