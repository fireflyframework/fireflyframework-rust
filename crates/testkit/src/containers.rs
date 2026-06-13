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

//! Testcontainers-style integration-test fixtures — the `@ServiceConnection`
//! equivalent.
//!
//! The Spring Boot `@Testcontainers` / `@ServiceConnection` workflow is: spin up
//! a real Postgres / MySQL / Redis / MongoDB / Kafka / RabbitMQ, then wire its
//! connection details straight into framework config keys. pyfly does this with
//! `pyfly.testing.testcontainers` (`postgres_container()` … plus
//! `pyfly_config_for(container)` / `pyfly_config(*containers)`).
//!
//! This module is the Rust analog, decoupled from any specific container
//! runtime: it takes the *connection details* you already have — a connection
//! URL, or a `(host, port)` start handle from any container library, Docker
//! Compose, or a long-running local service — and produces a ready
//! [`ConfigOverrides`] mapping to the canonical `firefly.*` config keys (the
//! `@ServiceConnection` step). [`docker_available`] is the
//! `requires_docker`-skip guard so an integration test skips cleanly where no
//! container runtime is reachable.
//!
//! Available only with the `testcontainers` feature.
//!
//! ```
//! # #[cfg(feature = "testcontainers")] {
//! use firefly_testkit::containers::{ServiceContainer, config_for};
//!
//! // A `testcontainers`/`dockertest` handle hands you a host + mapped port;
//! // wrap it (here a literal for the doctest) and map it to framework config.
//! let pg = ServiceContainer::postgres_at("127.0.0.1", 54_321, "app", "secret", "app");
//! let overrides = config_for(&pg);
//! assert_eq!(
//!     overrides.get("firefly.data.url"),
//!     Some("postgres://app:secret@127.0.0.1:54321/app")
//! );
//! # }
//! ```
//!
//! # Guarding a real-infra test
//!
//! ```
//! # #[cfg(feature = "testcontainers")] {
//! use firefly_testkit::containers::docker_available;
//!
//! # fn body() {
//! if !docker_available() {
//!     // No container runtime reachable — skip cleanly (the `requires_docker` analog).
//!     return;
//! }
//! // ... start a container, map it with `config_for`, boot the app ...
//! # }
//! # }
//! ```

use std::collections::BTreeMap;

/// A started backing service's connection details, ready to map to framework
/// config via [`config_for`].
///
/// Each variant carries exactly the fields the corresponding `firefly.*` config
/// keys need. Construct one from the host + mapped port a container library
/// (`testcontainers`, `dockertest`, Docker Compose, …) hands back, or directly
/// from a connection URL with [`ServiceContainer::from_url`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceContainer {
    /// A PostgreSQL server (`firefly.data.url` / `firefly.datasource.url`).
    Postgres {
        /// Bound host (e.g. `"127.0.0.1"`).
        host: String,
        /// Mapped port for the container's `5432`.
        port: u16,
        /// Database user.
        user: String,
        /// Database password.
        password: String,
        /// Database name.
        database: String,
    },
    /// A MySQL server (`firefly.data.url` / `firefly.datasource.url`).
    MySql {
        /// Bound host.
        host: String,
        /// Mapped port for the container's `3306`.
        port: u16,
        /// Database user.
        user: String,
        /// Database password.
        password: String,
        /// Database name.
        database: String,
    },
    /// A Redis server (`firefly.cache.redis.url` + `firefly.session.redis.url`).
    Redis {
        /// Bound host.
        host: String,
        /// Mapped port for the container's `6379`.
        port: u16,
        /// Logical database index (defaults to `0`).
        db: u8,
    },
    /// A MongoDB server (`firefly.data.url` / `firefly.data.document.uri`).
    MongoDb {
        /// Bound host.
        host: String,
        /// Mapped port for the container's `27017`.
        port: u16,
    },
    /// A Kafka broker (`firefly.eda.kafka.bootstrap-servers`).
    Kafka {
        /// `host:port` bootstrap server.
        bootstrap_servers: String,
    },
    /// A RabbitMQ broker (`firefly.eda.rabbitmq.url` + `firefly.messaging.rabbitmq.url`).
    RabbitMq {
        /// Bound host.
        host: String,
        /// Mapped port for the container's `5672`.
        port: u16,
        /// Broker user (defaults to `guest`).
        user: String,
        /// Broker password (defaults to `guest`).
        password: String,
    },
}

impl ServiceContainer {
    /// A Postgres handle from a host, mapped port, and credentials.
    #[must_use]
    pub fn postgres_at(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: impl Into<String>,
        database: impl Into<String>,
    ) -> Self {
        ServiceContainer::Postgres {
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            database: database.into(),
        }
    }

    /// A MySQL handle from a host, mapped port, and credentials.
    #[must_use]
    pub fn mysql_at(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: impl Into<String>,
        database: impl Into<String>,
    ) -> Self {
        ServiceContainer::MySql {
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            database: database.into(),
        }
    }

    /// A Redis handle from a host and mapped port (logical database `0`).
    #[must_use]
    pub fn redis_at(host: impl Into<String>, port: u16) -> Self {
        ServiceContainer::Redis {
            host: host.into(),
            port,
            db: 0,
        }
    }

    /// A MongoDB handle from a host and mapped port.
    #[must_use]
    pub fn mongodb_at(host: impl Into<String>, port: u16) -> Self {
        ServiceContainer::MongoDb {
            host: host.into(),
            port,
        }
    }

    /// A Kafka handle from a `host:port` bootstrap-server string.
    #[must_use]
    pub fn kafka_at(bootstrap_servers: impl Into<String>) -> Self {
        ServiceContainer::Kafka {
            bootstrap_servers: bootstrap_servers.into(),
        }
    }

    /// A RabbitMQ handle from a host and mapped port (`guest`/`guest` credentials).
    #[must_use]
    pub fn rabbitmq_at(host: impl Into<String>, port: u16) -> Self {
        ServiceContainer::RabbitMq {
            host: host.into(),
            port,
            user: "guest".to_string(),
            password: "guest".to_string(),
        }
    }

    /// Infer a [`ServiceContainer`] from a connection URL by its scheme.
    ///
    /// Recognises `postgres(ql)://`, `mysql://`, `redis(s)://`, `mongodb://`,
    /// `amqp(s)://`, and a bare `host:port` (treated as a Kafka bootstrap
    /// server). The URL is parsed with a small built-in scanner — no extra
    /// dependency — covering the `user:pass@host:port/db` shape the container
    /// libraries emit.
    ///
    /// # Errors
    /// Returns a descriptive message string when the scheme is unrecognised or
    /// the authority cannot be parsed.
    pub fn from_url(url: &str) -> Result<Self, String> {
        if let Some(rest) = url
            .strip_prefix("postgresql://")
            .or_else(|| url.strip_prefix("postgres://"))
        {
            let p = parse_authority(rest)?;
            return Ok(ServiceContainer::Postgres {
                host: p.host,
                port: p.port.unwrap_or(5432),
                user: p.user.unwrap_or_default(),
                password: p.password.unwrap_or_default(),
                database: p.path.unwrap_or_default(),
            });
        }
        if let Some(rest) = url.strip_prefix("mysql://") {
            let p = parse_authority(rest)?;
            return Ok(ServiceContainer::MySql {
                host: p.host,
                port: p.port.unwrap_or(3306),
                user: p.user.unwrap_or_default(),
                password: p.password.unwrap_or_default(),
                database: p.path.unwrap_or_default(),
            });
        }
        if let Some(rest) = url
            .strip_prefix("rediss://")
            .or_else(|| url.strip_prefix("redis://"))
        {
            let p = parse_authority(rest)?;
            let db = p.path.and_then(|s| s.parse().ok()).unwrap_or(0);
            return Ok(ServiceContainer::Redis {
                host: p.host,
                port: p.port.unwrap_or(6379),
                db,
            });
        }
        if let Some(rest) = url.strip_prefix("mongodb://") {
            let p = parse_authority(rest)?;
            return Ok(ServiceContainer::MongoDb {
                host: p.host,
                port: p.port.unwrap_or(27017),
            });
        }
        if let Some(rest) = url
            .strip_prefix("amqps://")
            .or_else(|| url.strip_prefix("amqp://"))
        {
            let p = parse_authority(rest)?;
            return Ok(ServiceContainer::RabbitMq {
                host: p.host,
                port: p.port.unwrap_or(5672),
                user: p.user.unwrap_or_else(|| "guest".to_string()),
                password: p.password.unwrap_or_else(|| "guest".to_string()),
            });
        }
        if !url.contains("://") && url.contains(':') {
            return Ok(ServiceContainer::Kafka {
                bootstrap_servers: url.to_string(),
            });
        }
        Err(format!("unrecognised connection URL scheme: {url:?}"))
    }
}

/// The parsed `user:pass@host:port/path` parts of a connection authority.
struct Authority {
    user: Option<String>,
    password: Option<String>,
    host: String,
    port: Option<u16>,
    path: Option<String>,
}

/// Parse the `user:pass@host:port/path?query` portion that follows a URL
/// scheme. Tolerant: missing pieces yield `None`.
fn parse_authority(rest: &str) -> Result<Authority, String> {
    // Drop any query string.
    let rest = rest.split('?').next().unwrap_or(rest);
    let (authority, path) = match rest.split_once('/') {
        Some((auth, path)) => (auth, (!path.is_empty()).then(|| path.to_string())),
        None => (rest, None),
    };
    let (creds, hostport) = match authority.rsplit_once('@') {
        Some((creds, hp)) => (Some(creds), hp),
        None => (None, authority),
    };
    let (user, password) = match creds {
        Some(c) => match c.split_once(':') {
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
            None => (Some(c.to_string()), None),
        },
        None => (None, None),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| format!("invalid port in authority {authority:?}"))?;
            (h.to_string(), Some(port))
        }
        None => (hostport.to_string(), None),
    };
    if host.is_empty() {
        return Err(format!("missing host in authority {authority:?}"));
    }
    Ok(Authority {
        user,
        password,
        host,
        port,
        path,
    })
}

/// Flat `firefly.*` config overrides produced by [`config_for`] — the
/// `@ServiceConnection` result.
///
/// Keys are the canonical dotted form (`firefly.data.url`,
/// `firefly.cache.redis.url`, …). Render them as a plain map with [`as_map`],
/// merge several with [`extend`], or — with the `testcontainers` feature, which
/// also pulls in `firefly-config` — turn them into a real
/// [`firefly_config::StaticSource`](https://docs.rs/firefly-config) via
/// [`into_source`](ConfigOverrides::into_source) to drop straight into the
/// framework's layered config.
///
/// [`as_map`]: ConfigOverrides::as_map
/// [`extend`]: ConfigOverrides::extend
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigOverrides {
    entries: BTreeMap<String, String>,
}

impl ConfigOverrides {
    /// An empty set of overrides.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one dotted config key (last write wins). Returns `self` for chaining.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.entries.insert(key.into(), value.into());
        self
    }

    /// The value for a dotted key, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Merge another set of overrides into this one (its keys win on conflict).
    pub fn extend(&mut self, other: &ConfigOverrides) {
        for (key, value) in &other.entries {
            self.entries.insert(key.clone(), value.clone());
        }
    }

    /// Borrow the flat dotted-key map.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.entries
    }

    /// Consume into the flat dotted-key map.
    #[must_use]
    pub fn into_map(self) -> BTreeMap<String, String> {
        self.entries
    }

    /// Turn these overrides into a named [`firefly_config::StaticSource`] so they
    /// drop straight into the framework's layered config (the
    /// `@ServiceConnection`-into-`Environment` step).
    ///
    /// Place it last in a [`firefly_config::Layered`] so its container URLs win
    /// over the application defaults.
    ///
    /// ```
    /// # #[cfg(feature = "testcontainers")] {
    /// use firefly_testkit::containers::{config_for, ServiceContainer};
    /// use firefly_config::{Layered, Source, StaticSource};
    ///
    /// let pg = ServiceContainer::postgres_at("127.0.0.1", 5432, "app", "pw", "app");
    /// let source = config_for(&pg).into_source();
    /// let layered = Layered::new(vec![
    ///     Box::new(StaticSource::new("defaults", Default::default())),
    ///     Box::new(source),
    /// ]);
    /// let flat = layered.map().unwrap();
    /// assert_eq!(flat["firefly.data.url"], "postgres://app:pw@127.0.0.1:5432/app");
    /// # }
    /// ```
    #[must_use]
    pub fn into_source(self) -> firefly_config::StaticSource {
        firefly_config::StaticSource::new("testcontainers", self.entries.into_iter().collect())
    }
}

/// Map a started [`ServiceContainer`] to framework config overrides — the Rust
/// `@ServiceConnection` / `pyfly_config_for` equivalent.
///
/// The emitted keys are the canonical dotted form the framework reads:
///
/// | Service   | Keys                                                              |
/// |-----------|-------------------------------------------------------------------|
/// | Postgres  | `firefly.data.url`, `firefly.datasource.url`                      |
/// | MySQL     | `firefly.data.url`, `firefly.datasource.url`                      |
/// | Redis     | `firefly.cache.redis.url`, `firefly.session.redis.url`            |
/// | MongoDB   | `firefly.data.url`, `firefly.data.document.uri`                   |
/// | Kafka     | `firefly.eda.kafka.bootstrap-servers`                             |
/// | RabbitMQ  | `firefly.eda.rabbitmq.url`, `firefly.messaging.rabbitmq.url`      |
#[must_use]
pub fn config_for(container: &ServiceContainer) -> ConfigOverrides {
    match container {
        ServiceContainer::Postgres {
            host,
            port,
            user,
            password,
            database,
        } => {
            let url = format!("postgres://{user}:{password}@{host}:{port}/{database}");
            ConfigOverrides::new()
                .with("firefly.data.url", url.clone())
                .with("firefly.datasource.url", url)
        }
        ServiceContainer::MySql {
            host,
            port,
            user,
            password,
            database,
        } => {
            let url = format!("mysql://{user}:{password}@{host}:{port}/{database}");
            ConfigOverrides::new()
                .with("firefly.data.url", url.clone())
                .with("firefly.datasource.url", url)
        }
        ServiceContainer::Redis { host, port, db } => {
            let url = format!("redis://{host}:{port}/{db}");
            ConfigOverrides::new()
                .with("firefly.cache.redis.url", url.clone())
                .with("firefly.session.redis.url", url)
        }
        ServiceContainer::MongoDb { host, port } => {
            let url = format!("mongodb://{host}:{port}");
            ConfigOverrides::new()
                .with("firefly.data.url", url.clone())
                .with("firefly.data.document.uri", url)
        }
        ServiceContainer::Kafka { bootstrap_servers } => {
            ConfigOverrides::new().with("firefly.eda.kafka.bootstrap-servers", bootstrap_servers)
        }
        ServiceContainer::RabbitMq {
            host,
            port,
            user,
            password,
        } => {
            let url = format!("amqp://{user}:{password}@{host}:{port}/");
            ConfigOverrides::new()
                .with("firefly.eda.rabbitmq.url", url.clone())
                .with("firefly.messaging.rabbitmq.url", url)
        }
    }
}

/// Merge [`config_for`] over every started container into one set of overrides
/// — the one-call analog of pyfly's `pyfly_config(*containers)`.
///
/// Later containers override earlier ones on key conflict.
///
/// ```
/// # #[cfg(feature = "testcontainers")] {
/// use firefly_testkit::containers::{config_for_all, ServiceContainer};
///
/// let pg = ServiceContainer::postgres_at("127.0.0.1", 5432, "u", "p", "db");
/// let redis = ServiceContainer::redis_at("127.0.0.1", 6379);
/// let overrides = config_for_all([&pg, &redis]);
/// assert!(overrides.get("firefly.data.url").is_some());
/// assert!(overrides.get("firefly.cache.redis.url").is_some());
/// # }
/// ```
#[must_use]
pub fn config_for_all<'a, I>(containers: I) -> ConfigOverrides
where
    I: IntoIterator<Item = &'a ServiceContainer>,
{
    let mut merged = ConfigOverrides::new();
    for container in containers {
        let one = config_for(container);
        merged.extend(&one);
    }
    merged
}

/// Whether a container runtime appears reachable — the `requires_docker`-skip
/// guard for integration tests.
///
/// Returns `true` when `DOCKER_HOST` is set, a Unix Docker socket exists
/// (`/var/run/docker.sock` or `$XDG_RUNTIME_DIR/docker.sock`), or the
/// `FIREFLY_TEST_DOCKER` opt-in flag is set. An integration test should call
/// this first and `return` early when it is `false`, so the suite skips cleanly
/// on machines without Docker rather than failing.
///
/// ```
/// # #[cfg(feature = "testcontainers")] {
/// use firefly_testkit::containers::docker_available;
/// // Always safe to call; never panics.
/// let _ = docker_available();
/// # }
/// ```
#[must_use]
pub fn docker_available() -> bool {
    let env_flag = std::env::var_os("FIREFLY_TEST_DOCKER").is_some()
        || std::env::var_os("DOCKER_HOST").is_some();
    let mut socket_paths: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(unix)]
    {
        socket_paths.push(std::path::PathBuf::from("/var/run/docker.sock"));
        if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            socket_paths.push(std::path::Path::new(&runtime).join("docker.sock"));
        }
    }
    docker_available_from(env_flag, &socket_paths)
}

/// Pure decision used by [`docker_available`]: `true` when an opt-in/`DOCKER_HOST`
/// env flag is set, or any candidate Docker socket path exists.
fn docker_available_from(env_flag: bool, socket_paths: &[std::path::PathBuf]) -> bool {
    env_flag || socket_paths.iter().any(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_maps_to_data_and_datasource_url() {
        let pg = ServiceContainer::postgres_at("127.0.0.1", 54_321, "app", "secret", "shop");
        let overrides = config_for(&pg);
        let url = "postgres://app:secret@127.0.0.1:54321/shop";
        assert_eq!(overrides.get("firefly.data.url"), Some(url));
        assert_eq!(overrides.get("firefly.datasource.url"), Some(url));
        assert_eq!(overrides.len(), 2);
    }

    #[test]
    fn mysql_maps_to_data_and_datasource_url() {
        let my = ServiceContainer::mysql_at("db", 3306, "root", "pw", "app");
        let overrides = config_for(&my);
        let url = "mysql://root:pw@db:3306/app";
        assert_eq!(overrides.get("firefly.data.url"), Some(url));
        assert_eq!(overrides.get("firefly.datasource.url"), Some(url));
    }

    #[test]
    fn redis_maps_to_cache_and_session_url() {
        let redis = ServiceContainer::redis_at("127.0.0.1", 6_379);
        let overrides = config_for(&redis);
        let url = "redis://127.0.0.1:6379/0";
        assert_eq!(overrides.get("firefly.cache.redis.url"), Some(url));
        assert_eq!(overrides.get("firefly.session.redis.url"), Some(url));
    }

    #[test]
    fn mongodb_maps_to_data_and_document_uri() {
        let mongo = ServiceContainer::mongodb_at("127.0.0.1", 27_017);
        let overrides = config_for(&mongo);
        let url = "mongodb://127.0.0.1:27017";
        assert_eq!(overrides.get("firefly.data.url"), Some(url));
        assert_eq!(overrides.get("firefly.data.document.uri"), Some(url));
    }

    #[test]
    fn kafka_maps_to_bootstrap_servers() {
        let kafka = ServiceContainer::kafka_at("127.0.0.1:9092");
        let overrides = config_for(&kafka);
        assert_eq!(
            overrides.get("firefly.eda.kafka.bootstrap-servers"),
            Some("127.0.0.1:9092")
        );
        assert_eq!(overrides.len(), 1);
    }

    #[test]
    fn rabbitmq_maps_to_eda_and_messaging_url() {
        let rabbit = ServiceContainer::rabbitmq_at("127.0.0.1", 5_672);
        let overrides = config_for(&rabbit);
        let url = "amqp://guest:guest@127.0.0.1:5672/";
        assert_eq!(overrides.get("firefly.eda.rabbitmq.url"), Some(url));
        assert_eq!(overrides.get("firefly.messaging.rabbitmq.url"), Some(url));
    }

    #[test]
    fn config_for_all_merges_multiple_containers() {
        let pg = ServiceContainer::postgres_at("h", 5432, "u", "p", "db");
        let redis = ServiceContainer::redis_at("h", 6379);
        let overrides = config_for_all([&pg, &redis]);
        assert_eq!(overrides.len(), 4);
        assert!(overrides.get("firefly.data.url").is_some());
        assert!(overrides.get("firefly.cache.redis.url").is_some());
    }

    #[test]
    fn from_url_parses_postgres_with_credentials() {
        let c = ServiceContainer::from_url("postgresql://app:secret@127.0.0.1:54321/shop").unwrap();
        assert_eq!(
            c,
            ServiceContainer::Postgres {
                host: "127.0.0.1".into(),
                port: 54_321,
                user: "app".into(),
                password: "secret".into(),
                database: "shop".into(),
            }
        );
        // round-trips back to the same data.url
        assert_eq!(
            config_for(&c).get("firefly.data.url"),
            Some("postgres://app:secret@127.0.0.1:54321/shop")
        );
    }

    #[test]
    fn from_url_parses_redis_with_db_index() {
        let c = ServiceContainer::from_url("redis://127.0.0.1:6380/3").unwrap();
        assert_eq!(
            c,
            ServiceContainer::Redis {
                host: "127.0.0.1".into(),
                port: 6_380,
                db: 3,
            }
        );
    }

    #[test]
    fn from_url_parses_amqp_defaults_credentials() {
        let c = ServiceContainer::from_url("amqp://host:5672/").unwrap();
        assert_eq!(
            c,
            ServiceContainer::RabbitMq {
                host: "host".into(),
                port: 5_672,
                user: "guest".into(),
                password: "guest".into(),
            }
        );
    }

    #[test]
    fn from_url_parses_mongodb_default_port() {
        let c = ServiceContainer::from_url("mongodb://localhost").unwrap();
        assert_eq!(
            c,
            ServiceContainer::MongoDb {
                host: "localhost".into(),
                port: 27_017,
            }
        );
    }

    #[test]
    fn from_url_treats_bare_hostport_as_kafka() {
        let c = ServiceContainer::from_url("broker:9092").unwrap();
        assert_eq!(
            c,
            ServiceContainer::Kafka {
                bootstrap_servers: "broker:9092".into(),
            }
        );
    }

    #[test]
    fn from_url_rejects_unknown_scheme() {
        assert!(ServiceContainer::from_url("ftp://x/y").is_err());
    }

    #[test]
    fn overrides_extend_last_wins() {
        let mut a = ConfigOverrides::new().with("k", "1");
        let b = ConfigOverrides::new().with("k", "2");
        a.extend(&b);
        assert_eq!(a.get("k"), Some("2"));
    }

    #[test]
    fn docker_available_decision_is_pure_and_deterministic() {
        // The opt-in / DOCKER_HOST env flag forces `true` without a daemon.
        assert!(docker_available_from(true, &[]));
        // A present socket path forces `true` (this crate's manifest always exists).
        let present = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(present.exists());
        assert!(docker_available_from(false, std::slice::from_ref(&present)));
        // Neither flag nor any existing socket -> `false`.
        let absent = std::path::PathBuf::from("/definitely/not/a/docker.sock");
        assert!(!docker_available_from(false, std::slice::from_ref(&absent)));
        // `docker_available()` itself never panics.
        let _ = docker_available();
    }

    #[test]
    fn into_source_yields_a_static_source() {
        use firefly_config::Source;
        let pg = ServiceContainer::postgres_at("h", 5432, "u", "p", "db");
        let source = config_for(&pg).into_source();
        assert_eq!(source.name(), "testcontainers");
        let map = source.load().unwrap();
        assert_eq!(
            map.get("firefly.data.url").map(String::as_str),
            Some("postgres://u:p@h:5432/db")
        );
    }
}
