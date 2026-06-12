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

//! JSON test helpers that fail the test on error.
//!
//! Rust tests fail on panic, so the Go `t.Fatalf` calls become panics with
//! the same error context — usage inside `#[test]` is behaviorally
//! identical.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// JSON-encodes `v` or fails the test.
///
/// # Panics
///
/// Panics (failing the enclosing test) if `v` cannot be serialized.
pub fn must_encode<T: Serialize + ?Sized>(v: &T) -> Vec<u8> {
    match serde_json::to_vec(v) {
        Ok(b) => b,
        Err(err) => panic!("serde_json::to_vec: {err}"),
    }
}

/// JSON-decodes `data` into a `T` or fails the test.
///
/// # Panics
///
/// Panics (failing the enclosing test) if `data` is not valid JSON for `T`.
pub fn must_decode<T: DeserializeOwned>(data: &[u8]) -> T {
    match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(err) => panic!("serde_json::from_slice: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    // Port of Go TestMustEncodeDecode.
    #[test]
    fn encode_decode_roundtrip() {
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct X {
            a: i32,
        }
        let b = must_encode(&X { a: 7 });
        let out: X = must_decode(&b);
        assert_eq!(out, X { a: 7 }, "roundtrip: {out:?}");
    }

    // Rust-specific: serde_json::Value round-trip and stable field bytes.
    #[test]
    fn encode_produces_exact_json_bytes() {
        let b = must_encode(&serde_json::json!({ "id": 1 }));
        assert_eq!(b, br#"{"id":1}"#.to_vec());
        let v: serde_json::Value = must_decode(&b);
        assert_eq!(v["id"], 1);
    }

    #[test]
    #[should_panic(expected = "serde_json::from_slice")]
    fn decode_invalid_json_fails_the_test() {
        let _: serde_json::Value = must_decode(b"{not json");
    }
}
