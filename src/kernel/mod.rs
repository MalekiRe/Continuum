pub mod snapshot;
pub mod native;
pub mod scheduler;

use crate::vm::env::EnvRef;
use crate::vm::eval;
use crate::vm::value::Value;
use crate::vm::value::Function;
use serde::{Deserialize, Serialize};

/// The Persistent Agent Lisp Harness Kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kernel {
    pub env: EnvRef,
    pub frames: Vec<Frame>,
    pub storage: SnapshotConfig,
    pub event_counter: u64,
    pub next_frame_id: u64,
    pub version: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Continuation {
    pub depth: u32,
    pub description: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub timestamp: String,
    pub kind: EventKind,
    pub frame_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    EvalRequest { source: String },
    EvalResult { value: String, success: bool },
    Define { name: String },
    Undefine { name: String },
    AgentCall { child_name: String },
    AgentReturn { value: String },
    HumanMessage { text: String },
    HumanInterrupt { text: String },
    Snapshot { kind: SnapshotKind, id: String },
    Restart { kind: String, downtime: String },
    Supervise { action: String, reason: String },
    Compact { summary: String },
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
            },
        };
        kernel.frames.push(root_frame);
        kernel.next_frame_id += 1;

        kernel
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, eval::EvalError> {
        self.record_event(EventKind::EvalRequest { source: source.to_string() });
        let result = eval::eval(source, &mut self.env);
        match &result {
            Ok(val) => {
                self.record_event(EventKind::EvalResult {
                    value: val.to_string(),
                    success: true,
                });
            }
            Err(e) => {
                self.record_event(EventKind::EvalResult {
                    value: e.to_string(),
                    success: false,
                });
            }
        }
        result
    }

    fn register_natives(&mut self) {
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

        // Control natives
        self.define_native("control/Continue", 0, |_args| {
            Ok(Value::Keyword("Continue".to_string()))
        });

        self.define_native("control/CancelCurrent", 1, |args| {
            Ok(Value::Tagged {
                family: "control".into(),
                variant: "CancelCurrent".into(),
                fields: args.to_vec(),
            })
        });

        // System
        self.define_native("system/version", 0, |_args| {
            Ok(Value::string("persistent-lisp-harness/0.1.0"))
        });

        self.define_native("system/clock", 0, |_args| {
            Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
        });
    }

    fn define_native(&mut self, qualified_name: &str, arity: u32, func: fn(Vec<Value>) -> Result<Value, String>) {
        let val = Value::Function(Function::Native {
            name: qualified_name.to_string(),
            arity,
            func,
            authority: vec![],
        });
        let _ = self.env.force_define(qualified_name, val);
    }

    fn record_event(&mut self, kind: EventKind) {
        self.event_counter += 1;
        let _event = Event {
            id: self.event_counter,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind,
            frame_id: self.frames.last().map(|f| f.id.clone()),
        };
    }

    pub fn snapshot(&mut self, kind: SnapshotKind) -> Snapshot {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let id = format!("snap-{:06}", self.storage.snapshot_count + 1);

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

        let filename = format!("{}/{}-{}.snap", self.storage.snapshot_dir, kind_name(&kind), id);
        let _ = std::fs::create_dir_all(&self.storage.snapshot_dir);
        if let Ok(bytes) = bincode::serialize(&snapshot) {
            let _ = std::fs::write(&filename, &bytes);
            let meta = serde_json::json!({
                "id": id,
                "timestamp": timestamp,
                "kind": kind_name(&kind),
                "checksum": checksum,
                "size_bytes": bytes.len(),
            });
            let _ = std::fs::write(
                format!("{}/{}-{}.meta", self.storage.snapshot_dir, kind_name(&kind), id),
                serde_json::to_string_pretty(&meta).unwrap_or_default(),
            );
        }

        snapshot
    }

    pub fn recover_from_latest() -> Result<Self, String> {
        let snapshot_dir = "snapshots";
        let _ = std::fs::create_dir_all(snapshot_dir);

        let mut snap_files: Vec<_> = std::fs::read_dir(snapshot_dir)
            .map_err(|e| format!("cannot read snapshot dir: {}", e))?
            .filter_map(|entry| entry.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "snap"))
            .collect();

        snap_files.sort_by_key(|e| e.path().to_str().map(|s| s.to_string()).unwrap_or_default());

        let latest = snap_files.last()
            .ok_or_else(|| "no snapshots found".to_string())?;

        let bytes = std::fs::read(latest.path())
            .map_err(|e| format!("cannot read snapshot: {}", e))?;

        let snapshot: Snapshot = bincode::deserialize(&bytes)
            .map_err(|e| format!("cannot deserialize snapshot: {}", e))?;

        let kernel_bytes = serde_json::to_vec(&snapshot.kernel).unwrap_or_default();
        {
            use sha2::{Sha256, Digest};
            let mut hasher = Sha256::new();
            hasher.update(&kernel_bytes);
            let actual = hex::encode(hasher.finalize());
            if actual != snapshot.checksum {
                return Err(format!("checksum mismatch: expected {}, got {}", snapshot.checksum, actual));
            }
        }

        Ok(snapshot.kernel)
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
}

fn kind_name(kind: &SnapshotKind) -> &str {
    match kind {
        SnapshotKind::Full => "full",
        SnapshotKind::Incremental => "inc",
    }
}
