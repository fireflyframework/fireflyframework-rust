//! Type-driven binder: decodes a flat dot-keyed map onto any
//! `serde`-deserializable target.
//!
//! This is the Rust analog of the Go port's reflection binder: the target
//! type drives the decoding, so `"9090"` binds onto an integer field,
//! `"alpha,beta"` splits onto a `Vec<String>`, and `"true"` parses onto a
//! `bool`. Missing keys produce zero values (`0`, `""`, `false`, empty
//! vec) exactly like Go's zero-valued struct, so plain `#[derive(Deserialize)]`
//! structs bind without `#[serde(default)]` annotations.

use std::collections::{BTreeSet, HashMap};

use serde::de::value::{StrDeserializer, StringDeserializer};
use serde::de::{DeserializeOwned, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::error::ConfigError;
use crate::source::{merge, Source};

/// Decodes `flat` onto a fresh `T` via `serde`. Nested structs use
/// dot-joined paths; field names (post-`#[serde(rename)]`) are lower-cased
/// before lookup, matching the Go binder's tag-or-field-name rule.
///
/// Supported leaf kinds: `String`, `bool` (Go `strconv.ParseBool` syntax:
/// `1/t/T/true/TRUE/True` and `0/f/F/false/FALSE/False`), all integer
/// widths, `f32`/`f64`, `char`, unit enums (by variant name), `Option<T>`
/// (`None` when no key or section is present), and sequences of scalars
/// (comma-separated, items trimmed). Maps (`HashMap<String, _>`) collect
/// every immediate child segment under their prefix.
///
/// Keys in `flat` are expected lower-case, as produced by
/// [`Layered::map`](crate::Layered::map).
pub fn bind<T: DeserializeOwned>(flat: &HashMap<String, String>) -> Result<T, ConfigError> {
    T::deserialize(FlatDeserializer {
        flat,
        prefix: String::new(),
    })
}

/// The canonical entry point: merges the sources (later wins) and binds
/// the result onto a fresh `T`.
pub fn load<T: DeserializeOwned>(sources: &[Box<dyn Source>]) -> Result<T, ConfigError> {
    let flat = merge(sources)?;
    bind(&flat)
}

/// Joins a lower-cased path segment onto a dotted prefix.
fn join(prefix: &str, segment: &str) -> String {
    let segment = segment.to_lowercase();
    if prefix.is_empty() {
        segment
    } else {
        format!("{prefix}.{segment}")
    }
}

/// Go `strconv.ParseBool` acceptance set.
fn parse_go_bool(raw: &str) -> Option<bool> {
    match raw {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Deserializer positioned at a dotted path inside the flat map.
struct FlatDeserializer<'a> {
    flat: &'a HashMap<String, String>,
    prefix: String,
}

impl FlatDeserializer<'_> {
    /// The leaf view of the current position (raw value, if any).
    fn leaf(&self) -> Leaf<'_> {
        Leaf {
            raw: self.flat.get(&self.prefix).map(String::as_str),
            key: &self.prefix,
        }
    }

    /// Whether any key extends the current prefix (i.e. this is a section).
    fn has_children(&self) -> bool {
        if self.prefix.is_empty() {
            return !self.flat.is_empty();
        }
        let dot = format!("{}.", self.prefix);
        self.flat.keys().any(|k| k.starts_with(&dot))
    }
}

macro_rules! forward_to_leaf {
    ($($method:ident)*) => {
        $(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: Visitor<'de>,
            {
                self.leaf().$method(visitor)
            }
        )*
    };
}

impl<'de> Deserializer<'de> for FlatDeserializer<'_> {
    type Error = ConfigError;

    forward_to_leaf! {
        deserialize_bool
        deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
        deserialize_f32 deserialize_f64
        deserialize_char deserialize_str deserialize_string
        deserialize_bytes deserialize_byte_buf
        deserialize_identifier
    }

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.has_children() {
            self.deserialize_map(visitor)
        } else {
            self.leaf().deserialize_any(visitor)
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.flat.contains_key(&self.prefix) || self.has_children() {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        // Mirrors the Go binder: a present value splits on commas with
        // per-item trimming; a missing key yields an empty sequence.
        let parts: Vec<String> = match self.flat.get(&self.prefix) {
            None => Vec::new(),
            Some(raw) => raw.split(',').map(|p| p.trim().to_string()).collect(),
        };
        visitor.visit_seq(SeqParts {
            parts: parts.into_iter(),
            key: self.prefix,
        })
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let dot = if self.prefix.is_empty() {
            String::new()
        } else {
            format!("{}.", self.prefix)
        };
        let mut segments = BTreeSet::new();
        for key in self.flat.keys() {
            let rest = if dot.is_empty() {
                Some(key.as_str())
            } else {
                key.strip_prefix(dot.as_str())
            };
            if let Some(rest) = rest {
                if let Some(segment) = rest.split('.').next() {
                    if !segment.is_empty() {
                        segments.insert(segment.to_string());
                    }
                }
            }
        }
        visitor.visit_map(FlatMapAccess {
            flat: self.flat,
            prefix: self.prefix,
            segments: segments.into_iter(),
            current: None,
        })
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_map(StructAccess {
            flat: self.flat,
            prefix: self.prefix,
            fields: fields.iter(),
            current: None,
        })
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.leaf().deserialize_enum(name, variants, visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }
}

/// Struct binding: visits every declared field in order, so absent keys
/// still reach the leaf deserializer and produce zero values.
struct StructAccess<'a> {
    flat: &'a HashMap<String, String>,
    prefix: String,
    fields: std::slice::Iter<'static, &'static str>,
    current: Option<&'static str>,
}

impl<'de> MapAccess<'de> for StructAccess<'_> {
    type Error = ConfigError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        match self.fields.next() {
            None => Ok(None),
            Some(field) => {
                self.current = Some(field);
                seed.deserialize(StrDeserializer::<ConfigError>::new(field))
                    .map(Some)
            }
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let field = self
            .current
            .take()
            .expect("next_value_seed called before next_key_seed");
        seed.deserialize(FlatDeserializer {
            flat: self.flat,
            prefix: join(&self.prefix, field),
        })
    }
}

/// Map binding: every immediate child segment under the prefix becomes a
/// key; values recurse with the extended prefix.
struct FlatMapAccess<'a> {
    flat: &'a HashMap<String, String>,
    prefix: String,
    segments: std::collections::btree_set::IntoIter<String>,
    current: Option<String>,
}

impl<'de> MapAccess<'de> for FlatMapAccess<'_> {
    type Error = ConfigError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        match self.segments.next() {
            None => Ok(None),
            Some(segment) => {
                let result =
                    seed.deserialize(StringDeserializer::<ConfigError>::new(segment.clone()));
                self.current = Some(segment);
                result.map(Some)
            }
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let segment = self
            .current
            .take()
            .expect("next_value_seed called before next_key_seed");
        seed.deserialize(FlatDeserializer {
            flat: self.flat,
            prefix: join(&self.prefix, &segment),
        })
    }
}

/// Sequence binding over comma-separated parts.
struct SeqParts {
    parts: std::vec::IntoIter<String>,
    key: String,
}

impl<'de> SeqAccess<'de> for SeqParts {
    type Error = ConfigError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.parts.next() {
            None => Ok(None),
            Some(part) => seed
                .deserialize(Leaf {
                    raw: Some(&part),
                    key: &self.key,
                })
                .map(Some),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.parts.len())
    }
}

/// Scalar deserializer: parses one raw string (or its absence) into the
/// requested leaf kind. Absent values produce Go-style zero values.
struct Leaf<'a> {
    raw: Option<&'a str>,
    key: &'a str,
}

macro_rules! leaf_int {
    ($($method:ident : $ty:ty => $visit:ident)*) => {
        $(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: Visitor<'de>,
            {
                match self.raw {
                    None => visitor.$visit(0),
                    Some(raw) => match raw.parse::<$ty>() {
                        Ok(n) => visitor.$visit(n),
                        Err(err) => Err(ConfigError::bind(self.key, err)),
                    },
                }
            }
        )*
    };
}

macro_rules! leaf_float {
    ($($method:ident)*) => {
        $(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: Visitor<'de>,
            {
                match self.raw {
                    None => visitor.visit_f64(0.0),
                    Some(raw) => match raw.parse::<f64>() {
                        Ok(f) => visitor.visit_f64(f),
                        Err(err) => Err(ConfigError::bind(self.key, err)),
                    },
                }
            }
        )*
    };
}

macro_rules! leaf_unsupported {
    ($($method:ident($($arg:ident: $argty:ty),*))*) => {
        $(
            fn $method<V>(self, $($arg: $argty,)* _visitor: V) -> Result<V::Value, Self::Error>
            where
                V: Visitor<'de>,
            {
                $(let _ = $arg;)*
                Err(ConfigError::bind(
                    self.key,
                    "unsupported nested kind in scalar position",
                ))
            }
        )*
    };
}

impl<'de> Deserializer<'de> for Leaf<'_> {
    type Error = ConfigError;

    leaf_int! {
        deserialize_i8: i8 => visit_i8
        deserialize_i16: i16 => visit_i16
        deserialize_i32: i32 => visit_i32
        deserialize_i64: i64 => visit_i64
        deserialize_i128: i128 => visit_i128
        deserialize_u8: u8 => visit_u8
        deserialize_u16: u16 => visit_u16
        deserialize_u32: u32 => visit_u32
        deserialize_u64: u64 => visit_u64
        deserialize_u128: u128 => visit_u128
    }

    leaf_float! {
        deserialize_f32 deserialize_f64
    }

    leaf_unsupported! {
        deserialize_seq()
        deserialize_map()
        deserialize_tuple(_len: usize)
        deserialize_tuple_struct(_name: &'static str, _len: usize)
        deserialize_struct(_name: &'static str, _fields: &'static [&'static str])
    }

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.raw {
            None => visitor.visit_unit(),
            Some("true") => visitor.visit_bool(true),
            Some("false") => visitor.visit_bool(false),
            Some(raw) => {
                if let Ok(n) = raw.parse::<i64>() {
                    visitor.visit_i64(n)
                } else if let Ok(f) = raw.parse::<f64>() {
                    visitor.visit_f64(f)
                } else {
                    visitor.visit_str(raw)
                }
            }
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.raw {
            None => visitor.visit_bool(false),
            Some(raw) => match parse_go_bool(raw) {
                Some(b) => visitor.visit_bool(b),
                None => Err(ConfigError::bind(self.key, format!("invalid bool {raw:?}"))),
            },
        }
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.raw {
            None => visitor.visit_char('\0'),
            Some(raw) => {
                let mut chars = raw.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => visitor.visit_char(c),
                    _ => Err(ConfigError::bind(self.key, format!("invalid char {raw:?}"))),
                }
            }
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_str(self.raw.unwrap_or(""))
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_bytes(self.raw.unwrap_or("").as_bytes())
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        if self.raw.is_some() {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.raw {
            Some(raw) => visitor.visit_enum(StringDeserializer::<ConfigError>::new(raw.to_owned())),
            None => Err(ConfigError::bind(self.key, "missing value for enum")),
        }
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn flat(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Kinds {
        s: String,
        b: bool,
        i: i32,
        i64f: i64,
        u: u16,
        f: f64,
        f32f: f32,
        list: Vec<String>,
    }

    #[test]
    fn binds_every_leaf_kind() {
        let cfg: Kinds = bind(&flat(&[
            ("s", "hello"),
            ("b", "true"),
            ("i", "-42"),
            ("i64f", "9000000000"),
            ("u", "65535"),
            ("f", "2.5"),
            ("f32f", "0.5"),
            ("list", "a, b ,c"),
        ]))
        .unwrap();
        assert_eq!(
            cfg,
            Kinds {
                s: "hello".to_string(),
                b: true,
                i: -42,
                i64f: 9_000_000_000,
                u: 65_535,
                f: 2.5,
                f32f: 0.5,
                list: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            }
        );
    }

    #[test]
    fn missing_keys_produce_zero_values() {
        let cfg: Kinds = bind(&HashMap::new()).unwrap();
        assert_eq!(cfg, Kinds::default());
        assert!(cfg.list.is_empty());
    }

    #[test]
    fn go_parse_bool_acceptance_set() {
        for raw in ["1", "t", "T", "true", "TRUE", "True"] {
            let cfg: Kinds = bind(&flat(&[("b", raw)])).unwrap();
            assert!(cfg.b, "expected true for {raw:?}");
        }
        for raw in ["0", "f", "F", "false", "FALSE", "False"] {
            let cfg: Kinds = bind(&flat(&[("b", raw)])).unwrap();
            assert!(!cfg.b, "expected false for {raw:?}");
        }
        let err = bind::<Kinds>(&flat(&[("b", "yes")])).unwrap_err();
        assert!(err.to_string().contains("key \"b\""), "got: {err}");
    }

    #[test]
    fn invalid_int_error_mentions_dotted_key() {
        #[derive(Debug, Default, Deserialize)]
        struct Web {
            #[allow(dead_code)]
            port: i32,
        }
        #[derive(Debug, Default, Deserialize)]
        struct Cfg {
            #[allow(dead_code)]
            web: Web,
        }
        let err = bind::<Cfg>(&flat(&[("web.port", "abc")])).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("key \"web.port\""), "got: {text}");
    }

    #[test]
    fn serde_rename_is_lowercased_like_go_tags() {
        #[derive(Debug, Default, Deserialize)]
        struct Cfg {
            #[serde(rename = "Adapter")]
            adapter: String,
        }
        let cfg: Cfg = bind(&flat(&[("adapter", "redis")])).unwrap();
        assert_eq!(cfg.adapter, "redis");
    }

    #[test]
    fn option_fields_are_none_when_absent_some_when_present() {
        #[derive(Debug, Default, Deserialize)]
        struct Inner {
            port: i32,
        }
        #[derive(Debug, Default, Deserialize)]
        struct Cfg {
            host: Option<String>,
            web: Option<Inner>,
        }
        let cfg: Cfg = bind(&HashMap::new()).unwrap();
        assert!(cfg.host.is_none());
        assert!(cfg.web.is_none());

        let cfg: Cfg = bind(&flat(&[("host", "h"), ("web.port", "1")])).unwrap();
        assert_eq!(cfg.host.as_deref(), Some("h"));
        assert_eq!(cfg.web.unwrap().port, 1);
    }

    #[test]
    fn unit_enum_binds_by_variant_name() {
        #[derive(Debug, Deserialize, PartialEq)]
        enum Mode {
            #[serde(rename = "fast")]
            Fast,
            #[serde(rename = "safe")]
            Safe,
        }
        #[derive(Debug, Deserialize)]
        struct Cfg {
            mode: Mode,
        }
        let cfg: Cfg = bind(&flat(&[("mode", "safe")])).unwrap();
        assert_eq!(cfg.mode, Mode::Safe);
        assert!(bind::<Cfg>(&flat(&[("mode", "warp")])).is_err());
    }

    #[test]
    fn string_map_collects_subtree_segments() {
        #[derive(Debug, Default, Deserialize)]
        struct Cfg {
            labels: HashMap<String, String>,
        }
        let cfg: Cfg = bind(&flat(&[
            ("labels.env", "prod"),
            ("labels.region", "eu-west-1"),
            ("other", "x"),
        ]))
        .unwrap();
        assert_eq!(cfg.labels.len(), 2);
        assert_eq!(cfg.labels["env"], "prod");
        assert_eq!(cfg.labels["region"], "eu-west-1");
    }

    #[test]
    fn vec_of_ints_parses_each_part() {
        #[derive(Debug, Default, Deserialize)]
        struct Cfg {
            ports: Vec<u16>,
        }
        let cfg: Cfg = bind(&flat(&[("ports", "80, 443,8080")])).unwrap();
        assert_eq!(cfg.ports, vec![80, 443, 8080]);
    }

    #[test]
    fn present_empty_value_yields_single_empty_element_like_go_split() {
        #[derive(Debug, Default, Deserialize)]
        struct Cfg {
            tags: Vec<String>,
        }
        // Go: strings.Split("", ",") == [""], so a present-but-empty key
        // binds to a one-element slice. Absent keys bind to an empty one.
        let cfg: Cfg = bind(&flat(&[("tags", "")])).unwrap();
        assert_eq!(cfg.tags, vec![String::new()]);
    }

    #[test]
    fn binds_into_serde_json_value_via_deserialize_any() {
        let value: serde_json::Value = bind(&flat(&[
            ("web.port", "8080"),
            ("web.host", "0.0.0.0"),
            ("debug", "true"),
        ]))
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "web": {"port": 8080, "host": "0.0.0.0"},
                "debug": true,
            })
        );
    }
}
