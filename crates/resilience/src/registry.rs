//! `ResilienceRegistry` — config-driven registry of named resilience
//! instances, the port of pyfly's `pyfly.resilience.registry` (Resilience4j's
//! named-registry model).
//!
//! The registry materialises [`CircuitBreaker`]s, [`RateLimiter`]s,
//! [`Bulkhead`]s, and time-limiter timeouts from `firefly.resilience.*`
//! configuration keys. Where pyfly binds a nested `Config` mapping, the Rust
//! port consumes the flat dot-keyed map produced by `firefly-config` sources
//! (the closest analogue of pyfly's `Config.get_section`):
//!
//! ```text
//! firefly.resilience.circuit-breaker.<name>.failure-threshold      int      (default 5)
//! firefly.resilience.circuit-breaker.<name>.recovery-timeout       duration (default 30s)
//! firefly.resilience.circuit-breaker.<name>.failure-rate-threshold float    (optional)
//! firefly.resilience.circuit-breaker.<name>.window-size            int      (default 10)
//! firefly.resilience.circuit-breaker.<name>.half-open-max-calls    int      (default 1)
//! firefly.resilience.rate-limiter.<name>.max-tokens                int      (default 10)
//! firefly.resilience.rate-limiter.<name>.refill-rate               float    (default 10.0)
//! firefly.resilience.bulkhead.<name>.max-concurrent                int      (default 10)
//! firefly.resilience.time-limiter.<name>.timeout                   duration (default 30s)
//! ```
//!
//! Relaxed binding mirrors pyfly (and `firefly-config`'s merge
//! normalization): kebab-case and snake_case segments are interchangeable
//! everywhere — section, property, **and instance name** —
//! (`circuit_breaker.svc.failure_threshold` binds the same as
//! `circuit-breaker.svc.failure-threshold`, and
//! `registry.bulkhead("db-pool")` finds an instance configured as
//! `db_pool`). Names are stored and listed in their snake_case form.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use firefly_config::{ConfigError, Layered, Source};

use crate::bulkhead::Bulkhead;
use crate::circuit_breaker::{CircuitBreaker, CircuitConfig};
use crate::rate_limiter::RateLimiter;
use crate::timeout::Timeout;

/// Errors produced while materialising or querying a [`ResilienceRegistry`].
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// No instance registered under the requested name. The message mirrors
    /// pyfly's `KeyError` text, including the sorted list of available names.
    #[error("No {kind} named '{name}'. Available: {available}")]
    NotFound {
        /// The instance kind (`circuit-breaker`, `rate-limiter`, `bulkhead`,
        /// `time-limiter`).
        kind: &'static str,
        /// The requested name.
        name: String,
        /// Rendered list of registered names, or `(none)`.
        available: String,
    },

    /// A duration value could not be parsed. The message mirrors pyfly's
    /// `ValueError` text.
    #[error("Cannot parse duration '{raw}'; expected e.g. '5s', '500ms', '1m', '2h', or a number")]
    InvalidDuration {
        /// The raw value as found in configuration.
        raw: String,
    },

    /// A numeric property failed to parse against its expected type.
    #[error("invalid value '{value}' for config key '{key}': {reason}")]
    InvalidValue {
        /// The offending (normalized) config key.
        key: String,
        /// The raw value as found in configuration.
        value: String,
        /// Why the value was rejected.
        reason: String,
    },

    /// A `firefly-config` source failed to load while building the registry.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// Parses a human duration into a [`Duration`] — the port of pyfly's
/// `pyfly.resilience.registry.parse_duration` (reused there by the callbacks
/// circuit-breaker config).
///
/// Accepts, in order of preference:
///
/// * bare numbers, treated as **seconds**: `"5"`, `"2.5"`
/// * a number with a pyfly unit suffix: `"5s"`, `"500ms"`, `"1m"`, `"2h"`
///   (fractions allowed: `"2.5s"`)
/// * anything `humantime` understands beyond that, e.g. `"1h 30m"`, `"2min"`
///
/// ```
/// use std::time::Duration;
/// use firefly_resilience::parse_duration;
///
/// assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
/// assert_eq!(parse_duration("2.5").unwrap(), Duration::from_secs_f64(2.5));
/// assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
/// ```
pub fn parse_duration(raw: &str) -> Result<Duration, RegistryError> {
    let invalid = || RegistryError::InvalidDuration {
        raw: raw.to_string(),
    };
    let trimmed = raw.trim();
    // Bare number → seconds (pyfly parity: `5`, `2.5`).
    if let Ok(secs) = trimmed.parse::<f64>() {
        return Duration::try_from_secs_f64(secs).map_err(|_| invalid());
    }
    // pyfly's `<float><unit>` regex: ms | s | m | h (case-insensitive).
    let lower = trimmed.to_ascii_lowercase();
    for (suffix, scale) in [("ms", 0.001), ("s", 1.0), ("m", 60.0), ("h", 3600.0)] {
        if let Some(number) = lower.strip_suffix(suffix) {
            if let Ok(value) = number.trim().parse::<f64>() {
                return Duration::try_from_secs_f64(value * scale).map_err(|_| invalid());
            }
        }
    }
    // Richer formats (e.g. "1h 30m") fall through to humantime.
    humantime::parse_duration(trimmed).map_err(|_| invalid())
}

/// Per-instance property map collected from the flat config.
type Params = HashMap<String, String>;

/// Spring-style relaxed binding for names: lower-cased, `-` → `_` — the same
/// normalization `firefly-config` applies when merging sources, so lookups
/// succeed regardless of which spelling the YAML / env / caller used.
fn relaxed(name: &str) -> String {
    name.to_lowercase().replace('-', "_")
}

/// `ResilienceRegistry` holds named resilience instances built from
/// configuration — the Rust port of pyfly's `ResilienceRegistry`.
///
/// Instances are stored behind [`Arc`] so the registry can hand the same
/// breaker/limiter/bulkhead to many call sites (state is shared, exactly as
/// a Resilience4j named registry shares instances).
///
/// ```
/// use std::collections::HashMap;
/// use firefly_resilience::ResilienceRegistry;
///
/// let flat: HashMap<String, String> = HashMap::from([
///     (
///         "firefly.resilience.bulkhead.db-pool.max-concurrent".to_string(),
///         "5".to_string(),
///     ),
/// ]);
/// let registry = ResilienceRegistry::from_map(&flat)?;
/// let bh = registry.bulkhead("db-pool")?;
/// assert_eq!(bh.max_concurrent(), 5);
/// # Ok::<(), firefly_resilience::RegistryError>(())
/// ```
#[derive(Debug, Default)]
pub struct ResilienceRegistry {
    circuit_breakers: HashMap<String, Arc<CircuitBreaker>>,
    rate_limiters: HashMap<String, Arc<RateLimiter>>,
    bulkheads: HashMap<String, Arc<Bulkhead>>,
    time_limiters: HashMap<String, Duration>,
}

impl ResilienceRegistry {
    /// Returns an empty registry — populate it with the `register_*`
    /// methods (pyfly's direct-construction path).
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------------
    // Factories
    // ------------------------------------------------------------------

    /// Materialises a registry from layered configuration sources — the
    /// Rust analogue of pyfly's `ResilienceRegistry.from_config(config)`.
    /// Sources are merged (last write wins) and the flat map is bound via
    /// [`from_map`](Self::from_map). Missing sections yield an empty
    /// registry, never an error.
    pub fn from_config(config: &Layered) -> Result<Self, RegistryError> {
        Self::from_map(&config.map()?)
    }

    /// Materialises a registry from a slice of configuration sources,
    /// merging left to right (later wins) like
    /// [`firefly_config::load`](firefly_config::load).
    pub fn from_sources(sources: Vec<Box<dyn Source>>) -> Result<Self, RegistryError> {
        Self::from_config(&Layered::new(sources))
    }

    /// Materialises a registry from a flat dot-keyed map (the shape every
    /// `firefly-config` [`Source`] produces). Only keys under
    /// `firefly.resilience.` are considered; kebab/snake segments bind
    /// interchangeably.
    pub fn from_map(flat: &HashMap<String, String>) -> Result<Self, RegistryError> {
        const PREFIX: &str = "firefly.resilience.";
        // kind → name → property → raw value (BTreeMap for deterministic
        // materialisation order).
        let mut sections: BTreeMap<String, BTreeMap<String, Params>> = BTreeMap::new();
        for (key, value) in flat {
            let Some(rest) = key.strip_prefix(PREFIX) else {
                continue;
            };
            let mut parts = rest.splitn(3, '.');
            let (Some(kind), Some(name), Some(prop)) = (parts.next(), parts.next(), parts.next())
            else {
                continue; // parent-mapping entries from YAML flattening
            };
            if prop.is_empty() || name.is_empty() {
                continue;
            }
            sections
                .entry(kind.to_lowercase().replace('_', "-"))
                .or_default()
                .entry(relaxed(name))
                .or_default()
                .insert(relaxed(prop), value.clone());
        }

        let mut registry = Self::new();
        let empty = BTreeMap::new();
        for (name, params) in sections.get("circuit-breaker").unwrap_or(&empty) {
            registry.circuit_breakers.insert(
                name.clone(),
                Arc::new(CircuitBreaker::new(circuit_config(params)?)),
            );
        }
        for (name, params) in sections.get("rate-limiter").unwrap_or(&empty) {
            let max_tokens = get_usize(params, "max_tokens", 10)?;
            let refill_rate = get_f64(params, "refill_rate", 10.0)?;
            registry.rate_limiters.insert(
                name.clone(),
                Arc::new(RateLimiter::new(refill_rate, max_tokens)),
            );
        }
        for (name, params) in sections.get("bulkhead").unwrap_or(&empty) {
            let max_concurrent = get_usize(params, "max_concurrent", 10)?;
            registry
                .bulkheads
                .insert(name.clone(), Arc::new(Bulkhead::new(max_concurrent)));
        }
        for (name, params) in sections.get("time-limiter").unwrap_or(&empty) {
            let timeout = match params.get("timeout") {
                Some(raw) => parse_duration(raw)?,
                None => Duration::from_secs(30),
            };
            registry.time_limiters.insert(name.clone(), timeout);
        }
        Ok(registry)
    }

    // ------------------------------------------------------------------
    // Registration (direct construction)
    // ------------------------------------------------------------------

    /// Registers `breaker` under `name` (relaxed-bound; last write wins).
    pub fn register_circuit_breaker(
        &mut self,
        name: impl Into<String>,
        breaker: Arc<CircuitBreaker>,
    ) {
        self.circuit_breakers.insert(relaxed(&name.into()), breaker);
    }

    /// Registers `limiter` under `name` (relaxed-bound; last write wins).
    pub fn register_rate_limiter(&mut self, name: impl Into<String>, limiter: Arc<RateLimiter>) {
        self.rate_limiters.insert(relaxed(&name.into()), limiter);
    }

    /// Registers `bulkhead` under `name` (relaxed-bound; last write wins).
    pub fn register_bulkhead(&mut self, name: impl Into<String>, bulkhead: Arc<Bulkhead>) {
        self.bulkheads.insert(relaxed(&name.into()), bulkhead);
    }

    /// Registers a time-limiter `timeout` under `name` (relaxed-bound; last
    /// write wins).
    pub fn register_time_limiter(&mut self, name: impl Into<String>, timeout: Duration) {
        self.time_limiters.insert(relaxed(&name.into()), timeout);
    }

    // ------------------------------------------------------------------
    // Typed accessors
    // ------------------------------------------------------------------

    /// Returns the [`CircuitBreaker`] registered under `name`
    /// (relaxed-bound), or a [`RegistryError::NotFound`] listing the
    /// available names — pyfly's `KeyError` semantics.
    pub fn circuit_breaker(&self, name: &str) -> Result<Arc<CircuitBreaker>, RegistryError> {
        self.circuit_breakers
            .get(&relaxed(name))
            .cloned()
            .ok_or_else(|| not_found("circuit-breaker", name, self.circuit_breakers.keys()))
    }

    /// Returns the [`RateLimiter`] registered under `name` (relaxed-bound),
    /// or a [`RegistryError::NotFound`] listing the available names.
    pub fn rate_limiter(&self, name: &str) -> Result<Arc<RateLimiter>, RegistryError> {
        self.rate_limiters
            .get(&relaxed(name))
            .cloned()
            .ok_or_else(|| not_found("rate-limiter", name, self.rate_limiters.keys()))
    }

    /// Returns the [`Bulkhead`] registered under `name` (relaxed-bound), or
    /// a [`RegistryError::NotFound`] listing the available names.
    pub fn bulkhead(&self, name: &str) -> Result<Arc<Bulkhead>, RegistryError> {
        self.bulkheads
            .get(&relaxed(name))
            .cloned()
            .ok_or_else(|| not_found("bulkhead", name, self.bulkheads.keys()))
    }

    /// Returns the timeout [`Duration`] registered under `name`
    /// (relaxed-bound; pyfly returns a `timedelta`), or a
    /// [`RegistryError::NotFound`] listing the available names.
    pub fn time_limiter(&self, name: &str) -> Result<Duration, RegistryError> {
        self.time_limiters
            .get(&relaxed(name))
            .copied()
            .ok_or_else(|| not_found("time-limiter", name, self.time_limiters.keys()))
    }

    /// Convenience: the time-limiter under `name` materialised as a
    /// [`Timeout`] decorator ready for a [`Chain`](crate::Chain).
    pub fn timeout(&self, name: &str) -> Result<Timeout, RegistryError> {
        Ok(Timeout::new(self.time_limiter(name)?))
    }

    // ------------------------------------------------------------------
    // Name listings
    // ------------------------------------------------------------------

    /// Sorted list of registered circuit-breaker names.
    pub fn circuit_breaker_names(&self) -> Vec<String> {
        sorted(self.circuit_breakers.keys())
    }

    /// Sorted list of registered rate-limiter names.
    pub fn rate_limiter_names(&self) -> Vec<String> {
        sorted(self.rate_limiters.keys())
    }

    /// Sorted list of registered bulkhead names.
    pub fn bulkhead_names(&self) -> Vec<String> {
        sorted(self.bulkheads.keys())
    }

    /// Sorted list of registered time-limiter names.
    pub fn time_limiter_names(&self) -> Vec<String> {
        sorted(self.time_limiters.keys())
    }
}

/// Builds a [`CircuitConfig`] from a property map, applying pyfly's
/// defaults: threshold 5, recovery 30 s, window 10, half-open budget 1.
fn circuit_config(params: &Params) -> Result<CircuitConfig, RegistryError> {
    let failure_threshold = get_usize(params, "failure_threshold", 5)?;
    let open_duration = match params.get("recovery_timeout") {
        Some(raw) => parse_duration(raw)?,
        None => Duration::from_secs(30),
    };
    let failure_rate_threshold = match params.get("failure_rate_threshold") {
        Some(raw) => Some(parse_num::<f64>(raw, "failure_rate_threshold")?),
        None => None,
    };
    let window_size = get_usize(params, "window_size", 10)?;
    let half_open_max_calls = get_usize(params, "half_open_max_calls", 1)?;
    Ok(CircuitConfig {
        failure_threshold,
        window: Duration::ZERO, // pyfly counts consecutively, no time window
        open_duration,
        now: None,
        failure_rate_threshold,
        window_size,
        half_open_max_calls,
    })
}

/// Renders pyfly's `KeyError` message, listing available names sorted.
fn not_found<'k>(
    kind: &'static str,
    name: &str,
    keys: impl Iterator<Item = &'k String>,
) -> RegistryError {
    let names = sorted(keys);
    let available = if names.is_empty() {
        "(none)".to_string()
    } else {
        format!(
            "['{}']",
            names.join("', '") // pyfly renders the sorted list repr
        )
    };
    RegistryError::NotFound {
        kind,
        name: name.to_string(),
        available,
    }
}

fn sorted<'k>(keys: impl Iterator<Item = &'k String>) -> Vec<String> {
    let mut names: Vec<String> = keys.cloned().collect();
    names.sort();
    names
}

fn parse_num<T: std::str::FromStr>(raw: &str, key: &str) -> Result<T, RegistryError> {
    raw.trim()
        .parse::<T>()
        .map_err(|_| RegistryError::InvalidValue {
            key: key.to_string(),
            value: raw.to_string(),
            reason: format!("expected a {}", std::any::type_name::<T>()),
        })
}

fn get_usize(params: &Params, key: &str, default: usize) -> Result<usize, RegistryError> {
    match params.get(key) {
        Some(raw) => parse_num(raw, key),
        None => Ok(default),
    }
}

fn get_f64(params: &Params, key: &str, default: f64) -> Result<f64, RegistryError> {
    match params.get(key) {
        Some(raw) => parse_num(raw, key),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_config::StaticSource;

    // ------------------------------------------------------------------
    // Duration parser (port of pyfly TestParseDuration)
    // ------------------------------------------------------------------

    #[test]
    fn parse_duration_bare_int() {
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn parse_duration_bare_float() {
        assert_eq!(parse_duration("2.5").unwrap(), Duration::from_secs_f64(2.5));
    }

    #[test]
    fn parse_duration_seconds_suffix() {
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn parse_duration_milliseconds_suffix() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn parse_duration_minutes_suffix() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn parse_duration_hours_suffix() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_no_unit_defaults_to_seconds() {
        assert_eq!(parse_duration("10").unwrap(), Duration::from_secs(10));
    }

    #[test]
    fn parse_duration_fractional_with_unit() {
        assert_eq!(
            parse_duration("2.5s").unwrap(),
            Duration::from_secs_f64(2.5)
        );
    }

    #[test]
    fn parse_duration_humantime_compound() {
        assert_eq!(parse_duration("1h 30m").unwrap(), Duration::from_secs(5400));
    }

    #[test]
    fn parse_duration_invalid_raises() {
        let err = parse_duration("five seconds").unwrap_err();
        assert!(
            err.to_string().starts_with("Cannot parse duration"),
            "got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // from_config materialisation (port of pyfly
    // TestResilienceRegistryFromConfig)
    // ------------------------------------------------------------------

    /// Builds a registry directly from flat entries (no file I/O) — the
    /// analogue of pyfly's `Config(data)` helper.
    fn registry_from(pairs: &[(&str, &str)]) -> ResilienceRegistry {
        let entries: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ResilienceRegistry::from_map(&entries).expect("registry materialises")
    }

    #[test]
    fn circuit_breaker_materialised() {
        let registry = registry_from(&[
            (
                "firefly.resilience.circuit-breaker.payment-api.failure-threshold",
                "3",
            ),
            (
                "firefly.resilience.circuit-breaker.payment-api.recovery-timeout",
                "10s",
            ),
            (
                "firefly.resilience.circuit-breaker.payment-api.window-size",
                "8",
            ),
            (
                "firefly.resilience.circuit-breaker.payment-api.half-open-max-calls",
                "2",
            ),
        ]);
        let cb = registry.circuit_breaker("payment-api").unwrap();
        assert_eq!(cb.failure_threshold(), 3);
        assert_eq!(cb.open_duration(), Duration::from_secs(10));
        assert_eq!(cb.window_size(), 8);
        assert_eq!(cb.half_open_max_calls(), 2);
    }

    #[test]
    fn circuit_breaker_failure_rate_threshold() {
        let registry = registry_from(&[(
            "firefly.resilience.circuit-breaker.svc.failure-rate-threshold",
            "0.5",
        )]);
        let cb = registry.circuit_breaker("svc").unwrap();
        assert!((cb.failure_rate_threshold().unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn rate_limiter_materialised() {
        let registry = registry_from(&[
            (
                "firefly.resilience.rate-limiter.search-api.max-tokens",
                "200",
            ),
            (
                "firefly.resilience.rate-limiter.search-api.refill-rate",
                "100.0",
            ),
        ]);
        let rl = registry.rate_limiter("search-api").unwrap();
        assert_eq!(rl.burst(), 200);
        assert!((rl.rate() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn bulkhead_materialised() {
        let registry =
            registry_from(&[("firefly.resilience.bulkhead.db-pool.max-concurrent", "5")]);
        let bh = registry.bulkhead("db-pool").unwrap();
        assert_eq!(bh.max_concurrent(), 5);
    }

    #[test]
    fn time_limiter_materialised() {
        let registry =
            registry_from(&[("firefly.resilience.time-limiter.slow-report.timeout", "30s")]);
        assert_eq!(
            registry.time_limiter("slow-report").unwrap(),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn time_limiter_milliseconds() {
        let registry =
            registry_from(&[("firefly.resilience.time-limiter.fast-op.timeout", "500ms")]);
        assert_eq!(
            registry.time_limiter("fast-op").unwrap(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn multiple_named_instances() {
        let registry = registry_from(&[
            ("firefly.resilience.rate-limiter.default.max-tokens", "100"),
            (
                "firefly.resilience.rate-limiter.default.refill-rate",
                "50.0",
            ),
            (
                "firefly.resilience.rate-limiter.payment-api.max-tokens",
                "10",
            ),
            (
                "firefly.resilience.rate-limiter.payment-api.refill-rate",
                "2.0",
            ),
            ("firefly.resilience.bulkhead.default.max-concurrent", "20"),
            ("firefly.resilience.bulkhead.db-pool.max-concurrent", "5"),
        ]);
        assert_eq!(registry.rate_limiter("default").unwrap().burst(), 100);
        assert_eq!(registry.rate_limiter("payment-api").unwrap().burst(), 10);
        assert_eq!(registry.bulkhead("default").unwrap().max_concurrent(), 20);
        assert_eq!(registry.bulkhead("db-pool").unwrap().max_concurrent(), 5);
    }

    #[test]
    fn empty_config_gives_empty_registry() {
        let registry = registry_from(&[]);
        assert!(registry.circuit_breaker_names().is_empty());
        assert!(registry.rate_limiter_names().is_empty());
        assert!(registry.bulkhead_names().is_empty());
        assert!(registry.time_limiter_names().is_empty());
    }

    #[test]
    fn missing_resilience_section_gives_empty_registry() {
        let registry = registry_from(&[("firefly.app.name", "test")]);
        assert!(registry.bulkhead_names().is_empty());
    }

    #[test]
    fn instance_names_bind_relaxed() {
        // firefly-config's merge normalizes keys to snake_case; lookups with
        // either spelling must succeed.
        let registry =
            registry_from(&[("firefly.resilience.bulkhead.db_pool.max_concurrent", "5")]);
        assert_eq!(registry.bulkhead("db-pool").unwrap().max_concurrent(), 5);
        assert_eq!(registry.bulkhead("db_pool").unwrap().max_concurrent(), 5);
    }

    #[test]
    fn snake_case_keys_bind_like_kebab() {
        let registry = registry_from(&[(
            "firefly.resilience.circuit_breaker.svc.failure_threshold",
            "7",
        )]);
        assert_eq!(
            registry.circuit_breaker("svc").unwrap().failure_threshold(),
            7
        );
    }

    #[test]
    fn unknown_circuit_breaker_errors() {
        let registry = registry_from(&[]);
        let err = registry.circuit_breaker("missing").unwrap_err();
        assert!(
            err.to_string()
                .contains("No circuit-breaker named 'missing'"),
            "got: {err}"
        );
    }

    #[test]
    fn unknown_rate_limiter_errors() {
        let registry = registry_from(&[]);
        let err = registry.rate_limiter("missing").unwrap_err();
        assert!(
            err.to_string().contains("No rate-limiter named 'missing'"),
            "got: {err}"
        );
    }

    #[test]
    fn unknown_bulkhead_errors() {
        let registry = registry_from(&[]);
        let err = registry.bulkhead("missing").unwrap_err();
        assert!(
            err.to_string().contains("No bulkhead named 'missing'"),
            "got: {err}"
        );
    }

    #[test]
    fn unknown_time_limiter_errors() {
        let registry = registry_from(&[]);
        let err = registry.time_limiter("missing").unwrap_err();
        assert!(
            err.to_string().contains("No time-limiter named 'missing'"),
            "got: {err}"
        );
    }

    #[test]
    fn error_message_lists_available_names() {
        let registry = registry_from(&[
            ("firefly.resilience.bulkhead.alpha.max-concurrent", "2"),
            ("firefly.resilience.bulkhead.beta.max-concurrent", "3"),
        ]);
        let err = registry.bulkhead("unknown").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("alpha"), "got: {text}");
        assert!(text.contains("beta"), "got: {text}");
    }

    #[test]
    fn invalid_numeric_value_errors() {
        let entries: HashMap<String, String> = HashMap::from([(
            "firefly.resilience.bulkhead.db.max-concurrent".to_string(),
            "lots".to_string(),
        )]);
        let err = ResilienceRegistry::from_map(&entries).unwrap_err();
        assert!(
            err.to_string().contains("invalid value 'lots'"),
            "got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // from_config over firefly-config sources (the pyfly Config analogue)
    // ------------------------------------------------------------------

    #[test]
    fn from_config_reads_layered_sources() {
        let entries: HashMap<String, String> = HashMap::from([
            (
                "firefly.resilience.bulkhead.my-svc.max-concurrent".to_string(),
                "7".to_string(),
            ),
            (
                "firefly.resilience.time-limiter.slow.timeout".to_string(),
                "1m".to_string(),
            ),
        ]);
        let sources: Vec<Box<dyn Source>> = vec![Box::new(StaticSource::new("defaults", entries))];
        let registry = ResilienceRegistry::from_config(&Layered::new(sources)).unwrap();
        assert_eq!(registry.bulkhead("my-svc").unwrap().max_concurrent(), 7);
        assert_eq!(
            registry.time_limiter("slow").unwrap(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn from_sources_merges_last_write_wins() {
        let base: HashMap<String, String> = HashMap::from([(
            "firefly.resilience.bulkhead.db.max-concurrent".to_string(),
            "10".to_string(),
        )]);
        let overlay: HashMap<String, String> = HashMap::from([(
            "firefly.resilience.bulkhead.db.max-concurrent".to_string(),
            "3".to_string(),
        )]);
        let registry = ResilienceRegistry::from_sources(vec![
            Box::new(StaticSource::new("base", base)),
            Box::new(StaticSource::new("overlay", overlay)),
        ])
        .unwrap();
        assert_eq!(registry.bulkhead("db").unwrap().max_concurrent(), 3);
    }

    #[test]
    fn from_config_with_no_resilience_keys_is_empty() {
        let registry = ResilienceRegistry::from_sources(vec![Box::new(StaticSource::new(
            "defaults",
            HashMap::new(),
        ))])
        .unwrap();
        assert!(registry.bulkhead_names().is_empty());
    }

    // ------------------------------------------------------------------
    // Direct construction (port of pyfly TestResilienceRegistryDirect)
    // ------------------------------------------------------------------

    #[test]
    fn direct_construction_and_lookup() {
        let cb = Arc::new(CircuitBreaker::new(CircuitConfig {
            failure_threshold: 7,
            ..CircuitConfig::default()
        }));
        let mut registry = ResilienceRegistry::new();
        registry.register_circuit_breaker("svc", cb.clone());
        assert!(Arc::ptr_eq(&registry.circuit_breaker("svc").unwrap(), &cb));
    }

    #[test]
    fn name_lists_sorted() {
        let mut registry = ResilienceRegistry::new();
        registry.register_rate_limiter("zebra", Arc::new(RateLimiter::new(1.0, 10)));
        registry.register_rate_limiter("alpha", Arc::new(RateLimiter::new(2.0, 20)));
        assert_eq!(registry.rate_limiter_names(), vec!["alpha", "zebra"]);
    }

    #[test]
    fn timeout_convenience_builds_decorator() {
        let mut registry = ResilienceRegistry::new();
        registry.register_time_limiter("fast", Duration::from_millis(50));
        let _timeout: Timeout = registry.timeout("fast").unwrap();
        assert!(registry.timeout("missing").is_err());
    }
}
