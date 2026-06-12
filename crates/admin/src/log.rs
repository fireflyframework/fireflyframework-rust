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

//! In-memory log capture ring buffer + `tracing` layer — the Rust rendering
//! of pyfly's `AdminLogHandler`.
//!
//! [`LogBuffer`] is both a fixed-size ring buffer (capacity 2000) of captured
//! [`LogRecord`]s **and** a [`tracing_subscriber::Layer`]: install it on the
//! global subscriber (`Registry::default().with(buffer.clone())`) and every
//! event is captured with a monotonic id, level, target, message, fields, and
//! timestamp. The monotonic ids drive incremental SSE polling
//! ([`records_after`](LogBuffer::records_after)) exactly like pyfly's
//! `get_records(after=id)`.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Default ring-buffer capacity, matching pyfly's `AdminLogHandler`.
pub const DEFAULT_LOG_CAPACITY: usize = 2000;

/// One captured log event — the elements of the `{"records": […]}` array
/// served on `GET /admin/api/logfile`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    /// Monotonic id (1-based), used by incremental SSE polling.
    pub id: u64,
    /// RFC 3339 UTC instant at which the event was captured.
    pub timestamp: String,
    /// Spring-style level name (`ERROR`/`WARN`/`INFO`/`DEBUG`/`TRACE`).
    pub level: String,
    /// The event's target (the `tracing` analogue of a logger name).
    pub logger: String,
    /// The event's `message` field (its primary text).
    pub message: String,
    /// Remaining structured fields rendered as `key=value` pairs — pyfly's
    /// structlog `context`.
    pub context: String,
    /// The capturing thread's name, when available.
    pub thread: Option<String>,
}

/// In-memory log ring buffer that doubles as a [`tracing_subscriber::Layer`]
/// — pyfly's `AdminLogHandler`. Clone freely: clones share the same buffer
/// (the inner state is `Arc`-shared).
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<LogBufferInner>,
}

struct LogBufferInner {
    capacity: usize,
    counter: AtomicU64,
    records: Mutex<VecDeque<LogRecord>>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBuffer {
    /// Returns a buffer holding the last [`DEFAULT_LOG_CAPACITY`] records.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_LOG_CAPACITY)
    }

    /// Returns a buffer holding the last `capacity` records (minimum 1).
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner: Arc::new(LogBufferInner {
                capacity,
                counter: AtomicU64::new(0),
                records: Mutex::new(VecDeque::with_capacity(capacity)),
            }),
        }
    }

    /// Appends a fully-formed record, assigning it the next monotonic id and
    /// evicting the oldest when full. Exposed for tests and for callers that
    /// bridge another logging facade.
    pub fn push(&self, mut record: LogRecord) {
        let id = self.inner.counter.fetch_add(1, Ordering::SeqCst) + 1;
        record.id = id;
        let mut records = self.inner.records.lock().expect("log buffer lock poisoned");
        if records.len() == self.inner.capacity {
            records.pop_front();
        }
        records.push_back(record);
    }

    /// Every retained record, oldest first (pyfly's `get_all`).
    pub fn all(&self) -> Vec<LogRecord> {
        self.inner
            .records
            .lock()
            .expect("log buffer lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Records with `id > after`, for incremental SSE polling (pyfly's
    /// `get_records(after=id)`).
    pub fn records_after(&self, after: u64) -> Vec<LogRecord> {
        self.inner
            .records
            .lock()
            .expect("log buffer lock poisoned")
            .iter()
            .filter(|r| r.id > after)
            .cloned()
            .collect()
    }

    /// Removes every retained record (the monotonic counter is **not**
    /// reset, matching pyfly's `deque.clear()` which leaves `_counter`).
    pub fn clear(&self) {
        self.inner
            .records
            .lock()
            .expect("log buffer lock poisoned")
            .clear();
    }

    /// Number of retained records.
    pub fn len(&self) -> usize {
        self.inner
            .records
            .lock()
            .expect("log buffer lock poisoned")
            .len()
    }

    /// Whether nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `{"available": true, "records": […], "total": N}` body for
    /// `GET /admin/api/logfile` (pyfly's `LogfileProvider.get_logfile`).
    pub fn logfile_json(&self) -> Value {
        let records = self.all();
        let total = records.len();
        serde_json::json!({
            "available": true,
            "records": records,
            "total": total,
        })
    }
}

/// Collects an event's fields: the `message` field becomes the record
/// message, every other field is appended to the `key=value` context.
#[derive(Default)]
struct FieldVisitor {
    message: String,
    context: String,
}

impl FieldVisitor {
    fn append(&mut self, name: &str, value: impl fmt::Display) {
        if name == "message" {
            self.message = value.to_string();
        } else {
            if !self.context.is_empty() {
                self.context.push(' ');
            }
            self.context.push_str(name);
            self.context.push('=');
            self.context.push_str(&value.to_string());
        }
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.append(field.name(), format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.append(field.name(), value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.append(field.name(), value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.append(field.name(), value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.append(field.name(), value);
    }
}

/// Maps a `tracing` level to its Spring-style wire name.
fn level_name(level: &tracing::Level) -> &'static str {
    match *level {
        tracing::Level::ERROR => "ERROR",
        tracing::Level::WARN => "WARN",
        tracing::Level::INFO => "INFO",
        tracing::Level::DEBUG => "DEBUG",
        tracing::Level::TRACE => "TRACE",
    }
}

impl<S: Subscriber> Layer<S> for LogBuffer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.push(LogRecord {
            id: 0, // assigned in push()
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            level: level_name(metadata.level()).to_string(),
            logger: metadata.target().to_string(),
            message: visitor.message,
            context: visitor.context,
            thread: std::thread::current().name().map(str::to_owned),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    fn record(message: &str, level: &str) -> LogRecord {
        LogRecord {
            id: 0,
            timestamp: "2026-06-12T00:00:00Z".into(),
            level: level.into(),
            logger: "test".into(),
            message: message.into(),
            context: String::new(),
            thread: None,
        }
    }

    // pyfly: test_captures_log_records
    #[test]
    fn assigns_monotonic_ids() {
        let buf = LogBuffer::new();
        buf.push(record("hello world", "INFO"));
        buf.push(record("watch out", "WARN"));
        let all = buf.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, 1);
        assert_eq!(all[0].message, "hello world");
        assert_eq!(all[1].id, 2);
        assert_eq!(all[1].level, "WARN");
    }

    // pyfly: test_ring_buffer_evicts_old
    #[test]
    fn ring_buffer_evicts_old() {
        let buf = LogBuffer::with_capacity(3);
        for n in 0..5 {
            buf.push(record(&format!("msg-{n}"), "INFO"));
        }
        let all = buf.all();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].message, "msg-2");
        assert_eq!(all[2].message, "msg-4");
    }

    // pyfly: test_get_records_after_id
    #[test]
    fn records_after_skips_seen() {
        let buf = LogBuffer::new();
        buf.push(record("first", "INFO"));
        buf.push(record("second", "INFO"));
        buf.push(record("third", "INFO"));
        let after = buf.records_after(1);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].message, "second");
        assert_eq!(after[1].message, "third");
        assert!(buf.records_after(3).is_empty());
    }

    // pyfly: test_clear
    #[test]
    fn clear_empties_buffer() {
        let buf = LogBuffer::new();
        buf.push(record("will be cleared", "INFO"));
        assert_eq!(buf.len(), 1);
        buf.clear();
        assert!(buf.is_empty());
    }

    // pyfly: test_record_format — captured fields populate message + context.
    #[test]
    fn captures_tracing_events_as_records() {
        let buf = LogBuffer::new();
        let subscriber = tracing_subscriber::registry().with(buf.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(method = "GET", path = "/health", "http_request");
        });
        let all = buf.all();
        assert_eq!(all.len(), 1);
        let rec = &all[0];
        assert_eq!(rec.level, "ERROR");
        assert_eq!(rec.message, "http_request");
        assert!(rec.context.contains("method=GET"), "{}", rec.context);
        assert!(rec.context.contains("path=/health"), "{}", rec.context);
        assert!(rec.id >= 1);
    }

    #[test]
    fn logfile_json_shape() {
        let buf = LogBuffer::new();
        buf.push(record("hi", "INFO"));
        let body = buf.logfile_json();
        assert_eq!(body["available"], true);
        assert_eq!(body["total"], 1);
        assert_eq!(body["records"][0]["message"], "hi");
    }
}
