//! Connection and consumer-group configuration for the Redis Streams
//! broker.

/// Wiring for [`RedisStreamsBroker`](crate::RedisStreamsBroker) — the
/// Rust spelling of pyfly's `RedisStreamsEventBus(url=…, streams=…,
/// group=…, consumer_id=…, block_ms=…)` constructor keywords.
///
/// Build one with [`RedisConfig::new`] (which applies the same defaults
/// pyfly uses) and tweak fields as needed:
///
/// ```
/// use firefly_eda_redis::RedisConfig;
///
/// let cfg = RedisConfig::new("redis://localhost:6379/0")
///     .with_streams(["orders", "payments"])
///     .with_group("orders-svc")
///     .with_consumer_id("orders-svc-1")
///     .with_block_ms(2_000)
///     .with_count(32);
/// assert_eq!(cfg.group, "orders-svc");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisConfig {
    /// Redis connection URL (`redis://` or `rediss://`).
    pub url: String,
    /// Streams the consumer reads from; each stream key is a
    /// destination. Defaults to `["firefly.events"]`, mirroring pyfly's
    /// `["pyfly.events"]` default.
    pub streams: Vec<String>,
    /// Consumer-group name. Defaults to `"firefly-default"`
    /// (pyfly: `"pyfly-default"`).
    pub group: String,
    /// Stable consumer identifier inside the group; used by
    /// `XREADGROUP` for pending-entry tracking. Defaults to the
    /// machine hostname (pyfly: `socket.gethostname()`).
    pub consumer_id: String,
    /// `XREADGROUP` long-poll block timeout, in milliseconds. Defaults
    /// to `5000` (pyfly: `block_ms=5000`).
    pub block_ms: usize,
    /// Maximum entries returned per `XREADGROUP` call. Defaults to `10`
    /// (pyfly: `count=10`).
    pub count: usize,
}

impl RedisConfig {
    /// Builds a config for `url` with pyfly's defaults: a single
    /// `firefly.events` stream, the `firefly-default` group, the local
    /// hostname as consumer id, a 5-second block, and a batch of 10.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            streams: vec!["firefly.events".to_string()],
            group: "firefly-default".to_string(),
            consumer_id: default_consumer_id(),
            block_ms: 5_000,
            count: 10,
        }
    }

    /// Replaces the set of streams the consumer reads from.
    #[must_use]
    pub fn with_streams<I, S>(mut self, streams: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.streams = streams.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the consumer-group name.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Sets the stable consumer id within the group.
    #[must_use]
    pub fn with_consumer_id(mut self, consumer_id: impl Into<String>) -> Self {
        self.consumer_id = consumer_id.into();
        self
    }

    /// Sets the `XREADGROUP` block timeout in milliseconds.
    #[must_use]
    pub fn with_block_ms(mut self, block_ms: usize) -> Self {
        self.block_ms = block_ms;
        self
    }

    /// Sets the maximum number of entries returned per `XREADGROUP`.
    #[must_use]
    pub fn with_count(mut self, count: usize) -> Self {
        self.count = count;
        self
    }
}

/// Best-effort stable consumer id: the machine hostname, falling back
/// to `"firefly-consumer"` if the hostname cannot be read — the Rust
/// analog of pyfly's `socket.gethostname()` default.
fn default_consumer_id() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| hostname_via_uname().filter(|h| !h.is_empty()))
        .unwrap_or_else(|| "firefly-consumer".to_string())
}

/// Reads the hostname from the C library when the `HOSTNAME` env var is
/// not set. Returns `None` on any failure so callers fall back to a
/// deterministic constant.
fn hostname_via_uname() -> Option<String> {
    // `gethostname(2)` via the standard library is not exposed, so read
    // the conventional environment / files used by POSIX shells. Keep it
    // dependency-free: this is only a best-effort default and any value
    // is acceptable for consumer-group identification.
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_applies_pyfly_defaults() {
        let cfg = RedisConfig::new("redis://localhost:6379/0");
        assert_eq!(cfg.url, "redis://localhost:6379/0");
        assert_eq!(cfg.streams, vec!["firefly.events".to_string()]);
        assert_eq!(cfg.group, "firefly-default");
        assert_eq!(cfg.block_ms, 5_000);
        assert_eq!(cfg.count, 10);
        assert!(!cfg.consumer_id.is_empty());
    }

    #[test]
    fn builders_override_each_field() {
        let cfg = RedisConfig::new("redis://x")
            .with_streams(["a", "b"])
            .with_group("g")
            .with_consumer_id("c-1")
            .with_block_ms(123)
            .with_count(7);
        assert_eq!(cfg.streams, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(cfg.group, "g");
        assert_eq!(cfg.consumer_id, "c-1");
        assert_eq!(cfg.block_ms, 123);
        assert_eq!(cfg.count, 7);
    }
}
