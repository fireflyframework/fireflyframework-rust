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

//! firefly-config-server — Spring-Cloud-Config-compatible REST endpoint.
//!
//! `firefly-config-server` exposes a **Spring-Cloud-Config-compatible
//! REST endpoint** serving [`Environment`] payloads keyed by
//! `(application, profile, label)` — the Rust port of the Go
//! `configserver` module (Java original: `firefly-config-server`).
//! Existing Java / .NET SDKs that already speak Spring Cloud Config
//! talk to it without modification.
//!
//! The default [`MemoryStore`] is suitable for tests and development;
//! production deployments back onto a Git repository or a database via
//! the [`Store`] trait.
//!
//! # Wire format
//!
//! `GET /{application}/{profile}[/{label}]` returns:
//!
//! ```json
//! {
//!   "name": "orders",
//!   "profiles": ["prod"],
//!   "label": "main",
//!   "propertySources": [
//!     { "name": "default", "source": { "db.url": "jdbc:postgres://…" } }
//!   ]
//! }
//! ```
//!
//! Field names match Spring Cloud Config exactly; `version` and `state`
//! are omitted when empty, mirroring the Go port's `omitempty` tags.
//! The label defaults to `main` when the third path segment is absent.
//!
//! A missing application/profile is a **soft miss** — the server
//! returns an empty `propertySources` array with the queried name and
//! profile echoed back. This matches Spring Cloud Config's behaviour so
//! SDKs don't break.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//! use firefly_config_server::{router, Environment, MemoryStore, PropertySource};
//!
//! let store = Arc::new(MemoryStore::new());
//! store.put(
//!     "orders",
//!     "prod",
//!     "main",
//!     Environment {
//!         name: "orders".into(),
//!         profiles: vec!["prod".into()],
//!         label: "main".into(),
//!         property_sources: vec![PropertySource {
//!             name: "default".into(),
//!             source: [("db.url".to_string(), "jdbc:postgres://db:5432/orders".into())]
//!                 .into_iter()
//!                 .collect(),
//!         }],
//!         ..Environment::default()
//!     },
//! );
//!
//! let app: axum::Router = router(store);
//! // axum::serve(tokio::net::TcpListener::bind("0.0.0.0:8888").await?, app).await?;
//! ```
//!
//! # Plugging in a Git-backed store
//!
//! Implement the [`Store`] trait — the rest of the framework, including
//! existing Spring Cloud Config clients, doesn't need to change.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod backend;
mod server;

pub use backend::{
    BackendError, ConfigBackend, ConfigSource, FsStore, GitStore, MemoryBackend, Properties,
};
pub use server::ConfigServer;

/// Framework version stamp.
pub const VERSION: &str = "26.6.4";

/// Errors surfaced by a [`Store`] lookup.
///
/// The Go port's `Store` returns a plain `error`; this enum is its
/// typed counterpart. The HTTP handler maps any variant to a
/// `500 Internal Server Error` whose body is the error's `Display`
/// text, exactly as Go's `http.Error` does.
#[derive(Debug, Error)]
pub enum ConfigServerError {
    /// The backing store failed to produce an [`Environment`].
    #[error("{0}")]
    Store(String),
    /// The store does not support the requested write operation.
    ///
    /// Returned by the default [`Store::save`] implementation; a
    /// read-only store leaves it in place, a writable one overrides
    /// `save`.
    #[error("config-server: operation not supported: {0}")]
    Unsupported(String),
}

/// One logical source of properties (file, profile, db row).
///
/// Serializes with the exact Spring Cloud Config field names
/// (`name`, `source`). The `source` map is a
/// [`BTreeMap`](std::collections::BTreeMap), which always serializes its
/// keys in sorted order — byte-for-byte identical to Go's sorted map
/// encoding. A `BTreeMap` (rather than [`serde_json::Map`]) is used
/// deliberately so the sorted-key wire contract holds regardless of
/// whether the `serde_json/preserve_order` feature is enabled anywhere
/// in the workspace dependency graph (e.g. via `bson`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertySource {
    /// Source identifier, e.g. `"default"` or a file path.
    pub name: String,
    /// Flat `property → value` map, encoded with sorted keys.
    pub source: std::collections::BTreeMap<String, serde_json::Value>,
}

/// The wire-shape returned by `/{application}/{profile}`.
///
/// Field names match Spring Cloud Config exactly. `version` and
/// `state` are omitted from the JSON when empty, mirroring the Go
/// struct tags (`json:"version,omitempty"`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    /// Application name.
    pub name: String,
    /// Requested profiles (a single-element list for this endpoint).
    pub profiles: Vec<String>,
    /// Source label (branch / tag); defaults to `main` on the wire.
    pub label: String,
    /// Optional backing-store revision (e.g. a Git commit); omitted when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Optional store state token; omitted when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// Ordered property sources, highest precedence first.
    #[serde(rename = "propertySources")]
    pub property_sources: Vec<PropertySource>,
}

/// The persistence boundary the server queries.
///
/// Implement this trait to back the endpoint with a Git repository or
/// a database; [`MemoryStore`] is the in-process default.
#[async_trait]
pub trait Store: Send + Sync {
    /// Resolves the [`Environment`] for `(app, profile, label)`.
    ///
    /// Unknown coordinates should be a *soft miss*: return an
    /// `Environment` echoing the query with an empty
    /// `property_sources`, not an error.
    async fn lookup(
        &self,
        app: &str,
        profile: &str,
        label: &str,
    ) -> Result<Environment, ConfigServerError>;

    /// Persists `env` under `(app, profile, label)`.
    ///
    /// This is the **optional write path**: the default implementation
    /// returns [`ConfigServerError::Unsupported`], so a read-only store
    /// (the common case, including [`MemoryStore`]'s lookup-only
    /// contract) need not implement it and existing implementations keep
    /// compiling unchanged. A writable store — e.g. one backed by
    /// [`FsStore`] or [`GitStore`] — overrides `save` to commit changes.
    async fn save(
        &self,
        app: &str,
        profile: &str,
        label: &str,
        env: Environment,
    ) -> Result<(), ConfigServerError> {
        let _ = (app, profile, label, env);
        Err(ConfigServerError::Unsupported("save".to_string()))
    }
}

/// The default in-process [`Store`]. Use [`MemoryStore::put`] to seed values.
///
/// Interior mutability (an `RwLock`, like the Go `sync.RWMutex`) lets
/// callers keep seeding through a shared `Arc` after the router is
/// built.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: RwLock<HashMap<String, Environment>>,
}

impl MemoryStore {
    /// Returns an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the store with an [`Environment`] under `(app, profile, label)`.
    pub fn put(&self, app: &str, profile: &str, label: &str, env: Environment) {
        self.entries
            .write()
            .expect("MemoryStore lock poisoned")
            .insert(key(app, profile, label), env);
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn lookup(
        &self,
        app: &str,
        profile: &str,
        label: &str,
    ) -> Result<Environment, ConfigServerError> {
        if let Some(env) = self
            .entries
            .read()
            .expect("MemoryStore lock poisoned")
            .get(&key(app, profile, label))
        {
            return Ok(env.clone());
        }
        // Empty Environment is a soft miss matching Spring Cloud Config behaviour.
        Ok(Environment {
            name: app.to_string(),
            profiles: vec![profile.to_string()],
            label: label.to_string(),
            ..Environment::default()
        })
    }
}

fn key(app: &str, profile: &str, label: &str) -> String {
    format!("{app}|{profile}|{label}")
}

/// Returns an axum [`Router`] serving `/{app}/{profile}[/{label}]`.
///
/// The Rust counterpart of the Go `Handler(store)`. Go's `net/http`
/// hands the handler a percent-decoded `r.URL.Path`, so routing here
/// percent-decodes the raw path first and then splits the *decoded*
/// path — an encoded slash (`%2F`) therefore separates segments,
/// exactly as in Go. Any HTTP method is served, an invalid
/// percent-escape is a `400 Bad Request` (Go's server rejects such
/// request lines before the handler runs), fewer than two segments is
/// a `400` with the same message as the Go handler, the label defaults
/// to `main`, and segments beyond the third are ignored.
pub fn router(store: Arc<dyn Store>) -> Router {
    Router::new().fallback(serve).with_state(store)
}

/// Serves one request: decode the path, parse `(app, profile, label)`
/// from it, look it up, encode the [`Environment`] as JSON.
async fn serve(State(store): State<Arc<dyn Store>>, uri: Uri) -> Response {
    // hyper hands us the raw, still-encoded path; Go's net/http decodes
    // it (and rejects invalid escapes) before the handler ever runs.
    let Some(path) = percent_decode_path(uri.path()) else {
        // Go's pre-handler rejection: the body is "400 Bad Request"
        // with no trailing newline.
        return (StatusCode::BAD_REQUEST, "400 Bad Request").into_response();
    };
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.len() < 2 {
        // Same status + message as the Go handler's http.Error.
        return (
            StatusCode::BAD_REQUEST,
            "expect /{app}/{profile}[/{label}]\n",
        )
            .into_response();
    }
    let (app, profile) = (parts[0], parts[1]);
    let label = if parts.len() >= 3 { parts[2] } else { "main" };
    match store.lookup(app, profile, label).await {
        Ok(env) => match serde_json::to_vec(&env) {
            Ok(mut body) => {
                // Go's json.Encoder terminates the document with '\n'.
                body.push(b'\n');
                ([(header::CONTENT_TYPE, "application/json")], body).into_response()
            }
            Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}\n")).into_response(),
        },
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}\n")).into_response(),
    }
}

/// Decodes `%XX` escapes in the raw request path, mirroring Go's
/// `net/http`, which percent-decodes `r.URL.Path` before the handler
/// runs — so `%2F` becomes a real `/` and splits path segments.
///
/// Returns `None` for an invalid escape: Go's server rejects such
/// request lines with `400 Bad Request` before they reach the handler.
/// Decoded bytes that aren't valid UTF-8 are replaced with U+FFFD
/// (a Go `string` carries raw bytes; a Rust `String` cannot).
fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex_val(*bytes.get(i + 1)?)?;
            let lo = hex_val(*bytes.get(i + 2)?)?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_cargo_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    // Port of Go TestMemoryStoreSoftMiss.
    #[tokio::test]
    async fn memory_store_soft_miss() {
        let s = MemoryStore::new();
        let env = s.lookup("missing", "dev", "main").await.unwrap();
        assert_eq!(env.name, "missing", "soft miss shape: {env:?}");
        assert_eq!(
            env.profiles,
            vec!["dev".to_string()],
            "soft miss shape: {env:?}"
        );
        assert_eq!(env.label, "main");
        assert!(env.property_sources.is_empty());
        assert!(env.version.is_empty());
        assert!(env.state.is_empty());
    }

    #[tokio::test]
    async fn memory_store_put_then_lookup() {
        let s = MemoryStore::new();
        let seeded = Environment {
            name: "orders".into(),
            profiles: vec!["prod".into()],
            label: "main".into(),
            property_sources: vec![PropertySource {
                name: "default".into(),
                source: [("db.url".to_string(), "x".into())].into_iter().collect(),
            }],
            ..Environment::default()
        };
        s.put("orders", "prod", "main", seeded.clone());
        let env = s.lookup("orders", "prod", "main").await.unwrap();
        assert_eq!(env, seeded);
    }

    #[tokio::test]
    async fn memory_store_label_is_part_of_the_key() {
        let s = MemoryStore::new();
        s.put(
            "orders",
            "prod",
            "main",
            Environment {
                name: "orders".into(),
                profiles: vec!["prod".into()],
                label: "main".into(),
                property_sources: vec![PropertySource::default()],
                ..Environment::default()
            },
        );
        // Different label → soft miss, not the seeded value.
        let env = s.lookup("orders", "prod", "develop").await.unwrap();
        assert_eq!(env.label, "develop");
        assert!(env.property_sources.is_empty());
    }

    #[test]
    fn environment_omits_empty_version_and_state() {
        let env = Environment {
            name: "orders".into(),
            profiles: vec!["prod".into()],
            label: "main".into(),
            ..Environment::default()
        };
        let json = serde_json::to_string(&env).unwrap();
        assert_eq!(
            json,
            r#"{"name":"orders","profiles":["prod"],"label":"main","propertySources":[]}"#
        );
    }

    #[test]
    fn environment_includes_version_and_state_when_set() {
        let env = Environment {
            name: "orders".into(),
            profiles: vec!["prod".into()],
            label: "main".into(),
            version: "abc123".into(),
            state: "ok".into(),
            ..Environment::default()
        };
        let json = serde_json::to_string(&env).unwrap();
        assert_eq!(
            json,
            r#"{"name":"orders","profiles":["prod"],"label":"main","version":"abc123","state":"ok","propertySources":[]}"#
        );
    }

    #[test]
    fn environment_serde_round_trip() {
        let env = Environment {
            name: "orders".into(),
            profiles: vec!["prod".into(), "eu".into()],
            label: "main".into(),
            version: "abc123".into(),
            state: String::new(),
            property_sources: vec![PropertySource {
                name: "default".into(),
                source: [
                    ("db.url".to_string(), "jdbc:postgres://…".into()),
                    ("pool.size".to_string(), serde_json::json!(10)),
                ]
                .into_iter()
                .collect(),
            }],
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: Environment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn environment_deserializes_go_payload_without_version_and_state() {
        // The exact document the Go port emits on a soft miss.
        let json = r#"{"name":"missing","profiles":["dev"],"label":"main","propertySources":[]}"#;
        let env: Environment = serde_json::from_str(json).unwrap();
        assert_eq!(env.name, "missing");
        assert!(env.version.is_empty());
        assert!(env.state.is_empty());
    }

    // Regression for the Go-parity bug: the whole raw path is decoded
    // before splitting (so %2F yields a real '/'), and invalid escapes
    // are rejected, like Go net/http's pre-handler 400.
    #[test]
    fn percent_decode_path_mirrors_go_net_http() {
        assert_eq!(
            percent_decode_path("/my%20app/dev").as_deref(),
            Some("/my app/dev")
        );
        assert_eq!(percent_decode_path("/plain").as_deref(), Some("/plain"));
        assert_eq!(
            percent_decode_path("/my%2Fapp/dev").as_deref(),
            Some("/my/app/dev")
        );
        assert_eq!(percent_decode_path("%41%42").as_deref(), Some("AB"));
        // %25 decodes to a literal '%' and is not double-decoded.
        assert_eq!(
            percent_decode_path("/a%252Fb/dev").as_deref(),
            Some("/a%2Fb/dev")
        );
        // Invalid escapes: Go's server never lets these reach the handler.
        assert_eq!(percent_decode_path("/bad%zz/dev"), None);
        assert_eq!(percent_decode_path("/trailing%2"), None);
        assert_eq!(percent_decode_path("/trailing%"), None);
    }

    #[test]
    fn store_is_object_safe_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryStore>();
        assert_send_sync::<Arc<dyn Store>>();
        assert_send_sync::<Environment>();
        assert_send_sync::<PropertySource>();
        assert_send_sync::<ConfigServerError>();
        let _object: Arc<dyn Store> = Arc::new(MemoryStore::new());
    }
}
