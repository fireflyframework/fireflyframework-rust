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

//! The audit-stamp / soft-delete mixin every Firefly document carries —
//! the Rust port of pyfly's `BaseDocument` (`pyfly.data.document.mongodb`).
//!
//! Where pyfly hangs `created_at` / `updated_at` / `created_by` /
//! `updated_by` (and a soft-delete `deleted_at`) off a Beanie `Document`
//! base and stamps them with pydantic `default_factory` + listeners, the
//! Rust port models the same columns as a **flattened** [`BaseDocument`]
//! struct that an entity embeds with `#[serde(flatten)]`. Because the
//! crate stays storage-agnostic, the actual stamping is delegated to
//! firefly-data's [`Auditor`](firefly_data::Auditor) — the same primitive
//! the relational adapter uses — so audit semantics are identical across
//! every backend.
//!
//! The four audit fields reuse firefly-data's
//! [`AuditStamps`](firefly_data::AuditStamps) and the `deleted_at` column
//! reuses [`SoftDelete`](firefly_data::SoftDelete), both serialised with
//! `#[serde(flatten)]` so the document's BSON has top-level `createdAt`,
//! `updatedAt`, `createdBy`, `updatedBy`, and `deletedAt` keys — matching
//! pyfly's wire shape.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::Auditor;
//! use firefly_data_mongodb::BaseDocument;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct UserDocument {
//!     #[serde(rename = "_id")]
//!     id: String,
//!     name: String,
//!     #[serde(flatten)]
//!     base: BaseDocument,
//! }
//!
//! let auditor = Auditor::new();
//! let mut user = UserDocument {
//!     id: "u1".into(),
//!     name: "alice".into(),
//!     base: BaseDocument::default(),
//! };
//! // Stamp on insert — sets created_at / updated_at (and *_by if a
//! // UserProvider is wired into the Auditor).
//! user.base.stamp_insert(&auditor);
//! assert!(user.base.audit.created_at.is_some());
//! assert!(!user.base.is_deleted());
//! ```

use firefly_data::{AuditStamps, Auditor, SoftDelete};
use serde::{Deserialize, Serialize};

/// The audit-trail + soft-delete fields every Firefly MongoDB document
/// carries — the Rust analogue of pyfly's `BaseDocument`.
///
/// Embed it in a document entity with `#[serde(flatten)]` so its fields
/// surface at the top level of the stored BSON (`createdAt`, `updatedAt`,
/// `createdBy`, `updatedBy`, `deletedAt`), exactly as pyfly's
/// `BaseDocument` columns do:
///
/// ```
/// use firefly_data_mongodb::BaseDocument;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Serialize, Deserialize)]
/// struct OrderDocument {
///     #[serde(rename = "_id")]
///     id: String,
///     total: f64,
///     #[serde(flatten)]
///     base: BaseDocument,
/// }
/// ```
///
/// The stamping itself is delegated to firefly-data's
/// [`Auditor`](firefly_data::Auditor) and
/// [`SoftDelete`](firefly_data::SoftDelete), so the document inherits the
/// **same** audit / soft-delete semantics as the relational adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseDocument {
    /// The four audit columns (`createdAt` / `updatedAt` / `createdBy` /
    /// `updatedBy`), flattened to the document's top level.
    #[serde(flatten)]
    pub audit: AuditStamps,
    /// The soft-delete stamp (`deletedAt`), flattened to the document's
    /// top level. `None` while the row is live.
    #[serde(flatten)]
    pub soft_delete: SoftDelete,
}

impl BaseDocument {
    /// Returns a fresh, live (not-deleted), unstamped base document — all
    /// audit fields `None` and `deleted_at` `None`. The Rust equivalent
    /// of a freshly constructed pyfly `BaseDocument` before its first
    /// save.
    pub fn new() -> Self {
        BaseDocument::default()
    }

    /// Stamps this document as a fresh insert, resolving the current user
    /// from the `auditor`'s [`UserProvider`](firefly_data::UserProvider)
    /// — the Rust analogue of pyfly's `_on_insert` listener. Sets
    /// `created_at` and `updated_at` to the same instant (and the `*_by`
    /// fields when a user is resolved).
    pub fn stamp_insert(&mut self, auditor: &Auditor) {
        auditor.stamp_insert(&mut self.audit);
    }

    /// Stamps this document as an update, resolving the current user from
    /// the `auditor`'s [`UserProvider`](firefly_data::UserProvider) — the
    /// Rust analogue of pyfly's `_on_update` listener. Moves `updated_at`
    /// (and `updated_by` when a user is resolved); the creation fields are
    /// left untouched.
    pub fn stamp_update(&mut self, auditor: &Auditor) {
        auditor.stamp_update(&mut self.audit);
    }

    /// Whether this document is soft-deleted — the Rust equivalent of
    /// pyfly's `SoftDeleteMixin.is_deleted`.
    pub fn is_deleted(&self) -> bool {
        self.soft_delete.is_deleted()
    }

    /// Marks this document soft-deleted at the current UTC instant.
    pub fn mark_deleted_now(&mut self) {
        self.soft_delete.mark_deleted_now();
    }

    /// Clears the soft-delete stamp, restoring the document to "live".
    pub fn restore(&mut self) {
        self.soft_delete.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use firefly_data::UserProvider;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct UserDocument {
        #[serde(rename = "_id")]
        id: String,
        name: String,
        #[serde(flatten)]
        base: BaseDocument,
    }

    fn doc() -> UserDocument {
        UserDocument {
            id: "u1".into(),
            name: "alice".into(),
            base: BaseDocument::new(),
        }
    }

    /// A fresh base document is live with no audit stamps — pyfly parity.
    #[test]
    fn new_is_live_and_unstamped() {
        let b = BaseDocument::new();
        assert!(b.audit.created_at.is_none());
        assert!(b.audit.updated_at.is_none());
        assert!(b.audit.created_by.is_none());
        assert!(b.audit.updated_by.is_none());
        assert!(!b.is_deleted());
    }

    /// `stamp_insert` populates all four audit fields and resolves the
    /// current user from the wired provider.
    #[test]
    fn stamp_insert_sets_audit_and_user() {
        let provider: UserProvider = Arc::new(|| Some("alice".to_string()));
        let auditor =
            Auditor::with_provider_and_clock(provider, || Utc.timestamp_opt(1_000, 0).unwrap());
        let mut d = doc();
        d.base.stamp_insert(&auditor);
        assert_eq!(
            d.base.audit.created_at,
            Some(Utc.timestamp_opt(1_000, 0).unwrap())
        );
        assert_eq!(d.base.audit.created_at, d.base.audit.updated_at);
        assert_eq!(d.base.audit.created_by.as_deref(), Some("alice"));
        assert_eq!(d.base.audit.updated_by.as_deref(), Some("alice"));
    }

    /// `stamp_update` advances only the modification fields.
    #[test]
    fn stamp_update_touches_only_modification_fields() {
        let provider: UserProvider = Arc::new(|| Some("alice".to_string()));
        let insert_auditor =
            Auditor::with_provider_and_clock(provider, || Utc.timestamp_opt(1_000, 0).unwrap());
        let mut d = doc();
        d.base.stamp_insert(&insert_auditor);

        let bob: UserProvider = Arc::new(|| Some("bob".to_string()));
        let update_auditor =
            Auditor::with_provider_and_clock(bob, || Utc.timestamp_opt(2_000, 0).unwrap());
        d.base.stamp_update(&update_auditor);

        assert_eq!(
            d.base.audit.created_at,
            Some(Utc.timestamp_opt(1_000, 0).unwrap())
        );
        assert_eq!(d.base.audit.created_by.as_deref(), Some("alice"));
        assert_eq!(
            d.base.audit.updated_at,
            Some(Utc.timestamp_opt(2_000, 0).unwrap())
        );
        assert_eq!(d.base.audit.updated_by.as_deref(), Some("bob"));
    }

    /// Soft-delete / restore toggles `is_deleted`.
    #[test]
    fn soft_delete_and_restore() {
        let mut d = doc();
        assert!(!d.base.is_deleted());
        d.base.mark_deleted_now();
        assert!(d.base.is_deleted());
        d.base.restore();
        assert!(!d.base.is_deleted());
    }

    /// The flattened fields surface at the document's top level with the
    /// camelCase wire names pyfly uses.
    #[test]
    fn flatten_wire_shape() {
        let provider: UserProvider = Arc::new(|| Some("sys".to_string()));
        let auditor =
            Auditor::with_provider_and_clock(provider, || Utc.timestamp_opt(0, 0).unwrap());
        let mut d = doc();
        d.base.stamp_insert(&auditor);
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["_id"], json!("u1"));
        assert_eq!(v["name"], json!("alice"));
        assert_eq!(v["createdBy"], json!("sys"));
        assert_eq!(v["updatedBy"], json!("sys"));
        assert!(v.get("createdAt").is_some());
        assert!(v.get("updatedAt").is_some());
        // Live row: deletedAt is null, present as a flattened field.
        assert_eq!(v["deletedAt"], json!(null));
    }

    /// Round-trips through JSON, preserving every field.
    #[test]
    fn serde_round_trip() {
        let provider: UserProvider = Arc::new(|| Some("x".to_string()));
        let auditor =
            Auditor::with_provider_and_clock(provider, || Utc.timestamp_opt(42, 0).unwrap());
        let mut d = doc();
        d.base.stamp_insert(&auditor);
        let back: UserDocument = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
    }
}
