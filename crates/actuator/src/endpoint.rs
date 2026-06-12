//! Extensible actuator endpoints — the Rust counterpart of pyfly's
//! `ActuatorEndpoint` protocol + `ActuatorRegistry`.
//!
//! Implement [`Endpoint`] and register it on an [`EndpointRegistry`]
//! passed to [`mount`](crate::mount) via
//! [`ActuatorConfig::endpoints`](crate::ActuatorConfig); the endpoint is
//! served at `GET {base_path}/{id}` (plus `GET {base_path}/{id}/{selector}`
//! when [`Endpoint::supports_selector`] is true), honoring the exposure
//! model and per-endpoint enabled overrides.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::Value;

/// A custom actuator management endpoint, exposed at
/// `{base_path}/{id}` — pyfly's `ActuatorEndpoint` protocol adapted to a
/// trait (decorator discovery becomes explicit registration).
#[async_trait]
pub trait Endpoint: Send + Sync {
    /// URL path suffix: the endpoint is served at `{base_path}/{id}`.
    fn id(&self) -> &str;

    /// Default enable state; can be overridden per id via
    /// [`ExposureConfig::endpoint_enabled`](crate::ExposureConfig).
    fn enabled(&self) -> bool {
        true
    }

    /// Whether a `GET {base_path}/{id}/{selector}` drill-down route is
    /// mounted in addition to the base route.
    fn supports_selector(&self) -> bool {
        false
    }

    /// Handles a request and returns a JSON payload. `selector` carries
    /// the `/{selector}` path segment when present; `query` the query
    /// string parameters. Return `None` for a selector that matches
    /// nothing — rendered as 404 (pyfly's contract).
    async fn handle(
        &self,
        selector: Option<&str>,
        query: &HashMap<String, String>,
    ) -> Option<Value>;
}

/// Registry of custom [`Endpoint`]s consumed by
/// [`mount`](crate::mount) — pyfly's `ActuatorRegistry` minus the DI
/// container discovery (registration is explicit in Rust).
#[derive(Default)]
pub struct EndpointRegistry {
    endpoints: RwLock<BTreeMap<String, Arc<dyn Endpoint>>>,
}

impl EndpointRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an endpoint, replacing any previous endpoint with the
    /// same id.
    pub fn register<E: Endpoint + 'static>(&self, endpoint: E) {
        self.register_arc(Arc::new(endpoint));
    }

    /// Registers an already-shared endpoint, replacing any previous
    /// endpoint with the same id.
    pub fn register_arc(&self, endpoint: Arc<dyn Endpoint>) {
        self.endpoints
            .write()
            .expect("endpoint registry lock poisoned")
            .insert(endpoint.id().to_string(), endpoint);
    }

    /// Registers an endpoint only when its id is not already taken —
    /// the semantics of pyfly's `discover_from_context`.
    pub fn register_arc_if_absent(&self, endpoint: Arc<dyn Endpoint>) {
        self.endpoints
            .write()
            .expect("endpoint registry lock poisoned")
            .entry(endpoint.id().to_string())
            .or_insert(endpoint);
    }

    /// Returns the endpoint registered under `id`, if any.
    pub fn get(&self, id: &str) -> Option<Arc<dyn Endpoint>> {
        self.endpoints
            .read()
            .expect("endpoint registry lock poisoned")
            .get(id)
            .cloned()
    }

    /// All registered endpoints, sorted by id.
    pub fn all(&self) -> Vec<Arc<dyn Endpoint>> {
        self.endpoints
            .read()
            .expect("endpoint registry lock poisoned")
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct StaticEndpoint {
        id: &'static str,
        enabled: bool,
        payload: Value,
    }

    #[async_trait]
    impl Endpoint for StaticEndpoint {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            self.enabled
        }
        async fn handle(
            &self,
            _selector: Option<&str>,
            _query: &HashMap<String, String>,
        ) -> Option<Value> {
            Some(self.payload.clone())
        }
    }

    // pyfly: test_register_and_get
    #[tokio::test]
    async fn register_and_get() {
        let registry = EndpointRegistry::new();
        registry.register(StaticEndpoint {
            id: "test-on",
            enabled: true,
            payload: json!({"status": "ok"}),
        });
        let ep = registry.get("test-on").unwrap();
        assert_eq!(ep.id(), "test-on");
        assert!(ep.enabled());
        assert_eq!(
            ep.handle(None, &HashMap::new()).await,
            Some(json!({"status": "ok"}))
        );
        assert!(registry.get("missing").is_none());
    }

    // pyfly: test_discover_does_not_overwrite_existing
    #[test]
    fn register_if_absent_does_not_overwrite() {
        let registry = EndpointRegistry::new();
        registry.register(StaticEndpoint {
            id: "dup",
            enabled: true,
            payload: json!(1),
        });
        registry.register_arc_if_absent(Arc::new(StaticEndpoint {
            id: "dup",
            enabled: false,
            payload: json!(2),
        }));
        assert!(registry.get("dup").unwrap().enabled(), "original kept");
        assert_eq!(registry.all().len(), 1);
    }

    #[test]
    fn all_is_sorted_by_id() {
        let registry = EndpointRegistry::new();
        registry.register(StaticEndpoint {
            id: "zeta",
            enabled: true,
            payload: json!(1),
        });
        registry.register(StaticEndpoint {
            id: "alpha",
            enabled: true,
            payload: json!(2),
        });
        let all = registry.all();
        let ids: Vec<&str> = all.iter().map(|e| e.id()).collect();
        assert_eq!(ids, vec!["alpha", "zeta"]);
    }
}
