
pub mod snapshot;
pub mod native;
pub mod scheduler;
pub mod event_log;
pub mod compaction;
pub mod model;

use crate::vm::env::EnvRef;
use crate::vm::eval;
use crate::vm::value::Value;
use crate::vm::value::Function;
use serde::{Deserialize, Serialize};
use event_log::{EventLog, EventKind};
use std::sync::{OnceLock, Mutex};

/// Kernel pointer wrapper that is Send+Sync (single-threaded REPL usage).
pub struct KernelPtr(*mut Kernel);
unsafe impl Send for KernelPtr {}
unsafe impl Sync for KernelPtr {}

/// Global reference to the running kernel, for natives that need kernel access.
static KERNEL_HOOK: OnceLock<Mutex<KernelPtr>> = OnceLock::new();

/// Set the global kernel hook (called during initialization).
pub fn set_kernel_hook(k: &mut Kernel) {
    let ptr = KernelPtr(k as *mut Kernel);
    KERNEL_HOOK.get_or_init(|| Mutex::new(ptr));
}

/// Get access to the kernel from a native function.
pub fn with_kernel<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&mut Kernel) -> Result<R, String>,
{
    let lock = KERNEL_HOOK.get().ok_or_else(|| "kernel hook not set".to_string())?;
    let ptr = lock.lock().map_err(|e| format!("lock error: {}", e))?.0;
    if ptr.is_null() {
        return Err("kernel hook is null".to_string());
    }
    let kernel = unsafe { &mut *ptr };
    f(kernel)
}

/// The Persistent Agent Lisp Harness Kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kernel {
    pub env: EnvRef,
    pub frames: Vec<Frame>,
    pub storage: SnapshotConfig,
    pub event_counter: u64,
    pub next_frame_id: u64,
    pub version: String,
    /// Path for the event log (not serialized; restored from config).
    #[serde(skip)]
    pub event_log_path: String,
    /// In-memory compaction manager (rebuilt from event log).
    #[serde(skip)]
    pub compaction: compaction::CompactionManager,
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
    Suspended,
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
            storage: SnapshotConfig::default(),
            event_counter: 0,
            next_frame_id: 1,
            version: "0.1.0".into(),
            event_log_path: "data/event.log".into(),
            compaction: compaction::CompactionManager::new("data/event.log"),
        };

        kernel.register_natives();

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
            },
        };
        kernel.frames.push(root_frame);
        kernel.next_frame_id += 1;

        // Create data directory
        let _ = std::fs::create_dir_all("data");
        let _ = std::fs::create_dir_all("snapshots");

        // Set up kernel hook for system-level natives
        // (We create a temporary reference that lives for the kernel's lifetime)

        kernel
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, eval::EvalError> {
        // Record event
        self.record_event(EventKind::EvalRequest { source: source.to_string() }, self.current_frame_id());

        // Snapshot before evaluation (REPL boundary — save quiescent state)
        self.snapshot(SnapshotKind::Incremental);

        // Intercept kernel-native calls before regular evaluation
        if let Some(result) = self.try_kernel_intercept(source) {
            // Snapshot after kernel intercept
            self.snapshot(SnapshotKind::Incremental);
            return result;
        }

        let result = eval::eval(source, &mut self.env);

        match &result {
            Ok(val) => {
                self.record_event(EventKind::EvalResult {
                    value: val.to_string(),
                    success: true,
                }, self.current_frame_id());
            }
            Err(e) => {
                self.record_event(EventKind::EvalResult {
                    value: e.to_string(),
                    success: false,
                }, self.current_frame_id());
            }
        }

        // Snapshot after evaluation (another REPL boundary)
        self.snapshot(SnapshotKind::Incremental);

        result
    }

    /// Try to intercept and handle kernel-level calls.
    fn try_kernel_intercept(&mut self, source: &str) -> Option<Result<Value, eval::EvalError>> {
        let source = source.trim();

        // (system/snapshot)
        if source == "(system/snapshot)" || source == "(snapshot)" {
            let snap = self.snapshot(SnapshotKind::Incremental);
            return Some(Ok(Value::Tagged {
                family: "result".into(),
                variant: "Ok".into(),
                fields: vec![Value::string(&format!("snapshot saved: {}", snap.id))],
            }));
        }

        // (system/compact)
        if source == "(system/compact)" || source == "(compact)" {
            let summary = self.compact();
            return Some(Ok(Value::string(&format!(
                "compacted events {}..{}: {}",
                summary.from_id, summary.to_id, summary.summary
            ))));
        }

        // (system/event-log)
        if source == "(system/event-log)" || source == "(events)" {
            let events = format!("{} events recorded (latest id: {})", 
                self.event_counter, self.event_counter);
            return Some(Ok(Value::string(&events)));
        }

        // (inspect/namespaces)
        if source == "(inspect/namespaces)" || source == "(bindings)" {
            let ns_list: Vec<Value> = self.env.namespace_names()
                .iter()
                .map(|name| {
                    let count = self.env.namespaces.get(name)
                        .map(|ns| ns.list_bindings().len())
                        .unwrap_or(0);
                    Value::list(vec![
                        Value::symbol(name),
                        Value::int(count as i64),
                    ])
                })
                .collect();
            return Some(Ok(Value::List(ns_list)));
        }

        // (inspect/history name)
        if source.starts_with("(inspect/history") || source.starts_with("(history") {
            // Parse the name argument
            if let Some(name) = source.split_whitespace().nth(1) {
                let name = name.trim_end_matches(')');
                let qualified = if name.contains('/') { name.to_string() } else { format!("user/{}", name) };

                let parts: Vec<&str> = qualified.splitn(2, '/').collect();
                if parts.len() == 2 {
                    if let Some(ns) = self.env.namespaces.get(parts[0]) {
                        let records = ns.history(parts[1]);
                        if records.is_empty() {
                            return Some(Ok(Value::string("no history")));
                        }
                        let entries: Vec<Value> = records.iter().map(|r| {
                            Value::list(vec![
                                Value::string(&r.timestamp),
                                Value::int(r.version as i64),
                                Value::string(&format!("{}", r.value)),
                            ])
                        }).collect();
                        return Some(Ok(Value::List(entries)));
                    }
                }
                return Some(Err(eval::EvalError::UndefinedSymbol(name.to_string())));
            }
        }

        None
    }

    /// Evaluate Lisp in a read-eval-print loop, returning the result as a display string.
    pub fn eval_repl(&mut self, source: &str) -> String {
        match self.eval(source) {
            Ok(val) => format!("{}", val),
            Err(e) => format!("error: {}", e),
        }
    }

    /// Record an event in the append-only log.
    fn record_event(&mut self, kind: EventKind, frame_id: Option<String>) -> u64 {
        self.event_counter += 1;
        let id = self.event_counter;

        // Try to write to event log file
        let event = serde_json::json!({
            "id": id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "kind": kind,
            "frame_id": frame_id,
        });

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.event_log_path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{}", serde_json::to_string(&event).unwrap_or_default());
        }

        id
    }

    fn current_frame_id(&self) -> Option<String> {
        self.frames.last().map(|f| f.id.clone())
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

        // Record event
        self.record_event(EventKind::AgentCall {
            child_name: name.to_string(),
            request: request.to_string(),
        }, self.current_frame_id());

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

            self.record_event(
                EventKind::AgentReturn { value: value.to_string() },
                frame.parent_id,
            );
        }
    }

    /// Deliver a human message as an interrupt to the current frame.
    pub fn human_message(&mut self, text: &str) {
        if let Some(frame) = self.frames.last_mut() {
            frame.pending_message = Some(text.to_string());
            frame.message_queue.push(text.to_string());
            if frame.status == FrameStatus::Waiting {
                frame.status = FrameStatus::Running;
            }
        }

        self.record_event(
            EventKind::HumanMessage { text: text.to_string(), sender: "human".into() },
            self.current_frame_id(),
        );
    }

    /// Compact the event log into summaries.
    pub fn compact(&mut self) -> compaction::EventSummary {
        let latest = self.event_counter;
        let events_text = format!("{} events up to {}", latest, chrono::Utc::now().to_rfc3339());
        let summary = self.compaction.compact(latest, &events_text);

        self.record_event(
            EventKind::Compact {
                summary: summary.summary.clone(),
                covered_events: (summary.from_id, summary.to_id),
            },
            None,
        );

        summary
    }

    /// Perform a snapshot.
    pub fn snapshot(&mut self, kind: SnapshotKind) -> Snapshot {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let id = format!("snap-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f"));

        // Record snapshot event before serializing
        self.record_event(
            EventKind::Snapshot {
                kind: format!("{:?}", kind),
                id: id.clone(),
            },
            None,
        );

        let kernel_bytes = serde_json::to_vec(&self).unwrap_or_default();
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
        use std::path::Path;

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

        let mut kernel: Kernel = serde_json::from_slice(&bytes)
            .map_err(|e| format!("cannot deserialize kernel: {}", e))?;

        // Re-register native function pointers (they can't survive serialization)
        kernel.register_natives();

        println!("[kernel] recovered from {} (natives re-registered)", latest_path.display());
        Ok(kernel)
    }

    // ---- Registration ----

    pub fn register_natives(&mut self) {
        use crate::lisp_fn;
        // Arithmetic
        self.define_native("kernel/+", 2, |args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
                _ => Err("+: expected numbers".into()),
            }
        });

        self.define_native("kernel/-", 2, |args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 - b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - b as f64)),
                _ => Err("-: expected numbers".into()),
            }
        });

        self.define_native("kernel/*", 2, |args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 * b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * b as f64)),
                _ => Err("*: expected numbers".into()),
            }
        });

        self.define_native("kernel//", 2, |args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => {
                    if b == 0 { Err("/: division by zero".into()) } else { Ok(Value::Float(a as f64 / b as f64)) }
                }
                (Value::Float(a), Value::Float(b)) => {
                    if b == 0.0 { Err("/: division by zero".into()) } else { Ok(Value::Float(a / b)) }
                }
                _ => Err("/: expected numbers".into()),
            }
        });

        self.define_native("kernel/=", 2, |args| {
            Ok(Value::Bool(args[0] == args[1]))
        });

        self.define_native("kernel/<", 2, |args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                _ => Err("<: expected numbers".into()),
            }
        });

        self.define_native("kernel/>", 2, |args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                _ => Err(">: expected numbers".into()),
            }
        });

        self.define_native("kernel/cons", 2, |args| {
            let car = args[0].clone();
            let cdr = args[1].clone();
            match cdr {
                Value::List(mut items) => {
                    let mut new_list = vec![car];
                    new_list.append(&mut items);
                    Ok(Value::List(new_list))
                }
                Value::Nil => Ok(Value::List(vec![car])),
                _ => Err("cons: second argument must be a list".into()),
            }
        });

        self.define_native("kernel/car", 1, |args| {
            match &args[0] {
                Value::List(items) => items.first().cloned().ok_or_else(|| "car: empty list".into()),
                _ => Err("car: expected list".into()),
            }
        });

        self.define_native("kernel/cdr", 1, |args| {
            match &args[0] {
                Value::List(items) if items.len() >= 2 => Ok(Value::List(items[1..].to_vec())),
                Value::List(_) => Ok(Value::Nil),
                _ => Err("cdr: expected list".into()),
            }
        });

        self.define_native("kernel/list", 0, |args| {
            Ok(Value::List(args.to_vec()))
        });

        self.define_native("kernel/display", 1, |args| {
            print!("{}", args[0]);
            Ok(args[0].clone())
        });

        self.define_native("kernel/println", 1, |args| {
            println!("{}", args[0]);
            Ok(args[0].clone())
        });

        self.define_native("kernel/read", 0, |_args| {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)
                .map_err(|e| format!("read error: {}", e))?;
            Ok(Value::string(input.trim()))
        });

        // Type predicates
        self.define_native("kernel/nil?", 1, |args| {
            Ok(Value::Bool(matches!(args[0], Value::Nil)))
        });
        self.define_native("kernel/number?", 1, |args| {
            Ok(Value::Bool(matches!(args[0], Value::Int(_) | Value::Float(_))))
        });
        self.define_native("kernel/symbol?", 1, |args| {
            Ok(Value::Bool(matches!(args[0], Value::Symbol(_))))
        });
        self.define_native("kernel/string?", 1, |args| {
            Ok(Value::Bool(matches!(args[0], Value::String(_))))
        });
        self.define_native("kernel/list?", 1, |args| {
            Ok(Value::Bool(args[0].is_list()))
        });
        self.define_native("kernel/function?", 1, |args| {
            Ok(Value::Bool(matches!(args[0], Value::Function(_))))
        });
        self.define_native("kernel/keyword?", 1, |args| {
            Ok(Value::Bool(matches!(args[0], Value::Keyword(_))))
        });

        // Control
        self.define_native("control/Continue", 0, |_args| {
            Ok(Value::Keyword("Continue".to_string()))
        });
        self.define_native("control/Wait", 0, |_args| {
            Ok(Value::Keyword("Wait".to_string()))
        });
        self.define_native("control/Return", 0, |_args| {
            Ok(Value::Keyword("Return".to_string()))
        });
        self.define_native("control/CancelCurrent", 1, |args| {
            Ok(Value::Tagged {
                family: "control".into(),
                variant: "CancelCurrent".into(),
                fields: args.to_vec(),
            })
        });
        self.define_native("control/Error", 1, |args| {
            let msg = format!("{}", args[0]);
            Err(msg)
        });

        // System
        self.define_native("system/version", 0, |_args| {
            Ok(Value::string("persistent-lisp-harness/0.1.0"))
        });
        self.define_native("system/clock", 0, |_args| {
            Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
        });
        self.define_native("system/snapshot", 0, |_args| {
            with_kernel(|k| {
                let snap = k.snapshot(SnapshotKind::Incremental);
                Ok(Value::string(&format!("snapshot saved: {}", snap.id)))
            })
        });
        self.define_native("system/compact", 0, |_args| {
            with_kernel(|k| {
                let summary = k.compact();
                Ok(Value::string(&format!(
                    "compacted events {}..{}: {}",
                    summary.from_id, summary.to_id, summary.summary
                )))
            })
        });
        self.define_native("system/event-log", 0, |_args| {
            with_kernel(|k| {
                Ok(Value::string(&format!(
                    "{} events recorded (latest id: {})",
                    k.event_counter, k.event_counter
                )))
            })
        });
        self.define_native("inspect/namespaces", 0, |_args| {
            with_kernel(|k| {
                let names: Vec<Value> = k.env.namespace_names().iter().map(|n| {
                    let count = k.env.namespaces.get(n)
                        .map(|ns| ns.list_bindings().len())
                        .unwrap_or(0);
                    Value::list(vec![Value::symbol(n), Value::int(count as i64)])
                }).collect();
                Ok(Value::List(names))
            })
        });
        self.define_native("inspect/bindings", 1, |args| {
            let ns_name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                _ => return Err("inspect/bindings: expected symbol".into()),
            };
            with_kernel(|k| {
                let bindings = k.inspect_namespace(&ns_name)
                    .unwrap_or_default();
                let items: Vec<Value> = bindings.iter().map(|b| Value::symbol(b)).collect();
                Ok(Value::List(items))
            })
        });
        self.define_native("inspect/history", 1, |args| {
            let name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                _ => return Err("inspect/history: expected symbol".into()),
            };
            with_kernel(|k| {
                let qualified = if name.contains('/') { name.clone() } else { format!("user/{}", name) };
                let parts: Vec<&str> = qualified.splitn(2, '/').collect();
                if parts.len() == 2 {
                    if let Some(ns) = k.env.namespaces.get(parts[0]) {
                        let records = ns.history(parts[1]);
                        if records.is_empty() {
                            return Ok(Value::string("no history"));
                        }
                        let entries: Vec<Value> = records.iter().map(|r| {
                            Value::list(vec![
                                Value::string(&r.timestamp),
                                Value::int(r.version as i64),
                                Value::string(&format!("{}", r.value)),
                            ])
                        }).collect();
                        return Ok(Value::List(entries));
                    }
                }
                Err(format!("no history for {}", name))
            })
        });
    }

    fn define_native(&mut self, qualified_name: &str, arity: u32, func: fn(Vec<Value>) -> Result<Value, String>) {
        let val = Value::Function(Function::Native {
            name: qualified_name.to_string(),
            arity,
            func,
            authority: vec![],
        });
        self.env.force_define(qualified_name, val);
    }

    pub fn inspect_namespace(&self, name: &str) -> Option<Vec<String>> {
        self.env.namespaces.get(name).map(|ns| ns.list_bindings())
    }

    pub fn find_bindings(&self, query: &str) -> Vec<String> {
        let q = query.to_lowercase();
        let mut results = Vec::new();
        for (ns_name, ns) in &self.env.namespaces {
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
