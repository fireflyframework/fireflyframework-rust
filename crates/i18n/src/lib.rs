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

//! firefly-i18n — locale-aware message lookup for the Firefly Framework.
//!
//! `firefly-i18n` provides **locale-aware message lookup** with
//! `{name}`-style placeholder substitution, the Rust port of the Go
//! `i18n` module (Java original: Spring `MessageSource`). The default
//! [`Bundle`] stores `locale → key → template`; a fallback locale is
//! consulted when the requested locale (or its language root, for region
//! tags like `es-MX`) has no entry.
//!
//! [`LocaleLayer`] resolves the locale per request from the
//! `Accept-Language` header (q-value-aware) and stores it on the request
//! extensions, where handlers retrieve it with the [`Locale`] extractor
//! or [`locale_from`].
//!
//! # Quick start
//!
//! ```
//! use firefly_i18n::{Bundle, LocaleLayer, Locale};
//! use axum::{routing::get, Router};
//! use std::sync::Arc;
//!
//! let b = Arc::new(Bundle::new("en"));
//! b.load("en", [("hello", "Hello, {name}!")]);
//! b.load("es", [("hello", "¡Hola, {name}!")]);
//!
//! let layer = LocaleLayer::new(&b);
//! let bundle = Arc::clone(&b);
//! let app: Router = Router::new()
//!     .route(
//!         "/greet",
//!         get(move |Locale(loc): Locale| {
//!             let bundle = Arc::clone(&bundle);
//!             async move { bundle.t(&loc, "hello", &[("name", "alice")]) }
//!         }),
//!     )
//!     .layer(layer);
//!
//! assert_eq!(b.t("es", "hello", &[("name", "alice")]), "¡Hola, alice!");
//! ```
//!
//! `GET /greet` with `Accept-Language: es,en;q=0.5` → `¡Hola, alice!`
//! `GET /greet` with `Accept-Language: fr` → `Hello, alice!` (fallback)

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

use axum::extract::FromRequestParts;
use http::request::Parts;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tower::{Layer, Service};

/// Framework version stamp.
pub const VERSION: &str = "26.6.3";

/// Errors produced when bulk-loading messages from serialized maps.
#[derive(Debug, Error)]
pub enum I18nError {
    /// The JSON source was not a valid `key → template` string map.
    #[error("invalid JSON message map: {0}")]
    Json(#[from] serde_json::Error),
    /// The YAML source was not a valid `key → template` string map.
    #[error("invalid YAML message map: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Bundle is the canonical message store: locale → key → template.
///
/// All methods take `&self`; the store is internally synchronized with an
/// `RwLock`, so a `Bundle` can be shared across tasks behind an [`Arc`]
/// exactly like the Go original behind its `sync.RWMutex`.
#[derive(Debug)]
pub struct Bundle {
    messages: RwLock<HashMap<String, HashMap<String, String>>>,
    fallback: String,
}

impl Bundle {
    /// Returns an empty `Bundle` with the given fallback locale.
    pub fn new(fallback: impl Into<String>) -> Self {
        Self {
            messages: RwLock::new(HashMap::new()),
            fallback: fallback.into(),
        }
    }

    /// The fallback locale this bundle was created with.
    pub fn fallback(&self) -> &str {
        &self.fallback
    }

    /// Adds (or overwrites) a single localised message. Locales are
    /// stored lower-cased so lookups are case-insensitive.
    pub fn add(&self, locale: &str, key: impl Into<String>, template: impl Into<String>) {
        let mut messages = self.messages.write().expect("i18n bundle lock poisoned");
        messages
            .entry(locale.to_lowercase())
            .or_default()
            .insert(key.into(), template.into());
    }

    /// Merges every `(key, template)` entry from `src` into the bundle.
    pub fn load<I, K, V>(&self, locale: &str, src: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in src {
            self.add(locale, k, v);
        }
    }

    /// Merges a JSON object of `key → template` strings into the bundle.
    ///
    /// Rust-specific convenience over [`Bundle::load`] for message files.
    pub fn load_json(&self, locale: &str, json: &str) -> Result<(), I18nError> {
        let map: HashMap<String, String> = serde_json::from_str(json)?;
        self.load(locale, map);
        Ok(())
    }

    /// Merges a YAML mapping of `key → template` strings into the bundle.
    ///
    /// Rust-specific convenience over [`Bundle::load`] for message files.
    pub fn load_yaml(&self, locale: &str, yaml: &str) -> Result<(), I18nError> {
        let map: HashMap<String, String> = serde_yaml::from_str(yaml)?;
        self.load(locale, map);
        Ok(())
    }

    /// T (translate) returns the message for `key` in `locale`, falling
    /// back to the bundle's fallback locale, or returning `key` if
    /// neither matches. `args` is a slice of `{name}` substitutions.
    ///
    /// Region tags consult their language root automatically:
    /// `t("es-MX", ...)` tries `es-mx`, then `es`, then the fallback.
    pub fn t(&self, locale: &str, key: &str, args: &[(&str, &str)]) -> String {
        let messages = self.messages.read().expect("i18n bundle lock poisoned");
        let mut candidates = locale_chain(locale);
        candidates.push(self.fallback.clone());
        for l in &candidates {
            if let Some(tmpl) = messages.get(l).and_then(|msgs| msgs.get(key)) {
                return interpolate(tmpl, args);
            }
        }
        key.to_string()
    }
}

/// Substitutes `{name}` placeholders; unknown placeholders are left
/// verbatim, and templates without braces are returned untouched.
fn interpolate(tmpl: &str, args: &[(&str, &str)]) -> String {
    if !tmpl.contains('{') {
        return tmpl.to_string();
    }
    let mut out = tmpl.to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Lower-cases the locale and appends its language root when it carries
/// a region suffix (`es-mx` → `[es-mx, es]`).
fn locale_chain(loc: &str) -> Vec<String> {
    let loc = loc.to_lowercase();
    let mut out = vec![loc.clone()];
    if let Some(i) = loc.find('-') {
        if i > 0 {
            out.push(loc[..i].to_string());
        }
    }
    out
}

/// The locale resolved for a request, stored on the request extensions
/// by [`LocaleLayer`] — the Rust analogue of the Go module's context
/// value (`WithLocale` / `LocaleFrom`).
///
/// In axum handlers it doubles as an extractor; when no locale was set
/// it extracts as the empty string, mirroring Go's `LocaleFrom`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locale(pub String);

impl Locale {
    /// The locale as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Locale {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for Locale {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Locale {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[axum::async_trait]
impl<S> FromRequestParts<S> for Locale
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<Locale>()
            .cloned()
            .unwrap_or_default())
    }
}

/// Stores `locale` on `extensions` — the analogue of Go's
/// `WithLocale(ctx, locale)`.
pub fn with_locale(extensions: &mut http::Extensions, locale: impl Into<String>) {
    extensions.insert(Locale(locale.into()));
}

/// Retrieves the locale from `extensions`. Returns `""` when absent —
/// the analogue of Go's `LocaleFrom(ctx)`.
pub fn locale_from(extensions: &http::Extensions) -> String {
    extensions
        .get::<Locale>()
        .map(|l| l.0.clone())
        .unwrap_or_default()
}

/// Parses an `Accept-Language` header and returns the highest-quality
/// language tag (lower-cased), falling back if the header is absent or
/// carries no tags. Ordering between equal q-values is stable.
pub fn pick_locale(header: &str, fallback: &str) -> String {
    if header.is_empty() {
        return fallback.to_string();
    }
    let mut tags: Vec<(String, f64)> = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (mut tag, mut q) = (part, 1.0_f64);
        if let Some(i) = part.find(";q=") {
            tag = &part[..i];
            if let Ok(v) = part[i + 3..].parse::<f64>() {
                q = v;
            }
        }
        tags.push((tag.trim().to_lowercase(), q));
    }
    tags.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    match tags.into_iter().next() {
        Some((tag, _)) => tag,
        None => fallback.to_string(),
    }
}

/// Tower layer that extracts the highest-quality locale from the
/// `Accept-Language` header and stores it on the request extensions as
/// [`Locale`]. If no header is present, the bundle's fallback locale is
/// used. The Rust analogue of Go's `LocaleMiddleware`.
#[derive(Debug, Clone)]
pub struct LocaleLayer {
    fallback: Arc<str>,
}

impl LocaleLayer {
    /// Builds a layer that falls back to the bundle's fallback locale.
    pub fn new(bundle: &Bundle) -> Self {
        Self::with_fallback(bundle.fallback())
    }

    /// Builds a layer with an explicit fallback locale.
    pub fn with_fallback(fallback: impl Into<Arc<str>>) -> Self {
        Self {
            fallback: fallback.into(),
        }
    }
}

impl<S> Layer<S> for LocaleLayer {
    type Service = LocaleService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LocaleService {
            inner,
            fallback: Arc::clone(&self.fallback),
        }
    }
}

/// Middleware service produced by [`LocaleLayer`].
#[derive(Debug, Clone)]
pub struct LocaleService<S> {
    inner: S,
    fallback: Arc<str>,
}

impl<S, B> Service<http::Request<B>> for LocaleService<S>
where
    S: Service<http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        let header = req
            .headers()
            .get(http::header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let locale = pick_locale(header, &self.fallback);
        req.extensions_mut().insert(Locale(locale));
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // ---- ports of the Go test suite (i18n_test.go) ----

    #[test]
    fn translate_and_fallback() {
        let b = Bundle::new("en");
        b.load("en", [("hello", "Hello, {name}!")]);
        b.load("es", [("hello", "¡Hola, {name}!")]);

        assert_eq!(b.t("es", "hello", &[("name", "alice")]), "¡Hola, alice!");
        assert_eq!(b.t("fr", "hello", &[("name", "bob")]), "Hello, bob!");
        assert_eq!(b.t("en", "missing", &[]), "missing");
    }

    #[test]
    fn region_falls_back_to_language() {
        let b = Bundle::new("en");
        b.load("en", [("hi", "Hi")]);
        b.load("es", [("hi", "Hola")]);
        assert_eq!(b.t("es-MX", "hi", &[]), "Hola");
    }

    #[test]
    fn pick_locale_cases() {
        assert_eq!(pick_locale("es-MX,en;q=0.5", "en"), "es-mx");
        assert_eq!(pick_locale("", "en"), "en");
        assert_eq!(pick_locale("fr;q=0.7,en;q=0.9", "x"), "en");
    }

    async fn show_locale(Locale(loc): Locale) -> String {
        loc
    }

    async fn body_of(app: Router, req: http::Request<Body>) -> String {
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn locale_middleware() {
        let b = Bundle::new("en");
        let app = Router::new()
            .route("/x", get(show_locale))
            .layer(LocaleLayer::new(&b));

        let req = http::Request::builder()
            .uri("/x")
            .header("Accept-Language", "es,en;q=0.5")
            .body(Body::empty())
            .unwrap();
        assert_eq!(body_of(app.clone(), req).await, "es");

        // Empty header → fallback.
        let req = http::Request::builder()
            .uri("/x")
            .body(Body::empty())
            .unwrap();
        assert_eq!(body_of(app, req).await, "en");
    }

    #[test]
    fn locale_context() {
        let mut ext = http::Extensions::new();
        with_locale(&mut ext, "es");
        assert_eq!(locale_from(&ext), "es");
    }

    // ---- Rust-specific coverage ----

    #[test]
    fn locale_from_absent_is_empty() {
        let ext = http::Extensions::new();
        assert_eq!(locale_from(&ext), "");
    }

    #[tokio::test]
    async fn extractor_defaults_to_empty_without_layer() {
        let app = Router::new().route("/x", get(show_locale));
        let req = http::Request::builder()
            .uri("/x")
            .header("Accept-Language", "es")
            .body(Body::empty())
            .unwrap();
        assert_eq!(body_of(app, req).await, "");
    }

    #[test]
    fn pick_locale_edge_cases() {
        // Malformed q-value keeps the default quality of 1.0.
        assert_eq!(pick_locale("fr;q=0.9,en;q=abc", "x"), "en");
        // Whitespace around tags is trimmed; tags are lower-cased.
        assert_eq!(pick_locale("  es-MX , en;q=0.5 ", "en"), "es-mx");
        // Stable ordering: first tag wins ties.
        assert_eq!(pick_locale("fr;q=0.8,en;q=0.8", "x"), "fr");
        // Only empty parts → fallback.
        assert_eq!(pick_locale(", ,", "en"), "en");
    }

    #[test]
    fn add_overwrites_and_lookup_is_case_insensitive() {
        let b = Bundle::new("en");
        b.add("EN", "hi", "old");
        b.add("en", "hi", "new");
        assert_eq!(b.t("EN", "hi", &[]), "new");
    }

    #[test]
    fn interpolation_leaves_unknown_placeholders() {
        let b = Bundle::new("en");
        b.add("en", "msg", "Hi {name}, meet {other}");
        assert_eq!(
            b.t("en", "msg", &[("name", "alice")]),
            "Hi alice, meet {other}"
        );
        // No braces → template returned untouched even with args.
        b.add("en", "plain", "Plain");
        assert_eq!(b.t("en", "plain", &[("name", "alice")]), "Plain");
    }

    #[test]
    fn load_json_and_yaml() {
        let b = Bundle::new("en");
        b.load_json("en", r#"{"hello": "Hello, {name}!"}"#).unwrap();
        b.load_yaml("es", "hello: \"¡Hola, {name}!\"").unwrap();
        assert_eq!(b.t("en", "hello", &[("name", "a")]), "Hello, a!");
        assert_eq!(b.t("es", "hello", &[("name", "a")]), "¡Hola, a!");

        assert!(matches!(
            b.load_json("en", "[1,2]").unwrap_err(),
            I18nError::Json(_)
        ));
        assert!(matches!(
            b.load_yaml("en", "- 1\n- 2").unwrap_err(),
            I18nError::Yaml(_)
        ));
    }

    #[test]
    fn locale_serde_round_trip() {
        let loc = Locale("es-mx".to_string());
        let json = serde_json::to_string(&loc).unwrap();
        assert_eq!(json, "\"es-mx\"");
        let back: Locale = serde_json::from_str(&json).unwrap();
        assert_eq!(back, loc);
    }

    #[test]
    fn bundle_is_shareable_across_threads() {
        let b = Arc::new(Bundle::new("en"));
        b.add("en", "hi", "Hi {n}");
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let b = Arc::clone(&b);
                std::thread::spawn(move || {
                    let loc = format!("l{i}");
                    b.add(&loc, "k", "v");
                    assert_eq!(b.t("en", "hi", &[("n", "x")]), "Hi x");
                    assert_eq!(b.t(&loc, "k", &[]), "v");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn public_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Bundle>();
        assert_send_sync::<Locale>();
        assert_send_sync::<LocaleLayer>();
        assert_send_sync::<LocaleService<Router>>();
        assert_send_sync::<I18nError>();
    }
}
