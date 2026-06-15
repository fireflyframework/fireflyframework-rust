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

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, RwLock};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{Interest, SetGlobalDefaultError};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use crate::appender::{FileConfig, RollingFileWriter, TeeWriter};
use crate::redaction::{build_redactor, RedactionConfig, Redactor, RegexRedactor, REDACTED};

/// Output encoding of the log stream — the counterpart of the Go
/// `LogConfig.Format` string (`"json"` | `"text"`) and pyfly's
/// `pyfly.logging.format` (`json` | `logfmt` | `console`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LogFormat {
    /// One JSON object per line (the production default on every port).
    #[default]
    Json,
    /// `key=value` pairs, mirroring Go's `slog.NewTextHandler` and pyfly's
    /// `logfmt` renderer.
    Text,
    /// A human-friendly console/dev renderer — pyfly's
    /// `structlog.dev.ConsoleRenderer`. Renders `time [LEVEL] msg` with the
    /// remaining fields as `key=value` pairs, optionally ANSI-colored by
    /// level. Intended for local development, not production pipelines.
    Console,
}

impl LogFormat {
    /// Maps a config string to a format: `"text"` or `"logfmt"` select
    /// [`LogFormat::Text`]; `"console"`, `"pretty"`, or `"dev"` select
    /// [`LogFormat::Console`]; anything else (including `""`) selects
    /// [`LogFormat::Json`]. The Go branch only knew `"text"`; the `logfmt`
    /// alias and the console family are pyfly-parity additions.
    pub fn from_name(name: &str) -> Self {
        match name {
            "text" | "logfmt" => LogFormat::Text,
            "console" | "pretty" | "dev" => LogFormat::Console,
            _ => LogFormat::Json,
        }
    }
}

/// Tunes the logging layer. The defaults — JSON, info level, stdout —
/// match the Java/.NET/Go ports' production logging defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// Minimum level emitted; events below it are dropped. This is the
    /// root level — pyfly's `pyfly.logging.level.root`.
    pub level: Level,
    /// Value of the `service` attribute stamped on every record; omitted
    /// when empty (the Go port's `firefly.service.name` analog).
    pub service: String,
    /// Output encoding.
    pub format: LogFormat,
    /// Per-target level overrides — pyfly's per-logger level map
    /// (`{root: INFO, "my.module": DEBUG}`). Keys match `tracing` targets
    /// by longest prefix at a `::` (or `.`) boundary; the most specific
    /// match wins, falling back to [`LogConfig::level`].
    pub levels: BTreeMap<String, Level>,
    /// Optional rolling file appender — pyfly's `pyfly.logging.file.*`.
    /// When set, records are written to console **and** file
    /// ([`TeeWriter`]); when the file cannot be opened the layer falls
    /// back to console only (a logging misconfiguration never crashes the
    /// application).
    pub file: Option<FileConfig>,
    /// Optional PII redaction — pyfly's `pyfly.logging.redaction.*`.
    /// `None` (the default) keeps every existing wire shape untouched.
    pub redaction: Option<RedactionConfig>,
    /// Whether [`LogFormat::Console`] emits ANSI color escapes (level color +
    /// dim field keys). Ignored by the JSON/text renderers. Defaults to
    /// `false` — matching pyfly's `ConsoleRenderer(colors=False)` so the
    /// console output is plain text unless explicitly opted in. Enable it for
    /// an interactive terminal.
    pub console_colors: bool,
}

impl Default for LogConfig {
    /// The canonical config: JSON, info, stdout — Go's `DefaultLogConfig()`.
    fn default() -> Self {
        Self {
            level: Level::INFO,
            service: String::new(),
            format: LogFormat::Json,
            levels: BTreeMap::new(),
            file: None,
            redaction: None,
            console_colors: false,
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

    /// Adds a per-target level override (builder-style) — the pyfly
    /// `{"my.module": "DEBUG"}` level-map entry. The target `"root"` (or
    /// `""`) sets the root level instead.
    #[must_use]
    pub fn with_target_level(mut self, target: impl Into<String>, level: Level) -> Self {
        let target = target.into();
        if target.is_empty() || target == ROOT_TARGET {
            self.level = level;
        } else {
            self.levels.insert(target, level);
        }
        self
    }

    /// Enables the rolling file appender (builder-style).
    #[must_use]
    pub fn with_file(mut self, file: FileConfig) -> Self {
        self.file = Some(file);
        self
    }

    /// Enables PII redaction in the JSON/text writers (builder-style).
    #[must_use]
    pub fn with_redaction(mut self, redaction: RedactionConfig) -> Self {
        self.redaction = Some(redaction);
        self
    }

    /// Enables ANSI coloring for the [`LogFormat::Console`] renderer
    /// (builder-style). No effect on the JSON/text renderers.
    #[must_use]
    pub fn with_console_colors(mut self, enabled: bool) -> Self {
        self.console_colors = enabled;
        self
    }
}

/// The pseudo-target naming the root level — pyfly's `"root"` key.
pub const ROOT_TARGET: &str = "root";

/// Mutable level state shared between a [`CorrelationLayer`] and its
/// [`LevelHandle`]s.
#[derive(Debug)]
struct LevelState {
    root: Level,
    targets: BTreeMap<String, Level>,
}

impl LevelState {
    /// True when `prefix` matches `target` at a module boundary —
    /// `firefly_web` matches `firefly_web` and `firefly_web::routes`, but
    /// not `firefly_webx`. Both `::` (tracing) and `.` (pyfly dotted
    /// names) count as boundaries.
    fn target_matches(target: &str, prefix: &str) -> bool {
        match target.strip_prefix(prefix) {
            Some("") => true,
            Some(rest) => rest.starts_with("::") || rest.starts_with('.'),
            None => false,
        }
    }

    /// The level for `target`: the longest matching prefix wins, falling
    /// back to the root level.
    fn effective_level(&self, target: &str) -> Level {
        let mut best: Option<(usize, Level)> = None;
        for (prefix, level) in &self.targets {
            if Self::target_matches(target, prefix)
                && best.is_none_or(|(len, _)| prefix.len() >= len)
            {
                best = Some((prefix.len(), *level));
            }
        }
        best.map_or(self.root, |(_, level)| level)
    }
}

/// A clonable handle for reading and changing log levels at runtime — the
/// Rust analog of pyfly's `LoggingPort.set_level` (and the backing store
/// for an `/actuator/loggers` endpoint).
///
/// Obtain one from [`CorrelationLayer::level_handle`] or the
/// `*_with_handle` subscriber constructors. Changes take effect on the
/// next event.
///
/// ```
/// use firefly_observability::{subscriber_with_writer_and_handle, BufferWriter, LogConfig};
/// use tracing::Level;
///
/// let buf = BufferWriter::new();
/// let (sub, handle) = subscriber_with_writer_and_handle(LogConfig::new(), buf.clone());
/// tracing::subscriber::with_default(sub, || {
///     tracing::debug!("dropped");
///     handle.set_level("root", Level::DEBUG);
///     tracing::debug!("kept");
/// });
/// assert!(!buf.as_string().contains("dropped"));
/// assert!(buf.as_string().contains("kept"));
/// ```
#[derive(Debug, Clone)]
pub struct LevelHandle {
    state: Arc<RwLock<LevelState>>,
}

impl LevelHandle {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, LevelState> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, LevelState> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sets the level for `target` at runtime — pyfly's
    /// `set_level(name, level)`. The target `"root"` (or `""`) sets the
    /// root level.
    pub fn set_level(&self, target: &str, level: Level) {
        {
            let mut state = self.write();
            if target.is_empty() || target == ROOT_TARGET {
                state.root = level;
            } else {
                state.targets.insert(target.to_string(), level);
            }
        }
        tracing::callsite::rebuild_interest_cache();
    }

    /// Removes a per-target override so the target falls back to its
    /// parent prefix or the root level (the `/actuator/loggers` POST
    /// `null` reset). A no-op for `"root"`.
    pub fn clear_level(&self, target: &str) {
        {
            let mut state = self.write();
            state.targets.remove(target);
        }
        tracing::callsite::rebuild_interest_cache();
    }

    /// The effective level for `target` (longest-prefix match, falling
    /// back to root).
    pub fn level(&self, target: &str) -> Level {
        if target.is_empty() || target == ROOT_TARGET {
            return self.read().root;
        }
        self.read().effective_level(target)
    }

    /// A snapshot of every configured level, with the root level under
    /// the `"root"` key — the GET `/actuator/loggers` view.
    pub fn levels(&self) -> BTreeMap<String, Level> {
        let state = self.read();
        let mut map = state.targets.clone();
        map.insert(ROOT_TARGET.to_string(), state.root);
        map
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
    state: Arc<RwLock<LevelState>>,
    service: String,
    format: LogFormat,
    console_colors: bool,
    redactor: Option<RegexRedactor>,
    allow_fields: HashSet<String>,
    deny_fields: HashSet<String>,
    writer: Arc<Mutex<dyn Write + Send>>,
}

impl CorrelationLayer {
    /// Builds a layer with the given config, writing to stdout — the Go
    /// `NewLogger(cfg)` default sink. When [`LogConfig::file`] is set the
    /// output is teed to the rolling file appender as well (console stays
    /// on, like pyfly); if the file cannot be opened the layer falls back
    /// to stdout only.
    pub fn new(cfg: LogConfig) -> Self {
        Self::with_writer(cfg, io::stdout())
    }

    /// Builds a layer writing to the given sink — the analog of setting
    /// `LogConfig.Output` in Go. Pass a [`BufferWriter`] in tests. When
    /// [`LogConfig::file`] is set, `writer` plays the console role of the
    /// tee and the rolling file appender receives a copy of every record.
    pub fn with_writer(cfg: LogConfig, writer: impl Write + Send + 'static) -> Self {
        let sink: Arc<Mutex<dyn Write + Send>> = match cfg.file.as_ref() {
            Some(file_cfg) if !file_cfg.name.is_empty() => match RollingFileWriter::new(file_cfg) {
                Ok(file) => Arc::new(Mutex::new(TeeWriter::new(writer, file))),
                Err(_) => Arc::new(Mutex::new(writer)),
            },
            _ => Arc::new(Mutex::new(writer)),
        };
        let redactor = cfg.redaction.as_ref().and_then(build_redactor);
        let (allow_fields, deny_fields) = cfg
            .redaction
            .as_ref()
            .map(|r| {
                (
                    r.allow_fields.iter().cloned().collect(),
                    r.deny_fields.iter().cloned().collect(),
                )
            })
            .unwrap_or_default();
        Self {
            state: Arc::new(RwLock::new(LevelState {
                root: cfg.level,
                targets: cfg.levels,
            })),
            service: cfg.service,
            format: cfg.format,
            console_colors: cfg.console_colors,
            redactor,
            allow_fields,
            deny_fields,
            writer: sink,
        }
    }

    /// Returns a [`LevelHandle`] for reading/changing this layer's levels
    /// at runtime — pyfly's `LoggingPort.set_level`.
    pub fn level_handle(&self) -> LevelHandle {
        LevelHandle {
            state: Arc::clone(&self.state),
        }
    }

    fn effective_level(&self, target: &str) -> Level {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .effective_level(target)
    }

    /// Applies the configured PII redaction to the record — the analog of
    /// pyfly's structlog redaction processor: deny-listed keys are
    /// replaced wholesale with `<REDACTED>`; with a non-empty allow list
    /// only listed keys (plus the message) are scanned; string values are
    /// run through the regex engine. The `time`/`level` wire fields are
    /// never touched.
    fn apply_redaction(&self, record: &mut FieldSet) {
        let Some(redactor) = &self.redactor else {
            return;
        };
        for (key, value) in record.0.iter_mut() {
            if key == "time" || key == "level" {
                continue;
            }
            if self.deny_fields.contains(key) {
                *value = Value::from(REDACTED);
                continue;
            }
            // pyfly always scans the event message ("msg" here) even with
            // an allow list.
            if !self.allow_fields.is_empty() && !self.allow_fields.contains(key) && key != "msg" {
                continue;
            }
            if let Value::String(s) = value {
                if let std::borrow::Cow::Owned(redacted) = redactor.redact(s) {
                    *value = Value::from(redacted);
                }
            }
        }
    }

    /// Renders one record. `level` is the slog-compatible level name.
    fn write_record(&self, record: &FieldSet) {
        let mut line = match self.format {
            LogFormat::Json => render_json(record),
            LogFormat::Text => render_text(record),
            LogFormat::Console => render_console(record, self.console_colors),
        };
        line.push('\n');
        if let Ok(mut w) = self.writer.lock() {
            // One write per record so the rolling appender never splits a
            // line across a rotation.
            let _ = w.write_all(line.as_bytes());
            let _ = w.flush();
        }
    }
}

impl<S> Layer<S> for CorrelationLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        // Never cache the per-callsite decision: levels can change at
        // runtime via [`LevelHandle::set_level`].
        Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        metadata.level() <= &self.effective_level(metadata.target())
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
        if meta.level() > &self.effective_level(meta.target()) {
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
        // Inject the active span's trace_id / span_id so logs and traces
        // correlate in the same pipeline — the Rust analog of pyfly's
        // `_add_trace_ids` structlog processor (the SLF4J MDC equivalent).
        // Sourced from the W3C `traceparent` task-local
        // (`firefly_observability::current_traceparent`): the trace-id is the
        // 32-hex trace identifier and the span-id is the current parent-id
        // (16-hex). A no-op when no trace context is in scope, so it stays
        // zero-overhead without tracing.
        if let Some(raw) = crate::trace_context::current_traceparent() {
            if let Ok(tp) = crate::trace_context::TraceParent::parse(&raw) {
                record.insert("trace_id", Value::from(tp.trace_id));
                record.insert("span_id", Value::from(tp.parent_id));
            }
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

        self.apply_redaction(&mut record);
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

/// ANSI escape that resets all styling.
const ANSI_RESET: &str = "\u{1b}[0m";
/// Dim/faint styling for the leading timestamp and field keys.
const ANSI_DIM: &str = "\u{1b}[2m";

/// The ANSI color for each level name — green INFO, yellow WARN, red ERROR,
/// cyan DEBUG (matching structlog's `ConsoleRenderer` palette closely
/// enough for log-watching).
fn level_color(level: &str) -> &'static str {
    match level {
        "ERROR" => "\u{1b}[31m", // red
        "WARN" => "\u{1b}[33m",  // yellow
        "INFO" => "\u{1b}[32m",  // green
        _ => "\u{1b}[36m",       // cyan (DEBUG/other)
    }
}

/// Renders the record as a human-friendly console line — the Rust analog of
/// pyfly's `structlog.dev.ConsoleRenderer`. Layout:
///
/// `<time> [<LEVEL>] <msg> key=value key=value`
///
/// `time`, `level`, and `msg` lead the line (in the same fixed positions as
/// every renderer); the remaining fields trail as `key=value` pairs in
/// insertion order. With `colors`, the level is colorized and the timestamp +
/// field keys are dimmed; without it the output is plain text (pyfly's
/// `colors=False` default). Intended for local development, never for a
/// production log pipeline.
fn render_console(fields: &FieldSet, colors: bool) -> String {
    let mut time = "";
    let mut level = "";
    let mut msg = "";
    let mut rest: Vec<(&str, &Value)> = Vec::new();
    for (k, v) in &fields.0 {
        match k.as_str() {
            "time" => time = v.as_str().unwrap_or(""),
            "level" => level = v.as_str().unwrap_or(""),
            "msg" => msg = v.as_str().unwrap_or(""),
            _ => rest.push((k, v)),
        }
    }

    let mut out = String::new();
    if !time.is_empty() {
        if colors {
            out.push_str(ANSI_DIM);
            out.push_str(time);
            out.push_str(ANSI_RESET);
        } else {
            out.push_str(time);
        }
        out.push(' ');
    }
    // Level padded to a fixed width so messages align in a terminal.
    let level_field = format!("[{level:<5}]");
    if colors {
        out.push_str(level_color(level));
        out.push_str(&level_field);
        out.push_str(ANSI_RESET);
    } else {
        out.push_str(&level_field);
    }
    out.push(' ');
    out.push_str(msg);

    for (k, v) in rest {
        out.push(' ');
        if colors {
            out.push_str(ANSI_DIM);
            out.push_str(k);
            out.push('=');
            out.push_str(ANSI_RESET);
        } else {
            out.push_str(k);
            out.push('=');
        }
        out.push_str(&text_value(v));
    }
    out
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

/// Like [`subscriber`] but also returning the [`LevelHandle`] for runtime
/// level changes — pyfly's `LoggingPort.set_level` surface.
pub fn subscriber_with_handle(cfg: LogConfig) -> (impl Subscriber + Send + Sync, LevelHandle) {
    let layer = CorrelationLayer::new(cfg);
    let handle = layer.level_handle();
    (tracing_subscriber::registry().with(layer), handle)
}

/// Like [`subscriber_with_writer`] but also returning the [`LevelHandle`].
pub fn subscriber_with_writer_and_handle(
    cfg: LogConfig,
    writer: impl Write + Send + 'static,
) -> (impl Subscriber + Send + Sync, LevelHandle) {
    let layer = CorrelationLayer::with_writer(cfg, writer);
    let handle = layer.level_handle();
    (tracing_subscriber::registry().with(layer), handle)
}

/// Installs the [`subscriber`] as the process-global default. Call once
/// at startup; fails if a global subscriber is already set.
pub fn init_logging(cfg: LogConfig) -> Result<(), SetGlobalDefaultError> {
    tracing::subscriber::set_global_default(subscriber(cfg))
}

/// Like [`init_logging`] but returns the [`LevelHandle`] so log levels can
/// be changed at runtime (e.g. from an `/actuator/loggers` endpoint).
pub fn init_logging_with_handle(cfg: LogConfig) -> Result<LevelHandle, SetGlobalDefaultError> {
    let (sub, handle) = subscriber_with_handle(cfg);
    tracing::subscriber::set_global_default(sub)?;
    Ok(handle)
}

/// The concrete subscriber type produced by [`subscriber`] — the registry
/// with the [`CorrelationLayer`] installed. Exposed so callers can name the
/// [`Layer`] bound required by [`init_logging_with_layers`].
pub type FireflyRegistry =
    tracing_subscriber::layer::Layered<CorrelationLayer, tracing_subscriber::Registry>;

/// A boxed additional [`Layer`] composable over the Firefly logging
/// subscriber — e.g. the admin dashboard's in-memory capture buffer feeding
/// `/admin/api/logfile`. Build one with `my_layer.boxed()`.
pub type DynLogLayer = Box<dyn Layer<FireflyRegistry> + Send + Sync + 'static>;

/// Like [`init_logging_with_handle`] but also installs additional [`Layer`]s
/// over the correlation layer before setting the global default — the hook a
/// turnkey [`firefly_starter_core`]/admin wiring uses to tee every log record
/// into a dashboard capture buffer while the console JSON stream stays on.
/// Returns the [`LevelHandle`] for runtime level control.
///
/// ```no_run
/// use firefly_observability::{init_logging_with_layers, LogConfig};
/// // `extra` is typically `vec![log_buffer.clone().boxed()]`.
/// let _handle = init_logging_with_layers(LogConfig::default(), Vec::new());
/// ```
pub fn init_logging_with_layers(
    cfg: LogConfig,
    extra: Vec<DynLogLayer>,
) -> Result<LevelHandle, SetGlobalDefaultError> {
    let correlation = CorrelationLayer::new(cfg);
    let handle = correlation.level_handle();
    let subscriber = tracing_subscriber::registry().with(correlation).with(extra);
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(handle)
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

    #[test]
    fn target_matches_at_module_boundaries_only() {
        assert!(LevelState::target_matches("firefly_web", "firefly_web"));
        assert!(LevelState::target_matches(
            "firefly_web::routes",
            "firefly_web"
        ));
        assert!(LevelState::target_matches("my.module.sub", "my.module"));
        assert!(!LevelState::target_matches("firefly_webx", "firefly_web"));
        assert!(!LevelState::target_matches("firefly", "firefly_web"));
    }

    #[test]
    fn effective_level_longest_prefix_wins() {
        let mut targets = BTreeMap::new();
        targets.insert("app".to_string(), Level::WARN);
        targets.insert("app::orders".to_string(), Level::DEBUG);
        let state = LevelState {
            root: Level::INFO,
            targets,
        };
        assert_eq!(state.effective_level("app::orders::api"), Level::DEBUG);
        assert_eq!(state.effective_level("app::billing"), Level::WARN);
        assert_eq!(state.effective_level("elsewhere"), Level::INFO);
    }

    #[test]
    fn with_target_level_routes_root_to_level_field() {
        let cfg = LogConfig::new()
            .with_target_level("root", Level::DEBUG)
            .with_target_level("my.module", Level::TRACE);
        assert_eq!(cfg.level, Level::DEBUG);
        assert_eq!(cfg.levels.get("my.module"), Some(&Level::TRACE));
        assert!(!cfg.levels.contains_key("root"));
    }

    #[test]
    fn level_handle_reads_and_writes() {
        let layer = CorrelationLayer::with_writer(LogConfig::new(), BufferWriter::new());
        let handle = layer.level_handle();
        assert_eq!(handle.level("root"), Level::INFO);
        handle.set_level("my::target", Level::DEBUG);
        assert_eq!(handle.level("my::target::sub"), Level::DEBUG);
        assert_eq!(handle.level("other"), Level::INFO);
        let snapshot = handle.levels();
        assert_eq!(snapshot.get("root"), Some(&Level::INFO));
        assert_eq!(snapshot.get("my::target"), Some(&Level::DEBUG));
        handle.clear_level("my::target");
        assert_eq!(handle.level("my::target::sub"), Level::INFO);
        handle.set_level("", Level::ERROR);
        assert_eq!(handle.level("root"), Level::ERROR);
    }
}
