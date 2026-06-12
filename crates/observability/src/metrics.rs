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

//! Labeled metric primitives — the Rust port of pyfly's
//! `pyfly.observability.metrics` (`MetricsRegistry`, `@timed`, `@counted`).
//!
//! pyfly wraps `prometheus_client`; this port implements the same surface
//! natively: get-or-create [`Counter`] / [`Gauge`] / [`Histogram`] meters
//! with label support, plus [`timed`] / [`counted`] helper functions that
//! wrap futures (the builder/wrapper adaptation of pyfly's decorators) and
//! a Prometheus text exposition ([`MetricsRegistry::prometheus_text`]).
//!
//! Registration mirrors pyfly's **process-global, idempotent** model:
//! collector caches are shared by every [`MetricsRegistry`] created via
//! [`MetricsRegistry::new`], so a metric name is created exactly once per
//! process no matter how many registries exist (pyfly v26.06.97 fixed
//! exactly this — "Duplicated timeseries in CollectorRegistry").
//! [`MetricsRegistry::isolated`] returns a private registry for tests.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// The default histogram buckets — identical to `prometheus_client`'s
/// `Histogram.DEFAULT_BUCKETS` (without the implicit `+Inf`, which every
/// histogram always has).
pub const DEFAULT_BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

fn lock<'a, T>(m: &'a Mutex<T>) -> MutexGuard<'a, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Converts a Micrometer dot.case meter name to a Prometheus name —
/// pyfly's `_sanitize` (`orders.process` → `orders_process`).
pub fn sanitize_metric_name(name: &str) -> String {
    name.replace(['.', '-'], "_")
}

fn check_labels(metric: &str, expected: &[String], got: &[&str]) {
    assert!(
        expected.len() == got.len(),
        "metric {metric:?} expects {} label value(s) for {:?}, got {}",
        expected.len(),
        expected,
        got.len()
    );
}

// ---------------------------------------------------------------------------
// Counter
// ---------------------------------------------------------------------------

/// A monotonically increasing counter with optional labels — the analog of
/// `prometheus_client.Counter`. Exposed in the text format as
/// `<name>_total` (the suffix is appended at exposition time, exactly like
/// prometheus_client).
#[derive(Debug)]
pub struct Counter {
    name: String,
    description: String,
    label_names: Vec<String>,
    series: Mutex<BTreeMap<Vec<String>, f64>>,
}

impl Counter {
    fn new(name: &str, description: &str, labels: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            label_names: labels.iter().map(|s| s.to_string()).collect(),
            series: Mutex::new(BTreeMap::new()),
        }
    }

    /// The metric name (without the `_total` exposition suffix).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared label names, in declaration order.
    pub fn label_names(&self) -> &[String] {
        &self.label_names
    }

    /// Increments the unlabeled series by 1.
    ///
    /// # Panics
    /// Panics when the counter was declared with labels — pass values via
    /// [`Counter::labels`] (prometheus_client raises `ValueError` here).
    pub fn inc(&self) {
        self.inc_by(1.0);
    }

    /// Increments the unlabeled series by `v` (must be ≥ 0).
    pub fn inc_by(&self, v: f64) {
        check_labels(&self.name, &self.label_names, &[]);
        *lock(&self.series).entry(Vec::new()).or_insert(0.0) += v.max(0.0);
    }

    /// Returns the child series for the given label values, in declaration
    /// order — the analog of prometheus_client's `.labels(...)`.
    ///
    /// # Panics
    /// Panics when the number of values differs from the declared labels.
    pub fn labels(&self, values: &[&str]) -> LabeledCounter<'_> {
        check_labels(&self.name, &self.label_names, values);
        LabeledCounter {
            counter: self,
            values: values.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Current value of the unlabeled series (0 when never incremented).
    pub fn value(&self) -> f64 {
        self.value_with(&[])
    }

    /// Current value for the given label values (0 when never incremented).
    pub fn value_with(&self, values: &[&str]) -> f64 {
        let key: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        lock(&self.series).get(&key).copied().unwrap_or(0.0)
    }
}

/// One labeled child series of a [`Counter`].
#[derive(Debug)]
pub struct LabeledCounter<'a> {
    counter: &'a Counter,
    values: Vec<String>,
}

impl LabeledCounter<'_> {
    /// Increments this series by 1.
    pub fn inc(&self) {
        self.inc_by(1.0);
    }

    /// Increments this series by `v` (must be ≥ 0).
    pub fn inc_by(&self, v: f64) {
        *lock(&self.counter.series)
            .entry(self.values.clone())
            .or_insert(0.0) += v.max(0.0);
    }

    /// Current value of this series.
    pub fn value(&self) -> f64 {
        lock(&self.counter.series)
            .get(&self.values)
            .copied()
            .unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Gauge
// ---------------------------------------------------------------------------

/// A gauge (set / inc / dec) with optional labels — the analog of
/// `prometheus_client.Gauge`.
#[derive(Debug)]
pub struct Gauge {
    name: String,
    description: String,
    label_names: Vec<String>,
    series: Mutex<BTreeMap<Vec<String>, f64>>,
}

impl Gauge {
    fn new(name: &str, description: &str, labels: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            label_names: labels.iter().map(|s| s.to_string()).collect(),
            series: Mutex::new(BTreeMap::new()),
        }
    }

    /// The metric name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared label names, in declaration order.
    pub fn label_names(&self) -> &[String] {
        &self.label_names
    }

    /// Sets the unlabeled series to `v`.
    ///
    /// # Panics
    /// Panics when the gauge was declared with labels — use
    /// [`Gauge::labels`].
    pub fn set(&self, v: f64) {
        check_labels(&self.name, &self.label_names, &[]);
        lock(&self.series).insert(Vec::new(), v);
    }

    /// Adds `v` (may be negative) to the unlabeled series.
    pub fn add(&self, v: f64) {
        check_labels(&self.name, &self.label_names, &[]);
        *lock(&self.series).entry(Vec::new()).or_insert(0.0) += v;
    }

    /// Increments the unlabeled series by 1.
    pub fn inc(&self) {
        self.add(1.0);
    }

    /// Decrements the unlabeled series by 1.
    pub fn dec(&self) {
        self.add(-1.0);
    }

    /// Returns the child series for the given label values.
    ///
    /// # Panics
    /// Panics when the number of values differs from the declared labels.
    pub fn labels(&self, values: &[&str]) -> LabeledGauge<'_> {
        check_labels(&self.name, &self.label_names, values);
        LabeledGauge {
            gauge: self,
            values: values.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Current value of the unlabeled series (0 when never written).
    pub fn value(&self) -> f64 {
        self.value_with(&[])
    }

    /// Current value for the given label values (0 when never written).
    pub fn value_with(&self, values: &[&str]) -> f64 {
        let key: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        lock(&self.series).get(&key).copied().unwrap_or(0.0)
    }
}

/// One labeled child series of a [`Gauge`].
#[derive(Debug)]
pub struct LabeledGauge<'a> {
    gauge: &'a Gauge,
    values: Vec<String>,
}

impl LabeledGauge<'_> {
    /// Sets this series to `v`.
    pub fn set(&self, v: f64) {
        lock(&self.gauge.series).insert(self.values.clone(), v);
    }

    /// Adds `v` (may be negative) to this series.
    pub fn add(&self, v: f64) {
        *lock(&self.gauge.series)
            .entry(self.values.clone())
            .or_insert(0.0) += v;
    }

    /// Increments this series by 1.
    pub fn inc(&self) {
        self.add(1.0);
    }

    /// Decrements this series by 1.
    pub fn dec(&self) {
        self.add(-1.0);
    }

    /// Current value of this series.
    pub fn value(&self) -> f64 {
        lock(&self.gauge.series)
            .get(&self.values)
            .copied()
            .unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------
// Histogram
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct HistogramSeries {
    bucket_counts: Vec<u64>,
    sum: f64,
    count: u64,
}

/// A cumulative histogram with optional labels — the analog of
/// `prometheus_client.Histogram`. Exposes `<name>_bucket` (cumulative,
/// `le`-labeled), `<name>_sum` and `<name>_count` in the text format.
#[derive(Debug)]
pub struct Histogram {
    name: String,
    description: String,
    label_names: Vec<String>,
    buckets: Vec<f64>,
    series: Mutex<BTreeMap<Vec<String>, HistogramSeries>>,
}

impl Histogram {
    fn new(name: &str, description: &str, labels: &[&str], buckets: Option<&[f64]>) -> Self {
        let mut buckets: Vec<f64> = buckets.unwrap_or(&DEFAULT_BUCKETS).to_vec();
        buckets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        buckets.dedup();
        Self {
            name: name.to_string(),
            description: description.to_string(),
            label_names: labels.iter().map(|s| s.to_string()).collect(),
            buckets,
            series: Mutex::new(BTreeMap::new()),
        }
    }

    /// The metric name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared label names, in declaration order.
    pub fn label_names(&self) -> &[String] {
        &self.label_names
    }

    /// The configured bucket upper bounds (excluding the implicit `+Inf`).
    pub fn buckets(&self) -> &[f64] {
        &self.buckets
    }

    fn observe_series(&self, key: Vec<String>, v: f64) {
        let mut series = lock(&self.series);
        let entry = series.entry(key).or_insert_with(|| HistogramSeries {
            bucket_counts: vec![0; self.buckets.len()],
            sum: 0.0,
            count: 0,
        });
        for (i, le) in self.buckets.iter().enumerate() {
            if v <= *le {
                entry.bucket_counts[i] += 1;
            }
        }
        entry.sum += v;
        entry.count += 1;
    }

    /// Records `v` into the unlabeled series.
    ///
    /// # Panics
    /// Panics when the histogram was declared with labels — use
    /// [`Histogram::labels`].
    pub fn observe(&self, v: f64) {
        check_labels(&self.name, &self.label_names, &[]);
        self.observe_series(Vec::new(), v);
    }

    /// Returns the child series for the given label values.
    ///
    /// # Panics
    /// Panics when the number of values differs from the declared labels.
    pub fn labels(&self, values: &[&str]) -> LabeledHistogram<'_> {
        check_labels(&self.name, &self.label_names, values);
        LabeledHistogram {
            histogram: self,
            values: values.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Number of observations in the unlabeled series.
    pub fn count(&self) -> u64 {
        self.count_with(&[])
    }

    /// Number of observations for the given label values.
    pub fn count_with(&self, values: &[&str]) -> u64 {
        let key: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        lock(&self.series).get(&key).map_or(0, |s| s.count)
    }

    /// Sum of observations in the unlabeled series.
    pub fn sum(&self) -> f64 {
        self.sum_with(&[])
    }

    /// Sum of observations for the given label values.
    pub fn sum_with(&self, values: &[&str]) -> f64 {
        let key: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        lock(&self.series).get(&key).map_or(0.0, |s| s.sum)
    }
}

/// One labeled child series of a [`Histogram`].
#[derive(Debug)]
pub struct LabeledHistogram<'a> {
    histogram: &'a Histogram,
    values: Vec<String>,
}

impl LabeledHistogram<'_> {
    /// Records `v` into this series.
    pub fn observe(&self, v: f64) {
        self.histogram.observe_series(self.values.clone(), v);
    }

    /// Number of observations in this series.
    pub fn count(&self) -> u64 {
        lock(&self.histogram.series)
            .get(&self.values)
            .map_or(0, |s| s.count)
    }

    /// Sum of observations in this series.
    pub fn sum(&self) -> f64 {
        lock(&self.histogram.series)
            .get(&self.values)
            .map_or(0.0, |s| s.sum)
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct RegistryInner {
    counters: Mutex<BTreeMap<String, Arc<Counter>>>,
    gauges: Mutex<BTreeMap<String, Arc<Gauge>>>,
    histograms: Mutex<BTreeMap<String, Arc<Histogram>>>,
}

fn global_inner() -> Arc<RegistryInner> {
    static GLOBAL: OnceLock<Arc<RegistryInner>> = OnceLock::new();
    GLOBAL.get_or_init(Arc::default).clone()
}

/// Registry for application metrics — the Rust port of pyfly's
/// `MetricsRegistry` (the Prometheus `MetricsRecorder` adapter).
///
/// Registration is **process-global and idempotent**: every registry
/// created via [`MetricsRegistry::new`] shares one process-wide collector
/// cache, so each metric name is created exactly once per process no
/// matter how many registries exist — pyfly's module-level cache model.
/// The first declaration of a name wins; later calls return the existing
/// collector (description / labels / buckets of later calls are ignored,
/// exactly like pyfly's `if name not in _COUNTERS` guard).
///
/// ```
/// use firefly_observability::MetricsRegistry;
///
/// let a = MetricsRegistry::new();
/// let b = MetricsRegistry::new();
/// let c1 = a.counter("doc_requests", "Total requests", &["route"]);
/// let c2 = b.counter("doc_requests", "ignored", &["route"]);
/// assert!(std::sync::Arc::ptr_eq(&c1, &c2));
/// ```
#[derive(Debug, Clone)]
pub struct MetricsRegistry {
    inner: Arc<RegistryInner>,
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsRegistry {
    /// Returns a handle to the process-global registry (pyfly's default).
    pub fn new() -> Self {
        Self {
            inner: global_inner(),
        }
    }

    /// Returns a private registry that shares nothing with the global one —
    /// useful in tests and for embedded exporters.
    pub fn isolated() -> Self {
        Self {
            inner: Arc::default(),
        }
    }

    /// Gets or creates a counter (idempotent process-wide for [`Self::new`]
    /// registries).
    pub fn counter(&self, name: &str, description: &str, labels: &[&str]) -> Arc<Counter> {
        lock(&self.inner.counters)
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Counter::new(name, description, labels)))
            .clone()
    }

    /// Gets or creates a gauge (idempotent process-wide for [`Self::new`]
    /// registries).
    pub fn gauge(&self, name: &str, description: &str, labels: &[&str]) -> Arc<Gauge> {
        lock(&self.inner.gauges)
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Gauge::new(name, description, labels)))
            .clone()
    }

    /// Gets or creates a histogram (idempotent process-wide for
    /// [`Self::new`] registries). `buckets` defaults to
    /// [`DEFAULT_BUCKETS`] when `None`.
    pub fn histogram(
        &self,
        name: &str,
        description: &str,
        labels: &[&str],
        buckets: Option<&[f64]>,
    ) -> Arc<Histogram> {
        lock(&self.inner.histograms)
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Histogram::new(name, description, labels, buckets)))
            .clone()
    }

    /// Renders every registered metric in the Prometheus text exposition
    /// format (version 0.0.4) — counters as `<name>_total`, histograms as
    /// `_bucket`/`_sum`/`_count`, label pairs sorted by label name, exactly
    /// like `prometheus_client.generate_latest`.
    pub fn prometheus_text(&self) -> String {
        let mut out = String::new();
        for counter in lock(&self.inner.counters).values() {
            let exp_name = if counter.name.ends_with("_total") {
                counter.name.clone()
            } else {
                format!("{}_total", counter.name)
            };
            out.push_str(&format!("# HELP {exp_name} {}\n", counter.description));
            out.push_str(&format!("# TYPE {exp_name} counter\n"));
            for (values, v) in lock(&counter.series).iter() {
                out.push_str(&format!(
                    "{exp_name}{} {}\n",
                    render_labels(&counter.label_names, values, None),
                    render_value(*v)
                ));
            }
        }
        for gauge in lock(&self.inner.gauges).values() {
            out.push_str(&format!("# HELP {} {}\n", gauge.name, gauge.description));
            out.push_str(&format!("# TYPE {} gauge\n", gauge.name));
            for (values, v) in lock(&gauge.series).iter() {
                out.push_str(&format!(
                    "{}{} {}\n",
                    gauge.name,
                    render_labels(&gauge.label_names, values, None),
                    render_value(*v)
                ));
            }
        }
        for histogram in lock(&self.inner.histograms).values() {
            let name = &histogram.name;
            out.push_str(&format!("# HELP {name} {}\n", histogram.description));
            out.push_str(&format!("# TYPE {name} histogram\n"));
            for (values, series) in lock(&histogram.series).iter() {
                // `bucket_counts` is cumulative by construction: every
                // observation increments all buckets whose bound ≥ value.
                for (i, le) in histogram.buckets.iter().enumerate() {
                    out.push_str(&format!(
                        "{name}_bucket{} {}\n",
                        render_labels(
                            &histogram.label_names,
                            values,
                            Some(("le", &render_value(*le)))
                        ),
                        render_value(series.bucket_counts[i] as f64)
                    ));
                }
                out.push_str(&format!(
                    "{name}_bucket{} {}\n",
                    render_labels(&histogram.label_names, values, Some(("le", "+Inf"))),
                    render_value(series.count as f64)
                ));
                let plain = render_labels(&histogram.label_names, values, None);
                out.push_str(&format!("{name}_sum{plain} {}\n", render_value(series.sum)));
                out.push_str(&format!(
                    "{name}_count{plain} {}\n",
                    render_value(series.count as f64)
                ));
            }
        }
        out
    }
}

/// Formats a sample value like prometheus_client: integral floats keep a
/// trailing `.0` (`1.0`), everything else uses the shortest representation.
fn render_value(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn render_labels(names: &[String], values: &[String], extra: Option<(&str, &str)>) -> String {
    let mut pairs: Vec<(&str, &str)> = names
        .iter()
        .zip(values.iter())
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    if let Some((k, v)) = extra {
        pairs.push((k, v));
    }
    if pairs.is_empty() {
        return String::new();
    }
    pairs.sort_by_key(|(n, _)| *n);
    let body: Vec<String> = pairs
        .iter()
        .map(|(n, v)| format!("{n}=\"{}\"", escape_label_value(v)))
        .collect();
    format!("{{{}}}", body.join(","))
}

fn escape_label_value(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

// ---------------------------------------------------------------------------
// timed / counted — pyfly decorator → Rust future-wrapper adaptation
// ---------------------------------------------------------------------------

/// Returns the unqualified name of `T` (`my::mod::Error<X>` → `Error`) —
/// the Rust analog of pyfly's `type(exc).__name__` exception label.
fn short_type_name<T: ?Sized>() -> &'static str {
    let full = std::any::type_name::<T>();
    let no_generics = full.split('<').next().unwrap_or(full);
    no_generics.rsplit("::").next().unwrap_or(no_generics)
}

/// Builder for timing a future, Micrometer `@Timed` style — the
/// builder/wrapper adaptation of pyfly's `@timed` decorator.
///
/// The meter name accepts Micrometer dot.case (`orders.process`) and is
/// exposed as a Prometheus timer `<name>_seconds`
/// (`_count`/`_sum`/`_bucket`) tagged with `class`, `method`, `exception`
/// (+ any extra tags). pyfly derives `class`/`method` from the function's
/// qualname; in Rust set them explicitly with [`Timed::class`] /
/// [`Timed::method`] (both default to `""`).
///
/// ```
/// # let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
/// # rt.block_on(async {
/// use firefly_observability::{MetricsRegistry, Timed};
///
/// let registry = MetricsRegistry::isolated();
/// let out = Timed::new(&registry, "orders.process")
///     .method("process")
///     .record(async { 42 })
///     .await;
/// assert_eq!(out, 42);
/// let h = registry.histogram("orders_process_seconds", "", &["class", "method", "exception"], None);
/// assert_eq!(h.count_with(&["", "process", "none"]), 1);
/// # });
/// ```
#[derive(Debug)]
pub struct Timed<'a> {
    registry: &'a MetricsRegistry,
    name: String,
    description: String,
    class: String,
    method: String,
    extra: Vec<(String, String)>,
}

impl<'a> Timed<'a> {
    /// Starts a timer builder for the given Micrometer meter name.
    pub fn new(registry: &'a MetricsRegistry, name: &str) -> Self {
        Self {
            registry,
            name: name.to_string(),
            description: "Timed method execution".to_string(),
            class: String::new(),
            method: String::new(),
            extra: Vec::new(),
        }
    }

    /// Sets the meter description (pyfly's `description` argument).
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the `class` tag value.
    #[must_use]
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the `method` tag value.
    #[must_use]
    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Adds an extra tag (pyfly's `extra_tags`).
    #[must_use]
    pub fn tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.push((key.into(), value.into()));
        self
    }

    fn histogram(&self) -> Arc<Histogram> {
        let mut prom_name = sanitize_metric_name(&self.name);
        if !prom_name.ends_with("_seconds") {
            prom_name.push_str("_seconds");
        }
        let mut label_names: Vec<&str> = vec!["class", "method", "exception"];
        label_names.extend(self.extra.iter().map(|(k, _)| k.as_str()));
        self.registry
            .histogram(&prom_name, &self.description, &label_names, None)
    }

    fn label_values(&self, exception: &str) -> Vec<String> {
        let mut values = vec![
            self.class.clone(),
            self.method.clone(),
            exception.to_string(),
        ];
        values.extend(self.extra.iter().map(|(_, v)| v.clone()));
        values
    }

    /// Runs `fut`, observing its duration with `exception="none"`.
    pub async fn record<T>(self, fut: impl Future<Output = T>) -> T {
        let histogram = self.histogram();
        let start = Instant::now();
        let out = fut.await;
        let values = self.label_values("none");
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        histogram
            .labels(&refs)
            .observe(start.elapsed().as_secs_f64());
        out
    }

    /// Runs `fut`, observing its duration; on `Err` the `exception` tag is
    /// the unqualified error type name (pyfly: `type(exc).__name__`).
    pub async fn record_result<T, E>(
        self,
        fut: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let histogram = self.histogram();
        let start = Instant::now();
        let out = fut.await;
        let exception = if out.is_err() {
            short_type_name::<E>()
        } else {
            "none"
        };
        let values = self.label_values(exception);
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        histogram
            .labels(&refs)
            .observe(start.elapsed().as_secs_f64());
        out
    }
}

/// Builder for counting invocations, Micrometer `@Counted` style — the
/// builder/wrapper adaptation of pyfly's `@counted` decorator.
///
/// The meter name accepts Micrometer dot.case and is exposed as a
/// Prometheus counter `<name>_total` tagged with `class`, `method`,
/// `result` (`success`/`failure`) and `exception` (+ any extra tags).
#[derive(Debug)]
pub struct Counted<'a> {
    registry: &'a MetricsRegistry,
    name: String,
    description: String,
    class: String,
    method: String,
    extra: Vec<(String, String)>,
}

impl<'a> Counted<'a> {
    /// Starts a counter builder for the given Micrometer meter name.
    pub fn new(registry: &'a MetricsRegistry, name: &str) -> Self {
        Self {
            registry,
            name: name.to_string(),
            description: "Counted method invocations".to_string(),
            class: String::new(),
            method: String::new(),
            extra: Vec::new(),
        }
    }

    /// Sets the meter description (pyfly's `description` argument).
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the `class` tag value.
    #[must_use]
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the `method` tag value.
    #[must_use]
    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Adds an extra tag (pyfly's `extra_tags`).
    #[must_use]
    pub fn tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.push((key.into(), value.into()));
        self
    }

    fn counter(&self) -> Arc<Counter> {
        // The exposition appends `_total`; drop a user-supplied suffix,
        // exactly like pyfly (prometheus_client appends it itself).
        let mut prom_name = sanitize_metric_name(&self.name);
        if let Some(stripped) = prom_name.strip_suffix("_total") {
            prom_name = stripped.to_string();
        }
        let mut label_names: Vec<&str> = vec!["class", "method", "result", "exception"];
        label_names.extend(self.extra.iter().map(|(k, _)| k.as_str()));
        self.registry
            .counter(&prom_name, &self.description, &label_names)
    }

    fn label_values(&self, result: &str, exception: &str) -> Vec<String> {
        let mut values = vec![
            self.class.clone(),
            self.method.clone(),
            result.to_string(),
            exception.to_string(),
        ];
        values.extend(self.extra.iter().map(|(_, v)| v.clone()));
        values
    }

    /// Runs `fut`, counting it with `result="success"`, `exception="none"`.
    pub async fn record<T>(self, fut: impl Future<Output = T>) -> T {
        let counter = self.counter();
        let out = fut.await;
        let values = self.label_values("success", "none");
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        counter.labels(&refs).inc();
        out
    }

    /// Runs `fut`, counting `Ok` as `result="success"` / `exception="none"`
    /// and `Err` as `result="failure"` with the unqualified error type name.
    pub async fn record_result<T, E>(
        self,
        fut: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let counter = self.counter();
        let out = fut.await;
        let (result, exception) = if out.is_err() {
            ("failure", short_type_name::<E>())
        } else {
            ("success", "none")
        };
        let values = self.label_values(result, exception);
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        counter.labels(&refs).inc();
        out
    }
}

/// Times `fut` under the Micrometer meter `name` — shorthand for
/// [`Timed::new(registry, name).record(fut)`](Timed::record).
pub async fn timed<T>(registry: &MetricsRegistry, name: &str, fut: impl Future<Output = T>) -> T {
    Timed::new(registry, name).record(fut).await
}

/// Times a fallible `fut`, tagging `exception` with the error type name on
/// `Err` — shorthand for [`Timed::record_result`].
pub async fn timed_result<T, E>(
    registry: &MetricsRegistry,
    name: &str,
    fut: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    Timed::new(registry, name).record_result(fut).await
}

/// Counts one invocation of `fut` under the Micrometer meter `name` —
/// shorthand for [`Counted::record`].
pub async fn counted<T>(registry: &MetricsRegistry, name: &str, fut: impl Future<Output = T>) -> T {
    Counted::new(registry, name).record(fut).await
}

/// Counts a fallible `fut` as success/failure — shorthand for
/// [`Counted::record_result`].
pub async fn counted_result<T, E>(
    registry: &MetricsRegistry,
    name: &str,
    fut: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    Counted::new(registry, name).record_result(fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_converts_dot_case() {
        assert_eq!(sanitize_metric_name("orders.process"), "orders_process");
        assert_eq!(sanitize_metric_name("a-b.c"), "a_b_c");
    }

    #[test]
    fn short_type_name_strips_path_and_generics() {
        assert_eq!(short_type_name::<std::io::Error>(), "Error");
        assert_eq!(short_type_name::<Vec<u8>>(), "Vec");
        assert_eq!(short_type_name::<u32>(), "u32");
    }

    #[test]
    fn render_value_matches_prometheus_client() {
        assert_eq!(render_value(1.0), "1.0");
        assert_eq!(render_value(0.5), "0.5");
        assert_eq!(render_value(0.005), "0.005");
        assert_eq!(render_value(f64::INFINITY), "inf");
    }

    #[test]
    fn labels_render_sorted_by_name() {
        let names = vec!["method".to_string(), "class".to_string()];
        let values = vec!["m".to_string(), "C".to_string()];
        assert_eq!(
            render_labels(&names, &values, None),
            r#"{class="C",method="m"}"#
        );
        assert_eq!(render_labels(&[], &[], None), "");
    }

    #[test]
    #[should_panic(expected = "expects 1 label value")]
    fn label_arity_mismatch_panics() {
        let registry = MetricsRegistry::isolated();
        let c = registry.counter("arity", "c", &["route"]);
        c.inc();
    }

    #[test]
    fn histogram_buckets_cumulative() {
        let registry = MetricsRegistry::isolated();
        let h = registry.histogram("h", "hist", &[], Some(&[1.0, 2.0]));
        h.observe(0.5);
        h.observe(1.5);
        h.observe(9.0);
        assert_eq!(h.count(), 3);
        assert!((h.sum() - 11.0).abs() < 1e-9);
        let text = registry.prometheus_text();
        assert!(text.contains(r#"h_bucket{le="1.0"} 1.0"#), "{text}");
        assert!(text.contains(r#"h_bucket{le="2.0"} 2.0"#), "{text}");
        assert!(text.contains(r#"h_bucket{le="+Inf"} 3.0"#), "{text}");
        assert!(text.contains("h_count 3.0"), "{text}");
    }
}
