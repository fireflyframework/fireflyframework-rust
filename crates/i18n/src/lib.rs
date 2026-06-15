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
//! # pyfly parity surface
//!
//! On top of the Go-parity surface above, the crate ports pyfly's
//! `i18n` package so a migrating service finds the same pluggable shape:
//!
//! - **[`MessageSource`] port** — a pluggable resolution trait
//!   (`get_message` / `get_message_or_default`) so consumers depend on an
//!   abstraction rather than the concrete [`Bundle`]; a miss is a typed
//!   [`MessageNotFound`] (pyfly's `KeyError`).
//! - **Positional `{0}`/`{1}` MessageFormat** — [`format_message`] and
//!   [`Bundle::tn`] substitute positional arguments with
//!   `java.text.MessageFormat` quote semantics, alongside the existing
//!   named `{name}` substitution.
//! - **File-convention loader** — [`Bundle::load_dir`] reads
//!   `messages_{locale}.{yaml,yml,json}` from a base directory and
//!   flattens nested keys with dots.
//! - **[`LocaleResolver`] port** — [`FixedLocaleResolver`] (always one
//!   locale) and [`AcceptHeaderLocaleResolver`] (highest-quality
//!   `Accept-Language` tag, reduced to its language root).
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
pub const VERSION: &str = "26.6.10";

/// Errors produced when bulk-loading messages from serialized maps.
#[derive(Debug, Error)]
pub enum I18nError {
    /// The JSON source was not a valid `key → template` string map.
    #[error("invalid JSON message map: {0}")]
    Json(#[from] serde_json::Error),
    /// The YAML source was not a valid `key → template` string map.
    #[error("invalid YAML message map: {0}")]
    Yaml(#[from] serde_yaml::Error),
    /// A message-resource file under the convention directory could not
    /// be read (raised by [`Bundle::load_dir`]).
    #[error("failed to read message resource {path}: {source}")]
    Io {
        /// The path that could not be read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Raised by [`MessageSource::get_message`] when a code cannot be
/// resolved in the requested locale (nor the fallback) — the Rust analog
/// of pyfly's `MessageSource.get_message` raising `KeyError`.
///
/// Carries the unresolved `code` and the `locale` that was requested, so
/// callers can log or rethrow with context.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("no message found for code '{code}' in locale '{locale}'")]
pub struct MessageNotFound {
    /// The message code that could not be resolved.
    pub code: String,
    /// The locale the lookup was requested for.
    pub locale: String,
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

    /// Loads `locale`'s messages from the on-disk file-naming convention
    /// under `base_path`, flattening nested keys with dots — the Rust port
    /// of pyfly's `ResourceBundleMessageSource` directory loader.
    ///
    /// The first existing file in this preference order is read:
    ///
    /// ```text
    /// {base_path}/messages_{locale}.yaml
    /// {base_path}/messages_{locale}.yml
    /// {base_path}/messages_{locale}.json
    /// ```
    ///
    /// Nested mappings are flattened with `.`, so a YAML structure of
    ///
    /// ```yaml
    /// greeting:
    ///   hello: "Hello, {0}!"
    /// ```
    ///
    /// is stored (and looked up) under the key `greeting.hello`. Scalar
    /// leaf values are stringified, so numbers and booleans become their
    /// textual form. Returns whether a file was found and loaded
    /// (`false` when no `messages_{locale}.{yaml,yml,json}` exists), so a
    /// caller can decide whether the locale is configured. A present-but
    /// unreadable or malformed file surfaces an [`I18nError`].
    pub fn load_dir(
        &self,
        base_path: impl AsRef<std::path::Path>,
        locale: &str,
    ) -> Result<bool, I18nError> {
        let base = base_path.as_ref();
        let yaml = base.join(format!("messages_{locale}.yaml"));
        let yml = base.join(format!("messages_{locale}.yml"));
        let json = base.join(format!("messages_{locale}.json"));

        let (path, is_json) = if yaml.is_file() {
            (yaml, false)
        } else if yml.is_file() {
            (yml, false)
        } else if json.is_file() {
            (json, true)
        } else {
            return Ok(false);
        };

        let content = std::fs::read_to_string(&path).map_err(|source| I18nError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let flat = if is_json {
            let value: serde_json::Value = serde_json::from_str(&content)?;
            flatten_json(&value)
        } else {
            let value: serde_yaml::Value = serde_yaml::from_str(&content)?;
            flatten_yaml(&value)
        };
        self.load(locale, flat);
        Ok(true)
    }

    /// Like [`t`](Bundle::t) but substitutes **positional** `{0}`, `{1}`,
    /// … placeholders ([`format_message`]) instead of `{name}` ones — the
    /// `java.text.MessageFormat` form pyfly's `ResourceBundleMessageSource`
    /// uses. Locale resolution (region → language root → fallback) and the
    /// "return the key on a total miss" behaviour are identical to `t`.
    ///
    /// ```
    /// use firefly_i18n::Bundle;
    ///
    /// let b = Bundle::new("en");
    /// b.add("en", "greeting.hello", "Hello, {0}!");
    /// assert_eq!(b.tn("en", "greeting.hello", &["World"]), "Hello, World!");
    /// ```
    pub fn tn(&self, locale: &str, key: &str, args: &[&str]) -> String {
        let messages = self.messages.read().expect("i18n bundle lock poisoned");
        let mut candidates = locale_chain(locale);
        candidates.push(self.fallback.clone());
        for l in &candidates {
            if let Some(tmpl) = messages.get(l).and_then(|msgs| msgs.get(key)) {
                return format_message(tmpl, args);
            }
        }
        key.to_string()
    }

    /// Looks up `key` in `locale` (then its language root, then the
    /// fallback) and returns the raw template, or `None` on a total miss —
    /// the no-substitution, miss-aware primitive the [`MessageSource`]
    /// implementation builds on. Unlike [`t`](Bundle::t), a miss is
    /// signalled by `None` rather than echoing the key back.
    fn lookup(&self, locale: &str, key: &str) -> Option<String> {
        let messages = self.messages.read().expect("i18n bundle lock poisoned");
        let mut candidates = locale_chain(locale);
        candidates.push(self.fallback.clone());
        candidates
            .iter()
            .find_map(|l| messages.get(l).and_then(|msgs| msgs.get(key)).cloned())
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

/// A pluggable message-resolution port — the Rust port of pyfly's
/// `MessageSource` protocol. Decouples consumers from the concrete
/// [`Bundle`] so a database-backed or remote resolver can be substituted
/// behind the same surface.
///
/// Substitution is **positional** (`{0}`, `{1}`, …) via [`format_message`],
/// matching pyfly's `java.text.MessageFormat`-style backend. The default
/// [`Bundle`] implementation resolves through its locale chain (region →
/// language root → fallback locale) exactly like [`Bundle::t`].
///
/// ```
/// use firefly_i18n::{Bundle, MessageSource};
///
/// let b = Bundle::new("en");
/// b.add("en", "greeting.hello", "Hello, {0}!");
/// assert_eq!(b.get_message("greeting.hello", &["World"], "en").unwrap(), "Hello, World!");
/// assert_eq!(b.get_message_or_default("missing", "fallback {0}", &["x"], "en"), "fallback x");
/// assert!(b.get_message("missing", &[], "en").is_err());
/// ```
pub trait MessageSource: Send + Sync {
    /// Resolves `code` for `locale`, substituting positional `args`.
    /// Returns [`MessageNotFound`] when the code resolves in neither the
    /// requested locale nor the fallback — pyfly's `KeyError`.
    fn get_message(
        &self,
        code: &str,
        args: &[&str],
        locale: &str,
    ) -> Result<String, MessageNotFound>;

    /// Resolves `code` for `locale`, returning `default` (with positional
    /// `args` substituted into it) on a miss — pyfly's
    /// `get_message_or_default`.
    fn get_message_or_default(
        &self,
        code: &str,
        default: &str,
        args: &[&str],
        locale: &str,
    ) -> String {
        match self.get_message(code, args, locale) {
            Ok(msg) => msg,
            Err(_) => format_message(default, args),
        }
    }
}

impl MessageSource for Bundle {
    fn get_message(
        &self,
        code: &str,
        args: &[&str],
        locale: &str,
    ) -> Result<String, MessageNotFound> {
        match self.lookup(locale, code) {
            Some(tmpl) => Ok(format_message(&tmpl, args)),
            None => Err(MessageNotFound {
                code: code.to_string(),
                locale: locale.to_string(),
            }),
        }
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

/// Substitutes positional `{0}`, `{1}`, … placeholders in `template`
/// from `args`, honouring `java.text.MessageFormat` quote semantics — the
/// Rust port of pyfly's `ResourceBundleMessageSource._substitute`
/// (audit #187):
///
/// - `''` renders as a single literal quote.
/// - Single-quoted text (`'…'`) is copied verbatim; placeholders inside
///   are **not** substituted, so `'{0}'` renders as `{0}`.
/// - `{n}` and `{n,type,style}` reference `args[n]`; the format
///   type/style after the index is parsed but not locale-applied — only
///   the positional argument is inserted (a documented MessageFormat
///   subset, matching pyfly).
/// - An index with no corresponding argument is left as the literal
///   placeholder (the same lenient behaviour the named [`interpolate`]
///   uses for unknown names).
///
/// ```
/// use firefly_i18n::format_message;
///
/// assert_eq!(format_message("Hello, {0}!", &["World"]), "Hello, World!");
/// assert_eq!(format_message("{0} of {1}", &["3", "5"]), "3 of 5");
/// // Single-quoted text is literal; doubled quotes collapse to one.
/// assert_eq!(format_message("It''s '{0}'", &["x"]), "It's {0}");
/// // An out-of-range index stays a literal placeholder.
/// assert_eq!(format_message("{0} {1}", &["a"]), "a {1}");
/// ```
#[must_use]
pub fn format_message(template: &str, args: &[&str]) -> String {
    let chars: Vec<char> = template.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    let mut in_quote = false;
    while i < n {
        let ch = chars[i];
        if ch == '\'' {
            if i + 1 < n && chars[i + 1] == '\'' {
                out.push('\''); // '' -> literal single quote
                i += 2;
                continue;
            }
            in_quote = !in_quote; // toggle a literal-text section
            i += 1;
            continue;
        }
        if ch == '{' && !in_quote {
            // Find the matching closing brace.
            if let Some(close) = (i + 1..n).find(|&j| chars[j] == '}') {
                let inner: String = chars[i + 1..close].iter().collect();
                // `{n,type,style}` — only the index before the first comma
                // selects the argument.
                let index_part = inner.split(',').next().unwrap_or("").trim();
                match index_part.parse::<usize>() {
                    Ok(idx) if idx < args.len() => out.push_str(args[idx]),
                    // Leave unmatched/out-of-range placeholders literal.
                    _ => out.extend(chars[i..=close].iter()),
                }
                i = close + 1;
                continue;
            }
            // Unterminated `{` — emit literally.
            out.push(ch);
            i += 1;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// Flattens a parsed JSON object into dot-separated `key → string` pairs,
/// stringifying scalar leaves — pyfly's `_flatten`. Non-object roots
/// (an array or a bare scalar) flatten to nothing, since a message bundle
/// is a key/value mapping.
fn flatten_json(value: &serde_json::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    flatten_json_into(value, "", &mut out);
    out
}

fn flatten_json_into(value: &serde_json::Value, prefix: &str, out: &mut HashMap<String, String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = join_key(prefix, k);
                flatten_json_into(v, &key, out);
            }
        }
        // Leaf: record the scalar under the accumulated key.
        leaf if !prefix.is_empty() => {
            out.insert(prefix.to_string(), json_scalar_to_string(leaf));
        }
        _ => {}
    }
}

/// Stringifies a scalar JSON value the way pyfly's `str(value)` does for
/// flattened leaves: strings unquoted, everything else via its JSON form.
fn json_scalar_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Flattens a parsed YAML mapping into dot-separated `key → string`
/// pairs, stringifying scalar leaves — the YAML counterpart of
/// [`flatten_json`].
fn flatten_yaml(value: &serde_yaml::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    flatten_yaml_into(value, "", &mut out);
    out
}

fn flatten_yaml_into(value: &serde_yaml::Value, prefix: &str, out: &mut HashMap<String, String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                // YAML keys are usually strings; non-string keys are
                // rendered through their scalar form so nothing is lost.
                let key_str = yaml_scalar_to_string(k);
                let key = join_key(prefix, &key_str);
                flatten_yaml_into(v, &key, out);
            }
        }
        leaf if !prefix.is_empty() => {
            out.insert(prefix.to_string(), yaml_scalar_to_string(leaf));
        }
        _ => {}
    }
}

/// Stringifies a scalar YAML value for a flattened leaf: strings
/// unquoted, scalars by their natural text, and anything non-scalar
/// (a nested seq/map landing here) by its serialized form.
fn yaml_scalar_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim_end()
            .to_string(),
    }
}

/// Joins a dot path: `""` + `k` → `k`; `prefix` + `k` → `prefix.k`.
fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
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

/// A pluggable strategy for deciding which locale an incoming request
/// should be served in — the Rust port of pyfly's `LocaleResolver`
/// protocol. Object-safe so resolvers compose behind
/// `Arc<dyn LocaleResolver>`.
///
/// Where the [`LocaleLayer`] tower middleware is the request-pipeline
/// integration, a `LocaleResolver` is the underlying decision: given a
/// request's headers, return the locale string. The two built-ins mirror
/// pyfly's: [`FixedLocaleResolver`] always returns a configured locale,
/// and [`AcceptHeaderLocaleResolver`] parses the `Accept-Language` header.
pub trait LocaleResolver: Send + Sync {
    /// Resolves the locale for a request from its `headers`.
    fn resolve(&self, headers: &http::HeaderMap) -> String;
}

/// A [`LocaleResolver`] that always returns the same configured locale,
/// ignoring the request — pyfly's `FixedLocaleResolver`. Useful for
/// single-locale services, tests, and CLI tooling.
///
/// ```
/// use firefly_i18n::{FixedLocaleResolver, LocaleResolver};
///
/// let r = FixedLocaleResolver::new("es");
/// assert_eq!(r.resolve(&http::HeaderMap::new()), "es");
/// ```
#[derive(Debug, Clone)]
pub struct FixedLocaleResolver {
    locale: String,
}

impl FixedLocaleResolver {
    /// Builds a resolver that always returns `locale`.
    pub fn new(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
        }
    }
}

impl Default for FixedLocaleResolver {
    /// The pyfly default locale (`"en"`).
    fn default() -> Self {
        Self::new("en")
    }
}

impl LocaleResolver for FixedLocaleResolver {
    fn resolve(&self, _headers: &http::HeaderMap) -> String {
        self.locale.clone()
    }
}

/// A [`LocaleResolver`] that parses the `Accept-Language` header and
/// returns the **language root** of the highest-quality tag (e.g.
/// `en-US,fr;q=0.8` → `en`) — pyfly's `AcceptHeaderLocaleResolver`.
///
/// When the header is absent, empty, or carries no parseable tag, the
/// configured default locale is returned. Unlike the lower-level
/// [`pick_locale`] (which keeps the full region tag, e.g. `es-mx`), this
/// resolver reduces to the language subtag to match pyfly's
/// `tag.split("-")[0]` behaviour.
///
/// ```
/// use firefly_i18n::{AcceptHeaderLocaleResolver, LocaleResolver};
///
/// let r = AcceptHeaderLocaleResolver::new("en");
/// let mut headers = http::HeaderMap::new();
/// headers.insert(http::header::ACCEPT_LANGUAGE, "es-MX,en;q=0.5".parse().unwrap());
/// assert_eq!(r.resolve(&headers), "es");
/// assert_eq!(r.resolve(&http::HeaderMap::new()), "en");
/// ```
#[derive(Debug, Clone)]
pub struct AcceptHeaderLocaleResolver {
    default_locale: String,
}

impl AcceptHeaderLocaleResolver {
    /// Builds a resolver that falls back to `default_locale` when no tag
    /// can be resolved.
    pub fn new(default_locale: impl Into<String>) -> Self {
        Self {
            default_locale: default_locale.into(),
        }
    }
}

impl Default for AcceptHeaderLocaleResolver {
    /// The pyfly default locale (`"en"`).
    fn default() -> Self {
        Self::new("en")
    }
}

impl LocaleResolver for AcceptHeaderLocaleResolver {
    fn resolve(&self, headers: &http::HeaderMap) -> String {
        let header = headers
            .get(http::header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if header.is_empty() {
            return self.default_locale.clone();
        }
        // `pick_locale` returns the full highest-q tag (lower-cased), or
        // the fallback when nothing parses. Reduce to the language root to
        // match pyfly's `tag.split("-")[0]`.
        let picked = pick_locale(header, &self.default_locale);
        picked.split('-').next().unwrap_or(&picked).to_string()
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
        assert_send_sync::<MessageNotFound>();
        assert_send_sync::<FixedLocaleResolver>();
        assert_send_sync::<AcceptHeaderLocaleResolver>();
    }

    // ---- pyfly parity: positional MessageFormat ----

    #[test]
    fn positional_message_format_substitutes_indices() {
        assert_eq!(format_message("Hello, {0}!", &["World"]), "Hello, World!");
        assert_eq!(format_message("{0} of {1}", &["3", "5"]), "3 of 5");
        // Reused index.
        assert_eq!(format_message("{0}-{0}", &["x"]), "x-x");
    }

    #[test]
    fn positional_message_format_honours_quotes() {
        // '' -> a literal single quote.
        assert_eq!(format_message("It''s", &[]), "It's");
        // Single-quoted text is copied verbatim; placeholders inside are not
        // substituted (pyfly audit #187 / java.text.MessageFormat).
        assert_eq!(format_message("'{0}'", &["x"]), "{0}");
        assert_eq!(format_message("It''s '{0}'", &["x"]), "It's {0}");
    }

    #[test]
    fn positional_message_format_leaves_unmatched_and_typed_forms() {
        // Out-of-range index stays literal.
        assert_eq!(format_message("{0} {1}", &["a"]), "a {1}");
        // `{n,type,style}` — only the index selects the argument.
        assert_eq!(format_message("{0,number,integer}", &["42"]), "42");
        // Non-numeric index is left literal.
        assert_eq!(format_message("{name}", &["a"]), "{name}");
        // Unterminated brace.
        assert_eq!(format_message("a {0", &["x"]), "a {0");
    }

    #[test]
    fn bundle_tn_substitutes_positionally_with_fallback() {
        let b = Bundle::new("en");
        b.add("en", "greeting.hello", "Hello, {0}!");
        b.add("es", "greeting.hello", "¡Hola, {0}!");
        assert_eq!(b.tn("es", "greeting.hello", &["World"]), "¡Hola, World!");
        // Fallback to en.
        assert_eq!(b.tn("fr", "greeting.hello", &["Bob"]), "Hello, Bob!");
        // Total miss echoes the key (like `t`).
        assert_eq!(b.tn("en", "missing", &["x"]), "missing");
    }

    // ---- pyfly parity: MessageSource port ----

    #[test]
    fn message_source_get_message_and_miss() {
        let b = Bundle::new("en");
        b.add("en", "greeting.hello", "Hello, {0}!");
        assert_eq!(
            b.get_message("greeting.hello", &["World"], "en").unwrap(),
            "Hello, World!"
        );
        let err = b.get_message("missing", &[], "fr").unwrap_err();
        assert_eq!(err.code, "missing");
        assert_eq!(err.locale, "fr");
    }

    #[test]
    fn message_source_falls_back_to_default_locale() {
        let b = Bundle::new("en");
        b.add("en", "only.en", "English");
        // Requested es is absent → falls back to the en bundle (pyfly's
        // default-locale fallback inside get_message).
        assert_eq!(b.get_message("only.en", &[], "es").unwrap(), "English");
    }

    #[test]
    fn message_source_get_message_or_default_substitutes_default() {
        let b = Bundle::new("en");
        b.add("en", "present", "Found {0}");
        assert_eq!(
            b.get_message_or_default("present", "Default {0}", &["x"], "en"),
            "Found x"
        );
        // Miss → the default itself is positionally formatted.
        assert_eq!(
            b.get_message_or_default("absent", "Default {0}", &["x"], "en"),
            "Default x"
        );
    }

    // ---- pyfly parity: LocaleResolver ----

    #[test]
    fn fixed_locale_resolver_ignores_headers() {
        let r = FixedLocaleResolver::new("es");
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::ACCEPT_LANGUAGE, "fr".parse().unwrap());
        assert_eq!(r.resolve(&headers), "es");
        assert_eq!(FixedLocaleResolver::default().resolve(&headers), "en");
    }

    #[test]
    fn accept_header_resolver_picks_language_root() {
        let r = AcceptHeaderLocaleResolver::new("en");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ACCEPT_LANGUAGE,
            "es-MX,en;q=0.5".parse().unwrap(),
        );
        // Highest-q tag es-MX reduced to its language root `es`.
        assert_eq!(r.resolve(&headers), "es");
        // No header → default.
        assert_eq!(r.resolve(&http::HeaderMap::new()), "en");
    }

    #[test]
    fn accept_header_resolver_respects_quality() {
        let r = AcceptHeaderLocaleResolver::new("x");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::ACCEPT_LANGUAGE,
            "fr;q=0.7,en-GB;q=0.9".parse().unwrap(),
        );
        assert_eq!(r.resolve(&headers), "en");
    }

    #[test]
    fn resolvers_compose_behind_arc_dyn() {
        let resolvers: Vec<Arc<dyn LocaleResolver>> = vec![
            Arc::new(FixedLocaleResolver::new("de")),
            Arc::new(AcceptHeaderLocaleResolver::new("en")),
        ];
        let headers = http::HeaderMap::new();
        assert_eq!(resolvers[0].resolve(&headers), "de");
        assert_eq!(resolvers[1].resolve(&headers), "en");
    }

    // ---- pyfly parity: file-convention dir loader ----

    #[test]
    fn load_dir_reads_yaml_and_flattens_nested_keys() {
        let dir = std::env::temp_dir().join(format!("ff-i18n-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages_en.yaml");
        std::fs::write(
            &path,
            "greeting:\n  hello: \"Hello, {0}!\"\nsimple: \"Plain\"\ncount: 7\n",
        )
        .unwrap();

        let b = Bundle::new("en");
        assert!(b.load_dir(&dir, "en").unwrap());
        assert_eq!(b.tn("en", "greeting.hello", &["World"]), "Hello, World!");
        assert_eq!(b.t("en", "simple", &[]), "Plain");
        // Scalar leaves are stringified.
        assert_eq!(b.t("en", "count", &[]), "7");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_dir_prefers_yaml_then_yml_then_json_and_reports_missing() {
        let dir = std::env::temp_dir().join(format!("ff-i18n-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Only JSON present.
        let json_path = dir.join("messages_es.json");
        std::fs::write(&json_path, r#"{"greeting": {"hello": "¡Hola, {0}!"}}"#).unwrap();

        let b = Bundle::new("en");
        assert!(b.load_dir(&dir, "es").unwrap());
        assert_eq!(b.tn("es", "greeting.hello", &["Mundo"]), "¡Hola, Mundo!");

        // No file for `fr` → Ok(false), nothing loaded.
        assert!(!b.load_dir(&dir, "fr").unwrap());

        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_dir(&dir);
    }
}
