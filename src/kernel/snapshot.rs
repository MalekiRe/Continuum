use serde::{Deserialize, Serialize};

/// Snapshot configuration for rotation (used by kernel.snapshot()).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    pub snapshot_dir: String,
    pub max_full_snapshots: usize,
    pub max_incremental_snapshots: usize,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        SnapshotConfig {
            snapshot_dir: "snapshots".into(),
            max_full_snapshots: 10,
            max_incremental_snapshots: 100,
        }
    }
}
