//! Connection configuration and `rdkafka` client-config building.

use rdkafka::config::ClientConfig;

/// Connection settings for a [`KafkaBroker`](crate::KafkaBroker).
///
/// Field-for-field the shape of [`firefly_eda::KafkaConfig`] so the
/// starter can hand the same configuration to either the scaffold or
/// this concrete adapter, plus a [`KafkaConfig::with_property`] escape
/// hatch for arbitrary `librdkafka` tuning.
///
/// ```
/// use firefly_eda_kafka::KafkaConfig;
///
/// let cfg = KafkaConfig {
///     brokers: vec!["kafka:9092".into()],
///     consumer_group: "orders-svc".into(),
///     ..Default::default()
/// };
/// assert_eq!(cfg.bootstrap_servers(), "kafka:9092");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KafkaConfig {
    /// Bootstrap broker addresses, e.g. `["kafka:9092"]`. Joined with
    /// commas into librdkafka's `bootstrap.servers`.
    pub brokers: Vec<String>,
    /// Client id presented to the cluster (`client.id`); left to
    /// librdkafka's default when empty.
    pub client_id: String,
    /// Consumer-group id for the subscribing side (`group.id`); left to
    /// librdkafka's default when empty.
    pub consumer_group: String,
    /// When `true`, dials the brokers over TLS by setting
    /// `security.protocol=ssl`.
    pub tls: bool,
    /// Schema-registry endpoint, when Avro/Protobuf schemas are used.
    /// Carried for parity with the scaffold; the JSON codec ignores it.
    pub schema_reg_url: String,
    /// Extra `librdkafka` properties applied verbatim to both the
    /// producer and consumer configs after the derived defaults, so
    /// they win on conflict — the escape hatch for any tuning this
    /// struct does not surface (e.g. `acks`, `auto.offset.reset`,
    /// SASL credentials).
    pub properties: Vec<(String, String)>,
}

impl KafkaConfig {
    /// Returns the comma-joined `bootstrap.servers` string librdkafka
    /// expects, derived from [`KafkaConfig::brokers`].
    #[must_use]
    pub fn bootstrap_servers(&self) -> String {
        self.brokers.join(",")
    }

    /// Adds an extra `librdkafka` property (builder style) applied to
    /// both producer and consumer; later entries override earlier ones
    /// and the derived defaults.
    #[must_use]
    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.push((key.into(), value.into()));
        self
    }

    /// Builds the producer [`ClientConfig`]: `bootstrap.servers`,
    /// optional `client.id` / `security.protocol`, then every
    /// [`KafkaConfig::properties`] entry.
    #[must_use]
    pub fn producer_config(&self) -> ClientConfig {
        let mut cfg = ClientConfig::new();
        cfg.set("bootstrap.servers", self.bootstrap_servers());
        if !self.client_id.is_empty() {
            cfg.set("client.id", &self.client_id);
        }
        self.apply_common(&mut cfg);
        cfg
    }

    /// Builds the consumer [`ClientConfig`]: the producer defaults plus
    /// `group.id` (when set), auto-commit enabled, and
    /// `auto.offset.reset=earliest` — matching pyfly's `KafkaEventBus`
    /// consumer (`enable_auto_commit=True`, `auto_offset_reset="earliest"`).
    /// [`KafkaConfig::properties`] are applied last so they can override
    /// any of these.
    #[must_use]
    pub fn consumer_config(&self) -> ClientConfig {
        let mut cfg = ClientConfig::new();
        cfg.set("bootstrap.servers", self.bootstrap_servers());
        if !self.client_id.is_empty() {
            cfg.set("client.id", &self.client_id);
        }
        if !self.consumer_group.is_empty() {
            cfg.set("group.id", &self.consumer_group);
        }
        cfg.set("enable.auto.commit", "true");
        cfg.set("auto.offset.reset", "earliest");
        self.apply_common(&mut cfg);
        cfg
    }

    /// Applies the TLS toggle and user-supplied properties shared by the
    /// producer and consumer configs.
    fn apply_common(&self, cfg: &mut ClientConfig) {
        if self.tls {
            cfg.set("security.protocol", "ssl");
        }
        for (key, value) in &self.properties {
            cfg.set(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_servers_joins_brokers_with_commas() {
        let cfg = KafkaConfig {
            brokers: vec!["a:9092".into(), "b:9092".into()],
            ..Default::default()
        };
        assert_eq!(cfg.bootstrap_servers(), "a:9092,b:9092");
    }

    #[test]
    fn bootstrap_servers_empty_when_no_brokers() {
        assert_eq!(KafkaConfig::default().bootstrap_servers(), "");
    }

    #[test]
    fn producer_config_sets_bootstrap_and_client_id() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            client_id: "orders".into(),
            ..Default::default()
        };
        let client = cfg.producer_config();
        assert_eq!(client.get("bootstrap.servers"), Some("kafka:9092"));
        assert_eq!(client.get("client.id"), Some("orders"));
    }

    #[test]
    fn producer_config_omits_empty_client_id() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            ..Default::default()
        };
        assert_eq!(cfg.producer_config().get("client.id"), None);
    }

    #[test]
    fn consumer_config_mirrors_pyfly_defaults() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            consumer_group: "svc".into(),
            ..Default::default()
        };
        let client = cfg.consumer_config();
        assert_eq!(client.get("group.id"), Some("svc"));
        assert_eq!(client.get("enable.auto.commit"), Some("true"));
        assert_eq!(client.get("auto.offset.reset"), Some("earliest"));
    }

    #[test]
    fn consumer_config_omits_empty_group() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            ..Default::default()
        };
        assert_eq!(cfg.consumer_config().get("group.id"), None);
    }

    #[test]
    fn tls_sets_security_protocol() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            tls: true,
            ..Default::default()
        };
        assert_eq!(cfg.producer_config().get("security.protocol"), Some("ssl"));
        assert_eq!(cfg.consumer_config().get("security.protocol"), Some("ssl"));
    }

    #[test]
    fn no_tls_leaves_security_protocol_unset() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            ..Default::default()
        };
        assert_eq!(cfg.producer_config().get("security.protocol"), None);
    }

    #[test]
    fn extra_properties_apply_to_both_and_win_on_conflict() {
        let cfg = KafkaConfig {
            brokers: vec!["kafka:9092".into()],
            ..Default::default()
        }
        .with_property("auto.offset.reset", "latest")
        .with_property("acks", "all");
        let consumer = cfg.consumer_config();
        // user property overrides the derived earliest default
        assert_eq!(consumer.get("auto.offset.reset"), Some("latest"));
        let producer = cfg.producer_config();
        assert_eq!(producer.get("acks"), Some("all"));
    }
}
