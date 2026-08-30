
pub mod snapshot;
pub mod native;
use crate::vm::env::EnvRef;
use crate::vm::eval;
use crate::vm::value::Value;
use crate::vm::value::Function;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeEntry {
    pub wake_at: String,
    pub action: String,
    pub frame_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kernel {
    pub env: EnvRef,
    pub frames: Vec<Frame>,
    pub storage: SnapshotConfig,
    pub event_counter: u64,
    pub next_frame_id: u64,
    pub version: String,
    pub wake_timers: Vec<WakeEntry>,
}

/// An agent frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub status: FrameStatus,
    pub pending_message: Option<String>,
    pub message_queue: Vec<String>,
    pub state: FrameState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FrameStatus {
    Running,
    Waiting,
    Cancelled,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameState {
    pub local_bindings: Vec<(String, Value)>,
    pub current_continuation: Option<Continuation>,
    /// Stores the pending agent/call return (subagent result).
    pub pending_subagent_result: Option<Value>,
    pub cancelled_tokens: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Continuation {
    pub depth: u32,
    pub description: String,
    /// Saved source expression for the continuation.
    pub saved_source: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    pub snapshot_dir: String,
    pub full_snapshot_interval_hours: u64,
    pub last_full_snapshot: Option<String>,
    pub snapshot_count: u64,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        SnapshotConfig {
            snapshot_dir: "snapshots".into(),
            full_snapshot_interval_hours: 1,
            last_full_snapshot: None,
            snapshot_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub timestamp: String,
    pub kernel: Kernel,
    pub env: EnvRef,
    pub checksum: String,
    pub kind: SnapshotKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SnapshotKind {
    Full,
    Incremental,
}

impl Kernel {
    pub fn new() -> Self {
        let mut kernel = Kernel {
            env: EnvRef::new(),
            frames: Vec::new(),
            wake_timers: Vec::new(),
            storage: SnapshotConfig::default(),
            event_counter: 0,
            next_frame_id: 1,
            version: "0.1.0".into(),

        };

        kernel.register_tools();

        let root_frame = Frame {
            id: format!("frame-{}", kernel.next_frame_id),
            name: "root".into(),
            parent_id: None,
            status: FrameStatus::Running,
            pending_message: None,
            message_queue: Vec::new(),
            state: FrameState {
                local_bindings: Vec::new(),
                current_continuation: None,
                pending_subagent_result: None,
                cancelled_tokens: Vec::new(),
            },
        };
        kernel.frames.push(root_frame);
        kernel.next_frame_id += 1;

        // Create data directory
        let _ = std::fs::create_dir_all("data");
        let _ = std::fs::create_dir_all("snapshots");

        kernel
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, eval::EvalError> {
        let result = eval::eval(source, self);

        result
    }

/// Evaluate Lisp in a read-eval-print loop, returning the result as a display string.
    /// Checks that the source hasn't been cancelled before executing.
    pub fn eval_repl(&mut self, source: &str) -> String {
        // Check if this source matches a cancelled call
        if let Some(frame) = self.frames.last() {
            for token in &frame.state.cancelled_tokens {
                if source.trim() == token.as_str() {
                    return format!("cancelled: call was previously cancelled and will not be re-executed");
                }
            }
        }

        match self.eval(source) {
            Ok(val) => {
                // If the result was CancelCurrent, record the cancelled SOURCE
                // so the exact same call can't be re-executed.
                if let Value::Tagged { family, variant, fields: _ } = &val {
                    if family == "control" && variant == "CancelCurrent" {
                        if let Some(frame) = self.frames.last_mut() {
                            // Store the original source expression, not the reason.
                            // The reason is a human message; the source is the expression
                            // that was cancelled. Comparing source against source is correct.
                            frame.state.cancelled_tokens.push(source.to_string());
                        }
                    }
                }
                format!("{}", val)
            }
            Err(e) => format!("error: {}", e),
        }
    }

    

/// Create a child frame for a subagent call.
    /// Returns the child's frame ID.
    pub fn spawn_subagent(&mut self, name: &str, request: &str) -> Result<String, String> {
        let id = format!("frame-{}", self.next_frame_id);
        self.next_frame_id += 1;

        let parent_id = self.frames.last().map(|f| f.id.clone());

        let frame = Frame {
            id: id.clone(),
            name: name.to_string(),
            parent_id: parent_id.clone(),
            status: FrameStatus::Running,
            pending_message: None,
            message_queue: Vec::new(),
            state: FrameState {
                local_bindings: Vec::new(),
                current_continuation: None,
                pending_subagent_result: None,
                cancelled_tokens: Vec::new(),
            },
        };

        let child_source = format!(
            "(begin (define request '{}') (define parent '{}'))",
            request,
            parent_id.unwrap_or_default(),
        );

        // Push child frame, evaluate the request setup
        self.frames.push(frame);
        let _ = self.eval(&child_source);

        Ok(id)
    }

    /// Complete the current subagent frame and return its result to the parent.
    pub fn return_from_subagent(&mut self, value: Value) {
        if let Some(mut frame) = self.frames.pop() {
            frame.status = FrameStatus::Completed;

            // Deliver result to parent frame
            if let Some(parent_frame) = self.frames.last_mut() {
                parent_frame.state.pending_subagent_result = Some(value.clone());
                // If parent was waiting, wake it
                if parent_frame.status == FrameStatus::Waiting {
                    parent_frame.status = FrameStatus::Running;
                }
            }

        }
    }

    /// Deliver a human message as an interrupt to the current frame.
    pub fn human_message(&mut self, text: &str) {
        // Deliver the notice to EVERY active frame (stack notices)
        for frame in self.frames.iter_mut() {
            // Queue the notice — every frame sees it before it next thinks
            let notice = format!("(system/HumanMessage {:?})", text);
            frame.message_queue.push(notice);
            frame.pending_message = Some(text.to_string());
            if frame.status == FrameStatus::Waiting {
                frame.status = FrameStatus::Running;
            }
        }

    }

    /// Check and fire scheduled wake timers.
    pub fn check_wake_timers(&mut self) -> usize {
        let now = chrono::Utc::now().to_rfc3339();
        let mut fired = Vec::new();
        self.wake_timers.retain(|entry| {
            if entry.wake_at.as_str() <= now.as_str() {
                fired.push(entry.action.clone());
                false
            } else {
                true
            }
        });
        for action in &fired {
            if let Some(frame) = self.frames.last_mut() {
                frame.message_queue.push(action.clone());
                if frame.status == FrameStatus::Waiting {
                    frame.status = FrameStatus::Running;
                }
            }
        }
        fired.len()
    }

    

    /// Perform a snapshot.
    /// Check if an hourly full snapshot is due, and take one if so.
    pub fn check_hourly_snapshot(&mut self) {
        if let Some(ref last) = self.storage.last_full_snapshot {
            if let Ok(last_time) = chrono::DateTime::parse_from_rfc3339(last) {
                let last_utc = last_time.with_timezone(&chrono::Utc);
                let elapsed = (chrono::Utc::now() - last_utc).num_hours();
                if elapsed >= self.storage.full_snapshot_interval_hours as i64 {
                    self.snapshot(SnapshotKind::Full);
                    self.storage.last_full_snapshot = Some(chrono::Utc::now().to_rfc3339());
                }
            }
        } else {
            self.snapshot(SnapshotKind::Full);
            self.storage.last_full_snapshot = Some(chrono::Utc::now().to_rfc3339());
        }

        // Prune snapshots older than 7 days
        if let Ok(entries) = std::fs::read_dir(&self.storage.snapshot_dir) {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(7);
            for entry in entries.filter_map(|e| e.ok()) {
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(modified) = metadata.created() {
                        let modified_utc: chrono::DateTime<chrono::Utc> = modified.into();
                        if modified_utc < cutoff {
                            let path = entry.path();
                            if path.extension().map_or(false, |ext| ext == "json" || ext == "meta") {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn snapshot(&mut self, kind: SnapshotKind) -> Snapshot {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let id = format!("snap-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f"));

        // Record snapshot event before serializing

        let state = serde_json::json!({
            "kernel": self,
            "env": &self.env,
        });
        let kernel_bytes = serde_json::to_vec(&state).unwrap_or_default();
        let checksum = {
            use sha2::{Sha256, Digest};
            let mut hasher = Sha256::new();
            hasher.update(&kernel_bytes);
            hex::encode(hasher.finalize())
        };

        let snapshot = Snapshot {
            id: id.clone(),
            timestamp: timestamp.clone(),
            kernel: self.clone(),
            env: self.env.clone(),
            checksum: checksum.clone(),
            kind: kind.clone(),
        };

        self.storage.snapshot_count += 1;

        let filename = format!("{}/{}-{}.json", self.storage.snapshot_dir, kind_name(&kind), id);
        let _ = std::fs::create_dir_all(&self.storage.snapshot_dir);
        let _ = std::fs::write(&filename, &kernel_bytes);

        let meta = serde_json::json!({
            "id": id,
            "timestamp": timestamp,
            "kind": kind_name(&kind),
            "checksum": checksum,
            "size_bytes": kernel_bytes.len(),
        });
        let _ = std::fs::write(
            format!("{}/{}-{}.meta", self.storage.snapshot_dir, kind_name(&kind), id),
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        );

        snapshot
    }

    pub fn recover_from_latest() -> Result<Self, String> {

        let snapshot_dir = "snapshots";
        let _ = std::fs::create_dir_all(snapshot_dir);

        // Try to find the latest full snapshot first
        let mut full_files: Vec<_> = std::fs::read_dir(snapshot_dir)
            .map_err(|e| format!("cannot read snapshot dir: {}", e))?
            .filter_map(|entry| entry.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
            .filter(|e| {
                e.path().file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n.starts_with("full-"))
            })
            .collect();

        full_files.sort_by_key(|e| {
            e.path().file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .unwrap_or_default()
        });

        // Fall back to incremental snapshots if no full snapshot exists
        let latest_path = if let Some(full) = full_files.last() {
            full.path().clone()
        } else {
            let mut inc_files: Vec<_> = std::fs::read_dir(snapshot_dir)
                .map_err(|e| format!("cannot read snapshot dir: {}", e))?
                .filter_map(|entry| entry.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
                .filter(|e| {
                    e.path().file_name()
                        .and_then(|n| n.to_str())
                        .map_or(false, |n| n.starts_with("inc-"))
                })
                .collect();

            inc_files.sort_by_key(|e| {
                e.path().file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            });

            inc_files.last()
                .ok_or_else(|| "no snapshots found".to_string())?
                .path()
                .clone()
        };

        let bytes = std::fs::read(&latest_path)
            .map_err(|e| format!("cannot read snapshot: {}", e))?;

        let Snapshot { kernel: kernel_raw, .. } = serde_json::from_slice(&bytes)
            .map_err(|e| format!("cannot deserialize snapshot: {}", e))?;

        let mut kernel = kernel_raw;

        // Re-register native function pointers (they can't survive serialization)
        kernel.register_tools();

        // Queue (system/Restarted) event for every active frame
        let downtime = chrono::Utc::now().to_rfc3339();
        for frame in &mut kernel.frames {
            let restarted = Value::Tagged {
                family: "system".into(),
                variant: "Restarted".into(),
                fields: vec![
                    Value::keyword("unclean"),
                    Value::string(&downtime),
                ],
            };
            frame.message_queue.push(format!("(system/Restarted :kind :unclean :downtime {:?})", downtime));
            frame.pending_message = Some(format!("{}", restarted));
        }

        println!("[kernel] recovered from {} (natives re-registered, {} frames notified)", 
            latest_path.display(), kernel.frames.len());
        Ok(kernel)
    }

    // ---- Registration ----

    

    fn define_native(&mut self, qualified_name: &str, arity: u32, func: fn(&mut Kernel, Vec<Value>) -> Result<Value, String>) {
        let val = Value::Function(Function::Native {
            name: qualified_name.to_string(),
            arity,
            func,
        });
        // Qualify unqualified names with "kernel/" so force_define works.
        let full_name = if qualified_name.contains('/') {
            qualified_name.to_string()
        } else {
            format!("kernel/{}", qualified_name)
        };
        self.env.force_define(&full_name, val);
    }

    pub fn inspect_namespace(&self, name: &str) -> Option<Vec<String>> {
        self.env.namespaces.get(name).map(|ns| ns.list_bindings())
    }

    pub fn find_bindings(&self, query: &str) -> Vec<String> {
        let q = query.to_lowercase();
        let mut results = Vec::new();
        for (ns_name, ns) in self.env.namespaces.iter() {
            for binding in ns.list_bindings() {
                let qualified = format!("{}/{}", ns_name, binding);
                if qualified.to_lowercase().contains(&q) {
                    results.push(qualified);
                }
            }
        }
        results.sort();
        results
    }

    /// Get the current frame's pending human message, if any.
    pub fn take_pending_message(&mut self) -> Option<String> {
        self.frames.last_mut().and_then(|f| f.pending_message.take())
    }

    /// Take a pending subagent result.
    pub fn take_subagent_result(&mut self) -> Option<Value> {
        self.frames.last_mut().and_then(|f| f.state.pending_subagent_result.take())
    }
}
fn kind_name(kind: &SnapshotKind) -> &str {
    match kind {
        SnapshotKind::Full => "full",
        SnapshotKind::Incremental => "inc",
    }
}
