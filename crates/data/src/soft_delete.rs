//! Soft-delete support — a `deleted_at` stamp plus predicate injection
//! that hides logically deleted rows from every read path.
//!
//! This is the Rust port of pyfly's `SoftDeleteMixin` and the read-path
//! predicate guard threaded through `SoftDeleteRepository`. Rather than
//! physically removing a row, a soft delete sets its `deleted_at`
//! timestamp; every list query then has a `deleted_at IS NULL` predicate
//! injected so deleted rows stay hidden unless explicitly requested.
//!
//! The crate stays storage-agnostic: [`SoftDelete`] is the column helper,
//! and [`SoftDeletePolicy`] does the [`Filter`] / [`Specification`]
//! injection that a concrete repository applies before rendering SQL.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Filter, SoftDeletePolicy};
//!
//! let policy = SoftDeletePolicy::new(); // column "deleted_at"
//!
//! // A user's filter gains a `deleted_at IS NULL` guard up front.
//! let f = policy.apply(Filter::new().where_eq("name", "alice"));
//! let (sql, _) = f.to_sql();
//! assert_eq!(sql, r#" WHERE "deleted_at" IS NULL AND "name" = $1"#);
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::filter::{Filter, Predicate};
use crate::specification::Specification;

/// The default column name used for the soft-delete timestamp, matching
/// pyfly's `SoftDeleteMixin.deleted_at`.
pub const DEFAULT_DELETED_AT_COLUMN: &str = "deleted_at";

/// The soft-delete column helper, mirroring pyfly's `SoftDeleteMixin`:
/// a nullable `deleted_at` timestamp where `None` means "live" and a set
/// value means "logically deleted".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoftDelete {
    /// Instant the row was soft-deleted; `None` while the row is live.
    pub deleted_at: Option<DateTime<Utc>>,
}

impl SoftDelete {
    /// Returns a live (not-deleted) stamp.
    pub fn live() -> Self {
        SoftDelete { deleted_at: None }
    }

    /// Whether the row is soft-deleted — the Rust equivalent of pyfly's
    /// `SoftDeleteMixin.is_deleted` property.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Marks the row deleted at the given instant (pyfly's `delete`,
    /// which sets `deleted_at = datetime.now(UTC)`).
    pub fn mark_deleted(&mut self, at: DateTime<Utc>) {
        self.deleted_at = Some(at);
    }

    /// Marks the row deleted at the current UTC instant.
    pub fn mark_deleted_now(&mut self) {
        self.deleted_at = Some(Utc::now());
    }

    /// Clears the deletion stamp — pyfly's `restore`.
    pub fn restore(&mut self) {
        self.deleted_at = None;
    }
}

/// Injects the "not deleted" guard into queries so soft-deleted rows are
/// excluded from every read path — the behaviour pyfly threads through
/// `SoftDeleteRepository`'s overridden readers (audit #103).
///
/// The guard column defaults to `deleted_at`; override it with
/// [`SoftDeletePolicy::for_column`] for entities that name the column
/// differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftDeletePolicy {
    column: String,
}

impl Default for SoftDeletePolicy {
    fn default() -> Self {
        SoftDeletePolicy::new()
    }
}

impl SoftDeletePolicy {
    /// Returns a policy guarding the default `deleted_at` column.
    pub fn new() -> Self {
        SoftDeletePolicy {
            column: DEFAULT_DELETED_AT_COLUMN.to_string(),
        }
    }

    /// Returns a policy guarding a custom soft-delete column.
    pub fn for_column(column: impl Into<String>) -> Self {
        SoftDeletePolicy {
            column: column.into(),
        }
    }

    /// The guarded column name.
    pub fn column(&self) -> &str {
        &self.column
    }

    /// The standalone "live row" predicate: `"<column>" IS NULL`.
    pub fn predicate(&self) -> Predicate {
        Predicate::is_nil(&self.column)
    }

    /// Injects the not-deleted guard at the **front** of a [`Filter`]'s
    /// predicate list, so the rendered SQL reads
    /// `WHERE "deleted_at" IS NULL AND <user predicates>`. Sorts and
    /// paging are untouched.
    ///
    /// Injection is idempotent: a filter that already carries the guard
    /// is returned unchanged, so applying the policy twice does not emit
    /// a duplicate clause.
    pub fn apply(&self, mut filter: Filter) -> Filter {
        let guard = self.predicate();
        if filter.predicates.contains(&guard) {
            return filter;
        }
        filter.predicates.insert(0, guard);
        filter
    }

    /// Combines the not-deleted guard with a [`Specification`] under
    /// AND, so the live-row restriction is always present. `spec.and`
    /// flattening keeps the rendered SQL flat.
    pub fn apply_spec(&self, spec: Specification) -> Specification {
        Specification::pred(self.predicate()).and(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Direction, Op};
    use chrono::TimeZone;
    use serde_json::json;

    fn fixed(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    // ---- SoftDelete column helper (pyfly SoftDeleteMixin) ----

    /// Port of `test_deleted_at_defaults_to_none`.
    #[test]
    fn test_deleted_at_defaults_to_none() {
        assert!(SoftDelete::default().deleted_at.is_none());
        assert!(SoftDelete::live().deleted_at.is_none());
    }

    /// Port of `test_is_deleted_false_when_not_deleted`.
    #[test]
    fn test_is_deleted_false_when_not_deleted() {
        assert!(!SoftDelete::live().is_deleted());
    }

    /// Port of `test_is_deleted_true_after_soft_delete` /
    /// `test_soft_delete_sets_deleted_at`.
    #[test]
    fn test_mark_deleted_sets_deleted_at_and_is_deleted() {
        let mut s = SoftDelete::live();
        s.mark_deleted(fixed(1_000));
        assert_eq!(s.deleted_at, Some(fixed(1_000)));
        assert!(s.is_deleted());
    }

    /// Port of `test_restore_clears_deleted_at`.
    #[test]
    fn test_restore_clears_deleted_at() {
        let mut s = SoftDelete::live();
        s.mark_deleted(fixed(1_000));
        s.restore();
        assert!(s.deleted_at.is_none());
        assert!(!s.is_deleted());
    }

    #[test]
    fn test_mark_deleted_now_is_recent() {
        let before = Utc::now();
        let mut s = SoftDelete::live();
        s.mark_deleted_now();
        let after = Utc::now();
        let ts = s.deleted_at.unwrap();
        assert!(ts >= before && ts <= after);
    }

    // ---- SoftDeletePolicy injection (pyfly read-path guard) ----

    /// The injected guard renders first, before the user predicates —
    /// the SQL equivalent of `find_all().where(deleted_at == None)`.
    #[test]
    fn test_apply_injects_not_deleted_guard_first() {
        let policy = SoftDeletePolicy::new();
        let f = policy.apply(Filter::new().where_eq("name", "alice"));
        let (sql, args) = f.to_sql();
        assert_eq!(sql, r#" WHERE "deleted_at" IS NULL AND "name" = $1"#);
        assert_eq!(args, vec![json!("alice")]);
    }

    /// The guard preserves sorting and paging.
    #[test]
    fn test_apply_preserves_sort_and_paging() {
        let policy = SoftDeletePolicy::new();
        let f = policy.apply(
            Filter::new()
                .where_eq("name", "alice")
                .order_by("id", Direction::Asc)
                .paged(1, 10),
        );
        let (sql, _) = f.to_sql();
        assert!(sql.contains(r#""deleted_at" IS NULL"#));
        assert!(sql.contains(r#"ORDER BY "id" ASC"#));
        assert!(sql.contains("LIMIT 10 OFFSET 10"));
    }

    /// Applying the policy twice does not duplicate the guard.
    #[test]
    fn test_apply_is_idempotent() {
        let policy = SoftDeletePolicy::new();
        let f = policy.apply(policy.apply(Filter::new().where_eq("id", 1)));
        assert_eq!(
            f.predicates
                .iter()
                .filter(|p| p.op == Op::IsNil && p.field == "deleted_at")
                .count(),
            1
        );
    }

    /// The guard works on an otherwise empty filter.
    #[test]
    fn test_apply_on_empty_filter() {
        let policy = SoftDeletePolicy::new();
        let (sql, args) = policy.apply(Filter::new()).to_sql();
        assert_eq!(sql, r#" WHERE "deleted_at" IS NULL"#);
        assert!(args.is_empty());
    }

    /// A custom column name is honoured.
    #[test]
    fn test_for_column_uses_custom_name() {
        let policy = SoftDeletePolicy::for_column("removed_on");
        assert_eq!(policy.column(), "removed_on");
        let (sql, _) = policy.apply(Filter::new()).to_sql();
        assert_eq!(sql, r#" WHERE "removed_on" IS NULL"#);
    }

    /// The standalone predicate is an `IS NULL` over the guard column.
    #[test]
    fn test_predicate_is_isnil() {
        let p = SoftDeletePolicy::new().predicate();
        assert_eq!(p.op, Op::IsNil);
        assert_eq!(p.field, "deleted_at");
    }

    /// The guard combines with a `Specification`, AND-first.
    #[test]
    fn test_apply_spec_prepends_guard() {
        let policy = SoftDeletePolicy::new();
        let spec = policy.apply_spec(Specification::eq("role", "admin"));
        let (sql, args) = spec.to_sql();
        assert_eq!(sql, r#"("deleted_at" IS NULL AND "role" = $1)"#);
        assert_eq!(args, vec![json!("admin")]);
    }

    /// The guard combines with an OR specification, keeping the OR
    /// grouped so the guard is not accidentally OR-ed away (audit #103).
    #[test]
    fn test_apply_spec_guards_an_or_query() {
        let policy = SoftDeletePolicy::new();
        let spec = policy
            .apply_spec(Specification::eq("role", "admin") | Specification::eq("active", true));
        let (sql, _) = spec.to_sql();
        assert_eq!(
            sql,
            r#"("deleted_at" IS NULL AND ("role" = $1 OR "active" = $2))"#
        );
    }

    #[test]
    fn test_serde_round_trip() {
        let mut s = SoftDelete::live();
        s.mark_deleted(fixed(7));
        let back: SoftDelete = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        // wire shape is camelCase
        let json = serde_json::to_value(s).unwrap();
        assert!(json.get("deletedAt").is_some());
    }
}
