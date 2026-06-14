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

//! Broker configuration and the unit-testable AMQP declaration plan.

/// Connection and topology settings for a [`RabbitMqBroker`](crate::RabbitMqBroker).
///
/// Mirrors the constructor parameters of pyfly's `RabbitMqEventBus`
/// (`url`, `exchange_name`, `destinations`, `group`) so the Rust adapter
/// declares the identical RabbitMQ topology: a single durable `direct`
/// exchange, and one durable queue named `<group>.<destination>` bound
/// to the exchange with `<destination>` as its routing key, for every
/// configured destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RabbitMqBrokerConfig {
    /// AMQP connection URL, e.g. `amqp://guest:guest@localhost:5672/`.
    ///
    /// Defaults to `amqp://guest:guest@localhost/`.
    pub url: String,
    /// Name of the durable `direct` exchange to declare and publish to.
    ///
    /// Defaults to `firefly`.
    pub exchange: String,
    /// Routing keys the consumer binds to. Each destination gets a
    /// durable queue named `<group>.<destination>` bound with that
    /// destination as routing key.
    ///
    /// Defaults to `["firefly.events"]`.
    pub destinations: Vec<String>,
    /// Consumer-group prefix used in queue names. Defaults to
    /// `firefly-default`.
    pub group: String,
}

impl Default for RabbitMqBrokerConfig {
    fn default() -> Self {
        Self {
            url: "amqp://guest:guest@localhost/".to_string(),
            exchange: "firefly".to_string(),
            destinations: vec!["firefly.events".to_string()],
            group: "firefly-default".to_string(),
        }
    }
}

impl RabbitMqBrokerConfig {
    /// Returns the default configuration (the local-guest broker, the
    /// `firefly` exchange, the `firefly.events` destination, the
    /// `firefly-default` group).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the AMQP connection URL and returns the config.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Sets the durable `direct` exchange name and returns the config.
    #[must_use]
    pub fn with_exchange(mut self, exchange: impl Into<String>) -> Self {
        self.exchange = exchange.into();
        self
    }

    /// Replaces the destination list (one bound queue each) and returns
    /// the config.
    #[must_use]
    pub fn with_destinations<I, S>(mut self, destinations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.destinations = destinations.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the consumer-group queue-name prefix and returns the config.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Computes the queue name for `destination`: `<group>.<destination>`,
    /// the exact scheme pyfly's `_start_consumer` uses.
    pub fn queue_name(&self, destination: &str) -> String {
        format!("{}.{}", self.group, destination)
    }

    /// Produces the full AMQP [`DeclarationPlan`] — the ordered set of
    /// `exchange_declare`, `queue_declare`, `queue_bind`, and
    /// `basic_consume` operations the broker performs on
    /// [`start`](crate::RabbitMqBroker::start).
    ///
    /// Exposing the plan as data makes the topology assertable in a
    /// unit test without a live RabbitMQ, matching pyfly's
    /// `test_start_declares_exchange_and_queues` which checks the
    /// declared exchange, the two queue names, and that each queue is
    /// bound and consumed.
    pub fn declaration_plan(&self) -> DeclarationPlan {
        DeclarationPlan {
            exchange: ExchangeDeclaration {
                name: self.exchange.clone(),
                durable: true,
            },
            queues: self
                .destinations
                .iter()
                .map(|destination| QueueDeclaration {
                    name: self.queue_name(destination),
                    durable: true,
                    routing_key: destination.clone(),
                    exchange: self.exchange.clone(),
                })
                .collect(),
        }
    }
}

/// The ordered topology a [`RabbitMqBroker`](crate::RabbitMqBroker)
/// declares before consuming: one exchange and one bound queue per
/// destination. Built by [`RabbitMqBrokerConfig::declaration_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationPlan {
    /// The single durable `direct` exchange declared first.
    pub exchange: ExchangeDeclaration,
    /// One durable queue per destination, each bound and consumed.
    pub queues: Vec<QueueDeclaration>,
}

/// A planned `exchange_declare`: a durable `direct` exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeDeclaration {
    /// Exchange name.
    pub name: String,
    /// Whether the exchange survives a broker restart (always `true`).
    pub durable: bool,
}

/// A planned `queue_declare` + `queue_bind` + `basic_consume` for one
/// destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueDeclaration {
    /// Queue name, `<group>.<destination>`.
    pub name: String,
    /// Whether the queue survives a broker restart (always `true`).
    pub durable: bool,
    /// Routing key the queue is bound with (the destination itself).
    pub routing_key: String,
    /// Exchange the queue binds to.
    pub exchange: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_firefly_topology() {
        let cfg = RabbitMqBrokerConfig::default();
        assert_eq!(cfg.url, "amqp://guest:guest@localhost/");
        assert_eq!(cfg.exchange, "firefly");
        assert_eq!(cfg.destinations, vec!["firefly.events".to_string()]);
        assert_eq!(cfg.group, "firefly-default");
    }

    #[test]
    fn queue_name_is_group_dot_destination() {
        let cfg = RabbitMqBrokerConfig::default().with_group("svc");
        assert_eq!(cfg.queue_name("orders"), "svc.orders");
        assert_eq!(cfg.queue_name("payments"), "svc.payments");
    }

    #[test]
    fn declaration_plan_declares_one_durable_direct_exchange() {
        let cfg = RabbitMqBrokerConfig::default().with_exchange("test-exchange");
        let plan = cfg.declaration_plan();
        assert_eq!(plan.exchange.name, "test-exchange");
        assert!(plan.exchange.durable);
    }

    #[test]
    fn declaration_plan_one_bound_queue_per_destination() {
        // pyfly test_start_declares_exchange_and_queues: two destinations,
        // queue names svc.orders / svc.payments, each bound + consumed.
        let cfg = RabbitMqBrokerConfig::default()
            .with_exchange("test-exchange")
            .with_destinations(["orders", "payments"])
            .with_group("svc");
        let plan = cfg.declaration_plan();

        assert_eq!(plan.queues.len(), 2);
        let names: Vec<&str> = plan.queues.iter().map(|q| q.name.as_str()).collect();
        assert!(names.contains(&"svc.orders"));
        assert!(names.contains(&"svc.payments"));

        for q in &plan.queues {
            assert!(q.durable);
            assert_eq!(q.exchange, "test-exchange");
            // Routing key equals the destination, and the queue name is
            // <group>.<routing_key>.
            assert_eq!(q.name, format!("svc.{}", q.routing_key));
        }
    }

    #[test]
    fn builders_compose() {
        let cfg = RabbitMqBrokerConfig::new()
            .with_url("amqp://test/")
            .with_exchange("ex")
            .with_destinations(vec!["a".to_string(), "b".to_string()])
            .with_group("g");
        assert_eq!(cfg.url, "amqp://test/");
        assert_eq!(cfg.exchange, "ex");
        assert_eq!(cfg.destinations, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(cfg.group, "g");
    }
}
