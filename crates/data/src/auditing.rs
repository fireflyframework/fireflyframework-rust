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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
/// [`Auditor::new`], which reads `Utc::now()`. Unlike pyfly's listener,
/// which resolves the user from a thread/`contextvar` `RequestContext`,
/// the current user is passed in explicitly to keep the helper
/// dependency-free.
pub struct Auditor {
    clock: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
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
    /// Returns an auditor whose clock is the system UTC wall clock.
    pub fn new() -> Self {
        Auditor {
            clock: Box::new(Utc::now),
        }
    }

    /// Returns an auditor backed by a custom clock — useful for
    /// deterministic tests that need to assert on exact timestamps.
    pub fn with_clock(clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Auditor {
            clock: Box::new(clock),
        }
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
}
