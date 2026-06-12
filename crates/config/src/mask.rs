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

//! Sensitive-value masking (Spring Boot `Sanitizer` / pyfly `mask_value`
//! parity).
//!
//! Two heuristics, both ported verbatim from pyfly:
//!
//! - **key-based** — a property whose key names a secret (`password`,
//!   `secret`, `token`, `credential`, `*key`, …) masks its whole value as
//!   [`MASK`];
//! - **value-based** — any other value that looks like a URI gets the
//!   password inside its userinfo redacted
//!   (`postgresql://user:hunter2@host` → `postgresql://user:******@host`).
//!
//! Used by [`Layered::property_sources`](crate::Layered::property_sources)
//! and exposed publicly so actuator-style endpoints can sanitize their own
//! views.

use std::sync::OnceLock;

use regex::Regex;

/// The replacement written over masked values: `"******"`.
pub const MASK: &str = "******";

/// Substrings that mark a property as sensitive — matched case-insensitively
/// against the full dotted key, mirroring Spring Boot's `Sanitizer` defaults.
const SENSITIVE_KEY_PARTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "credential",
    "api-key",
    "apikey",
    "api_key",
    "private-key",
    "private_key",
    "client-secret",
    "client_secret",
];

/// Matches the password in a URI's userinfo, e.g. `scheme://user:PASSWORD@host`.
fn uri_userinfo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?P<scheme>[a-zA-Z][\w+.-]*://[^:/?#@\s]+:)(?P<pwd>[^@/?#\s]+)(?P<at>@)")
            .expect("uri userinfo regex")
    })
}

/// Returns `true` if `key`'s final segment looks like a secret: the leaf is
/// `key` (or ends in `-key` / `_key`), or any segment of the full dotted key
/// contains one of the sensitive substrings (`password`, `secret`, `token`,
/// `credential`, `api-key`, …).
pub fn is_sensitive_key(key: &str) -> bool {
    let full = key.to_lowercase();
    let leaf = full.rsplit('.').next().unwrap_or(full.as_str());
    if leaf == "key" || leaf.ends_with("-key") || leaf.ends_with("_key") {
        return true;
    }
    SENSITIVE_KEY_PARTS.iter().any(|part| full.contains(part))
}

/// Redacts the password embedded in a URI's userinfo (Spring `Sanitizer`
/// parity): `scheme://user:secret@host` becomes `scheme://user:******@host`.
/// Values without a userinfo password are returned unchanged.
pub fn sanitize_uri(value: &str) -> String {
    uri_userinfo_re()
        .replace_all(value, format!("${{scheme}}{MASK}${{at}}"))
        .into_owned()
}

/// Masks a property value for display: fully (as [`MASK`]) when `key` names
/// a secret per [`is_sensitive_key`], otherwise redacting any password
/// embedded in a URI-shaped value via [`sanitize_uri`].
pub fn mask_value(key: &str, value: &str) -> String {
    if is_sensitive_key(key) {
        return MASK.to_string();
    }
    if value.contains("://") {
        return sanitize_uri(value);
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // pyfly: test_masks_sensitive_keys
    #[test]
    fn masks_sensitive_keys() {
        assert_eq!(mask_value("firefly.security.jwt.secret", "abc"), MASK);
        assert_eq!(mask_value("db.password", "hunter2"), MASK);
        assert_eq!(mask_value("api.token", "xyz"), MASK);
        assert_eq!(mask_value("svc.credentials", "creds"), MASK);
        assert_eq!(mask_value("vendor.api-key", "k"), MASK);
        assert_eq!(mask_value("signing.key", "k"), MASK);
        assert_eq!(mask_value("tls.private_key", "k"), MASK);
        assert_eq!(mask_value("oauth.client-secret", "k"), MASK);
    }

    // pyfly: test_does_not_mask_normal_keys
    #[test]
    fn does_not_mask_normal_keys() {
        assert_eq!(mask_value("firefly.web.port", "8080"), "8080");
        assert_eq!(mask_value("app.name", "svc"), "svc");
        // "monkey" ends in "key" but the leaf is not the literal segment
        // `key` nor `-key`/`_key` suffixed.
        assert_eq!(mask_value("app.monkey", "bonobo"), "bonobo");
    }

    // pyfly: test_redacts_password_in_uri_value
    #[test]
    fn redacts_password_in_uri_value() {
        assert_eq!(
            mask_value("firefly.data.url", "postgresql://user:hunter2@localhost/db"),
            "postgresql://user:******@localhost/db"
        );
        // No userinfo password -> unchanged.
        assert_eq!(
            mask_value("firefly.data.url", "sqlite:///firefly.db"),
            "sqlite:///firefly.db"
        );
    }

    #[test]
    fn sensitive_key_is_case_insensitive() {
        assert!(is_sensitive_key("DB.PASSWORD"));
        assert!(is_sensitive_key("Spring.Security.Token"));
        assert!(!is_sensitive_key("web.host"));
    }
}
