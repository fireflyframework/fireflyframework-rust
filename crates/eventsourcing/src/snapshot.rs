//! Periodic state captures: [`Snapshot`], the [`SnapshotStore`] port and
//! its in-memory default.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::aggregate::base64_bytes;
use crate::error::EventSourcingError;

/// Snapshot is a periodic state capture used to bound rehydration cost.
///
/// JSON wire format matches the Go port: `aggregateId`, `aggregateType`,
/// `version`, and a base64-encoded `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Identifier of the snapshotted aggregate.
    #[serde(rename = "aggregateId")]
    pub aggregate_id: String,
    /// Aggregate type discriminator.
    #[serde(rename = "aggregateType")]
    pub aggregate_type: String,
    /// Version of the last event folded into this capture.
    pub version: i64,
    /// Serialized aggregate state, base64-encoded on the wire like Go's
    /// `[]byte`.
    #[serde(with = "base64_bytes")]
    pub payload: Vec<u8>,
}

/// SnapshotStore persists snapshots. The default in-memory implementation
/// keeps only the latest version per aggregate.
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Returns the most recent snapshot for `aggregate_id`, or `Ok(None)`
    /// when none exists — no snapshot is a soft miss, not an error.
    async fn latest(&self, aggregate_id: &str) -> Result<Option<Snapshot>, EventSourcingError>;

    /// Stores `snapshot`, replacing any previous capture for the same
    /// aggregate.
    async fn save(&self, snapshot: Snapshot) -> Result<(), EventSourcingError>;
}

/// MemorySnapshotStore implements [`SnapshotStore`] in-process.
#[derive(Debug, Default)]
pub struct MemorySnapshotStore {
    snapshots: RwLock<HashMap<String, Snapshot>>,
}

impl MemorySnapshotStore {
    /// Returns an empty snapshot store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SnapshotStore for MemorySnapshotStore {
    async fn latest(&self, aggregate_id: &str) -> Result<Option<Snapshot>, EventSourcingError> {
        let snapshots = self.snapshots.read().expect("snapshot store lock poisoned");
        Ok(snapshots.get(aggregate_id).cloned())
    }

    async fn save(&self, snapshot: Snapshot) -> Result<(), EventSourcingError> {
        let mut snapshots = self
            .snapshots
            .write()
            .expect("snapshot store lock poisoned");
        snapshots.insert(snapshot.aggregate_id.clone(), snapshot);
        Ok(())
    }
}
