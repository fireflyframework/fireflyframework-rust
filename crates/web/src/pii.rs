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

//! PII masking helpers — the Rust port of the Go module's `pii.go`
//! (`MaskPII`, `MaskMap`). Scrubs emails, IBANs, card numbers, and
//! E.164 phone numbers before log lines leave a service.

use std::sync::LazyLock;

use regex::{NoExpand, Regex};
use serde_json::{Map, Value};

/// The canonical PII regexes scrubbed before logs leave a service —
/// the same patterns as the Go/Java/.NET ports. Go's RE2 gives `\b`
/// and `\d` ASCII semantics while the Rust `regex` crate defaults to
/// Unicode, so the IBAN/card/phone patterns disable Unicode mode
/// (`(?-u)`) to keep the exact Go matching behavior: a card or phone
/// number adjacent to a non-ASCII letter (`nº`, `é`, …) is still
/// masked, and non-ASCII digit runs (e.g. Arabic-Indic) are not
/// mistaken for card numbers. Order matters: most specific first.
static PII_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        (
            "email",
            Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email regex"),
        ),
        (
            "iban",
            Regex::new(r"(?-u)\b[A-Z]{2}[0-9]{2}[A-Z0-9]{10,30}\b").expect("iban regex"),
        ),
        (
            "card",
            Regex::new(r"(?-u)\b(?:\d[ -]*?){12,19}\b").expect("card regex"),
        ),
        (
            "phone",
            Regex::new(r"(?-u)\+[1-9][0-9]{6,14}\b").expect("phone regex"),
        ),
    ]
});

/// The well-known sensitive key fragments whose map entries are replaced
/// wholesale by [`mask_map`].
const SENSITIVE_KEYS: [&str; 8] = [
    "password",
    "secret",
    "token",
    "authorization",
    "cookie",
    "api_key",
    "apikey",
    "private_key",
];

/// Replaces PII fragments in `s` with redaction placeholders of the form
/// `[REDACTED:<kind>]` — emails, IBANs, card numbers, and E.164 phone
/// numbers. Use this when emitting structured logs that might contain
/// user-supplied free-form text.
///
/// ```
/// assert_eq!(
///     firefly_web::mask_pii("contact a@b.co"),
///     "contact [REDACTED:email]"
/// );
/// ```
pub fn mask_pii(s: &str) -> String {
    let mut out = s.to_owned();
    for (name, re) in PII_PATTERNS.iter() {
        let placeholder = format!("[REDACTED:{name}]");
        out = re.replace_all(&out, NoExpand(&placeholder)).into_owned();
    }
    out
}

/// Returns a copy of the JSON object `m` with all string values run
/// through [`mask_pii`], nested objects masked recursively, and entries
/// whose key contains one of the well-known sensitive names (`password`,
/// `secret`, `token`, `authorization`, `cookie`, `api_key`, `apikey`,
/// `private_key`) replaced wholesale with `"[REDACTED]"` — the Rust
/// analog of the Go port's `MaskMap`.
///
/// ```
/// let m = serde_json::json!({"password": "hunter2", "note": "a@b.co"});
/// let masked = firefly_web::mask_map(m.as_object().unwrap());
/// assert_eq!(masked["password"], "[REDACTED]");
/// assert_eq!(masked["note"], "[REDACTED:email]");
/// ```
pub fn mask_map(m: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in m {
        if is_sensitive_key(k) {
            out.insert(k.clone(), Value::String("[REDACTED]".to_owned()));
            continue;
        }
        let masked = match v {
            Value::String(s) => Value::String(mask_pii(s)),
            Value::Object(nested) => Value::Object(mask_map(nested)),
            other => other.clone(),
        };
        out.insert(k.clone(), masked);
    }
    out
}

/// Reports whether `k` contains any of the well-known sensitive key
/// fragments, case-insensitively.
fn is_sensitive_key(k: &str) -> bool {
    let lk = k.to_lowercase();
    SENSITIVE_KEYS.iter().any(|needle| lk.contains(needle))
}
