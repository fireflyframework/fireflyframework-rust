//! Pure SQL/DDL strings, identifier validation, DSN normalisation, and
//! the advisory-lock key fold — everything the Postgres broker needs
//! that is exercisable without a live database.

use sha2::{Digest, Sha256};

/// DDL for the append-only outbox table, ported verbatim from pyfly
/// (`pyfly_eda_outbox` -> `firefly_eda_outbox`).
///
/// A monotonic `BIGSERIAL` `id` is the cursor space; `payload` and
/// `headers` are `JSONB`; the `(destination, id)` index backs the
/// destination-filtered drain query.
pub(crate) const DDL_OUTBOX: &str = "\
CREATE TABLE IF NOT EXISTS firefly_eda_outbox (
    id          BIGSERIAL PRIMARY KEY,
    destination TEXT NOT NULL,
    event_type  TEXT NOT NULL,
    payload     JSONB NOT NULL,
    headers     JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS firefly_eda_outbox_dest_idx
    ON firefly_eda_outbox (destination, id);
";

/// DDL for the per-consumer-group cursor table, ported verbatim from
/// pyfly (`pyfly_eda_offsets` -> `firefly_eda_offsets`).
pub(crate) const DDL_OFFSETS: &str = "\
CREATE TABLE IF NOT EXISTS firefly_eda_offsets (
    consumer_group TEXT PRIMARY KEY,
    last_event_id  BIGINT NOT NULL DEFAULT 0,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
";

/// `INSERT` that appends an event to the outbox and returns its id.
// `$3::text::jsonb` (not `$3::jsonb`): the inner `::text` pins the BIND type
// of the parameter to `text` so tokio-postgres serializes the Rust `String`
// as text, then Postgres casts text -> jsonb. With a bare `$3::jsonb`, the
// server infers the parameter's own type as `jsonb`, and tokio-postgres
// (built without the serde_json feature) cannot serialize a `String` as jsonb
// -> "error serializing parameter". The read path already uses `payload::text`.
pub(crate) const INSERT_OUTBOX: &str = "\
INSERT INTO firefly_eda_outbox (destination, event_type, payload, headers)
VALUES ($1, $2, $3::text::jsonb, $4::text::jsonb)
RETURNING id";

/// Seeds the offset row for a consumer group (idempotent).
pub(crate) const INSERT_OFFSET: &str = "\
INSERT INTO firefly_eda_offsets (consumer_group, last_event_id)
VALUES ($1, 0)
ON CONFLICT (consumer_group) DO NOTHING";

/// Reads the committed cursor for a consumer group.
pub(crate) const SELECT_OFFSET: &str =
    "SELECT last_event_id FROM firefly_eda_offsets WHERE consumer_group = $1";

/// Fetches the next batch of events for a destination-filtered group.
pub(crate) const SELECT_BATCH_FILTERED: &str = "\
SELECT id, destination, event_type, payload::text, headers::text, \
to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')
FROM firefly_eda_outbox
WHERE id > $1 AND destination = ANY($2)
ORDER BY id
LIMIT 100";

/// Fetches the next batch of events for a group with no destination
/// filter (all destinations).
pub(crate) const SELECT_BATCH_ALL: &str = "\
SELECT id, destination, event_type, payload::text, headers::text, \
to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')
FROM firefly_eda_outbox
WHERE id > $1
ORDER BY id
LIMIT 100";

/// Advances the cursor, guarded so a slower drainer can never rewind a
/// faster one (`last_event_id < $1`).
pub(crate) const UPDATE_OFFSET: &str = "\
UPDATE firefly_eda_offsets
SET last_event_id = $1, updated_at = now()
WHERE consumer_group = $2 AND last_event_id < $1";

/// Error raised when a channel identifier is unsafe to interpolate into
/// a `NOTIFY` statement (which cannot take bind parameters).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("firefly/eda-postgres: invalid identifier: {0:?}")]
pub struct IdentError(pub String);

/// Validates a Postgres identifier (the `NOTIFY` channel name) so it can
/// be interpolated safely — `NOTIFY` does not accept bind parameters.
///
/// Accepts the conventional unquoted-identifier shape
/// `^[A-Za-z_][A-Za-z0-9_]*$` and returns it unchanged; anything else
/// (spaces, `;`, quotes) is rejected with [`IdentError`]. This is the
/// Rust port of pyfly's `_quote_ident`.
///
/// ```
/// use firefly_eda_postgres::quote_ident;
/// assert_eq!(quote_ident("firefly_eda").unwrap(), "firefly_eda");
/// assert!(quote_ident("bad channel").is_err());
/// ```
pub fn quote_ident(name: &str) -> Result<&str, IdentError> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if valid {
        Ok(name)
    } else {
        Err(IdentError(name.to_string()))
    }
}

/// Folds a consumer-group name to the stable signed-64-bit key
/// `pg_try_advisory_lock` expects — the Rust port of pyfly's
/// `_group_lock_key`.
///
/// pyfly hashes the group with SHA-256, reads the first 8 bytes
/// big-endian as an *unsigned* 64-bit integer, then folds into the
/// signed range by subtracting `2^64` when the value is `>= 2^63`. That
/// fold is exactly the two's-complement reinterpretation of the same
/// bytes, so [`i64::from_be_bytes`] over the first 8 digest bytes yields
/// the identical value — two workers configured with the same group land
/// on the same lock key deterministically across replicas and restarts.
///
/// ```
/// use firefly_eda_postgres::group_lock_key;
/// assert_eq!(group_lock_key("workers"), group_lock_key("workers"));
/// assert_ne!(group_lock_key("workers"), group_lock_key("other"));
/// ```
#[must_use]
pub fn group_lock_key(group: &str) -> i64 {
    let digest = Sha256::digest(group.as_bytes());
    let bytes: [u8; 8] = digest[..8].try_into().expect("sha256 is 32 bytes");
    i64::from_be_bytes(bytes)
}

/// Strips SQLAlchemy-style dialect markers (`postgresql+asyncpg://`,
/// `postgresql+psycopg://`, `postgres+asyncpg://`) from a DSN so the
/// bare `postgresql://` scheme remains — the Rust port of pyfly's
/// `_normalise_dsn`. Connection strings without a marker pass through
/// unchanged.
///
/// ```
/// use firefly_eda_postgres::normalise_dsn;
/// assert_eq!(
///     normalise_dsn("postgresql+asyncpg://u:p@h/db"),
///     "postgresql://u:p@h/db"
/// );
/// assert_eq!(normalise_dsn("host=db user=app"), "host=db user=app");
/// ```
#[must_use]
pub fn normalise_dsn(dsn: &str) -> String {
    for marker in [
        "postgresql+asyncpg://",
        "postgresql+psycopg://",
        "postgres+asyncpg://",
    ] {
        if let Some(rest) = dsn.strip_prefix(marker) {
            return format!("postgresql://{rest}");
        }
    }
    dsn.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- quote_ident: pyfly test_valid_identifier_accepted / channel_validated.

    #[test]
    fn valid_identifiers_accepted() {
        assert_eq!(quote_ident("firefly_eda").unwrap(), "firefly_eda");
        assert_eq!(quote_ident("Firefly_Eda_123").unwrap(), "Firefly_Eda_123");
        assert_eq!(quote_ident("_underscore").unwrap(), "_underscore");
    }

    #[test]
    fn unsafe_identifiers_rejected() {
        assert!(quote_ident("bad channel").is_err());
        assert!(quote_ident("x;DROP TABLE").is_err());
        assert!(quote_ident("1leading_digit").is_err());
        assert!(quote_ident("").is_err());
        assert!(quote_ident("has-dash").is_err());
        assert!(quote_ident("quote\"d").is_err());
    }

    // --- group_lock_key: pyfly TestGroupLockKey.

    #[test]
    fn same_group_yields_same_key() {
        assert_eq!(
            group_lock_key("flydocs-workers"),
            group_lock_key("flydocs-workers")
        );
    }

    #[test]
    fn different_groups_yield_different_keys() {
        assert_ne!(
            group_lock_key("flydocs-workers"),
            group_lock_key("flydocs-bbox-workers")
        );
    }

    #[test]
    fn fits_in_signed_bigint() {
        // i64 by construction; the round-trip vs the pyfly fold is the
        // real check below. Exercise a spread of group names anyway.
        for group in ["a", "flydocs-workers", "very-long-group-name-with-suffix"] {
            let _key = group_lock_key(group);
        }
    }

    #[test]
    fn fold_matches_pyfly_unsigned_then_subtract() {
        // Reproduce pyfly's exact arithmetic: read 8 bytes big-endian as
        // u64, subtract 2^64 when >= 2^63, and confirm it equals our
        // i64::from_be_bytes fold for a value that lands in the negative
        // (high-bit-set) range.
        for group in ["a", "workers", "x", "flydocs-bbox-workers", "default"] {
            let digest = Sha256::digest(group.as_bytes());
            let bytes: [u8; 8] = digest[..8].try_into().unwrap();
            let raw = u64::from_be_bytes(bytes);
            let pyfly = if raw >= 1u64 << 63 {
                (raw as i128 - (1i128 << 64)) as i64
            } else {
                raw as i64
            };
            assert_eq!(group_lock_key(group), pyfly, "group {group:?}");
        }
    }

    // --- normalise_dsn: pyfly test_normalise_dsn_strips_dialect_markers.

    #[test]
    fn normalise_dsn_strips_dialect_markers() {
        assert_eq!(
            normalise_dsn("postgresql+asyncpg://u:p@h:5432/db"),
            "postgresql://u:p@h:5432/db"
        );
        assert_eq!(
            normalise_dsn("postgresql+psycopg://u:p@h/db"),
            "postgresql://u:p@h/db"
        );
        assert_eq!(
            normalise_dsn("postgres+asyncpg://u:p@h/db"),
            "postgresql://u:p@h/db"
        );
        assert_eq!(
            normalise_dsn("postgresql://u:p@h/db"),
            "postgresql://u:p@h/db"
        );
    }

    #[test]
    fn normalise_dsn_passes_keyword_dsn_through() {
        // tokio-postgres keyword/value DSNs have no scheme to strip.
        assert_eq!(
            normalise_dsn("host=db user=app dbname=app"),
            "host=db user=app dbname=app"
        );
    }

    // --- DDL/SQL string golden tests (port pyfly's verbatim DDL).

    #[test]
    fn outbox_ddl_matches_pyfly_schema() {
        assert!(DDL_OUTBOX.contains("CREATE TABLE IF NOT EXISTS firefly_eda_outbox"));
        assert!(DDL_OUTBOX.contains("id          BIGSERIAL PRIMARY KEY"));
        assert!(DDL_OUTBOX.contains("destination TEXT NOT NULL"));
        assert!(DDL_OUTBOX.contains("event_type  TEXT NOT NULL"));
        assert!(DDL_OUTBOX.contains("payload     JSONB NOT NULL"));
        assert!(DDL_OUTBOX.contains("headers     JSONB NOT NULL DEFAULT '{}'::jsonb"));
        assert!(DDL_OUTBOX.contains("created_at  TIMESTAMPTZ NOT NULL DEFAULT now()"));
        assert!(DDL_OUTBOX.contains("CREATE INDEX IF NOT EXISTS firefly_eda_outbox_dest_idx"));
        assert!(DDL_OUTBOX.contains("ON firefly_eda_outbox (destination, id)"));
    }

    #[test]
    fn offsets_ddl_matches_pyfly_schema() {
        assert!(DDL_OFFSETS.contains("CREATE TABLE IF NOT EXISTS firefly_eda_offsets"));
        assert!(DDL_OFFSETS.contains("consumer_group TEXT PRIMARY KEY"));
        assert!(DDL_OFFSETS.contains("last_event_id  BIGINT NOT NULL DEFAULT 0"));
        assert!(DDL_OFFSETS.contains("updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()"));
    }

    #[test]
    fn publish_sql_inserts_and_returns_id() {
        assert!(INSERT_OUTBOX.contains("INSERT INTO firefly_eda_outbox"));
        assert!(INSERT_OUTBOX.contains("(destination, event_type, payload, headers)"));
        assert!(INSERT_OUTBOX.contains("VALUES ($1, $2, $3::text::jsonb, $4::text::jsonb)"));
        assert!(INSERT_OUTBOX.contains("RETURNING id"));
    }

    #[test]
    fn offset_seed_is_idempotent() {
        assert!(INSERT_OFFSET.contains("INSERT INTO firefly_eda_offsets"));
        assert!(INSERT_OFFSET.contains("ON CONFLICT (consumer_group) DO NOTHING"));
    }

    #[test]
    fn batch_queries_order_by_id_and_limit() {
        assert!(SELECT_BATCH_ALL.contains("WHERE id > $1"));
        assert!(SELECT_BATCH_ALL.contains("ORDER BY id"));
        assert!(SELECT_BATCH_ALL.ends_with("LIMIT 100"));
        assert!(SELECT_BATCH_FILTERED.contains("WHERE id > $1 AND destination = ANY($2)"));
        assert!(SELECT_BATCH_FILTERED.contains("ORDER BY id"));
        assert!(SELECT_BATCH_FILTERED.ends_with("LIMIT 100"));
    }

    #[test]
    fn batch_projection_casts_jsonb_and_timestamp_to_text() {
        // No optional tokio-postgres type features: JSONB and TIMESTAMPTZ
        // decode as text. Both batch queries share the same projection.
        for sql in [SELECT_BATCH_ALL, SELECT_BATCH_FILTERED] {
            assert!(sql.contains("payload::text"));
            assert!(sql.contains("headers::text"));
            assert!(sql.contains("created_at AT TIME ZONE 'UTC'"));
            assert!(sql.contains("id, destination, event_type"));
        }
    }

    #[test]
    fn update_offset_never_rewinds() {
        assert!(UPDATE_OFFSET.contains("SET last_event_id = $1, updated_at = now()"));
        assert!(UPDATE_OFFSET.contains("WHERE consumer_group = $2 AND last_event_id < $1"));
    }

    #[test]
    fn select_offset_reads_committed_cursor() {
        assert!(SELECT_OFFSET.contains("SELECT last_event_id FROM firefly_eda_offsets"));
        assert!(SELECT_OFFSET.contains("WHERE consumer_group = $1"));
    }
}
