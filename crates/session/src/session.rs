//! [`Session`] / [`SessionInner`] — the server-side session handle, the
//! Rust port of pyfly's `HttpSession`.
//!
//! pyfly's `HttpSession` wraps a `dict[str, Any]` behind attribute
//! accessors plus modified/invalidated tracking and an anti-fixation
//! `rotate_id`. In Rust the data is a `HashMap<String, serde_json::Value>`
//! and attribute access is *typed*: [`Session::attribute`] deserializes
//! into any `T: DeserializeOwned` and [`Session::set_attribute`] serializes
//! any `T: Serialize`. This serde typing subsumes pyfly's importlib-based
//! tagged-dataclass rehydration allowlist (`allow_session_type`) safely —
//! there is no arbitrary-object gadget to guard against.
//!
//! The inner state lives behind a cloneable [`Session`] = `Arc<Mutex<…>>`
//! so the [`crate::SessionLayer`] and the handler (via
//! `axum::Extension<Session>` or the [`crate::SessionExt`] extractor) share
//! the same mutable session for the lifetime of a request.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

/// Internal metadata key holding the creation timestamp (epoch millis).
/// Mirrors pyfly's `_created_at`; hidden from [`Session::attribute_names`].
pub(crate) const CREATED_AT_KEY: &str = "_created_at";
/// Internal metadata key holding the last-access timestamp (epoch millis).
/// Mirrors pyfly's `_last_accessed`; hidden from [`Session::attribute_names`].
pub(crate) const LAST_ACCESSED_KEY: &str = "_last_accessed";

/// The owned, lock-free state of a session — the direct analog of the
/// fields pyfly's `HttpSession` keeps. Construct one with [`SessionInner::new`]
/// or [`SessionInner::load`]; in normal use it is wrapped in a [`Session`].
#[derive(Debug, Clone)]
pub struct SessionInner {
    id: String,
    previous_id: Option<String>,
    is_new: bool,
    invalidated: bool,
    modified: bool,
    data: HashMap<String, Value>,
}

impl SessionInner {
    /// Creates a brand-new session with `id`, empty data, `is_new = true`
    /// and `modified = true` (so a freshly created session is always
    /// persisted), seeding `_created_at` / `_last_accessed` — matching
    /// pyfly's `HttpSession(id, is_new=True)`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        let mut data = HashMap::new();
        let now = now_millis();
        data.insert(CREATED_AT_KEY.to_string(), Value::from(now));
        data.insert(LAST_ACCESSED_KEY.to_string(), Value::from(now));
        Self {
            id: id.into(),
            previous_id: None,
            is_new: true,
            invalidated: false,
            modified: true,
            data,
        }
    }

    /// Rehydrates an existing session loaded from a [`crate::SessionStore`]:
    /// `is_new = false`, `modified = false`. `_last_accessed` is refreshed
    /// to now (mirroring pyfly's constructor, which always stamps it) but
    /// this does **not** flip `modified`, so a read-only request issues no
    /// store write — only the cookie's `Max-Age` slides forward.
    #[must_use]
    pub fn load(id: impl Into<String>, mut data: HashMap<String, Value>) -> Self {
        let now = now_millis();
        data.entry(CREATED_AT_KEY.to_string())
            .or_insert_with(|| Value::from(now));
        data.insert(LAST_ACCESSED_KEY.to_string(), Value::from(now));
        Self {
            id: id.into(),
            previous_id: None,
            is_new: false,
            invalidated: false,
            modified: false,
            data,
        }
    }

    /// The current session identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The id this session was rotated away from (set by [`Self::rotate_id`]),
    /// or `None` if it has never rotated. The [`crate::SessionLayer`] deletes
    /// this id from the store on persist so a fixed/stale id can no longer
    /// resolve to this session.
    #[must_use]
    pub fn previous_id(&self) -> Option<&str> {
        self.previous_id.as_deref()
    }

    /// `true` if the session was created during the current request.
    #[must_use]
    pub fn is_new(&self) -> bool {
        self.is_new
    }

    /// `true` once [`Self::invalidate`] has been called.
    #[must_use]
    pub fn invalidated(&self) -> bool {
        self.invalidated
    }

    /// `true` if the session has been mutated since it was loaded/created
    /// and therefore needs to be persisted.
    #[must_use]
    pub fn modified(&self) -> bool {
        self.modified
    }

    /// The creation timestamp, decoded from the `_created_at` metadata key.
    #[must_use]
    pub fn created_at(&self) -> Option<DateTime<Utc>> {
        self.data.get(CREATED_AT_KEY).and_then(millis_to_datetime)
    }

    /// The last-access timestamp, decoded from the `_last_accessed`
    /// metadata key.
    #[must_use]
    pub fn last_accessed(&self) -> Option<DateTime<Utc>> {
        self.data
            .get(LAST_ACCESSED_KEY)
            .and_then(millis_to_datetime)
    }

    /// Returns the attribute `name` deserialized into `T`, or `None` when
    /// the attribute is absent or fails to deserialize into `T`. The typed
    /// replacement for pyfly's untyped `get_attribute`.
    #[must_use]
    pub fn attribute<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.data
            .get(name)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Returns the raw JSON value of attribute `name`, or `None`.
    #[must_use]
    pub fn attribute_value(&self, name: &str) -> Option<&Value> {
        self.data.get(name)
    }

    /// Serializes `value` and stores it under `name`, flipping `modified`.
    /// Returns an error only if `value` fails to serialize to JSON.
    pub fn set_attribute<T: Serialize>(
        &mut self,
        name: impl Into<String>,
        value: T,
    ) -> Result<(), serde_json::Error> {
        let json = serde_json::to_value(value)?;
        self.data.insert(name.into(), json);
        self.modified = true;
        Ok(())
    }

    /// Stores an already-built JSON `value` under `name` (no serialization
    /// step), flipping `modified`.
    pub fn set_attribute_value(&mut self, name: impl Into<String>, value: Value) {
        self.data.insert(name.into(), value);
        self.modified = true;
    }

    /// Removes attribute `name` if present, flipping `modified` when a
    /// value was actually removed (matching pyfly).
    pub fn remove_attribute(&mut self, name: &str) {
        if self.data.remove(name).is_some() {
            self.modified = true;
        }
    }

    /// All attribute names excluding internal metadata keys (those starting
    /// with `_`), matching pyfly's `get_attribute_names`.
    #[must_use]
    pub fn attribute_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .data
            .keys()
            .filter(|k| !k.starts_with('_'))
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Assigns a fresh id (UUID v4 simple hex), preserving all data and
    /// recording [`Self::previous_id`]. Anti-session-fixation: call on
    /// authentication / privilege elevation. A no-op once
    /// [`Self::invalidate`] has been called (matching pyfly).
    pub fn rotate_id(&mut self) {
        if self.invalidated {
            return;
        }
        self.previous_id = Some(std::mem::replace(&mut self.id, new_session_id()));
        self.modified = true;
    }

    /// Marks the session for deletion. The [`crate::SessionLayer`] deletes
    /// its store entry and the cookie on persist.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.modified = true;
    }

    /// The raw session data map (including internal metadata), as persisted
    /// to the store — the analog of pyfly's `get_data`.
    #[must_use]
    pub fn data(&self) -> &HashMap<String, Value> {
        &self.data
    }
}

/// A cloneable, shareable session handle — `Arc<Mutex<SessionInner>>`.
///
/// The [`crate::SessionLayer`] inserts a clone into the request extensions
/// before calling the inner service, so handlers extract it with
/// `axum::Extension<Session>` (or the [`crate::SessionExt`] extractor) and
/// mutate the *same* session the layer later persists. Async access goes
/// through [`Session::lock`].
#[derive(Debug, Clone)]
pub struct Session(Arc<Mutex<SessionInner>>);

impl Session {
    /// Wraps owned [`SessionInner`] state in a shareable handle.
    #[must_use]
    pub fn new(inner: SessionInner) -> Self {
        Self(Arc::new(Mutex::new(inner)))
    }

    /// Locks the inner state for read/write. The lock is held only for the
    /// duration of the returned guard.
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, SessionInner> {
        self.0.lock().await
    }

    /// Convenience: the session id (clones the string under a short lock).
    pub async fn id(&self) -> String {
        self.0.lock().await.id().to_string()
    }

    /// Convenience: read attribute `name` deserialized into `T`.
    pub async fn attribute<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        self.0.lock().await.attribute(name)
    }

    /// Convenience: set attribute `name` to a serializable `value`.
    pub async fn set_attribute<T: Serialize>(
        &self,
        name: impl Into<String>,
        value: T,
    ) -> Result<(), serde_json::Error> {
        self.0.lock().await.set_attribute(name, value)
    }

    /// Convenience: remove attribute `name`.
    pub async fn remove_attribute(&self, name: &str) {
        self.0.lock().await.remove_attribute(name);
    }

    /// Convenience: rotate the session id (anti-fixation).
    pub async fn rotate_id(&self) {
        self.0.lock().await.rotate_id();
    }

    /// Convenience: invalidate the session.
    pub async fn invalidate(&self) {
        self.0.lock().await.invalidate();
    }
}

/// Generates a fresh session id: a UUID v4 rendered as 32 lowercase hex
/// chars (no dashes), matching pyfly's `uuid.uuid4().hex`.
#[must_use]
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The current wall-clock time in epoch milliseconds.
fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Decodes an epoch-millis JSON number into a [`DateTime<Utc>`].
fn millis_to_datetime(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_i64()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_roundtrip_flips_modified() {
        // pyfly: TestHttpSession.test_attribute_roundtrip
        let mut s = SessionInner::new("sid");
        s.modified = false; // reset the new-session modified flag to observe set
        s.set_attribute("user", "ada").unwrap();
        assert_eq!(s.attribute::<String>("user").as_deref(), Some("ada"));
        assert!(s.modified());
        s.remove_attribute("user");
        assert_eq!(s.attribute::<String>("user"), None);
    }

    #[test]
    fn typed_attribute_roundtrip() {
        let mut s = SessionInner::new("sid");
        s.set_attribute("count", 7u32).unwrap();
        assert_eq!(s.attribute::<u32>("count"), Some(7));
        // Wrong type deserializes to None rather than panicking.
        assert_eq!(s.attribute::<String>("count"), None);
    }

    #[test]
    fn new_session_is_modified_and_new() {
        let s = SessionInner::new("sid");
        assert!(s.is_new());
        assert!(s.modified());
    }

    #[test]
    fn loaded_session_is_not_new_or_modified() {
        let mut data = HashMap::new();
        data.insert("user".to_string(), Value::from("ada"));
        let s = SessionInner::load("sid", data);
        assert!(!s.is_new());
        assert!(!s.modified());
        assert_eq!(s.attribute::<String>("user").as_deref(), Some("ada"));
    }

    #[test]
    fn invalidate_sets_flag() {
        // pyfly: TestHttpSession.test_invalidate
        let mut data = HashMap::new();
        data.insert("k".to_string(), Value::from("v"));
        let mut s = SessionInner::load("sid", data);
        s.invalidate();
        assert!(s.invalidated());
        assert!(s.modified());
    }

    #[test]
    fn rotate_id_assigns_new_id_and_preserves_data() {
        // pyfly: TestHttpSession.test_rotate_id_assigns_new_id_and_preserves_data
        let mut data = HashMap::new();
        data.insert("k".to_string(), Value::from("v"));
        let mut s = SessionInner::load("old-id", data);
        s.rotate_id();
        assert_ne!(s.id(), "old-id");
        assert_eq!(s.previous_id(), Some("old-id"));
        assert_eq!(s.attribute::<String>("k").as_deref(), Some("v"));
        assert!(s.modified());
    }

    #[test]
    fn rotate_id_is_noop_when_invalidated() {
        // pyfly: TestHttpSession.test_rotate_id_is_noop_when_invalidated
        let mut s = SessionInner::new("old-id");
        s.invalidate();
        s.rotate_id();
        assert_eq!(s.id(), "old-id");
        assert_eq!(s.previous_id(), None);
    }

    #[test]
    fn attribute_names_excludes_metadata() {
        let mut s = SessionInner::new("sid");
        s.set_attribute("user", "ada").unwrap();
        s.set_attribute("role", "admin").unwrap();
        assert_eq!(s.attribute_names(), vec!["role", "user"]);
    }

    #[test]
    fn created_and_last_accessed_present() {
        let s = SessionInner::new("sid");
        assert!(s.created_at().is_some());
        assert!(s.last_accessed().is_some());
    }

    #[tokio::test]
    async fn handle_shares_state() {
        let session = Session::new(SessionInner::new("sid"));
        session.set_attribute("a", 1u8).await.unwrap();
        let clone = session.clone();
        assert_eq!(clone.attribute::<u8>("a").await, Some(1));
        clone.rotate_id().await;
        assert_ne!(session.id().await, "sid");
    }

    #[test]
    fn new_session_id_is_simple_hex() {
        let id = new_session_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
