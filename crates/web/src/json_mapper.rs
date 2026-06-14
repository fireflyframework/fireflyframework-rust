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

//! [`ObjectMapper`] — a configurable JSON (de)serialization facade, the Rust
//! analog of Jackson's `ObjectMapper`.
//!
//! `serde` already does the typed encode/decode at compile time; what Jackson
//! adds — and what this supplies — is **runtime, global** policy applied over
//! the whole document without per-field attributes:
//!
//! * a [`PropertyNaming`] strategy (Jackson's `PropertyNamingStrategy`) that
//!   rewrites object keys on the wire, so a `snake_case` Rust struct can speak
//!   `camelCase` JSON without a `#[serde(rename_all)]` on every type, and
//! * an [`Inclusion`] policy (Jackson's `JsonInclude.Include`) that drops
//!   `null`/empty members, plus optional pretty-printing.
//!
//! On read the key transform runs in reverse (back to the canonical
//! `snake_case` Rust field spelling) so a `camelCase` body deserializes into a
//! plain struct. Wire it into content negotiation with [`MappingJsonConverter`]
//! to make every JSON response/request observe the policy.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use firefly_kernel::FireflyError;

use crate::content_negotiation::MessageConverter;

/// Object-key naming strategy — Jackson's `PropertyNamingStrategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PropertyNaming {
    /// Keys are written exactly as serde produced them (the default — honours
    /// any `#[serde(rename)]` already on the type).
    #[default]
    AsIs,
    /// `lowerCamelCase` (`accountId`) — Jackson's `LOWER_CAMEL_CASE`.
    CamelCase,
    /// `snake_case` (`account_id`) — Jackson's `SNAKE_CASE`.
    SnakeCase,
    /// `kebab-case` (`account-id`) — Jackson's `KEBAB_CASE`.
    KebabCase,
    /// `UpperCamelCase` (`AccountId`) — Jackson's `UPPER_CAMEL_CASE`.
    PascalCase,
    /// `SCREAMING_SNAKE_CASE` (`ACCOUNT_ID`).
    ScreamingSnakeCase,
}

/// Which properties to emit when serializing — Jackson's `JsonInclude.Include`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Inclusion {
    /// Emit every property (the default).
    #[default]
    Always,
    /// Omit properties whose value is `null` — Jackson's `NON_NULL`.
    NonNull,
    /// Omit `null`, `""`, `[]`, and `{}` — Jackson's `NON_EMPTY`.
    NonEmpty,
}

/// A configurable JSON mapper — the Rust analog of Jackson's `ObjectMapper`.
///
/// ```
/// use firefly_web::{Inclusion, ObjectMapper, PropertyNaming};
/// use serde::Serialize;
///
/// #[derive(Serialize)]
/// struct Account { account_id: u64, nickname: Option<String> }
///
/// let mapper = ObjectMapper::new()
///     .naming(PropertyNaming::CamelCase)
///     .inclusion(Inclusion::NonNull);
/// let json = mapper.to_string(&Account { account_id: 7, nickname: None }).unwrap();
/// assert_eq!(json, r#"{"accountId":7}"#);
/// ```
///
/// # Scope and limitations
///
/// A non-`AsIs` naming strategy rewrites **every** object key in the document,
/// including the keys of a free-form `Map`/`HashMap` that is *data* rather than
/// a struct's fields — the transform works on the JSON tree and has no type
/// information to tell the two apart. Use a renaming mapper on **DTO-shaped**
/// payloads; for a type whose payload carries arbitrary string-keyed data,
/// leave the global naming at the default [`PropertyNaming::AsIs`] and express
/// per-type naming with `#[serde(rename_all = "camelCase")]` instead — that is
/// type-aware and lossless. The default mapper (`AsIs`, `Always`) is a no-op
/// and always safe.
#[derive(Debug, Clone, Default)]
pub struct ObjectMapper {
    naming: PropertyNaming,
    inclusion: Inclusion,
    pretty: bool,
}

impl ObjectMapper {
    /// A mapper with default policy: keys as-is, include everything, compact.
    /// Equivalent to bare `serde_json`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the property naming strategy.
    pub fn naming(mut self, naming: PropertyNaming) -> Self {
        self.naming = naming;
        self
    }

    /// Sets the inclusion policy.
    pub fn inclusion(mut self, inclusion: Inclusion) -> Self {
        self.inclusion = inclusion;
        self
    }

    /// Enables pretty (indented) output — Jackson's
    /// `enable(SerializationFeature.INDENT_OUTPUT)`.
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }

    /// Serializes `value` to a JSON string under the configured policy.
    pub fn to_string<T: Serialize>(&self, value: &T) -> Result<String, FireflyError> {
        let raw = serde_json::to_value(value)
            .map_err(|e| FireflyError::internal(format!("JSON serialization failed: {e}")))?;
        let shaped = self.apply_write(raw);
        let out = if self.pretty {
            serde_json::to_string_pretty(&shaped)
        } else {
            serde_json::to_string(&shaped)
        };
        out.map_err(|e| FireflyError::internal(format!("JSON serialization failed: {e}")))
    }

    /// Serializes `value` to a policy-shaped [`Value`] (no string encoding).
    pub fn to_value<T: Serialize>(&self, value: &T) -> Result<Value, FireflyError> {
        let raw = serde_json::to_value(value)
            .map_err(|e| FireflyError::internal(format!("JSON serialization failed: {e}")))?;
        Ok(self.apply_write(raw))
    }

    /// Deserializes a JSON string into `T`, normalizing wire keys back to the
    /// canonical `snake_case` Rust spelling first.
    pub fn from_str<T: DeserializeOwned>(&self, json: &str) -> Result<T, FireflyError> {
        let raw: Value = serde_json::from_str(json)
            .map_err(|e| FireflyError::bad_request(format!("invalid JSON: {e}")))?;
        self.from_value(raw)
    }

    /// Deserializes a [`Value`] into `T`, normalizing wire keys first.
    pub fn from_value<T: DeserializeOwned>(&self, value: Value) -> Result<T, FireflyError> {
        let normalized = self.apply_read(value);
        serde_json::from_value(normalized)
            .map_err(|e| FireflyError::bad_request(format!("JSON does not match target type: {e}")))
    }

    /// Applies the write-side transform to a [`Value`]: rename keys per the
    /// naming strategy and drop members per the inclusion policy.
    pub fn apply_write(&self, value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::with_capacity(map.len());
                for (key, val) in map {
                    if self.omit(&val) {
                        continue;
                    }
                    out.insert(self.rename_write(&key), self.apply_write(val));
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.apply_write(v)).collect())
            }
            other => other,
        }
    }

    /// Applies the read-side transform: normalize keys back to `snake_case`.
    pub fn apply_read(&self, value: Value) -> Value {
        if self.naming == PropertyNaming::AsIs {
            return value;
        }
        match value {
            Value::Object(map) => {
                let mut out = Map::with_capacity(map.len());
                for (key, val) in map {
                    out.insert(
                        join_words(&split_words(&key), PropertyNaming::SnakeCase),
                        self.apply_read(val),
                    );
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.apply_read(v)).collect())
            }
            other => other,
        }
    }

    fn rename_write(&self, key: &str) -> String {
        match self.naming {
            PropertyNaming::AsIs => key.to_string(),
            other => join_words(&split_words(key), other),
        }
    }

    fn omit(&self, value: &Value) -> bool {
        match self.inclusion {
            Inclusion::Always => false,
            Inclusion::NonNull => value.is_null(),
            Inclusion::NonEmpty => match value {
                Value::Null => true,
                Value::String(s) => s.is_empty(),
                Value::Array(a) => a.is_empty(),
                Value::Object(o) => o.is_empty(),
                _ => false,
            },
        }
    }
}

/// Splits an identifier into lowercase word tokens, recognising `_`, `-`,
/// spaces, lower→upper camel-case boundaries, and **letter↔digit boundaries**
/// (so `accountId`, `account_id`, and `account-id` all tokenise to
/// `["account", "id"]`, and `line_2`, `line2`, and `oauth2Token` tokenise to
/// `["line", "2"]` / `["oauth", "2", "token"]`).
///
/// Treating a digit run as its own token is what makes the snake↔camel
/// transform **reversible** for the common digit-suffixed field (`line_2`
/// ⇄ `line2`): both sides tokenise to `["line", "2"]`. The one residual
/// ambiguity is a field spelled `line2` *without* an underscore, which a
/// round trip normalises to `line_2`; prefer the underscored form in structs
/// that travel through a renaming [`ObjectMapper`].
fn split_words(s: &str) -> Vec<String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Lower,
        Upper,
        Digit,
        None,
    }
    let mut words = Vec::new();
    let mut current = String::new();
    let mut prev = Class::None;
    for ch in s.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev = Class::None;
            continue;
        }
        let class = if ch.is_ascii_digit() {
            Class::Digit
        } else if ch.is_uppercase() {
            Class::Upper
        } else {
            Class::Lower
        };
        // Start a new word at a camel-case hump (aB) or any letter↔digit edge.
        let boundary = matches!(
            (prev, class),
            (Class::Lower, Class::Upper)
                | (Class::Lower, Class::Digit)
                | (Class::Upper, Class::Digit)
                | (Class::Digit, Class::Lower)
                | (Class::Digit, Class::Upper)
        );
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(ch.to_ascii_lowercase());
        prev = class;
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Joins lowercase word tokens into `strategy`'s spelling.
fn join_words(words: &[String], strategy: PropertyNaming) -> String {
    let cap = |w: &str| {
        let mut c = w.chars();
        match c.next() {
            Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
            None => String::new(),
        }
    };
    match strategy {
        PropertyNaming::AsIs => words.join("_"),
        PropertyNaming::SnakeCase => words.join("_"),
        PropertyNaming::KebabCase => words.join("-"),
        PropertyNaming::ScreamingSnakeCase => words.join("_").to_uppercase(),
        PropertyNaming::PascalCase => words.iter().map(|w| cap(w)).collect(),
        PropertyNaming::CamelCase => words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.clone() } else { cap(w) })
            .collect(),
    }
}

/// A [`MessageConverter`] for `application/json` that applies an
/// [`ObjectMapper`]'s naming + inclusion + pretty policy — register it on the
/// [`MessageConverterRegistry`](crate::MessageConverterRegistry) to make every
/// negotiated JSON response/request observe the global policy (Jackson's
/// auto-configured `ObjectMapper` wired into the message converter).
#[derive(Debug, Clone)]
pub struct MappingJsonConverter {
    mapper: ObjectMapper,
}

impl MappingJsonConverter {
    /// Wraps `mapper` as a JSON message converter.
    pub fn new(mapper: ObjectMapper) -> Self {
        Self { mapper }
    }
}

impl MessageConverter for MappingJsonConverter {
    fn media_types(&self) -> &[&str] {
        &["application/json"]
    }

    fn read(&self, body: &[u8]) -> Result<Value, FireflyError> {
        if body.is_empty() {
            return Ok(Value::Null);
        }
        let raw: Value = serde_json::from_slice(body)
            .map_err(|e| FireflyError::bad_request(format!("invalid JSON: {e}")))?;
        Ok(self.mapper.apply_read(raw))
    }

    fn write(&self, value: &Value) -> Result<(Vec<u8>, String), FireflyError> {
        let shaped = self.mapper.apply_write(value.clone());
        let body = if self.mapper.pretty {
            serde_json::to_vec_pretty(&shaped)
        } else {
            serde_json::to_vec(&shaped)
        }
        .map_err(|e| FireflyError::internal(format!("JSON serialization failed: {e}")))?;
        Ok((body, "application/json".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Account {
        account_id: u64,
        display_name: String,
        nickname: Option<String>,
        tags: Vec<String>,
    }

    #[test]
    fn camel_case_round_trips_a_snake_case_struct() {
        let mapper = ObjectMapper::new().naming(PropertyNaming::CamelCase);
        let acct = Account {
            account_id: 7,
            display_name: "Ada".into(),
            nickname: Some("ace".into()),
            tags: vec!["vip".into()],
        };
        let json = mapper.to_string(&acct).unwrap();
        assert_eq!(
            json,
            r#"{"accountId":7,"displayName":"Ada","nickname":"ace","tags":["vip"]}"#
        );
        // And back: a camelCase body deserializes into the snake_case struct.
        let back: Account = mapper.from_str(&json).unwrap();
        assert_eq!(back, acct);
    }

    #[test]
    fn inclusion_non_null_omits_null_members() {
        let mapper = ObjectMapper::new()
            .naming(PropertyNaming::CamelCase)
            .inclusion(Inclusion::NonNull);
        let acct = Account {
            account_id: 1,
            display_name: "Bo".into(),
            nickname: None,
            tags: vec![],
        };
        // `nickname` (null) dropped; empty `tags` array kept (NON_NULL only).
        assert_eq!(
            mapper.to_string(&acct).unwrap(),
            r#"{"accountId":1,"displayName":"Bo","tags":[]}"#
        );
    }

    #[test]
    fn inclusion_non_empty_drops_empty_collections_too() {
        let mapper = ObjectMapper::new().inclusion(Inclusion::NonEmpty);
        let acct = Account {
            account_id: 1,
            display_name: String::new(),
            nickname: None,
            tags: vec![],
        };
        // Empty string, null, and empty array all dropped.
        assert_eq!(mapper.to_string(&acct).unwrap(), r#"{"account_id":1}"#);
    }

    #[test]
    fn naming_strategies_render_each_spelling() {
        let words = split_words("accountId");
        assert_eq!(words, vec!["account", "id"]);
        assert_eq!(join_words(&words, PropertyNaming::CamelCase), "accountId");
        assert_eq!(join_words(&words, PropertyNaming::SnakeCase), "account_id");
        assert_eq!(join_words(&words, PropertyNaming::KebabCase), "account-id");
        assert_eq!(join_words(&words, PropertyNaming::PascalCase), "AccountId");
        assert_eq!(
            join_words(&words, PropertyNaming::ScreamingSnakeCase),
            "ACCOUNT_ID"
        );
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct WithDigits {
        line_2: String,
        oauth2_token: String,
    }

    #[test]
    fn digit_suffixed_fields_round_trip_reversibly() {
        // Letter↔digit boundaries tokenise so the transform is reversible —
        // the server can deserialize its own renamed output (the bug this
        // guards against was a 400 on a round-tripped digit-suffixed field).
        assert_eq!(split_words("line_2"), vec!["line", "2"]);
        assert_eq!(split_words("line2"), vec!["line", "2"]);
        assert_eq!(split_words("oauth2Token"), vec!["oauth", "2", "token"]);

        let mapper = ObjectMapper::new().naming(PropertyNaming::CamelCase);
        let value = WithDigits {
            line_2: "a".into(),
            oauth2_token: "b".into(),
        };
        let json = mapper.to_string(&value).unwrap();
        assert_eq!(json, r#"{"line2":"a","oauth2Token":"b"}"#);
        // The renamed output deserializes back into the snake_case struct.
        let back: WithDigits = mapper.from_str(&json).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn pretty_indents_output() {
        let mapper = ObjectMapper::new().pretty(true);
        let json = mapper.to_string(&serde_json::json!({"a": 1})).unwrap();
        assert!(json.contains('\n'), "pretty output is multi-line: {json}");
    }

    #[test]
    fn mapping_converter_applies_policy_on_write_and_read() {
        let mapper = ObjectMapper::new()
            .naming(PropertyNaming::CamelCase)
            .inclusion(Inclusion::NonNull);
        let conv = MappingJsonConverter::new(mapper);
        // Write: snake keys → camel, null dropped.
        let (body, ct) = conv
            .write(&serde_json::json!({"user_name": "x", "middle_name": null}))
            .unwrap();
        assert_eq!(ct, "application/json");
        assert_eq!(String::from_utf8(body).unwrap(), r#"{"userName":"x"}"#);
        // Read: camel keys → snake.
        let value = conv.read(br#"{"userName":"x"}"#).unwrap();
        assert_eq!(value, serde_json::json!({"user_name": "x"}));
    }
}
