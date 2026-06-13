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

//! Pluggable event serializers — the codec port a transport uses to turn
//! an [`Event`] into wire bytes and back.
//!
//! This is the Rust port of pyfly's `eda.serializers`
//! (`EventSerializer` protocol, `JsonEventSerializer`,
//! `AvroEventSerializer`, `ProtobufEventSerializer`): a swap point so a
//! migrating service can replace the codec without touching the broker.
//!
//! The default [`JsonEventSerializer`] encodes through the **canonical
//! [`Event`] JSON codec** (`serde_json`), so its bytes are byte-for-byte
//! wire-compatible with the Java, .NET, Go, and Python ports — the same
//! shape the in-memory broker and the transport crates already produce.
//! Selecting it is therefore a zero-behaviour-change default; an existing
//! deployment keeps the identical wire format.
//!
//! [`AvroEventSerializer`] and [`ProtobufEventSerializer`] are the
//! pluggable-but-unimplemented sentinels pyfly ships: constructing them is
//! fine, but `serialize` / `deserialize` return
//! [`EdaError::Serialization`](crate::EdaError::Serialization) until a
//! Schema-Registry / descriptor-backed adapter is wired in. They exist so
//! the *selection point* (e.g. a `serialization-format` config key) has a
//! real target, exactly as in pyfly.
//!
//! ```
//! use firefly_eda::{Event, EventSerializer, JsonEventSerializer};
//!
//! let codec = JsonEventSerializer::new();
//! let ev = Event::new("orders.created", "OrderCreated", "svc", Some(b"{}".to_vec()));
//! let bytes = codec.serialize(&ev).unwrap();
//! let back = codec.deserialize(&bytes).unwrap();
//! assert_eq!(back, ev);
//! assert_eq!(codec.name(), "json");
//! ```

use std::sync::Arc;

use crate::{EdaError, EdaResult, Event};

/// A codec that turns an [`Event`] into transport bytes and back — the
/// Rust port of pyfly's `EventSerializer` protocol.
///
/// Object-safe so a chosen codec can be shared as
/// `Arc<dyn EventSerializer>` and threaded into a transport broker. A
/// transport that adopts this port serializes via `serialize` on publish
/// and reconstructs via `deserialize` on receipt; one that does not keeps
/// using the canonical [`Event`] JSON codec directly (the same bytes
/// [`JsonEventSerializer`] produces).
pub trait EventSerializer: Send + Sync {
    /// A stable identifier for the codec (`"json"`, `"avro"`,
    /// `"protobuf"`) — pyfly's `EventSerializer.name`, also the value a
    /// `serialization-format` config key selects.
    fn name(&self) -> &str;

    /// Encodes `event` to wire bytes.
    fn serialize(&self, event: &Event) -> EdaResult<Vec<u8>>;

    /// Decodes wire bytes back into an [`Event`].
    fn deserialize(&self, data: &[u8]) -> EdaResult<Event>;
}

impl EventSerializer for Arc<dyn EventSerializer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn serialize(&self, event: &Event) -> EdaResult<Vec<u8>> {
        (**self).serialize(event)
    }
    fn deserialize(&self, data: &[u8]) -> EdaResult<Event> {
        (**self).deserialize(data)
    }
}

/// The default, built-in codec: the canonical [`Event`] JSON encoding via
/// `serde_json`. Wire-compatible with every sibling Firefly port, so it is
/// a drop-in for the codec a transport already uses — pyfly's
/// `JsonEventSerializer`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonEventSerializer;

impl JsonEventSerializer {
    /// Constructs the JSON codec.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl EventSerializer for JsonEventSerializer {
    fn name(&self) -> &str {
        "json"
    }

    fn serialize(&self, event: &Event) -> EdaResult<Vec<u8>> {
        serde_json::to_vec(event).map_err(|e| EdaError::Serialization {
            serializer: "json".to_string(),
            message: e.to_string(),
        })
    }

    fn deserialize(&self, data: &[u8]) -> EdaResult<Event> {
        serde_json::from_slice(data).map_err(|e| EdaError::Serialization {
            serializer: "json".to_string(),
            message: e.to_string(),
        })
    }
}

/// A not-yet-implemented Avro codec sentinel — pyfly's
/// `AvroEventSerializer`. Constructing it is fine, but `serialize` /
/// `deserialize` fail with [`EdaError::Serialization`] until a
/// Schema-Registry adapter is supplied. It exists so a
/// `serialization-format = "avro"` selection has a concrete (failing-loud)
/// target rather than silently doing nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct AvroEventSerializer;

impl AvroEventSerializer {
    /// Constructs the Avro sentinel.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn unimplemented() -> EdaError {
        EdaError::Serialization {
            serializer: "avro".to_string(),
            message: "Avro serializer requires a Schema Registry adapter".to_string(),
        }
    }
}

impl EventSerializer for AvroEventSerializer {
    fn name(&self) -> &str {
        "avro"
    }

    fn serialize(&self, _event: &Event) -> EdaResult<Vec<u8>> {
        Err(Self::unimplemented())
    }

    fn deserialize(&self, _data: &[u8]) -> EdaResult<Event> {
        Err(Self::unimplemented())
    }
}

/// A not-yet-implemented Protobuf codec sentinel — pyfly's
/// `ProtobufEventSerializer`. Like [`AvroEventSerializer`], it is a
/// failing-loud placeholder for the `serialization-format = "protobuf"`
/// selection until a descriptor-backed adapter is wired in.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProtobufEventSerializer;

impl ProtobufEventSerializer {
    /// Constructs the Protobuf sentinel.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn unimplemented() -> EdaError {
        EdaError::Serialization {
            serializer: "protobuf".to_string(),
            message: "Protobuf serializer requires a registered message type".to_string(),
        }
    }
}

impl EventSerializer for ProtobufEventSerializer {
    fn name(&self) -> &str {
        "protobuf"
    }

    fn serialize(&self, _event: &Event) -> EdaResult<Vec<u8>> {
        Err(Self::unimplemented())
    }

    fn deserialize(&self, _data: &[u8]) -> EdaResult<Event> {
        Err(Self::unimplemented())
    }
}

/// Selects a built-in [`EventSerializer`] by its
/// [`name`](EventSerializer::name) — the Rust port of pyfly's
/// `_make_serializer` reading the `serialization-format` config key.
///
/// Accepts `"json"` (the default), `"avro"`, and `"protobuf"`
/// case-insensitively; an empty string also selects JSON so an unset
/// config key falls back to the wire-compatible default. An unknown name
/// returns [`EdaError::Serialization`] naming the offending value.
///
/// ```
/// use firefly_eda::serializer_for;
///
/// assert_eq!(serializer_for("json").unwrap().name(), "json");
/// assert_eq!(serializer_for("AVRO").unwrap().name(), "avro");
/// assert!(serializer_for("yaml").is_err());
/// ```
pub fn serializer_for(name: &str) -> EdaResult<Arc<dyn EventSerializer>> {
    match name.trim().to_ascii_lowercase().as_str() {
        "" | "json" => Ok(Arc::new(JsonEventSerializer::new())),
        "avro" => Ok(Arc::new(AvroEventSerializer::new())),
        "protobuf" | "proto" => Ok(Arc::new(ProtobufEventSerializer::new())),
        other => Err(EdaError::Serialization {
            serializer: other.to_string(),
            message: format!(
                "unknown serialization-format '{other}' (expected json|avro|protobuf)"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Event {
        Event::new(
            "orders.created",
            "OrderCreated",
            "svc",
            Some(b"{\"id\":1}".to_vec()),
        )
        .with_header("tenant", "t1")
    }

    #[test]
    fn json_round_trips_an_event() {
        let codec = JsonEventSerializer::new();
        let ev = event();
        let bytes = codec.serialize(&ev).unwrap();
        let back = codec.deserialize(&bytes).unwrap();
        assert_eq!(back, ev);
        assert_eq!(codec.name(), "json");
    }

    #[test]
    fn json_bytes_match_canonical_event_codec() {
        // The JSON codec must produce exactly the canonical Event JSON, so
        // a transport adopting it stays wire-compatible with the in-memory
        // broker and every sibling port.
        let codec = JsonEventSerializer::new();
        let ev = event();
        let via_codec = codec.serialize(&ev).unwrap();
        let canonical = serde_json::to_vec(&ev).unwrap();
        assert_eq!(via_codec, canonical);
    }

    #[test]
    fn json_decode_error_names_the_serializer() {
        let codec = JsonEventSerializer::new();
        let err = codec.deserialize(b"not json").unwrap_err();
        match err {
            EdaError::Serialization { serializer, .. } => assert_eq!(serializer, "json"),
            other => panic!("expected Serialization, got {other:?}"),
        }
    }

    #[test]
    fn avro_and_protobuf_are_failing_loud_sentinels() {
        let avro = AvroEventSerializer::new();
        assert_eq!(avro.name(), "avro");
        assert!(avro.serialize(&event()).is_err());
        assert!(avro.deserialize(b"x").is_err());

        let proto = ProtobufEventSerializer::new();
        assert_eq!(proto.name(), "protobuf");
        assert!(proto.serialize(&event()).is_err());
        assert!(proto.deserialize(b"x").is_err());
    }

    #[test]
    fn serializer_for_selects_by_name() {
        assert_eq!(serializer_for("json").unwrap().name(), "json");
        assert_eq!(serializer_for("").unwrap().name(), "json");
        assert_eq!(serializer_for("  JSON ").unwrap().name(), "json");
        assert_eq!(serializer_for("avro").unwrap().name(), "avro");
        assert_eq!(serializer_for("Protobuf").unwrap().name(), "protobuf");
        assert_eq!(serializer_for("proto").unwrap().name(), "protobuf");
        assert!(serializer_for("yaml").is_err());
    }

    #[test]
    fn arc_dyn_forwards_through_blanket_impl() {
        let codec: Arc<dyn EventSerializer> = Arc::new(JsonEventSerializer::new());
        let ev = event();
        let bytes = codec.serialize(&ev).unwrap();
        assert_eq!(codec.deserialize(&bytes).unwrap(), ev);
        assert_eq!(EventSerializer::name(&codec), "json");
    }
}
