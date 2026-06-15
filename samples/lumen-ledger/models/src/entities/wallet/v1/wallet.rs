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

//! The [`Wallet`] persistence entity (`@Entity` / `@Table`).

use chrono::{DateTime, NaiveDateTime, Utc};
use firefly::data_sqlx::{AnyRow, ColumnValue, SqlxEntity};
use firefly::kernel::FireflyError;
use lumen_ledger_interfaces::WalletStatus;
use uuid::Uuid;

/// The persisted shape of a wallet — one row of the `wallets` table.
///
/// `status` is the typed [`WalletStatus`] enum end-to-end; the token↔enum
/// conversion happens exactly once, at the row boundary (the [`SqlxEntity`]
/// mapping below) — the `@Enumerated(STRING)` analog. `created_at` /
/// `updated_at` and `version` are managed by the store (the framework `Auditor`
/// and the `@Version` optimistic-locking column), not by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wallet {
    /// Primary key.
    pub id: Uuid,
    /// Human-facing account number (e.g. `"WAL-00001"`).
    pub account_number: String,
    /// Owner display name.
    pub owner: String,
    /// Balance in minor units (cents).
    pub balance: i64,
    /// ISO-4217 currency code.
    pub currency: String,
    /// Lifecycle status (`@Enumerated(STRING)`: stored as its lowercase token).
    pub status: WalletStatus,
    /// Optimistic-locking version (`@Version`) — bumped by the store on update.
    pub version: i64,
    /// Creation timestamp (`@CreatedDate`, stamped by the store on insert).
    pub created_at: DateTime<Utc>,
    /// Last-update timestamp (`@LastModifiedDate`, stamped on every write).
    pub updated_at: DateTime<Utc>,
}

/// The `@Entity` / `@Table` / `@Id` / `@Version` / `@Column` mapping. A
/// `#[derive(SqlxRepository)]` over this entity gives a fully-wired
/// `@Repository` from an injected `Db` — no factory boilerplate.
impl SqlxEntity for Wallet {
    type Id = Uuid;

    fn table() -> &'static str {
        "wallets"
    }

    fn id_column() -> &'static str {
        "id"
    }

    fn columns() -> &'static [&'static str] {
        &[
            "id",
            "account_number",
            "owner",
            "balance",
            "currency",
            "status",
            "version",
            "created_at",
            "updated_at",
        ]
    }

    fn version_column() -> Option<&'static str> {
        Some("version")
    }

    fn read_row(row: &AnyRow<'_>) -> Result<Self, FireflyError> {
        Ok(Wallet {
            id: Uuid::parse_str(&row.get_str("id")?)
                .map_err(|e| FireflyError::internal(format!("bad uuid: {e}")))?,
            account_number: row.get_str("account_number")?,
            owner: row.get_str("owner")?,
            balance: row.get_i64("balance")?,
            currency: row.get_str("currency")?,
            status: WalletStatus::from_token(&row.get_str("status")?),
            version: row.get_i64("version")?,
            created_at: read_dt(&row.get_str("created_at")?)?,
            updated_at: read_dt(&row.get_str("updated_at")?)?,
        })
    }

    fn write_row(&self) -> Vec<ColumnValue> {
        vec![
            ColumnValue::new("id", self.id.to_string()),
            ColumnValue::new("account_number", self.account_number.clone()),
            ColumnValue::new("owner", self.owner.clone()),
            ColumnValue::new("balance", self.balance),
            ColumnValue::new("currency", self.currency.clone()),
            // The single enum→token boundary (`@Enumerated(STRING)`).
            ColumnValue::new("status", self.status.as_str()),
            // `version` is sent as the LOADED value so the `@Version` guard can
            // compare it; the store bumps it on a successful UPDATE.
            ColumnValue::new("version", self.version),
            // `created_at` re-sent so an UPDATE preserves it (the auditor only
            // stamps it on INSERT); the auditor overwrites `updated_at`.
            ColumnValue::new("created_at", self.created_at.to_rfc3339()),
        ]
    }
}

/// Parses a stored timestamp, tolerating RFC 3339 (`T`-separated, what the
/// writer emits) and the space-separated form the sqlx SQLite/Postgres binders
/// produce for an auditor-stamped `DateTime<Utc>` — so an audit column written
/// by the framework round-trips back into the entity.
fn read_dt(s: &str) -> Result<DateTime<Utc>, FireflyError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%:z").map(|dt| dt.with_timezone(&Utc))
        })
        .or_else(|_| {
            NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f").map(|ndt| ndt.and_utc())
        })
        .map_err(|e| FireflyError::internal(format!("bad timestamp '{s}': {e}")))
}
