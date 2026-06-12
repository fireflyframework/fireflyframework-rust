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

//! The resolved-migration value type and its checksum helper.

use sha2::{Digest, Sha256};

/// A single resolved migration, ready to apply.
///
/// Migrations are identified by their numeric `version`; the runner
/// applies them in ascending version order, each inside a transaction,
/// and records the `checksum` in the `firefly_migrations` history table
/// so later runs can detect edits to committed files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Migration {
    /// Numeric version parsed from the `V{version}__` filename prefix.
    pub version: i64,
    /// Human-readable description — the `{description}` filename segment
    /// with underscores replaced by spaces.
    pub description: String,
    /// Original filename, e.g. `V001__init.sql`.
    pub filename: String,
    /// The SQL body to execute (may contain multiple statements).
    pub sql: String,
    /// Hex-encoded SHA-256 of the SQL bytes. File-backed sources compute
    /// this; a hand-built [`SliceSource`](crate::SliceSource) entry may
    /// leave it empty and have it filled on
    /// [`list`](crate::Source::list).
    pub checksum: String,
}

/// Hex-encoded SHA-256 of `b` — the checksum stored in the history table.
pub(crate) fn checksum(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

#[cfg(test)]
mod tests {
    use super::checksum;

    #[test]
    fn checksum_is_hex_sha256() {
        // Known SHA-256 vectors.
        assert_eq!(
            checksum(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            checksum(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
