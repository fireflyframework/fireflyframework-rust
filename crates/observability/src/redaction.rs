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

//! PII redaction for the logging subsystem — the Rust port of pyfly's
//! `pyfly.logging.redaction` (regex engine, builtin patterns, mask
//! styles).
//!
//! pyfly ships two engines: a fast regex engine (the cross-port contract)
//! and an optional Presidio NER engine (a Python-only library; pyfly
//! itself falls back to regex when it is absent). This port implements the
//! regex engine: [`RegexRedactor`] with pyfly's 10 builtin entities
//! (`EMAIL`, `CREDIT_CARD` with Luhn validation, `IBAN`, `US_SSN`, `JWT`,
//! `BEARER_TOKEN`, `URL_CREDENTIALS`, `PHONE`, `IPV4`, `IPV6`), the three
//! mask styles ([`MaskStyle`]), extra patterns and allow/deny field lists
//! ([`RedactionConfig`]). The engine is wired into the JSON/text log
//! writers via [`crate::LogConfig::with_redaction`].
//!
//! pyfly intercepts `sys.stdout`/`sys.stderr` for stream redaction; raw
//! `println!` from third-party crates is not interceptable in Rust, so
//! redaction applies at the log-layer writer boundary only.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// The replacement for deny-listed field values — pyfly's `<REDACTED>`.
pub const REDACTED: &str = "<REDACTED>";

/// Masks PII in a string. Implementations must be side-effect free —
/// pyfly's `Redactor` protocol.
pub trait Redactor: Send + Sync {
    /// Returns `text` with every detected entity masked. Borrows the
    /// input unchanged when nothing matches.
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str>;
}

/// True when `value`'s digits pass the Luhn checksum (credit cards) —
/// pyfly's `luhn_valid`. Non-digits are stripped first; digit counts
/// outside 13–19 fail.
pub fn luhn_valid(value: &str) -> bool {
    let digits: Vec<u32> = value.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let parity = digits.len() % 2;
    let mut checksum = 0u32;
    for (i, &d) in digits.iter().enumerate() {
        let mut d = d;
        if i % 2 == parity {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        checksum += d;
    }
    checksum.is_multiple_of(10)
}

/// How a detected token is replaced — pyfly's `mask` setting
/// (`placeholder | partial | hash`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum MaskStyle {
    /// `<ENTITY>` (the default).
    #[default]
    Placeholder,
    /// Stars except the last 4 characters (`************1111`).
    Partial,
    /// `<ENTITY:xxxxxxxx>` — the first 8 hex chars of the SHA-256 of the
    /// token, so equal tokens stay correlatable without exposure.
    Hash,
}

impl MaskStyle {
    /// Maps the pyfly config string: `"partial"` / `"hash"` select those
    /// styles; anything else selects [`MaskStyle::Placeholder`] (pyfly's
    /// fallthrough branch).
    pub fn from_name(name: &str) -> Self {
        match name {
            "partial" => MaskStyle::Partial,
            "hash" => MaskStyle::Hash,
            _ => MaskStyle::Placeholder,
        }
    }
}

fn mask_token(value: &str, entity: &str, style: MaskStyle) -> String {
    match style {
        MaskStyle::Partial => {
            let chars: Vec<char> = value.chars().collect();
            let keep: String = if chars.len() > 4 {
                chars[chars.len() - 4..].iter().collect()
            } else {
                String::new()
            };
            let stars = chars.len().saturating_sub(keep.chars().count());
            format!("{}{keep}", "*".repeat(stars))
        }
        MaskStyle::Hash => {
            let digest = Sha256::digest(value.as_bytes());
            format!("<{entity}:{}>", &hex::encode(digest)[..8])
        }
        MaskStyle::Placeholder => format!("<{entity}>"),
    }
}

/// The builtin entity names, in pyfly's declaration order.
pub const BUILTIN_ENTITIES: [&str; 10] = [
    "EMAIL",
    "CREDIT_CARD",
    "IBAN",
    "US_SSN",
    "JWT",
    "BEARER_TOKEN",
    "URL_CREDENTIALS",
    "PHONE",
    "IPV4",
    "IPV6",
];

struct BuiltinRule {
    entity: &'static str,
    pattern: &'static str,
    /// pyfly's `VALIDATORS` — a match is only redacted when this passes.
    validator: Option<fn(&str) -> bool>,
    /// Emulates pyfly's `(?<!\d)` / `(?!\d)` look-arounds on `PHONE`
    /// (the `regex` crate has no look-around): a match is only redacted
    /// when not directly preceded/followed by a digit.
    digit_boundary: bool,
}

/// Byte-identical to pyfly's `BUILTIN_PATTERNS`, except `PHONE` whose
/// look-arounds are expressed as a digit-boundary check (see
/// [`BuiltinRule::digit_boundary`]).
const BUILTIN_RULES: [BuiltinRule; 10] = [
    BuiltinRule {
        entity: "EMAIL",
        pattern: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "CREDIT_CARD",
        pattern: r"\b\d(?:[ -]?\d){12,18}\b",
        validator: Some(luhn_valid),
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "IBAN",
        pattern: r"\b[A-Z]{2}\d{2}[A-Z0-9]{10,30}\b",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "US_SSN",
        pattern: r"\b\d{3}-\d{2}-\d{4}\b",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "JWT",
        pattern: r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "BEARER_TOKEN",
        pattern: r"(?i)\bbearer\s+[A-Za-z0-9._\-]+",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "URL_CREDENTIALS",
        pattern: r"://[^/\s:@]+:([^/\s:@]+)@",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "PHONE",
        pattern: r"(?:\+?\d{1,3}[ .-]?)?(?:\(\d{2,4}\)[ .-]?)?\d{3}[ .-]?\d{4}",
        validator: None,
        digit_boundary: true,
    },
    BuiltinRule {
        entity: "IPV4",
        pattern: r"\b(?:(?:25[0-5]|2[0-4]\d|[01]?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|[01]?\d?\d)\b",
        validator: None,
        digit_boundary: false,
    },
    BuiltinRule {
        entity: "IPV6",
        pattern: r"\b(?:[A-Fa-f0-9]{1,4}:){2,7}[A-Fa-f0-9]{1,4}\b",
        validator: None,
        digit_boundary: false,
    },
];

fn builtin_regexes() -> &'static [(usize, Regex)] {
    static COMPILED: OnceLock<Vec<(usize, Regex)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        BUILTIN_RULES
            .iter()
            .enumerate()
            .map(|(i, rule)| {
                (
                    i,
                    Regex::new(rule.pattern).expect("builtin redaction pattern must compile"),
                )
            })
            .collect()
    })
}

/// Returns the builtin pattern for `entity`, or `None` for unknown names —
/// useful for asserting parity with pyfly's `BUILTIN_PATTERNS` map.
pub fn builtin_pattern(entity: &str) -> Option<&'static Regex> {
    let idx = BUILTIN_RULES.iter().position(|r| r.entity == entity)?;
    builtin_regexes()
        .iter()
        .find(|(i, _)| *i == idx)
        .map(|(_, re)| re)
}

/// PII redaction settings — pyfly's `RedactionProperties`
/// (`pyfly.logging.redaction.*`). Defaults mirror pyfly: enabled, the
/// 8 default entities (IP addresses are opt-in), placeholder masking and
/// `password`/`token`/`secret` deny-listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionConfig {
    /// Master switch; [`build_redactor`] returns `None` when false.
    pub enabled: bool,
    /// Entity names to detect (builtin names, see [`BUILTIN_ENTITIES`]).
    pub entities: Vec<String>,
    /// How detected tokens are replaced.
    pub mask: MaskStyle,
    /// Extra `(name, regex)` patterns; invalid regexes are ignored with a
    /// warning, exactly like pyfly.
    pub extra_patterns: Vec<(String, String)>,
    /// When non-empty, only these field keys (plus the message) are
    /// scanned.
    pub allow_fields: Vec<String>,
    /// Field keys whose values are replaced wholesale with
    /// [`REDACTED`].
    pub deny_fields: Vec<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            entities: [
                "EMAIL",
                "CREDIT_CARD",
                "IBAN",
                "US_SSN",
                "JWT",
                "BEARER_TOKEN",
                "URL_CREDENTIALS",
                "PHONE",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            mask: MaskStyle::Placeholder,
            extra_patterns: Vec::new(),
            allow_fields: Vec::new(),
            deny_fields: vec![
                "password".to_string(),
                "token".to_string(),
                "secret".to_string(),
            ],
        }
    }
}

impl RedactionConfig {
    /// The canonical defaults (see [`RedactionConfig::default`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the entity list (builder-style).
    #[must_use]
    pub fn with_entities<I, S>(mut self, entities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.entities = entities.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the mask style (builder-style).
    #[must_use]
    pub fn with_mask(mut self, mask: MaskStyle) -> Self {
        self.mask = mask;
        self
    }

    /// Adds an extra `(name, regex)` pattern (builder-style).
    #[must_use]
    pub fn with_extra_pattern(
        mut self,
        name: impl Into<String>,
        pattern: impl Into<String>,
    ) -> Self {
        self.extra_patterns.push((name.into(), pattern.into()));
        self
    }

    /// Replaces the allow-field list (builder-style).
    #[must_use]
    pub fn with_allow_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the deny-field list (builder-style).
    #[must_use]
    pub fn with_deny_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny_fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Disables redaction (builder-style).
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

enum RuleKind {
    Builtin(usize),
    Extra(String, Regex),
}

/// Pattern-based redactor — fast, no external services (pyfly's default
/// and the cross-port contract; the Presidio NER engine is Python-only).
///
/// ```
/// use firefly_observability::{MaskStyle, Redactor, RegexRedactor};
///
/// let r = RegexRedactor::new(&["EMAIL"], MaskStyle::Placeholder, &[]);
/// assert_eq!(r.redact("ping jane@acme.io ok"), "ping <EMAIL> ok");
/// ```
pub struct RegexRedactor {
    rules: Vec<RuleKind>,
    mask: MaskStyle,
}

impl std::fmt::Debug for RegexRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegexRedactor")
            .field("rules", &self.rules.len())
            .field("mask", &self.mask)
            .finish()
    }
}

impl RegexRedactor {
    /// Builds a redactor over the given builtin entity names + extra
    /// `(name, regex)` patterns. Unknown entities and invalid extra
    /// regexes are skipped (pyfly logs a warning and continues — a logging
    /// misconfiguration must never crash the application).
    pub fn new(entities: &[&str], mask: MaskStyle, extra_patterns: &[(String, String)]) -> Self {
        let mut rules = Vec::new();
        for entity in entities {
            if let Some(idx) = BUILTIN_RULES.iter().position(|r| r.entity == *entity) {
                rules.push(RuleKind::Builtin(idx));
            }
        }
        for (name, pattern) in extra_patterns {
            match Regex::new(pattern) {
                Ok(re) => rules.push(RuleKind::Extra(name.clone(), re)),
                Err(_) => {
                    tracing::warn!("Ignoring invalid redaction pattern for {name}");
                }
            }
        }
        Self { rules, mask }
    }

    /// Builds a redactor from a [`RedactionConfig`] (ignoring the
    /// allow/deny lists, which apply per-field in the log layer).
    pub fn from_config(config: &RedactionConfig) -> Self {
        let entities: Vec<&str> = config.entities.iter().map(String::as_str).collect();
        Self::new(&entities, config.mask, &config.extra_patterns)
    }

    fn apply_rule(
        &self,
        text: &str,
        entity: &str,
        re: &Regex,
        rule_idx: Option<usize>,
    ) -> Option<String> {
        let (validator, digit_boundary) = match rule_idx {
            Some(i) => (BUILTIN_RULES[i].validator, BUILTIN_RULES[i].digit_boundary),
            None => (None, false),
        };
        let mut out: Option<String> = None;
        let mut last = 0usize;
        let mut pos = 0usize;
        while pos <= text.len() {
            let Some(m) = re.find_at(text, pos) else {
                break;
            };
            if m.start() == m.end() {
                // Defensive: an empty match would loop forever.
                pos = m.end() + 1;
                continue;
            }
            if digit_boundary {
                let before_digit = text[..m.start()]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit());
                let after_digit = text[m.end()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit());
                if before_digit || after_digit {
                    // Mimic the failed look-around: retry from the next
                    // character, like Python's regex engine would.
                    let step = text[m.start()..].chars().next().map_or(1, char::len_utf8);
                    pos = m.start() + step;
                    continue;
                }
            }
            if let Some(validate) = validator {
                if !validate(m.as_str()) {
                    // pyfly returns the token unchanged and continues
                    // after the match.
                    pos = m.end();
                    continue;
                }
            }
            let buf = out.get_or_insert_with(String::new);
            buf.push_str(&text[last..m.start()]);
            buf.push_str(&mask_token(m.as_str(), entity, self.mask));
            last = m.end();
            pos = m.end();
        }
        if let Some(buf) = out.as_mut() {
            buf.push_str(&text[last..]);
        }
        out
    }
}

impl Redactor for RegexRedactor {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if text.is_empty() || self.rules.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut current = Cow::Borrowed(text);
        for rule in &self.rules {
            let (entity, re, idx) = match rule {
                RuleKind::Builtin(i) => {
                    let re = builtin_regexes()
                        .iter()
                        .find(|(j, _)| j == i)
                        .map(|(_, re)| re)
                        .expect("builtin rule index");
                    (BUILTIN_RULES[*i].entity, re, Some(*i))
                }
                RuleKind::Extra(name, re) => (name.as_str(), re, None),
            };
            if let Some(replaced) = self.apply_rule(&current, entity, re, idx) {
                current = Cow::Owned(replaced);
            }
        }
        current
    }
}

/// Resolves the configured redactor, or `None` when redaction is disabled
/// — pyfly's `build_redactor`. pyfly's `engine: presidio|auto` values fall
/// back to the regex engine when Presidio is unavailable; in Rust the
/// regex engine is the only engine, so this always builds a
/// [`RegexRedactor`] when enabled.
pub fn build_redactor(config: &RedactionConfig) -> Option<RegexRedactor> {
    if !config.enabled {
        return None;
    }
    Some(RegexRedactor::from_config(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pyfly `test_patterns_present`.
    #[test]
    fn patterns_present() {
        for entity in [
            "EMAIL",
            "CREDIT_CARD",
            "IBAN",
            "US_SSN",
            "JWT",
            "BEARER_TOKEN",
            "URL_CREDENTIALS",
            "PHONE",
            "IPV4",
        ] {
            assert!(builtin_pattern(entity).is_some(), "missing {entity}");
        }
        assert!(builtin_pattern("NOPE").is_none());
    }

    /// pyfly `test_email_matches`.
    #[test]
    fn email_matches() {
        assert!(builtin_pattern("EMAIL")
            .unwrap()
            .is_match("contact jane.doe@acme.io now"));
    }

    /// pyfly `test_luhn`.
    #[test]
    fn luhn() {
        assert!(luhn_valid("4111111111111111")); // valid test Visa
        assert!(!luhn_valid("4111111111111112"));
        assert!(luhn_valid("4111 1111 1111 1111"));
    }

    /// pyfly `test_credit_card_validator_rejects_random_16_digits`.
    #[test]
    fn credit_card_validator_rejects_random_16_digits() {
        assert!(!luhn_valid("1234567890123456"));
    }

    /// pyfly `test_placeholder_mask`.
    #[test]
    fn placeholder_mask() {
        let r = RegexRedactor::new(&["EMAIL"], MaskStyle::Placeholder, &[]);
        assert_eq!(r.redact("ping jane@acme.io ok"), "ping <EMAIL> ok");
    }

    /// pyfly `test_partial_mask`.
    #[test]
    fn partial_mask() {
        let r = RegexRedactor::new(&["CREDIT_CARD"], MaskStyle::Partial, &[]);
        let out = r.redact("card 4111 1111 1111 1111 end");
        assert!(out.ends_with("1111 end"), "{out}");
        assert!(out.contains('*'), "{out}");
    }

    #[test]
    fn hash_mask_is_stable_and_tagged() {
        let r = RegexRedactor::new(&["EMAIL"], MaskStyle::Hash, &[]);
        let a = r.redact("jane@acme.io").into_owned();
        let b = r.redact("jane@acme.io").into_owned();
        assert_eq!(a, b);
        assert!(a.starts_with("<EMAIL:"), "{a}");
        assert!(a.ends_with('>'), "{a}");
        assert_eq!(a.len(), "<EMAIL:".len() + 8 + 1);
    }

    /// pyfly `test_extra_patterns`.
    #[test]
    fn extra_patterns() {
        let extra = vec![("EMP".to_string(), r"EMP-\d{4}".to_string())];
        let r = RegexRedactor::new(&[], MaskStyle::Placeholder, &extra);
        assert_eq!(r.redact("id EMP-1234 x"), "id <EMP> x");
    }

    #[test]
    fn invalid_extra_pattern_is_ignored() {
        let extra = vec![("BAD".to_string(), "(".to_string())];
        let r = RegexRedactor::new(&["EMAIL"], MaskStyle::Placeholder, &extra);
        assert_eq!(r.redact("x jane@acme.io"), "x <EMAIL>");
    }

    /// pyfly `test_build_redactor_disabled_returns_none` +
    /// `test_build_redactor_regex_engine`.
    #[test]
    fn build_redactor_respects_enabled() {
        assert!(build_redactor(&RedactionConfig::new().disabled()).is_none());
        assert!(build_redactor(&RedactionConfig::new()).is_some());
    }

    #[test]
    fn credit_card_luhn_gates_redaction() {
        let r = RegexRedactor::new(&["CREDIT_CARD"], MaskStyle::Placeholder, &[]);
        assert_eq!(
            r.redact("pay 4111 1111 1111 1111 now"),
            "pay <CREDIT_CARD> now"
        );
        // Random 16 digits fail Luhn -> untouched.
        assert_eq!(
            r.redact("ref 1234567890123456 now"),
            "ref 1234567890123456 now"
        );
    }

    #[test]
    fn phone_respects_digit_boundaries() {
        let r = RegexRedactor::new(&["PHONE"], MaskStyle::Placeholder, &[]);
        assert_eq!(r.redact("call 555-123-4567 now"), "call <PHONE> now");
        // Embedded in a longer digit run -> the look-around emulation
        // refuses to mask inside it.
        let out = r.redact("id 99912345678999999 x").into_owned();
        assert_eq!(out, "id 99912345678999999 x");
    }

    #[test]
    fn jwt_bearer_and_url_credentials() {
        let r = RegexRedactor::new(
            &["JWT", "BEARER_TOKEN", "URL_CREDENTIALS"],
            MaskStyle::Placeholder,
            &[],
        );
        assert_eq!(
            r.redact("auth eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig done"),
            "auth <JWT> done"
        );
        assert_eq!(
            r.redact("Authorization: Bearer abc.def-123"),
            "Authorization: <BEARER_TOKEN>"
        );
        assert_eq!(
            r.redact("dsn postgres://user:hunter2@db:5432/x"),
            "dsn postgres<URL_CREDENTIALS>db:5432/x"
        );
    }

    #[test]
    fn ssn_iban_and_ips() {
        let r = RegexRedactor::new(&["US_SSN", "IBAN", "IPV4"], MaskStyle::Placeholder, &[]);
        assert_eq!(r.redact("ssn 123-45-6789."), "ssn <US_SSN>.");
        assert_eq!(
            r.redact("iban ES9121000418450200051332 ok"),
            "iban <IBAN> ok"
        );
        assert_eq!(r.redact("from 192.168.0.1 in"), "from <IPV4> in");
    }

    #[test]
    fn mask_style_from_name() {
        assert_eq!(MaskStyle::from_name("partial"), MaskStyle::Partial);
        assert_eq!(MaskStyle::from_name("hash"), MaskStyle::Hash);
        assert_eq!(MaskStyle::from_name("placeholder"), MaskStyle::Placeholder);
        assert_eq!(MaskStyle::from_name("anything"), MaskStyle::Placeholder);
    }

    #[test]
    fn default_config_mirrors_pyfly() {
        let cfg = RedactionConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.entities.len(), 8);
        assert!(!cfg.entities.contains(&"IPV4".to_string()));
        assert_eq!(cfg.mask, MaskStyle::Placeholder);
        assert_eq!(cfg.deny_fields, vec!["password", "token", "secret"]);
    }

    #[test]
    fn untouched_text_borrows() {
        let r = RegexRedactor::new(&["EMAIL"], MaskStyle::Placeholder, &[]);
        assert!(matches!(r.redact("no pii here"), Cow::Borrowed(_)));
    }
}
