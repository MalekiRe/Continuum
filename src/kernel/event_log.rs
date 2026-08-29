
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Write, BufReader, BufRead};
use std::path::Path;

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

/// Append-only event store.
pub struct EventLog {
    path: String,
    next_id: u64,
}

impl EventLog {
    pub fn new(path: &str) -> Result<Self, String> {
        fs::create_dir_all(Path::new(path).parent().unwrap_or(Path::new(".")))
            .map_err(|e| format!("cannot create log dir: {}", e))?;

        let count = if Path::new(path).exists() {
            let file = fs::File::open(path).map_err(|e| format!("cannot open log: {}", e))?;
            BufReader::new(file).lines().count()
        } else {
            0
        };

        Ok(EventLog {
            path: path.to_string(),
            next_id: count as u64 + 1,
        })
    }

    pub fn record_event(&mut self, kind: EventKind, frame_id: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let event = serde_json::json!({
            "id": id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "kind": kind,
            "frame_id": frame_id,
        });

        let line = serde_json::to_string(&event).unwrap_or_default();
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{}", line);
        }

        id
    }

    pub fn latest_id(&self) -> u64 {
        self.next_id - 1
    }

    pub fn read_range(&self, from: u64, to: u64) -> Result<Vec<serde_json::Value>, String> {
        let file = fs::File::open(&self.path)
            .map_err(|e| format!("cannot open log: {}", e))?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("read error: {}", e))?;
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(id) = event["id"].as_u64() {
                    if id >= from && id <= to {
                        events.push(event);
                    }
                    if id > to {
                        break;
                    }
                }
            }
        }
        Ok(events)
    }

    pub fn search(&self, query: &str) -> Result<Vec<serde_json::Value>, String> {
        let q = query.to_lowercase();
        let file = fs::File::open(&self.path)
            .map_err(|e| format!("cannot open log: {}", e))?;
        let reader = BufReader::new(file);

        let mut results = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("read error: {}", e))?;
            if line.to_lowercase().contains(&q) {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                    results.push(event);
                }
            }
        }
        Ok(results)
    }
}
