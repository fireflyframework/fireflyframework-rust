//! Structured logging with automatic correlation-id enrichment.
//!
//! The Go port builds a `slog.Logger` whose handler chain decorates every
//! record with the `correlationId` read from the `context.Context`. Rust's
//! logging facade is [`tracing`], so this module exposes the same behaviour
//! as a [`tracing_subscriber`] [`Layer`]: [`CorrelationLayer`] formats every
//! event as one JSON (or logfmt-style text) line and injects the
//! correlation id read from the [`firefly_kernel`] task-local scope.
//!
//! The JSON field names are byte-identical to the Go `slog` JSON handler —
//! `time`, `level`, `msg`, `service`, `correlationId` — so log pipelines
//! built for the Java/.NET/Go/Python ports parse Rust services unchanged.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::SetGlobalDefaultError;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// Output encoding of the log stream — the counterpart of the Go
/// `LogConfig.Format` string (`"json"` | `"text"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LogFormat {
    /// One JSON object per line (the production default on every port).
    #[default]
    Json,
    /// `key=value` pairs, mirroring Go's `slog.NewTextHandler`.
    Text,
}

impl LogFormat {
    /// Maps the Go config string to a format: `"text"` selects
    /// [`LogFormat::Text`], anything else (including `""`) selects
    /// [`LogFormat::Json`] — the exact branch `NewLogger` takes in Go.
    pub fn from_name(name: &str) -> Self {
        if name == "text" {
            LogFormat::Text
        } else {
            LogFormat::Json
        }
    }
}

/// Tunes the logging layer. The defaults — JSON, info level, stdout —
/// match the Java/.NET/Go ports' production logging defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// Minimum level emitted; events below it are dropped.
    pub level: Level,
    /// Value of the `service` attribute stamped on every record; omitted
    /// when empty (the Go port's `firefly.service.name` analog).
    pub service: String,
    /// Output encoding.
    pub format: LogFormat,
}

impl Default for LogConfig {
    /// The canonical config: JSON, info, stdout — Go's `DefaultLogConfig()`.
    fn default() -> Self {
        Self {
            level: Level::INFO,
            service: String::new(),
            format: LogFormat::Json,
        }
    }
}

impl LogConfig {
    /// Returns the canonical default config (JSON, info level).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the minimum emitted level (builder-style).
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Sets the `service` attribute (builder-style).
    #[must_use]
    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = service.into();
        self
    }

    /// Sets the output encoding (builder-style).
    #[must_use]
    pub fn with_format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }
}

/// Ordered field collection: preserves first-insertion order (so `time`,
/// `level`, `msg` lead every record, like Go's slog handlers) while
/// letting later writes replace earlier values by key.
#[derive(Debug, Clone, Default)]
struct FieldSet(Vec<(String, Value)>);

impl FieldSet {
    fn insert(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.0.push((key, value));
        }
    }

    fn remove(&mut self, key: &str) -> Option<Value> {
        self.0
            .iter()
            .position(|(k, _)| k == key)
            .map(|i| self.0.remove(i).1)
    }
}

/// Collects `tracing` field values into a [`FieldSet`] as JSON values.
struct JsonVisitor<'a>(&'a mut FieldSet);

impl Visit for JsonVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(
            field.name(),
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
        );
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name(), Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name(), Value::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.0.insert(field.name(), Value::from(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name(), Value::from(format!("{value:?}")));
    }
}

/// A [`tracing_subscriber`] [`Layer`] that writes one structured log line
/// per event and enriches it with the correlation id from the
/// [`firefly_kernel`] task-local scope — the Rust counterpart of the Go
/// port's `CorrelationHandler` + `slog.NewJSONHandler` chain.
///
/// Field names on the wire (`time`, `level`, `msg`, `service`,
/// `correlationId`) are identical to the Go `slog` output. Fields recorded
/// on enclosing spans are merged into every event, mirroring Go's
/// `logger.With(...)` handler attributes.
pub struct CorrelationLayer {
    level: Level,
    service: String,
    format: LogFormat,
    writer: Arc<Mutex<dyn Write + Send>>,
}

impl CorrelationLayer {
    /// Builds a layer with the given config, writing to stdout — the Go
    /// `NewLogger(cfg)` default sink.
    pub fn new(cfg: LogConfig) -> Self {
        Self::with_writer(cfg, io::stdout())
    }

    /// Builds a layer writing to the given sink — the analog of setting
    /// `LogConfig.Output` in Go. Pass a [`BufferWriter`] in tests.
    pub fn with_writer(cfg: LogConfig, writer: impl Write + Send + 'static) -> Self {
        Self {
            level: cfg.level,
            service: cfg.service,
            format: cfg.format,
            writer: Arc::new(Mutex::new(writer)),
        }
    }

    /// Renders one record. `level` is the slog-compatible level name.
    fn write_record(&self, record: &FieldSet) {
        let line = match self.format {
            LogFormat::Json => render_json(record),
            LogFormat::Text => render_text(record),
        };
        if let Ok(mut w) = self.writer.lock() {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}

impl<S> Layer<S> for CorrelationLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        metadata.level() <= &self.level
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut fields = FieldSet::default();
            attrs.record(&mut JsonVisitor(&mut fields));
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if let Some(fields) = ext.get_mut::<FieldSet>() {
                values.record(&mut JsonVisitor(fields));
            } else {
                let mut fields = FieldSet::default();
                values.record(&mut JsonVisitor(&mut fields));
                ext.insert(fields);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        if meta.level() > &self.level {
            return;
        }

        let mut event_fields = FieldSet::default();
        event.record(&mut JsonVisitor(&mut event_fields));
        let msg = match event_fields.remove("message") {
            Some(Value::String(s)) => s,
            Some(other) => other.to_string(),
            None => String::new(),
        };

        let mut record = FieldSet::default();
        record.insert(
            "time",
            Value::from(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
        );
        record.insert("level", Value::from(level_name(meta.level())));
        record.insert("msg", Value::from(msg));
        if !self.service.is_empty() {
            record.insert("service", Value::from(self.service.clone()));
        }
        if let Some(id) = firefly_kernel::correlation_id() {
            record.insert("correlationId", Value::from(id));
        }
        // Merge fields from enclosing spans, root first — the analog of
        // Go's logger.With(...) handler attributes.
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<FieldSet>() {
                    for (k, v) in &fields.0 {
                        record.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        for (k, v) in event_fields.0 {
            record.insert(k, v);
        }

        self.write_record(&record);
    }
}

/// Maps a [`tracing`] level to the Go `slog` level vocabulary. `tracing`
/// has a fifth level (`TRACE`) that slog lacks; it maps to `DEBUG` so the
/// wire set stays `DEBUG` / `INFO` / `WARN` / `ERROR` on every port.
fn level_name(level: &Level) -> &'static str {
    if *level == Level::ERROR {
        "ERROR"
    } else if *level == Level::WARN {
        "WARN"
    } else if *level == Level::INFO {
        "INFO"
    } else {
        "DEBUG"
    }
}

/// Serializes the record as a single JSON object, preserving field order.
fn render_json(fields: &FieldSet) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in fields.0.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(k).unwrap_or_default());
        out.push(':');
        out.push_str(&serde_json::to_string(v).unwrap_or_default());
    }
    out.push('}');
    out
}

/// Serializes the record as `key=value` pairs, quoting values that contain
/// whitespace, `=`, or `"` — the convention of Go's `slog.NewTextHandler`.
fn render_text(fields: &FieldSet) -> String {
    fields
        .0
        .iter()
        .map(|(k, v)| format!("{k}={}", text_value(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn text_value(v: &Value) -> String {
    match v {
        Value::String(s) => {
            if s.is_empty() || s.contains(|c: char| c.is_whitespace() || c == '"' || c == '=') {
                serde_json::to_string(s).unwrap_or_default()
            } else {
                s.clone()
            }
        }
        other => other.to_string(),
    }
}

/// Builds a complete subscriber (registry + [`CorrelationLayer`]) writing
/// to stdout — the Rust analog of Go's `NewLogger(cfg)`.
///
/// Install it globally with [`init_logging`], or scope it to a future with
/// [`tracing::instrument::WithSubscriber::with_subscriber`].
pub fn subscriber(cfg: LogConfig) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry().with(CorrelationLayer::new(cfg))
}

/// Like [`subscriber`] but writing to the given sink — the analog of
/// setting `LogConfig.Output` in Go.
pub fn subscriber_with_writer(
    cfg: LogConfig,
    writer: impl Write + Send + 'static,
) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry().with(CorrelationLayer::with_writer(cfg, writer))
}

/// Installs the [`subscriber`] as the process-global default. Call once
/// at startup; fails if a global subscriber is already set.
pub fn init_logging(cfg: LogConfig) -> Result<(), SetGlobalDefaultError> {
    tracing::subscriber::set_global_default(subscriber(cfg))
}

/// A clonable, thread-safe in-memory sink — the Rust stand-in for the
/// `bytes.Buffer` the Go tests pass as `LogConfig.Output`. All clones
/// share the same underlying buffer.
///
/// ```
/// use std::io::Write;
/// use firefly_observability::BufferWriter;
///
/// let buf = BufferWriter::new();
/// let mut clone = buf.clone();
/// writeln!(clone, "hello").unwrap();
/// assert_eq!(buf.as_string(), "hello\n");
/// ```
#[derive(Debug, Clone, Default)]
pub struct BufferWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl BufferWriter {
    /// Returns an empty shared buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a copy of everything written so far.
    pub fn contents(&self) -> Vec<u8> {
        self.lock().clone()
    }

    /// Returns everything written so far as a (lossy) UTF-8 string.
    pub fn as_string(&self) -> String {
        String::from_utf8_lossy(&self.lock()).into_owned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
        self.buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Write for BufferWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.lock().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_json_info() {
        let cfg = LogConfig::default();
        assert_eq!(cfg.level, Level::INFO);
        assert_eq!(cfg.format, LogFormat::Json);
        assert!(cfg.service.is_empty());
    }

    #[test]
    fn format_from_name_matches_go_branch() {
        assert_eq!(LogFormat::from_name("text"), LogFormat::Text);
        assert_eq!(LogFormat::from_name("json"), LogFormat::Json);
        assert_eq!(LogFormat::from_name(""), LogFormat::Json);
        assert_eq!(LogFormat::from_name("TEXT"), LogFormat::Json); // exact match, like Go
    }

    #[test]
    fn config_builder_chains() {
        let cfg = LogConfig::new()
            .with_level(Level::DEBUG)
            .with_service("orders")
            .with_format(LogFormat::Text);
        assert_eq!(cfg.level, Level::DEBUG);
        assert_eq!(cfg.service, "orders");
        assert_eq!(cfg.format, LogFormat::Text);
    }

    #[test]
    fn level_names_match_slog_vocabulary() {
        assert_eq!(level_name(&Level::ERROR), "ERROR");
        assert_eq!(level_name(&Level::WARN), "WARN");
        assert_eq!(level_name(&Level::INFO), "INFO");
        assert_eq!(level_name(&Level::DEBUG), "DEBUG");
        assert_eq!(level_name(&Level::TRACE), "DEBUG");
    }

    #[test]
    fn field_set_preserves_order_and_replaces() {
        let mut f = FieldSet::default();
        f.insert("a", Value::from(1));
        f.insert("b", Value::from(2));
        f.insert("a", Value::from(3));
        assert_eq!(render_json(&f), r#"{"a":3,"b":2}"#);
        assert_eq!(f.remove("a"), Some(Value::from(3)));
        assert_eq!(f.remove("a"), None);
    }

    #[test]
    fn text_values_quote_when_needed() {
        assert_eq!(text_value(&Value::from("plain")), "plain");
        assert_eq!(text_value(&Value::from("cold start")), r#""cold start""#);
        assert_eq!(text_value(&Value::from("")), r#""""#);
        assert_eq!(text_value(&Value::from(42)), "42");
        assert_eq!(text_value(&Value::from(true)), "true");
    }

    #[test]
    fn buffer_writer_clones_share_storage() {
        let buf = BufferWriter::new();
        let mut w = buf.clone();
        w.write_all(b"abc").unwrap();
        w.flush().unwrap();
        assert_eq!(buf.contents(), b"abc");
        assert_eq!(buf.as_string(), "abc");
    }
}
