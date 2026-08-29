use serde::{Deserialize, Serialize};

/// Kinds of events in the append-only log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    EvalRequest { source: String },
    EvalResult { value: String, success: bool },
    Define { name: String },
    Undefine { name: String },
    AgentCall { child_name: String, request: String },
    AgentReturn { value: String },
    HumanMessage { text: String, sender: String },
    HumanInterrupt { text: String },
    Snapshot { kind: String, id: String },
    Restart { kind: String, downtime_secs: u64 },
    Supervise { action: String, reason: String },
    Compact { summary: String, covered_events: (u64, u64) },
    FrameCreated { frame_id: String, name: String, parent_id: Option<String> },
    FrameCompleted { frame_id: String, result: String },
}
