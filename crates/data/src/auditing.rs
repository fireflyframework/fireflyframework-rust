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

//! Audit-stamp helpers — automatic `created_at` / `updated_at` /
//! `created_by` / `updated_by` population.
//!
//! This is the Rust port of pyfly's `AuditingEntityListener` plus the
//! `BaseEntity` audit columns. Where pyfly hangs SQLAlchemy
//! `before_insert` / `before_update` ORM events off a declarative base
//! and resolves the current user from a `contextvar`-backed
//! `RequestContext`, the Rust port models the four audit columns as a
//! plain [`AuditStamps`] struct and exposes an [`Auditor`] that stamps
//! them on insert and update. The current user is supplied explicitly
//! (the Rust idiom for pyfly's implicit `RequestContext.current()`),
//! keeping the crate storage- and framework-agnostic.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{AuditStamps, Auditor};
//!
//! let auditor = Auditor::new();
//! let mut stamps = AuditStamps::default();
//!
//! // On insert: all four fields are populated.
//! auditor.on_insert(&mut stamps, Some("alice"));
//! assert_eq!(stamps.created_by.as_deref(), Some("alice"));
//! assert_eq!(stamps.created_at, stamps.updated_at);
//!
//! // On update: only the modification fields move.
//! let created = stamps.created_at;
//! auditor.on_update(&mut stamps, Some("bob"));
//! assert_eq!(stamps.created_by.as_deref(), Some("alice")); // unchanged
//! assert_eq!(stamps.updated_by.as_deref(), Some("bob"));
//! assert_eq!(stamps.created_at, created); // unchanged
//! ```

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Resolves the **current user** identifier for audit stamping — the
/// Rust analogue of pyfly's implicit `RequestContext.current()` user
/// lookup.
///
/// An adapter (e.g. a `firefly-data-sqlx` repository) holds a
/// `UserProvider` and calls it on every write so [`Auditor::on_insert`]
/// / [`Auditor::on_update`] get the user *without the caller passing it
/// each time*. Wire it from the request-context / security crate: the
/// closure reads the authenticated principal (returning `None` for
/// unauthenticated / system writes). Because it is an
/// `Arc<dyn Fn() -> Option<String> + Send + Sync>`, the same provider is
/// cheaply shared across repositories and tasks.
///
/// ```
/// use std::sync::Arc;
/// use firefly_data::UserProvider;
///
/// // A fixed user (a real one would read the security context).
/// let provider: UserProvider = Arc::new(|| Some("alice".to_string()));
/// assert_eq!(provider(), Some("alice".to_string()));
///
/// // The unauthenticated / system path.
/// let system: UserProvider = Arc::new(|| None);
/// assert_eq!(system(), None);
/// ```
pub type UserProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The four audit columns every Firefly entity carries, mirroring
/// pyfly's `BaseEntity` (`created_at`, `updated_at`, `created_by`,
/// `updated_by`).
///
/// Timestamps are UTC. The `*_by` user identifiers are optional — they
/// stay `None` for unauthenticated / system writes, exactly as pyfly's
/// listener leaves them unset when no current user is resolved.
///
/// The struct serialises with `camelCase` field names so the wire shape
/// matches the other ports' JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditStamps {
    /// Instant the row was first inserted.
    pub created_at: Option<DateTime<Utc>>,
    /// Instant the row was last updated.
    pub updated_at: Option<DateTime<Utc>>,
    /// Identifier of the user who created the row, if known.
    pub created_by: Option<String>,
    /// Identifier of the user who last updated the row, if known.
    pub updated_by: Option<String>,
}

impl AuditStamps {
    /// Returns empty stamps — all four fields `None`. Equivalent to a
    /// freshly constructed `BaseEntity` before its first flush.
    pub fn new() -> Self {
        AuditStamps::default()
    }
}

/// Stamps [`AuditStamps`] on insert and update, the Rust analogue of
/// pyfly's `AuditingEntityListener`.
///
/// `Auditor` carries a clock so tests can pin time; production code uses
/// [`Auditor::new`], which reads `Utc::now()`. The current user can be
/// supplied explicitly per call ([`Auditor::on_insert`] /
/// [`Auditor::on_update`]) — the dependency-free path — *or* resolved
/// implicitly from a [`UserProvider`] wired in via
/// [`Auditor::with_user_provider`], the Rust analogue of pyfly's
/// `RequestContext`-backed user lookup. The implicit form
/// ([`Auditor::stamp_insert`] / [`Auditor::stamp_update`]) is what an
/// adapter calls so it never has to thread the user through every write.
pub struct Auditor {
    clock: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    user_provider: Option<UserProvider>,
}

impl std::fmt::Debug for Auditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auditor").finish_non_exhaustive()
    }
}

impl Default for Auditor {
    fn default() -> Self {
        Auditor::new()
    }
}

impl Auditor {
    /// Returns an auditor whose clock is the system UTC wall clock and
    /// which has no [`UserProvider`] (the explicit-user path).
    pub fn new() -> Self {
        Auditor {
            clock: Box::new(Utc::now),
            user_provider: None,
        }
    }

    /// Returns an auditor backed by a custom clock — useful for
    /// deterministic tests that need to assert on exact timestamps.
    pub fn with_clock(clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Auditor {
            clock: Box::new(clock),
            user_provider: None,
        }
    }

    /// Returns an auditor that resolves the current user implicitly from
    /// `provider` on every [`Auditor::stamp_insert`] /
    /// [`Auditor::stamp_update`] — the constructor an adapter uses for
    /// automatic audit stamping without the caller passing a user each
    /// time. The clock is the system UTC wall clock; chain
    /// [`Auditor::with_provider_and_clock`] to pin time in tests.
    pub fn with_user_provider(provider: UserProvider) -> Self {
        Auditor {
            clock: Box::new(Utc::now),
            user_provider: Some(provider),
        }
    }

    /// Returns an auditor with both a custom clock and a
    /// [`UserProvider`] — the deterministic-test form of
    /// [`Auditor::with_user_provider`].
    pub fn with_provider_and_clock(
        provider: UserProvider,
        clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
    ) -> Self {
        Auditor {
            clock: Box::new(clock),
            user_provider: Some(provider),
        }
    }

    /// The configured [`UserProvider`], if any. An adapter can call this
    /// to resolve the user once and reuse it across a multi-row write.
    pub fn user_provider(&self) -> Option<&UserProvider> {
        self.user_provider.as_ref()
    }

    /// Resolves the current user via the configured [`UserProvider`],
    /// returning `None` when no provider is wired or the provider yields
    /// no user (the unauthenticated / system path).
    pub fn current_user(&self) -> Option<String> {
        self.user_provider.as_ref().and_then(|p| p())
    }

    /// Stamps a freshly inserted entity, resolving the user from the
    /// configured [`UserProvider`] — the auto-application path an adapter
    /// calls on every insert. Equivalent to
    /// `auditor.on_insert(stamps, auditor.current_user().as_deref())`.
    pub fn stamp_insert(&self, stamps: &mut AuditStamps) {
        let user = self.current_user();
        self.on_insert(stamps, user.as_deref());
    }

    /// Stamps an updated entity, resolving the user from the configured
    /// [`UserProvider`] — the auto-application path an adapter calls on
    /// every update. Equivalent to
    /// `auditor.on_update(stamps, auditor.current_user().as_deref())`.
    pub fn stamp_update(&self, stamps: &mut AuditStamps) {
        let user = self.current_user();
        self.on_update(stamps, user.as_deref());
    }

    /// Stamps a freshly inserted entity: sets `created_at` and
    /// `updated_at` to the same instant, and — when `user` is supplied —
    /// `created_by` and `updated_by` to that user. Mirrors pyfly's
    /// `_on_insert`.
    pub fn on_insert(&self, stamps: &mut AuditStamps, user: Option<&str>) {
        let now = (self.clock)();
        stamps.created_at = Some(now);
        stamps.updated_at = Some(now);
        if let Some(u) = user {
            stamps.created_by = Some(u.to_string());
            stamps.updated_by = Some(u.to_string());
        }
    }

    /// Stamps an updated entity: moves `updated_at` to now and — when
    /// `user` is supplied — `updated_by` to that user. The creation
    /// fields are left untouched. Mirrors pyfly's `_on_update`.
    pub fn on_update(&self, stamps: &mut AuditStamps, user: Option<&str>) {
        stamps.updated_at = Some((self.clock)());
        if let Some(u) = user {
            stamps.updated_by = Some(u.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    /// Port of pyfly `AuditingEntityListener._on_insert`: all four
    /// fields populated, created/updated timestamps equal.
    #[test]
    fn test_on_insert_sets_all_fields() {
        let auditor = Auditor::with_clock(|| fixed(1_000));
        let mut s = AuditStamps::new();
        auditor.on_insert(&mut s, Some("alice"));
        assert_eq!(s.created_at, Some(fixed(1_000)));
        assert_eq!(s.updated_at, Some(fixed(1_000)));
        assert_eq!(s.created_by.as_deref(), Some("alice"));
        assert_eq!(s.updated_by.as_deref(), Some("alice"));
    }

    /// Port of pyfly `_on_insert` with no current user: timestamps set,
    /// user fields stay `None` (the unauthenticated path).
    #[test]
    fn test_on_insert_without_user_leaves_user_fields_none() {
        let auditor = Auditor::with_clock(|| fixed(1_000));
        let mut s = AuditStamps::new();
        auditor.on_insert(&mut s, None);
        assert_eq!(s.created_at, Some(fixed(1_000)));
        assert!(s.created_by.is_none());
        assert!(s.updated_by.is_none());
    }

    /// Port of pyfly `_on_update`: only the modification fields move;
    /// creation fields are preserved.
    #[test]
    fn test_on_update_only_touches_modification_fields() {
        let auditor = Auditor::with_clock(|| fixed(1_000));
        let mut s = AuditStamps::new();
        auditor.on_insert(&mut s, Some("alice"));

        // advance the clock and update as a different user
        let auditor2 = Auditor::with_clock(|| fixed(2_000));
        auditor2.on_update(&mut s, Some("bob"));

        assert_eq!(s.created_at, Some(fixed(1_000)), "created_at preserved");
        assert_eq!(
            s.created_by.as_deref(),
            Some("alice"),
            "created_by preserved"
        );
        assert_eq!(s.updated_at, Some(fixed(2_000)), "updated_at advanced");
        assert_eq!(s.updated_by.as_deref(), Some("bob"), "updated_by advanced");
    }

    /// On update with no user, the existing `updated_by` is preserved
    /// (the listener only overwrites it when a user is present).
    #[test]
    fn test_on_update_without_user_preserves_updated_by() {
        let auditor = Auditor::with_clock(|| fixed(1_000));
        let mut s = AuditStamps::new();
        auditor.on_insert(&mut s, Some("alice"));
        let auditor2 = Auditor::with_clock(|| fixed(2_000));
        auditor2.on_update(&mut s, None);
        assert_eq!(s.updated_at, Some(fixed(2_000)));
        assert_eq!(s.updated_by.as_deref(), Some("alice"));
    }

    #[test]
    fn test_default_stamps_are_empty() {
        let s = AuditStamps::default();
        assert!(s.created_at.is_none());
        assert!(s.updated_at.is_none());
        assert!(s.created_by.is_none());
        assert!(s.updated_by.is_none());
    }

    #[test]
    fn test_auditor_default_uses_wall_clock() {
        let auditor = Auditor::default();
        let mut s = AuditStamps::new();
        let before = Utc::now();
        auditor.on_insert(&mut s, None);
        let after = Utc::now();
        let ts = s.created_at.unwrap();
        assert!(ts >= before && ts <= after);
    }

    /// Wire shape uses camelCase so it matches the other ports.
    #[test]
    fn test_serde_wire_shape() {
        let auditor = Auditor::with_clock(|| fixed(0));
        let mut s = AuditStamps::new();
        auditor.on_insert(&mut s, Some("sys"));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["createdBy"], "sys");
        assert_eq!(json["updatedBy"], "sys");
        assert!(json.get("createdAt").is_some());
        assert!(json.get("updatedAt").is_some());
    }

    #[test]
    fn test_serde_round_trip() {
        let auditor = Auditor::with_clock(|| fixed(42));
        let mut s = AuditStamps::new();
        auditor.on_insert(&mut s, Some("x"));
        let back: AuditStamps = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn test_auditor_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Auditor>();
        assert_send_sync::<AuditStamps>();
    }

    // ---- UserProvider hook (implicit current-user resolution) -------

    #[test]
    fn test_with_user_provider_resolves_user_on_stamp_insert() {
        let provider: UserProvider = Arc::new(|| Some("alice".to_string()));
        let auditor = Auditor::with_provider_and_clock(provider, || fixed(1_000));
        let mut s = AuditStamps::new();
        auditor.stamp_insert(&mut s);
        assert_eq!(s.created_by.as_deref(), Some("alice"));
        assert_eq!(s.updated_by.as_deref(), Some("alice"));
        assert_eq!(s.created_at, Some(fixed(1_000)));
    }

    #[test]
    fn test_stamp_update_resolves_user() {
        let provider: UserProvider = Arc::new(|| Some("bob".to_string()));
        let auditor = Auditor::with_provider_and_clock(provider, || fixed(2_000));
        let mut s = AuditStamps::new();
        auditor.stamp_update(&mut s);
        assert_eq!(s.updated_by.as_deref(), Some("bob"));
        assert_eq!(s.updated_at, Some(fixed(2_000)));
    }

    #[test]
    fn test_no_provider_means_no_user() {
        let auditor = Auditor::with_clock(|| fixed(1_000));
        assert!(auditor.user_provider().is_none());
        assert_eq!(auditor.current_user(), None);
        let mut s = AuditStamps::new();
        auditor.stamp_insert(&mut s);
        // timestamps set, but no user without a provider
        assert_eq!(s.created_at, Some(fixed(1_000)));
        assert!(s.created_by.is_none());
    }

    #[test]
    fn test_provider_returning_none_is_system_write() {
        let provider: UserProvider = Arc::new(|| None);
        let auditor = Auditor::with_provider_and_clock(provider, || fixed(1_000));
        assert_eq!(auditor.current_user(), None);
        let mut s = AuditStamps::new();
        auditor.stamp_insert(&mut s);
        assert!(s.created_by.is_none());
        assert!(s.updated_by.is_none());
    }

    #[test]
    fn test_current_user_reads_provider_each_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&counter);
        let provider: UserProvider = Arc::new(move || {
            let n = c2.fetch_add(1, Ordering::SeqCst);
            Some(format!("user{n}"))
        });
        let auditor = Auditor::with_user_provider(provider);
        assert_eq!(auditor.current_user(), Some("user0".to_string()));
        assert_eq!(auditor.current_user(), Some("user1".to_string()));
    }
}
