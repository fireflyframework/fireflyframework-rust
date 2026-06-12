//! The canonical [`Event`] envelope.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The canonical envelope every Firefly event flows through.
///
/// Wire-compatible with the Java, .NET, Go, and Python ports — same
/// JSON field names, same omission rules, same header semantics:
///
/// | Rust field       | JSON           | Notes                                       |
/// |------------------|----------------|---------------------------------------------|
/// | `id`             | `id`           | always present                              |
/// | `event_type`     | `type`         | always present                              |
/// | `source`         | `source`       | always present                              |
/// | `topic`          | `topic`        | always present                              |
/// | `correlation_id` | `correlationId`| omitted when empty (Go `omitempty`)         |
/// | `time`           | `time`         | RFC 3339, UTC                               |
/// | `headers`        | `headers`      | omitted when empty (Go `omitempty`)         |
/// | `payload`        | `payload`      | standard base64; `null` when absent (Go `[]byte`) |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unique event id, freshly minted by [`Event::new`].
    pub id: String,
    /// Logical event type, e.g. `OrderCreated`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Logical producer, e.g. the service name.
    pub source: String,
    /// Destination topic the event is published to.
    pub topic: String,
    /// Correlation id propagated from the ambient request scope;
    /// omitted from the wire when empty.
    #[serde(
        rename = "correlationId",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub correlation_id: String,
    /// Event timestamp (UTC).
    pub time: DateTime<Utc>,
    /// Transport headers; omitted from the wire when empty. A sorted
    /// map so the encoding is deterministic, like Go's sorted map keys.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Opaque event body. Serialized as a standard-base64 string —
    /// Go's `[]byte` JSON encoding — and `null` when `None`.
    #[serde(default, with = "base64_bytes")]
    pub payload: Option<Vec<u8>>,
}

impl Event {
    /// Assembles an `Event` with a fresh id, `time` set to now (UTC),
    /// and the correlation id (if any) extracted from the kernel's
    /// task-local correlation scope — the Rust analog of Go's
    /// `NewEvent(ctx, …)` reading `kernel.CorrelationIDFrom(ctx)`.
    pub fn new(
        topic: impl Into<String>,
        event_type: impl Into<String>,
        source: impl Into<String>,
        payload: Option<Vec<u8>>,
    ) -> Self {
        Self {
            id: firefly_kernel::new_correlation_id(),
            event_type: event_type.into(),
            source: source.into(),
            topic: topic.into(),
            correlation_id: firefly_kernel::correlation_id().unwrap_or_default(),
            time: Utc::now(),
            headers: BTreeMap::new(),
            payload,
        }
    }

    /// Sets a transport header and returns the event — a small builder
    /// convenience the Go port spells as direct map assignment.
    #[must_use]
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }
}

/// Serde codec reproducing Go's `[]byte` JSON encoding: a
/// standard-base64 (padded) string, or `null` for a nil slice.
mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        payload: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match payload {
            Some(bytes) => serializer.serialize_str(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        let encoded: Option<String> = Option::deserialize(deserializer)?;
        encoded
            .map(|s| STANDARD.decode(s).map_err(D::Error::custom))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn golden_event() -> Event {
        Event {
            id: "evt-1".into(),
            event_type: "OrderCreated".into(),
            source: "orders-service".into(),
            topic: "orders.created".into(),
            correlation_id: "corr-1".into(),
            time: Utc.with_ymd_and_hms(2026, 6, 12, 10, 30, 0).unwrap(),
            headers: BTreeMap::from([("tenant".to_string(), "t1".to_string())]),
            payload: Some(br#"{"id":"o1"}"#.to_vec()),
        }
    }

    #[test]
    fn json_matches_go_field_for_field() {
        let json = serde_json::to_string(&golden_event()).unwrap();
        // Byte-for-byte the JSON Go's encoding/json emits for the same
        // Event value (struct field order, base64 payload, RFC 3339).
        assert_eq!(
            json,
            r#"{"id":"evt-1","type":"OrderCreated","source":"orders-service","topic":"orders.created","correlationId":"corr-1","time":"2026-06-12T10:30:00Z","headers":{"tenant":"t1"},"payload":"eyJpZCI6Im8xIn0="}"#
        );
    }

    #[test]
    fn empty_optionals_are_omitted_and_nil_payload_is_null() {
        let ev = Event {
            id: "evt-2".into(),
            event_type: "X".into(),
            source: "src".into(),
            topic: "t".into(),
            correlation_id: String::new(),
            time: Utc.with_ymd_and_hms(2026, 6, 12, 10, 30, 0).unwrap(),
            headers: BTreeMap::new(),
            payload: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"id":"evt-2","type":"X","source":"src","topic":"t","time":"2026-06-12T10:30:00Z","payload":null}"#
        );
    }

    #[test]
    fn deserializes_go_produced_json() {
        let go_json = r#"{"id":"evt-1","type":"OrderCreated","source":"orders-service","topic":"orders.created","correlationId":"corr-1","time":"2026-06-12T10:30:00Z","headers":{"tenant":"t1"},"payload":"eyJpZCI6Im8xIn0="}"#;
        let ev: Event = serde_json::from_str(go_json).unwrap();
        assert_eq!(ev, golden_event());
    }

    #[test]
    fn round_trips_with_subsecond_precision() {
        let mut ev = golden_event();
        ev.time = Utc
            .with_ymd_and_hms(2026, 6, 12, 10, 30, 0)
            .unwrap()
            .checked_add_signed(chrono::Duration::nanoseconds(123_456_789))
            .unwrap();
        let json = serde_json::to_string(&ev).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn missing_optional_fields_default() {
        let minimal = r#"{"id":"i","type":"T","source":"s","topic":"t","time":"2026-06-12T10:30:00Z","payload":null}"#;
        let ev: Event = serde_json::from_str(minimal).unwrap();
        assert!(ev.correlation_id.is_empty());
        assert!(ev.headers.is_empty());
        assert!(ev.payload.is_none());
    }

    #[test]
    fn with_header_inserts() {
        let ev = Event::new("t", "T", "s", None).with_header("k", "v");
        assert_eq!(ev.headers.get("k").map(String::as_str), Some("v"));
    }
}
