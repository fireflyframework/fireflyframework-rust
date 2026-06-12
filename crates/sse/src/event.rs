//! The [`Event`] type and its canonical SSE wire encoding.

use std::fmt::Write as _;

/// One Server-Sent Event.
///
/// The Rust counterpart of the Go port's `sse.Event` struct. All fields
/// are optional in the same sense as Go's zero values: an empty `id` or
/// `event` omits the field, and a `retry` of `0` omits the reconnect
/// hint. Construct events with struct-literal update syntax, mirroring
/// Go struct literals:
///
/// ```
/// use firefly_sse::Event;
///
/// let ev = Event {
///     id: "evt-42".into(),
///     event: "order".into(),
///     data: r#"{"id":"o1","customer":"alice"}"#.into(),
///     ..Event::default()
/// };
/// assert_eq!(
///     ev.to_wire(),
///     "id: evt-42\nevent: order\ndata: {\"id\":\"o1\",\"customer\":\"alice\"}\n\n",
/// );
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event {
    /// Optional event id; clients reconnect with `Last-Event-Id` when set.
    pub id: String,
    /// Optional event name (browsers default to `"message"` when empty).
    pub event: String,
    /// Payload — may contain newlines (the writer splits them into
    /// multiple `data:` lines per the spec).
    pub data: String,
    /// Optional millisecond reconnect hint; `0` omits the `retry:` field.
    pub retry: u64,
}

impl Event {
    /// Encodes the event in the canonical SSE wire syntax, byte-for-byte
    /// identical to the Go port's `Writer.Send` output:
    ///
    /// ```text
    /// retry: 5000
    /// id: evt-42
    /// event: order
    /// data: {"id":"o1","customer":"alice"}
    ///
    /// ```
    ///
    /// Fields are emitted in `retry`, `id`, `event`, `data` order; `data`
    /// containing newlines is split into one `data:` line per segment
    /// (an empty payload still emits a single empty `data:` line, exactly
    /// like Go's `strings.Split`), and every event ends with a blank line.
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        if self.retry > 0 {
            let _ = writeln!(out, "retry: {}", self.retry);
        }
        if !self.id.is_empty() {
            let _ = writeln!(out, "id: {}", self.id);
        }
        if !self.event.is_empty() {
            let _ = writeln!(out, "event: {}", self.event);
        }
        for line in self.data.split('\n') {
            let _ = writeln!(out, "data: {line}");
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_frame_emits_all_fields_in_canonical_order() {
        let ev = Event {
            id: "evt-42".into(),
            event: "order".into(),
            data: r#"{"id":"o1","customer":"alice"}"#.into(),
            retry: 5000,
        };
        assert_eq!(
            ev.to_wire(),
            "retry: 5000\nid: evt-42\nevent: order\ndata: {\"id\":\"o1\",\"customer\":\"alice\"}\n\n",
        );
    }

    #[test]
    fn data_only_frame_omits_optional_fields() {
        let ev = Event {
            data: "hello".into(),
            ..Event::default()
        };
        assert_eq!(ev.to_wire(), "data: hello\n\n");
    }

    #[test]
    fn multiline_data_splits_into_multiple_data_lines() {
        let ev = Event {
            data: "line1\nline2".into(),
            ..Event::default()
        };
        assert_eq!(ev.to_wire(), "data: line1\ndata: line2\n\n");
    }

    #[test]
    fn empty_data_still_emits_one_data_line() {
        // Go's strings.Split("", "\n") yields [""], so the Go writer
        // emits a single empty "data: " line. Parity preserved.
        assert_eq!(Event::default().to_wire(), "data: \n\n");
    }

    #[test]
    fn zero_retry_is_omitted_and_positive_retry_leads() {
        let without = Event {
            id: "1".into(),
            data: "x".into(),
            ..Event::default()
        };
        assert_eq!(without.to_wire(), "id: 1\ndata: x\n\n");

        let with = Event {
            id: "1".into(),
            data: "x".into(),
            retry: 1500,
            ..Event::default()
        };
        assert_eq!(with.to_wire(), "retry: 1500\nid: 1\ndata: x\n\n");
    }

    #[test]
    fn json_payload_passes_through_verbatim() {
        let payload = serde_json::json!({"customer": "alice", "id": "o1"}).to_string();
        let ev = Event {
            data: payload.clone(),
            ..Event::default()
        };
        assert_eq!(ev.to_wire(), format!("data: {payload}\n\n"));
    }
}
