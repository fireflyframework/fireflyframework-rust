//! [`SessionConfig`] + [`SameSite`] — the cookie & timeout configuration
//! for the [`crate::SessionLayer`], the Rust port of pyfly's
//! `SessionFilter` constructor parameters (`cookie_name`, `ttl`, `secure`)
//! plus the cookie attributes its `set_cookie` call hard-codes
//! (`HttpOnly`, `SameSite=Lax`, sliding `Max-Age`).
//!
//! It is a serde-`Deserialize` struct with `#[serde(default)]` so it can be
//! bound straight from `firefly.session.*` configuration (the established
//! workspace pattern, replacing pyfly's `@auto_configuration` /
//! `@conditional_on_property` beans), or built explicitly.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The default session cookie name, matching pyfly's `PYFLY_SESSION`.
pub const DEFAULT_COOKIE_NAME: &str = "PYFLY_SESSION";

/// The default session timeout: 30 minutes, matching pyfly's `ttl=1800`.
pub const DEFAULT_TTL_SECONDS: u64 = 1800;

/// The cookie `SameSite` attribute. `Lax` is the default (matching pyfly's
/// hard-coded `samesite="lax"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SameSite {
    /// `SameSite=Strict`.
    Strict,
    /// `SameSite=Lax` — pyfly's default.
    #[default]
    Lax,
    /// `SameSite=None` (requires `Secure`).
    None,
}

impl SameSite {
    /// The attribute value as it appears in a `Set-Cookie` header
    /// (`Strict` / `Lax` / `None`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        }
    }
}

/// Cookie and timeout configuration for the session layer.
///
/// Mirrors pyfly's `SessionFilter(cookie_name, ttl, secure)` plus the
/// cookie attributes its `set_cookie` hard-codes. Defaults match pyfly:
/// `PYFLY_SESSION`, path `/`, `HttpOnly` on, `SameSite=Lax`, `Secure` off
/// (auto-enabled over HTTPS / `X-Forwarded-Proto`), TTL 1800s.
///
/// `idle_timeout` is the sliding session TTL (the value used for the store
/// TTL and the cookie `Max-Age`, matching pyfly). `absolute_timeout`, when
/// set, caps the *total* lifetime from `_created_at` regardless of activity
/// — a hardening beyond pyfly, off by default so behavior is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct SessionConfig {
    /// The session cookie name. Default `PYFLY_SESSION`.
    pub cookie_name: String,
    /// The cookie `Path` attribute. Default `/`.
    pub path: String,
    /// The cookie `Domain` attribute, omitted when `None`.
    pub domain: Option<String>,
    /// Whether to set the `Secure` attribute unconditionally. Even when
    /// `false`, the layer auto-enables `Secure` for HTTPS requests (scheme
    /// or `X-Forwarded-Proto: https`), matching pyfly. Default `false`.
    pub secure: bool,
    /// Whether to set the `HttpOnly` attribute. Default `true` (pyfly).
    pub http_only: bool,
    /// The `SameSite` attribute. Default [`SameSite::Lax`] (pyfly).
    pub same_site: SameSite,
    /// The idle/sliding timeout in seconds — the store TTL and cookie
    /// `Max-Age`. Default 1800 (pyfly).
    pub idle_timeout_seconds: u64,
    /// The absolute timeout in seconds (total lifetime from creation),
    /// or `None` to disable. Default `None`.
    pub absolute_timeout_seconds: Option<u64>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cookie_name: DEFAULT_COOKIE_NAME.to_string(),
            path: "/".to_string(),
            domain: None,
            secure: false,
            http_only: true,
            same_site: SameSite::Lax,
            idle_timeout_seconds: DEFAULT_TTL_SECONDS,
            absolute_timeout_seconds: None,
        }
    }
}

impl SessionConfig {
    /// The idle/sliding timeout as a [`Duration`].
    #[must_use]
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_seconds)
    }

    /// The absolute timeout as a [`Duration`], or `None` when disabled.
    #[must_use]
    pub fn absolute_timeout(&self) -> Option<Duration> {
        self.absolute_timeout_seconds.map(Duration::from_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_pyfly() {
        let c = SessionConfig::default();
        assert_eq!(c.cookie_name, "PYFLY_SESSION");
        assert_eq!(c.path, "/");
        assert!(!c.secure);
        assert!(c.http_only);
        assert_eq!(c.same_site, SameSite::Lax);
        assert_eq!(c.idle_timeout_seconds, 1800);
        assert_eq!(c.absolute_timeout_seconds, None);
        assert_eq!(c.idle_timeout(), Duration::from_secs(1800));
        assert_eq!(c.absolute_timeout(), None);
    }

    #[test]
    fn same_site_strings() {
        assert_eq!(SameSite::Strict.as_str(), "Strict");
        assert_eq!(SameSite::Lax.as_str(), "Lax");
        assert_eq!(SameSite::None.as_str(), "None");
    }

    #[test]
    fn deserializes_partial_kebab_case() {
        // firefly.session.* binding: only some keys present, rest default.
        let json = r#"{ "cookie-name": "SID", "idle-timeout-seconds": 60, "same-site": "strict" }"#;
        let c: SessionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.cookie_name, "SID");
        assert_eq!(c.idle_timeout_seconds, 60);
        assert_eq!(c.same_site, SameSite::Strict);
        // Untouched keys fall back to defaults.
        assert_eq!(c.path, "/");
        assert!(c.http_only);
    }

    #[test]
    fn deserializes_empty_to_defaults() {
        let c: SessionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, SessionConfig::default());
    }

    #[test]
    fn absolute_timeout_roundtrips() {
        let json = r#"{ "absolute-timeout-seconds": 3600 }"#;
        let c: SessionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.absolute_timeout(), Some(Duration::from_secs(3600)));
    }
}
