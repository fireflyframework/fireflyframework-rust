//! Metric primitives exposed in Prometheus exposition format on
//! `GET /actuator/metrics` and `GET /actuator/prometheus`, plus the
//! Micrometer-style JSON drill-down on `GET /actuator/metrics/{name}`.
//!
//! [`Counter`] and [`Gauge`] use [`AtomicU64`] internally so
//! high-cardinality services never contend on metric writes — the same
//! lock-free design the Go port builds on `atomic.Uint64` (gauges store
//! `f64::to_bits`, the counterpart of `math.Float64bits`).
//!
//! The pyfly-parity layer adds **labels** (`counter_with` / `gauge_with`
//! / `histogram_with`), a fixed-bucket [`Histogram`] with a timer helper,
//! and the Micrometer JSON view ([`MetricRegistry::meter_names`] /
//! [`MetricRegistry::meter_json`]) with `availableTags` and `?tag=k:v`
//! drill-down filtering.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use serde_json::{json, Value};

/// Sorted `(key, value)` label pairs identifying one series in a family.
type LabelSet = Vec<(String, String)>;

/// Default histogram bucket upper bounds (seconds) — the classic
/// Prometheus client defaults, also used by Micrometer timers.
pub const DEFAULT_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Trailing unit tokens recognized when deriving a meter's `baseUnit`
/// for the Micrometer JSON view (pyfly's `_UNIT_SUFFIXES`).
const UNIT_SUFFIXES: [&str; 8] = [
    "seconds", "bytes", "ratio", "celsius", "volts", "joules", "percent", "info",
];

fn normalize_labels(labels: &[(&str, &str)]) -> LabelSet {
    let mut set: LabelSet = labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    set.sort();
    set
}

/// Atomically adds `v` to an `f64` stored as bits in an [`AtomicU64`].
fn atomic_f64_add(cell: &AtomicU64, v: f64) {
    let mut current = cell.load(Ordering::Relaxed);
    loop {
        let next = (f64::from_bits(current) + v).to_bits();
        match cell.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// Atomically raises an `f64` stored as bits to at least `v`.
fn atomic_f64_max(cell: &AtomicU64, v: f64) {
    let mut current = cell.load(Ordering::Relaxed);
    loop {
        if f64::from_bits(current) >= v {
            return;
        }
        match cell.compare_exchange_weak(current, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

/// A monotonically-increasing 64-bit counter — safe for concurrent use,
/// lock-free. One labeled series within its family.
pub struct Counter {
    name: String,
    labels: LabelSet,
    v: AtomicU64,
}

impl Counter {
    fn new(name: impl Into<String>, labels: LabelSet) -> Self {
        Self {
            name: name.into(),
            labels,
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

    /// Returns this series' sorted `(key, value)` label pairs (empty for
    /// the unlabeled series).
    pub fn labels(&self) -> &[(String, String)] {
        &self.labels
    }
}

/// A settable 64-bit float gauge. Stored as `f64::to_bits` inside an
/// [`AtomicU64`], mirroring Go's `math.Float64bits` over `atomic.Uint64`.
pub struct Gauge {
    name: String,
    labels: LabelSet,
    v: AtomicU64,
}

impl Gauge {
    fn new(name: impl Into<String>, labels: LabelSet) -> Self {
        Self {
            name: name.into(),
            labels,
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

    /// Returns this series' sorted `(key, value)` label pairs (empty for
    /// the unlabeled series).
    pub fn labels(&self) -> &[(String, String)] {
        &self.labels
    }
}

/// A fixed-bucket histogram — the pyfly/Micrometer timer primitive.
/// Lock-free: per-bucket [`AtomicU64`] bins plus CAS-updated `f64`
/// sum/max cells.
///
/// Rendered in Prometheus exposition format as `name_bucket{le="…"}`
/// cumulative counts (with the implicit `+Inf` bucket), `name_sum`, and
/// `name_count`; surfaced in the Micrometer JSON view as the `COUNT`,
/// `TOTAL_TIME`, and `MAX` statistics.
pub struct Histogram {
    name: String,
    labels: LabelSet,
    bounds: Vec<f64>,
    bins: Vec<AtomicU64>,
    count: AtomicU64,
    sum: AtomicU64,
    max: AtomicU64,
}

impl Histogram {
    fn new(name: impl Into<String>, labels: LabelSet, bounds: &[f64]) -> Self {
        let mut sorted: Vec<f64> = bounds.iter().copied().filter(|b| b.is_finite()).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("histogram bounds must not be NaN"));
        sorted.dedup();
        let bins = (0..sorted.len()).map(|_| AtomicU64::new(0)).collect();
        Self {
            name: name.into(),
            labels,
            bounds: sorted,
            bins,
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0f64.to_bits()),
            max: AtomicU64::new(0f64.to_bits()),
        }
    }

    /// Records one observation.
    pub fn observe(&self, value: f64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        atomic_f64_add(&self.sum, value);
        atomic_f64_max(&self.max, value);
        if let Some(idx) = self.bounds.iter().position(|b| value <= *b) {
            self.bins[idx].fetch_add(1, Ordering::Relaxed);
        }
        // Values above every bound land only in the implicit +Inf bucket,
        // which equals `count`.
    }

    /// Total number of observations.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of all observed values.
    pub fn sum(&self) -> f64 {
        f64::from_bits(self.sum.load(Ordering::Relaxed))
    }

    /// Largest observed value (0 when nothing was observed).
    pub fn max(&self) -> f64 {
        f64::from_bits(self.max.load(Ordering::Relaxed))
    }

    /// The configured finite bucket upper bounds, ascending (`+Inf` is
    /// implicit).
    pub fn bucket_bounds(&self) -> &[f64] {
        &self.bounds
    }

    /// Cumulative per-bucket counts aligned with
    /// [`Histogram::bucket_bounds`] (the `+Inf` bucket equals
    /// [`Histogram::count`]).
    pub fn bucket_counts(&self) -> Vec<u64> {
        let mut total = 0;
        self.bins
            .iter()
            .map(|bin| {
                total += bin.load(Ordering::Relaxed);
                total
            })
            .collect()
    }

    /// Returns the metric name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns this series' sorted `(key, value)` label pairs (empty for
    /// the unlabeled series).
    pub fn labels(&self) -> &[(String, String)] {
        &self.labels
    }

    /// Starts a timer; the elapsed seconds are observed when the guard
    /// is dropped (or explicitly via [`TimerGuard::stop`]).
    pub fn start_timer(self: &Arc<Self>) -> TimerGuard {
        TimerGuard {
            histogram: Arc::clone(self),
            start: Instant::now(),
            recorded: false,
        }
    }

    /// Awaits `fut` and observes its wall-clock duration in seconds —
    /// the Rust counterpart of pyfly's `@timed` decorator.
    pub async fn time<F, T>(&self, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        let start = Instant::now();
        let out = fut.await;
        self.observe(start.elapsed().as_secs_f64());
        out
    }
}

/// Records the elapsed seconds into its [`Histogram`] on drop. Obtain
/// via [`Histogram::start_timer`].
pub struct TimerGuard {
    histogram: Arc<Histogram>,
    start: Instant,
    recorded: bool,
}

impl TimerGuard {
    /// Stops the timer now, records the observation, and returns the
    /// elapsed seconds.
    pub fn stop(mut self) -> f64 {
        let elapsed = self.start.elapsed().as_secs_f64();
        self.histogram.observe(elapsed);
        self.recorded = true;
        elapsed
    }
}

impl Drop for TimerGuard {
    fn drop(&mut self) {
        if !self.recorded {
            self.histogram.observe(self.start.elapsed().as_secs_f64());
        }
    }
}

/// A histogram family: shared bucket bounds plus one series per label set.
struct HistogramFamily {
    bounds: Vec<f64>,
    series: BTreeMap<LabelSet, Arc<Histogram>>,
}

/// The actuator's metric store. Counters, gauges, and histograms are
/// exposed in Prometheus exposition format on `/actuator/metrics` and
/// `/actuator/prometheus` (sorted by name for stable output) and in
/// Micrometer JSON on `/actuator/metrics/{name}`.
#[derive(Default)]
pub struct MetricRegistry {
    counters: RwLock<BTreeMap<String, BTreeMap<LabelSet, Arc<Counter>>>>,
    gauges: RwLock<BTreeMap<String, BTreeMap<LabelSet, Arc<Gauge>>>>,
    histograms: RwLock<BTreeMap<String, HistogramFamily>>,
}

impl MetricRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns or creates the unlabeled counter series named `name`.
    pub fn counter(&self, name: &str) -> Arc<Counter> {
        self.counter_with(name, &[])
    }

    /// Returns or creates the counter series named `name` with the given
    /// labels (order-insensitive; labels are sorted by key).
    pub fn counter_with(&self, name: &str, labels: &[(&str, &str)]) -> Arc<Counter> {
        let set = normalize_labels(labels);
        let mut counters = self.counters.write().expect("counter lock poisoned");
        let family = counters.entry(name.to_string()).or_default();
        Arc::clone(
            family
                .entry(set.clone())
                .or_insert_with(|| Arc::new(Counter::new(name, set))),
        )
    }

    /// Returns or creates the unlabeled gauge series named `name`.
    pub fn gauge(&self, name: &str) -> Arc<Gauge> {
        self.gauge_with(name, &[])
    }

    /// Returns or creates the gauge series named `name` with the given
    /// labels (order-insensitive; labels are sorted by key).
    pub fn gauge_with(&self, name: &str, labels: &[(&str, &str)]) -> Arc<Gauge> {
        let set = normalize_labels(labels);
        let mut gauges = self.gauges.write().expect("gauge lock poisoned");
        let family = gauges.entry(name.to_string()).or_default();
        Arc::clone(
            family
                .entry(set.clone())
                .or_insert_with(|| Arc::new(Gauge::new(name, set))),
        )
    }

    /// Returns or creates the unlabeled histogram series named `name`
    /// with the [`DEFAULT_BUCKETS`].
    pub fn histogram(&self, name: &str) -> Arc<Histogram> {
        self.histogram_with(name, &[])
    }

    /// Returns or creates the histogram series named `name` with the
    /// given labels and the [`DEFAULT_BUCKETS`].
    pub fn histogram_with(&self, name: &str, labels: &[(&str, &str)]) -> Arc<Histogram> {
        self.histogram_with_buckets(name, labels, &DEFAULT_BUCKETS)
    }

    /// Returns or creates the histogram series named `name` with the
    /// given labels and bucket upper bounds. The first registration of a
    /// family fixes its buckets; later series reuse them.
    pub fn histogram_with_buckets(
        &self,
        name: &str,
        labels: &[(&str, &str)],
        buckets: &[f64],
    ) -> Arc<Histogram> {
        let set = normalize_labels(labels);
        let mut histograms = self.histograms.write().expect("histogram lock poisoned");
        let family = histograms
            .entry(name.to_string())
            .or_insert_with(|| HistogramFamily {
                bounds: buckets.to_vec(),
                series: BTreeMap::new(),
            });
        let bounds = family.bounds.clone();
        Arc::clone(
            family
                .series
                .entry(set.clone())
                .or_insert_with(|| Arc::new(Histogram::new(name, set, &bounds))),
        )
    }

    /// Renders every metric in Prometheus exposition format — counters
    /// first, then gauges, then histograms, each block sorted by name
    /// and label set. Gauges print with six decimal places, byte-for-byte
    /// what Go's `%f` verb produces; labeled series print as
    /// `name{key="value"} v`.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;

        let counters = self.counters.read().expect("counter lock poisoned");
        let gauges = self.gauges.read().expect("gauge lock poisoned");
        let histograms = self.histograms.read().expect("histogram lock poisoned");

        let mut out = String::new();
        for (name, family) in counters.iter() {
            let _ = writeln!(out, "# TYPE {name} counter");
            for (set, counter) in family {
                let _ = writeln!(out, "{name}{} {}", fmt_labels(set, None), counter.get());
            }
        }
        for (name, family) in gauges.iter() {
            let _ = writeln!(out, "# TYPE {name} gauge");
            for (set, gauge) in family {
                let _ = writeln!(out, "{name}{} {:.6}", fmt_labels(set, None), gauge.get());
            }
        }
        for (name, family) in histograms.iter() {
            let _ = writeln!(out, "# TYPE {name} histogram");
            for (set, histogram) in &family.series {
                let counts = histogram.bucket_counts();
                for (bound, cumulative) in histogram.bucket_bounds().iter().zip(counts) {
                    let _ = writeln!(
                        out,
                        "{name}_bucket{} {cumulative}",
                        fmt_labels(set, Some(("le", fmt_f64(*bound))))
                    );
                }
                let _ = writeln!(
                    out,
                    "{name}_bucket{} {}",
                    fmt_labels(set, Some(("le", "+Inf".to_string()))),
                    histogram.count()
                );
                let _ = writeln!(
                    out,
                    "{name}_sum{} {}",
                    fmt_labels(set, None),
                    fmt_f64(histogram.sum())
                );
                let _ = writeln!(
                    out,
                    "{name}_count{} {}",
                    fmt_labels(set, None),
                    histogram.count()
                );
            }
        }
        out
    }

    /// All registered meter names, sorted — the Micrometer
    /// `GET /actuator/metrics` `{"names": […]}` list.
    pub fn meter_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        names.extend(
            self.counters
                .read()
                .expect("counter lock poisoned")
                .keys()
                .cloned(),
        );
        names.extend(
            self.gauges
                .read()
                .expect("gauge lock poisoned")
                .keys()
                .cloned(),
        );
        names.extend(
            self.histograms
                .read()
                .expect("histogram lock poisoned")
                .keys()
                .cloned(),
        );
        names.sort();
        names.dedup();
        names
    }

    /// The Micrometer JSON detail for one meter — `{name, baseUnit?,
    /// measurements: [{statistic, value}], availableTags: [{tag,
    /// values}]}` — optionally filtered to the series carrying the
    /// `tag=(key, value)` label (Spring's `?tag=k:v` drill-down).
    /// Returns `None` when no meter is registered under `name`.
    pub fn meter_json(&self, name: &str, tag: Option<(&str, &str)>) -> Option<Value> {
        let matches = |set: &LabelSet| -> bool {
            match tag {
                Some((k, v)) => set.iter().any(|(sk, sv)| sk == k && sv == v),
                None => true,
            }
        };

        let mut measurements: Vec<Value> = Vec::new();
        let mut available: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        let mut collect_tags = |set: &LabelSet| {
            for (k, v) in set {
                available.entry(k.clone()).or_default().insert(v.clone());
            }
        };

        let counters = self.counters.read().expect("counter lock poisoned");
        let gauges = self.gauges.read().expect("gauge lock poisoned");
        let histograms = self.histograms.read().expect("histogram lock poisoned");

        if let Some(family) = counters.get(name) {
            let mut total = 0.0;
            let mut any = false;
            for (set, counter) in family {
                if matches(set) {
                    any = true;
                    collect_tags(set);
                    total += counter.get() as f64;
                }
            }
            if any {
                measurements.push(json!({ "statistic": "COUNT", "value": total }));
            }
        } else if let Some(family) = gauges.get(name) {
            let mut total = 0.0;
            let mut any = false;
            for (set, gauge) in family {
                if matches(set) {
                    any = true;
                    collect_tags(set);
                    total += gauge.get();
                }
            }
            if any {
                measurements.push(json!({ "statistic": "VALUE", "value": total }));
            }
        } else if let Some(family) = histograms.get(name) {
            let mut count = 0.0;
            let mut total_time = 0.0;
            let mut max = 0.0f64;
            let mut any = false;
            for (set, histogram) in &family.series {
                if matches(set) {
                    any = true;
                    collect_tags(set);
                    count += histogram.count() as f64;
                    total_time += histogram.sum();
                    max = max.max(histogram.max());
                }
            }
            if any {
                measurements.push(json!({ "statistic": "COUNT", "value": count }));
                measurements.push(json!({ "statistic": "TOTAL_TIME", "value": total_time }));
                measurements.push(json!({ "statistic": "MAX", "value": max }));
            }
        } else {
            return None;
        }

        let available_tags: Vec<Value> = available
            .iter()
            .map(|(k, values)| {
                json!({ "tag": k, "values": values.iter().cloned().collect::<Vec<_>>() })
            })
            .collect();

        let mut body = serde_json::Map::new();
        body.insert("name".into(), json!(name));
        body.insert("measurements".into(), Value::Array(measurements));
        body.insert("availableTags".into(), Value::Array(available_tags));
        if let Some(unit) = base_unit(name) {
            body.insert("baseUnit".into(), json!(unit));
        }
        Some(Value::Object(body))
    }
}

/// Derives a Micrometer `baseUnit` from a trailing unit token in the
/// metric name (`http_server_requests_seconds` → `seconds`), mirroring
/// pyfly's `_meter_name_and_unit`.
fn base_unit(name: &str) -> Option<&'static str> {
    UNIT_SUFFIXES
        .iter()
        .find(|suffix| {
            name.len() > suffix.len() + 1
                && name.ends_with(*suffix)
                && name.as_bytes()[name.len() - suffix.len() - 1] == b'_'
        })
        .copied()
}

/// Formats an `f64` the way the Prometheus Go client does — shortest
/// representation, `1` not `1.000000`.
fn fmt_f64(v: f64) -> String {
    if v == f64::INFINITY {
        "+Inf".to_string()
    } else {
        format!("{v}")
    }
}

/// Renders a Prometheus label block — empty string when there are no
/// labels, otherwise `{k="v",…}` with `extra` (e.g. `le`) appended last.
fn fmt_labels(set: &LabelSet, extra: Option<(&str, String)>) -> String {
    if set.is_empty() && extra.is_none() {
        return String::new();
    }
    let mut parts: Vec<String> = set
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape_label_value(v)))
        .collect();
    if let Some((k, v)) = extra {
        parts.push(format!("{k}=\"{}\"", escape_label_value(&v)));
    }
    format!("{{{}}}", parts.join(","))
}

/// Escapes a label value per the Prometheus exposition format.
fn escape_label_value(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
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

    // ----- pyfly parity: labels -----

    #[test]
    fn labeled_counters_are_distinct_series() {
        let reg = MetricRegistry::new();
        reg.counter_with("hits_total", &[("region", "eu")]).add(3);
        reg.counter_with("hits_total", &[("region", "us")]).add(7);
        assert_eq!(reg.counter_with("hits_total", &[("region", "eu")]).get(), 3);
        assert_eq!(reg.counter_with("hits_total", &[("region", "us")]).get(), 7);
        // Unlabeled series is yet another series.
        assert_eq!(reg.counter("hits_total").get(), 0);
    }

    #[test]
    fn label_order_is_insensitive() {
        let reg = MetricRegistry::new();
        reg.counter_with("c", &[("a", "1"), ("b", "2")]).inc();
        reg.counter_with("c", &[("b", "2"), ("a", "1")]).inc();
        assert_eq!(reg.counter_with("c", &[("a", "1"), ("b", "2")]).get(), 2);
    }

    #[test]
    fn render_labeled_series() {
        let reg = MetricRegistry::new();
        reg.counter_with("hits_total", &[("region", "eu")]).add(3);
        reg.gauge_with("depth", &[("queue", "q1")]).set(1.0);
        let out = reg.render();
        assert!(out.contains("hits_total{region=\"eu\"} 3\n"), "{out}");
        assert!(out.contains("depth{queue=\"q1\"} 1.000000\n"), "{out}");
    }

    #[test]
    fn label_values_are_escaped() {
        let reg = MetricRegistry::new();
        reg.counter_with("c_total", &[("k", "a\"b\\c\nd")]).inc();
        let out = reg.render();
        assert!(out.contains("c_total{k=\"a\\\"b\\\\c\\nd\"} 1\n"), "{out}");
    }

    // ----- pyfly parity: histograms -----

    #[test]
    fn histogram_observe_count_sum_max() {
        let reg = MetricRegistry::new();
        let h = reg.histogram("latency_seconds");
        h.observe(0.5);
        h.observe(1.5);
        assert_eq!(h.count(), 2);
        assert_eq!(h.sum(), 2.0);
        assert_eq!(h.max(), 1.5);
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let reg = MetricRegistry::new();
        let h = reg.histogram_with_buckets("h", &[], &[1.0, 2.0, 4.0]);
        h.observe(0.5); // <= 1
        h.observe(1.5); // <= 2
        h.observe(3.0); // <= 4
        h.observe(99.0); // +Inf only
        assert_eq!(h.bucket_bounds(), &[1.0, 2.0, 4.0]);
        assert_eq!(h.bucket_counts(), vec![1, 2, 3]);
        assert_eq!(h.count(), 4);
    }

    #[test]
    fn histogram_render_exposition() {
        let reg = MetricRegistry::new();
        let h = reg.histogram_with_buckets("req_seconds", &[("uri", "/a")], &[0.5, 1.0]);
        h.observe(0.25);
        h.observe(2.0);
        let out = reg.render();
        assert!(out.contains("# TYPE req_seconds histogram"), "{out}");
        assert!(
            out.contains("req_seconds_bucket{uri=\"/a\",le=\"0.5\"} 1\n"),
            "{out}"
        );
        assert!(
            out.contains("req_seconds_bucket{uri=\"/a\",le=\"1\"} 1\n"),
            "{out}"
        );
        assert!(
            out.contains("req_seconds_bucket{uri=\"/a\",le=\"+Inf\"} 2\n"),
            "{out}"
        );
        assert!(out.contains("req_seconds_sum{uri=\"/a\"} 2.25\n"), "{out}");
        assert!(out.contains("req_seconds_count{uri=\"/a\"} 2\n"), "{out}");
    }

    #[tokio::test]
    async fn histogram_timer_records() {
        let reg = MetricRegistry::new();
        let h = reg.histogram("op_seconds");
        let out = h.time(async { 21 * 2 }).await;
        assert_eq!(out, 42);
        assert_eq!(h.count(), 1);

        let guard = h.start_timer();
        let elapsed = guard.stop();
        assert!(elapsed >= 0.0);
        assert_eq!(h.count(), 2);

        {
            let _guard = h.start_timer();
        } // records on drop
        assert_eq!(h.count(), 3);
    }

    // ----- pyfly parity: Micrometer JSON view -----

    #[test]
    fn meter_names_lists_all_kinds_sorted() {
        let reg = MetricRegistry::new();
        reg.counter("b_total").inc();
        reg.gauge("a_depth").set(1.0);
        reg.histogram("c_seconds").observe(0.1);
        assert_eq!(reg.meter_names(), vec!["a_depth", "b_total", "c_seconds"]);
    }

    // pyfly: test_detail_counter_uses_count_statistic
    #[test]
    fn meter_json_counter_count_statistic_and_tags() {
        let reg = MetricRegistry::new();
        reg.counter_with("orders_total", &[("method", "GET")])
            .add(5);
        let body = reg.meter_json("orders_total", None).unwrap();
        assert_eq!(body["name"], "orders_total");
        assert_eq!(body["measurements"][0]["statistic"], "COUNT");
        assert_eq!(body["measurements"][0]["value"], 5.0);
        assert_eq!(body["availableTags"][0]["tag"], "method");
        assert_eq!(body["availableTags"][0]["values"][0], "GET");
    }

    // pyfly: test_detail_summary_count_sum_and_baseunit
    #[test]
    fn meter_json_histogram_count_total_time_max_and_base_unit() {
        let reg = MetricRegistry::new();
        let h = reg.histogram_with("latency_seconds", &[("uri", "/a")]);
        h.observe(0.5);
        h.observe(1.5);
        let body = reg.meter_json("latency_seconds", None).unwrap();
        let stats: BTreeMap<String, f64> = body["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["statistic"].as_str().unwrap().to_string(),
                    m["value"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(stats["COUNT"], 2.0);
        assert_eq!(stats["TOTAL_TIME"], 2.0);
        assert_eq!(stats["MAX"], 1.5);
        assert_eq!(body["baseUnit"], "seconds");
    }

    // pyfly: test_detail_tag_filter
    #[test]
    fn meter_json_tag_filter() {
        let reg = MetricRegistry::new();
        reg.counter_with("hits_total", &[("region", "eu")]).add(3);
        reg.counter_with("hits_total", &[("region", "us")]).add(7);
        let body = reg
            .meter_json("hits_total", Some(("region", "eu")))
            .unwrap();
        assert_eq!(body["measurements"][0]["value"], 3.0);
        // Only the matching series contributes availableTags.
        assert_eq!(
            body["availableTags"][0]["values"],
            serde_json::json!(["eu"])
        );
    }

    // pyfly: test_unknown_meter_returns_none
    #[test]
    fn meter_json_unknown_meter_is_none() {
        let reg = MetricRegistry::new();
        assert!(reg.meter_json("nope_does_not_exist", None).is_none());
    }

    #[test]
    fn meter_json_no_matching_series_has_empty_measurements() {
        let reg = MetricRegistry::new();
        reg.counter_with("hits_total", &[("region", "eu")]).add(3);
        let body = reg
            .meter_json("hits_total", Some(("region", "mars")))
            .unwrap();
        assert_eq!(body["measurements"], serde_json::json!([]));
    }

    #[test]
    fn base_unit_detection() {
        assert_eq!(base_unit("latency_seconds"), Some("seconds"));
        assert_eq!(base_unit("heap_bytes"), Some("bytes"));
        assert_eq!(base_unit("seconds"), None, "bare unit is not a suffix");
        assert_eq!(base_unit("orders_total"), None);
    }
}
