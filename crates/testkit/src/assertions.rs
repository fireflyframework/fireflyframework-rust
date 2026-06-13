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

//! Event-emission assertions layered on [`SpyBroker`](crate::SpyBroker).
//!
//! The Rust analog of pyfly's `assert_event_published` /
//! `assert_no_events_published`: instead of taking a `list[EventEnvelope]`,
//! these read the events a handler recorded into a [`SpyBroker`] and panic
//! (failing the enclosing `#[test]`) when the expectation is not met.
//!
//! ```
//! use firefly_testkit::{assert_event_published, assert_no_events_published, SpyBroker};
//!
//! let spy = SpyBroker::new();
//! assert_no_events_published(&spy); // nothing recorded yet
//!
//! spy.record("orders", "OrderPlaced", br#"{"id":1,"total":42}"#);
//! let event = assert_event_published(&spy, "OrderPlaced");
//! assert_eq!(event.topic, "orders");
//! ```

use crate::broker::{RecordedEvent, SpyBroker};

/// Asserts that an event of `event_type` was recorded on `spy`, returning the
/// first match.
///
/// The Rust analog of pyfly's `assert_event_published(events, event_type)`. To
/// also assert the payload, use [`assert_event_published_with`].
///
/// # Panics
///
/// Panics (failing the enclosing test) when no recorded event has that type,
/// reporting which event types *were* recorded — mirroring pyfly's error
/// message.
#[must_use = "the matched event is returned so you can make further assertions"]
pub fn assert_event_published(spy: &SpyBroker, event_type: &str) -> RecordedEvent {
    let matching = spy.find_by_type(event_type);
    match matching.into_iter().next() {
        Some(event) => event,
        None => {
            let published: Vec<String> = spy.items().into_iter().map(|e| e.event_type).collect();
            panic!(
                "expected event {event_type:?} to be published; published events: {published:?}"
            );
        }
    }
}

/// Asserts that an event of `event_type` was recorded on `spy` and that its
/// JSON payload contains every key/value pair in `payload_contains`, returning
/// the first match.
///
/// This is the Rust analog of pyfly's
/// `assert_event_published(events, event_type, payload_contains=...)`. The
/// payload is parsed as a JSON object; `payload_contains` is a *subset* check
/// (extra keys in the event payload are ignored), exactly like pyfly.
///
/// # Panics
///
/// Panics (failing the enclosing test) when:
/// - no recorded event has that type (reporting the published types), or
/// - the matched event's payload is not a JSON object, or
/// - an expected key is missing or its value differs.
#[must_use = "the matched event is returned so you can make further assertions"]
pub fn assert_event_published_with(
    spy: &SpyBroker,
    event_type: &str,
    payload_contains: &serde_json::Value,
) -> RecordedEvent {
    let event = assert_event_published(spy, event_type);

    let actual: serde_json::Value = serde_json::from_slice(&event.payload).unwrap_or_else(|err| {
        panic!(
            "event {event_type:?} payload is not valid JSON ({err}); payload bytes: {:?}",
            String::from_utf8_lossy(&event.payload)
        )
    });
    let actual_obj = actual
        .as_object()
        .unwrap_or_else(|| panic!("event {event_type:?} payload is not a JSON object: {actual}"));
    let expected_obj = payload_contains.as_object().unwrap_or_else(|| {
        panic!("payload_contains must be a JSON object, got: {payload_contains}")
    });

    for (key, expected) in expected_obj {
        match actual_obj.get(key) {
            None => panic!("expected key {key:?} in {event_type:?} payload; payload was: {actual}"),
            Some(got) if got != expected => {
                panic!("expected payload[{key:?}] == {expected}, got {got} (event {event_type:?})")
            }
            Some(_) => {}
        }
    }

    event
}

/// Asserts that *no* events were recorded on `spy`.
///
/// The Rust analog of pyfly's `assert_no_events_published(events)`.
///
/// # Panics
///
/// Panics (failing the enclosing test) when any event was recorded, reporting
/// the recorded event types.
pub fn assert_no_events_published(spy: &SpyBroker) {
    if !spy.is_empty() {
        let types: Vec<String> = spy.items().into_iter().map(|e| e.event_type).collect();
        panic!("expected no events to be published; got: {types:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn published_returns_first_match() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", br#"{"id":1}"#);
        spy.record("orders", "OrderPlaced", br#"{"id":2}"#);
        spy.record("orders", "OrderShipped", br#"{"id":1}"#);
        let event = assert_event_published(&spy, "OrderPlaced");
        assert_eq!(event.topic, "orders");
        assert_eq!(event.payload, br#"{"id":1}"#.to_vec());
    }

    #[test]
    #[should_panic(expected = "expected event \"Nope\" to be published")]
    fn published_panics_when_missing_and_lists_published_types() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", b"{}");
        let _ = assert_event_published(&spy, "Nope");
    }

    #[test]
    #[should_panic(expected = "OrderPlaced")]
    fn published_panic_message_includes_published_types() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", b"{}");
        let _ = assert_event_published(&spy, "Nope");
    }

    #[test]
    fn published_with_subset_matches() {
        let spy = SpyBroker::new();
        spy.record(
            "orders",
            "OrderPlaced",
            br#"{"id":1,"total":42,"extra":"x"}"#,
        );
        // subset match: `extra` is ignored, `total` and `id` are checked.
        let event =
            assert_event_published_with(&spy, "OrderPlaced", &json!({ "id": 1, "total": 42 }));
        assert_eq!(event.topic, "orders");
    }

    #[test]
    #[should_panic(expected = "expected key \"missing\"")]
    fn published_with_panics_on_missing_key() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", br#"{"id":1}"#);
        let _ = assert_event_published_with(&spy, "OrderPlaced", &json!({ "missing": true }));
    }

    #[test]
    #[should_panic(expected = "expected payload[\"id\"] == 2, got 1")]
    fn published_with_panics_on_value_mismatch() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", br#"{"id":1}"#);
        let _ = assert_event_published_with(&spy, "OrderPlaced", &json!({ "id": 2 }));
    }

    #[test]
    #[should_panic(expected = "payload is not valid JSON")]
    fn published_with_panics_on_non_json_payload() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", b"{not json");
        let _ = assert_event_published_with(&spy, "OrderPlaced", &json!({ "id": 1 }));
    }

    #[test]
    #[should_panic(expected = "payload is not a JSON object")]
    fn published_with_panics_on_non_object_payload() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", b"[1,2,3]");
        let _ = assert_event_published_with(&spy, "OrderPlaced", &json!({ "id": 1 }));
    }

    #[test]
    fn no_events_passes_on_empty() {
        let spy = SpyBroker::new();
        assert_no_events_published(&spy);
    }

    #[test]
    #[should_panic(expected = "expected no events to be published")]
    fn no_events_panics_when_any_recorded() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", b"{}");
        assert_no_events_published(&spy);
    }

    #[test]
    #[should_panic(expected = "OrderPlaced")]
    fn no_events_panic_message_lists_types() {
        let spy = SpyBroker::new();
        spy.record("orders", "OrderPlaced", b"{}");
        assert_no_events_published(&spy);
    }
}
