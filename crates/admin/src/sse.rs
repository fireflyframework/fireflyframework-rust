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

//! Server-Sent Event streams for the live dashboard — the Rust rendering of
//! pyfly's `admin/api/sse.py`.
//!
//! Each `GET /admin/api/sse/{health,metrics,traces,logfile,runtime,server}`
//! route returns an [`axum::response::Sse`] driven by a
//! [`tokio::time::interval`]: on every tick the corresponding [`data`] shaper
//! is sampled and pushed as a named SSE event. Two streams are *incremental*
//! — `traces` and `logfile` track a cursor so only new rows are pushed
//! (matching pyfly's `last_count` / `last_id`).

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{Stream, StreamExt};
use serde_json::Value;
use tokio_stream::wrappers::IntervalStream;

use crate::data;
use crate::deps::AdminDeps;

/// Builds a named SSE event whose data payload is `value` serialised to JSON.
fn event(name: &str, value: &Value) -> Event {
    Event::default().event(name).data(value.to_string())
}

/// The tick interval derived from the configured refresh interval (clamped to
/// a sane floor so a misconfigured `0` cannot busy-loop).
fn interval_from_ms(ms: u64) -> Duration {
    Duration::from_millis(ms.max(250))
}

/// `GET /admin/api/sse/health` — samples the health snapshot on every tick
/// and pushes a `health` event only when the overall status changed (pyfly's
/// `health_stream` `last_status` guard).
pub fn health_stream(
    deps: Arc<AdminDeps>,
    refresh_ms: u64,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(interval_from_ms(refresh_ms)));
    // `then` runs the async health check per tick; `scan` mutates the
    // last-seen status cursor in place, yielding the event only on change.
    let stream = ticker
        .then(move |_| {
            let deps = Arc::clone(&deps);
            async move {
                let (body, _down) = data::health(&deps.health).await;
                let status = body
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("UNKNOWN")
                    .to_string();
                (status, body)
            }
        })
        .scan(None::<String>, |last_status, (status, body)| {
            let changed = last_status.as_deref() != Some(status.as_str());
            *last_status = Some(status);
            let item = changed.then(|| Ok(event("health", &body)));
            futures::future::ready(Some(item))
        })
        .filter_map(futures::future::ready);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /admin/api/sse/metrics` — pushes the live metric-value snapshot every
/// tick (pyfly's `metrics_stream`).
pub fn metrics_stream(
    deps: Arc<AdminDeps>,
    refresh_ms: u64,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(interval_from_ms(refresh_ms)));
    let stream = ticker.map(move |_| {
        let body = data::metric_values(&deps.metrics);
        Ok(event("metrics", &body))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /admin/api/sse/traces` — incrementally pushes each newly captured
/// trace, one `trace` event per row (pyfly's `traces_stream`, ~2s cadence).
pub fn traces_stream(deps: Arc<AdminDeps>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(Duration::from_secs(2)));
    let stream = ticker
        .scan(0usize, move |last_count, _| {
            let entries = deps.traces.entries();
            let current = entries.len();
            let new_rows: Vec<Value> = if current > *last_count {
                entries[*last_count..]
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
                    .collect()
            } else {
                Vec::new()
            };
            *last_count = current;
            futures::future::ready(Some(new_rows))
        })
        .flat_map(|rows| {
            futures::stream::iter(
                rows.into_iter()
                    .map(|row| Ok(event("trace", &row)))
                    .collect::<Vec<_>>(),
            )
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /admin/api/sse/logfile` — incrementally pushes each new log record,
/// one `log` event per record, tracking the monotonic id cursor (pyfly's
/// `logfile_stream`, ~1s cadence).
pub fn logfile_stream(deps: Arc<AdminDeps>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(Duration::from_secs(1)));
    let logs = deps.logs.clone();
    let stream = ticker
        .scan(0u64, move |last_id, _| {
            let records = logs.records_after(*last_id);
            if let Some(last) = records.last() {
                *last_id = last.id;
            }
            futures::future::ready(Some(records))
        })
        .flat_map(|records| {
            futures::stream::iter(
                records
                    .into_iter()
                    .map(|r| {
                        let value = serde_json::to_value(&r).unwrap_or(Value::Null);
                        Ok(event("log", &value))
                    })
                    .collect::<Vec<_>>(),
            )
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /admin/api/sse/beans` — every ~10s, samples each bean's
/// `resolution_count` from the wired DI container and pushes a `beans` event
/// carrying only the rows whose count changed since the last sample (pyfly's
/// `beans_stream` `last_counts` diff). The event payload is
/// `{"updates": [{"name", "resolution_count"}]}`; ticks with no change emit
/// nothing. Without a wired container the stream stays open and idle.
pub fn beans_stream(deps: Arc<AdminDeps>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(Duration::from_secs(10)));
    let stream = ticker
        .scan(
            std::collections::BTreeMap::<String, u64>::new(),
            move |last_counts, _| {
                let current = data::bean_resolution_counts(deps.container.as_ref());
                let updates: Vec<Value> = current
                    .iter()
                    .filter(|(name, count)| last_counts.get(*name) != Some(*count))
                    .map(|(name, count)| serde_json::json!({ "name": name, "resolution_count": count }))
                    .collect();
                *last_counts = current;
                let item = (!updates.is_empty())
                    .then(|| Ok(event("beans", &serde_json::json!({ "updates": updates }))));
                futures::future::ready(Some(item))
            },
        )
        .filter_map(futures::future::ready);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /admin/api/sse/runtime` — pushes the runtime snapshot every tick
/// (pyfly's `runtime_stream`).
pub fn runtime_stream(refresh_ms: u64) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(interval_from_ms(refresh_ms)));
    let stream = ticker.map(|_| Ok(event("runtime", &data::runtime())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /admin/api/sse/server` — pushes the server snapshot every tick
/// (pyfly's `server_stream`).
pub fn server_stream(refresh_ms: u64) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let ticker = IntervalStream::new(tokio::time::interval(interval_from_ms(refresh_ms)));
    let stream = ticker.map(|_| Ok(event("server", &data::server())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_renders_named_data() {
        let ev = event("health", &serde_json::json!({ "status": "UP" }));
        // axum Events render lazily; assert the field encoding via the wire.
        let body = format!("{ev:?}");
        assert!(body.contains("health"), "{body}");
    }

    #[test]
    fn interval_floor_is_enforced() {
        assert_eq!(interval_from_ms(0), Duration::from_millis(250));
        assert_eq!(interval_from_ms(5000), Duration::from_millis(5000));
    }
}
