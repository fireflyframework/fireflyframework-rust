//! Metric primitives exposed in Prometheus exposition format on
//! `GET /actuator/metrics`.
//!
//! [`Counter`] and [`Gauge`] use [`AtomicU64`] internally so
//! high-cardinality services never contend on metric writes — the same
//! lock-free design the Go port builds on `atomic.Uint64` (gauges store
//! `f64::to_bits`, the counterpart of `math.Float64bits`).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// A monotonically-increasing 64-bit counter — safe for concurrent use,
/// lock-free.
pub struct Counter {
    name: String,
    v: AtomicU64,
}

impl Counter {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            v: AtomicU64::new(0),
        }
    }

    /// Atomically increments the counter by one.
    pub fn inc(&self) {
        self.v.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the counter by `n`.
    pub fn add(&self, n: u64) {
        self.v.fetch_add(n, Ordering::Relaxed);
    }

    /// Returns the current value.
    pub fn get(&self) -> u64 {
        self.v.load(Ordering::Relaxed)
    }

    /// Returns the metric name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A settable 64-bit float gauge. Stored as `f64::to_bits` inside an
/// [`AtomicU64`], mirroring Go's `math.Float64bits` over `atomic.Uint64`.
pub struct Gauge {
    name: String,
    v: AtomicU64,
}

impl Gauge {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            v: AtomicU64::new(0f64.to_bits()),
        }
    }

    /// Atomically replaces the gauge value.
    pub fn set(&self, value: f64) {
        self.v.store(value.to_bits(), Ordering::Relaxed);
    }

    /// Returns the current value.
    pub fn get(&self) -> f64 {
        f64::from_bits(self.v.load(Ordering::Relaxed))
    }

    /// Returns the metric name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The actuator's metric store. Counters and gauges are exposed in
/// Prometheus exposition format on `/actuator/metrics`, sorted by name
/// for stable output.
#[derive(Default)]
pub struct MetricRegistry {
    counters: RwLock<BTreeMap<String, Arc<Counter>>>,
    gauges: RwLock<BTreeMap<String, Arc<Gauge>>>,
}

impl MetricRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns or creates a monotonically-increasing counter named `name`.
    pub fn counter(&self, name: &str) -> Arc<Counter> {
        let mut counters = self.counters.write().expect("counter lock poisoned");
        Arc::clone(
            counters
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(Counter::new(name))),
        )
    }

    /// Returns or creates a settable gauge named `name`.
    pub fn gauge(&self, name: &str) -> Arc<Gauge> {
        let mut gauges = self.gauges.write().expect("gauge lock poisoned");
        Arc::clone(
            gauges
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(Gauge::new(name))),
        )
    }

    /// Renders every metric in Prometheus exposition format — counters
    /// first, then gauges, each block sorted by name. Gauges print with
    /// six decimal places, byte-for-byte what Go's `%f` verb produces.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;

        let counters = self.counters.read().expect("counter lock poisoned");
        let gauges = self.gauges.read().expect("gauge lock poisoned");

        let mut out = String::new();
        for (name, counter) in counters.iter() {
            let _ = writeln!(out, "# TYPE {name} counter");
            let _ = writeln!(out, "{name} {}", counter.get());
        }
        for (name, gauge) in gauges.iter() {
            let _ = writeln!(out, "# TYPE {name} gauge");
            let _ = writeln!(out, "{name} {:.6}", gauge.get());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_inc_add_get() {
        let reg = MetricRegistry::new();
        let c = reg.counter("orders_placed_total");
        c.inc();
        c.add(2);
        assert_eq!(c.get(), 3);
        assert_eq!(c.name(), "orders_placed_total");
    }

    #[test]
    fn registry_returns_same_instance() {
        let reg = MetricRegistry::new();
        reg.counter("hits").inc();
        reg.counter("hits").inc();
        assert_eq!(reg.counter("hits").get(), 2);

        reg.gauge("depth").set(1.5);
        assert_eq!(reg.gauge("depth").get(), 1.5);
    }

    #[test]
    fn gauge_set_get_round_trip() {
        let reg = MetricRegistry::new();
        let g = reg.gauge("queue_depth");
        assert_eq!(g.get(), 0.0);
        g.set(42.5);
        assert_eq!(g.get(), 42.5);
        g.set(-3.25);
        assert_eq!(g.get(), -3.25);
        assert_eq!(g.name(), "queue_depth");
    }

    #[test]
    fn render_prometheus_format_sorted() {
        let reg = MetricRegistry::new();
        reg.counter("b_total").add(2);
        reg.counter("a_total").inc();
        reg.gauge("queue_depth").set(42.5);

        let out = reg.render();
        assert_eq!(
            out,
            "# TYPE a_total counter\na_total 1\n\
             # TYPE b_total counter\nb_total 2\n\
             # TYPE queue_depth gauge\nqueue_depth 42.500000\n"
        );
    }

    #[test]
    fn render_empty_registry_is_empty() {
        assert_eq!(MetricRegistry::default().render(), "");
    }

    #[test]
    fn counters_are_thread_safe() {
        let reg = Arc::new(MetricRegistry::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    reg.counter("contended").inc();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.counter("contended").get(), 8000);
    }
}
