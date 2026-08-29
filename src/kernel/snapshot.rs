use crate::kernel::{Kernel, SnapshotKind};
use sha2::{Sha256, Digest};

#[derive(Debug, Clone)]
pub struct SnapshotManager {
    pub config: SnapshotConfig,
    pub snapshot_dir: String,
    pub max_full_snapshots: usize,
    pub max_incremental_snapshots: usize,
}

#[derive(Debug, Clone)]
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

impl SnapshotManager {
    pub fn new(config: SnapshotConfig) -> Self {
        std::fs::create_dir_all(&config.snapshot_dir).ok();
        SnapshotManager {
            snapshot_dir: config.snapshot_dir.clone(),
            max_full_snapshots: config.max_full_snapshots,
            max_incremental_snapshots: config.max_incremental_snapshots,
            config,
        }
    }

    pub fn save(&self, kernel: &Kernel, kind: SnapshotKind) -> Result<String, String> {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let id = format!("snap-{:06}", chrono::Utc::now().timestamp());

        // Serialize kernel as JSON (handles all serde types correctly)
        let json_bytes = serde_json::to_vec(kernel)
            .map_err(|e| format!("kernel serialization error: {}", e))?;

        let mut hasher = Sha256::new();
        hasher.update(&json_bytes);
        let checksum = hex::encode(hasher.finalize());

        // Write kernel JSON directly (simpler than wrapping in Snapshot struct)
        let kind_str = match kind {
            SnapshotKind::Full => "full",
            SnapshotKind::Incremental => "inc",
        };

        let filename = format!("{}/{}-{}.json", self.snapshot_dir, kind_str, id);
        std::fs::write(&filename, &json_bytes)
            .map_err(|e| format!("write error: {}", e))?;

        // Write metadata
        let meta = serde_json::json!({
            "id": id,
            "timestamp": timestamp,
            "kind": kind_str,
            "checksum": checksum,
            "size_bytes": json_bytes.len(),
            "kernel_version": kernel.version,
        });

        let meta_path = format!("{}/{}-{}.meta", self.snapshot_dir, kind_str, id);
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default())
            .map_err(|e| format!("meta write error: {}", e))?;

        Ok(id)
    }

    pub fn load_latest(&self) -> Result<Kernel, String> {
        use std::path::Path;
        let snap_dir = Path::new(&self.snapshot_dir);

        let mut snap_files: Vec<_> = std::fs::read_dir(snap_dir)
            .map_err(|e| format!("cannot read snapshot dir: {}", e))?
            .filter_map(|entry| entry.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
            .filter(|e| {
                e.path().file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n.starts_with("full-") || n.starts_with("inc-"))
            })
            .collect();

        snap_files.sort_by_key(|e| {
            e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_default()
        });

        let latest = snap_files.last()
            .ok_or_else(|| "no snapshots found".to_string())?;

        let bytes = std::fs::read(latest.path())
            .map_err(|e| format!("cannot read snapshot: {}", e))?;

        let kernel: Kernel = serde_json::from_slice(&bytes)
            .map_err(|e| format!("cannot deserialize kernel: {}", e))?;

        println!("[snapshot] loaded from {}", latest.path().display());
        Ok(kernel)
    }

    pub fn list_snapshots(&self) -> Result<Vec<serde_json::Value>, String> {
        use std::path::Path;
        let snap_dir = Path::new(&self.snapshot_dir);
        let mut snapshots = Vec::new();

        for entry in std::fs::read_dir(snap_dir).map_err(|e| format!("cannot read dir: {}", e))? {
            let entry = entry.map_err(|e| format!("entry error: {}", e))?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "meta") {
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read meta: {}", e))?;
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                    snapshots.push(meta);
                }
            }
        }

        snapshots.sort_by(|a, b| {
            let a_id = a["id"].as_str().unwrap_or("");
            let b_id = b["id"].as_str().unwrap_or("");
            a_id.cmp(b_id)
        });

        Ok(snapshots)
    }
}
