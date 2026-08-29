
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Write, BufReader, BufRead};
use std::path::Path;

/// A logged event in the append-only event store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedEvent {
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
    AgentCall { child_name: String, request: String },
    AgentReturn { value: String },
    HumanMessage { text: String, sender: String },
    HumanInterrupt { text: String },
    Snapshot { kind: String, id: String },
    Restart { kind: String, downtime_secs: u64 },
    Supervise { action: String, reason: String },
    Compact { summary: String, covered_events: std::ops::Range<u64> },
    FrameCreated { frame_id: String, name: String, parent_id: Option<String> },
    FrameCompleted { frame_id: String, result: String },
}

/// Append-only event store backed by a JSON-lines file.
#[derive(Debug, Clone)]
pub struct EventLog {
    path: String,
    next_id: u64,
    buffer: Vec<LoggedEvent>,
    auto_flush: usize,
}

impl EventLog {
    pub fn new(path: &str) -> Result<Self, String> {
        fs::create_dir_all(Path::new(path).parent().unwrap_or(Path::new(".")))
            .map_err(|e| format!("cannot create log dir: {}", e))?;

        // Count existing events to set next_id
        let count = if Path::new(path).exists() {
            let file = fs::File::open(path).map_err(|e| format!("cannot open log: {}", e))?;
            BufReader::new(file).lines().count()
        } else {
            0
        };

        Ok(EventLog {
            path: path.to_string(),
            next_id: count as u64 + 1,
            buffer: Vec::new(),
            auto_flush: 100,
        })
    }

    pub fn record(&mut self, kind: EventKind, frame_id: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let event = LoggedEvent {
            id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            kind,
            frame_id,
        };

        // Write to log file immediately
        let line = serde_json::to_string(&event).unwrap_or_default();
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{}", line);
        }

        self.buffer.push(event);

        // Trim buffer when it grows too large
        if self.buffer.len() > 10000 {
            self.buffer.drain(0..5000);
        }

        id
    }

    /// Read back a range of events from the log file.
    pub fn read_range(&self, from: u64, to: u64) -> Result<Vec<LoggedEvent>, String> {
        let file = fs::File::open(&self.path)
            .map_err(|e| format!("cannot open log: {}", e))?;
        let reader = BufReader::new(file);

        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("read error: {}", e))?;
            if let Ok(event) = serde_json::from_str::<LoggedEvent>(&line) {
                if event.id >= from && event.id <= to {
                    events.push(event);
                }
                if event.id > to {
                    break;
                }
            }
        }
        Ok(events)
    }

    /// Read a specific event by ID.
    pub fn read_by_id(&self, id: u64) -> Result<Option<LoggedEvent>, String> {
        let events = self.read_range(id, id)?;
        Ok(events.into_iter().next())
    }

    /// Search events by kind and text content.
    pub fn search(&self, query: &str) -> Result<Vec<LoggedEvent>, String> {
        let q = query.to_lowercase();
        let file = fs::File::open(&self.path)
            .map_err(|e| format!("cannot open log: {}", e))?;
        let reader = BufReader::new(file);

        let mut results = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| format!("read error: {}", e))?;
            if line.to_lowercase().contains(&q) {
                if let Ok(event) = serde_json::from_str::<LoggedEvent>(&line) {
                    results.push(event);
                }
            }
        }
        Ok(results)
    }

    pub fn latest_id(&self) -> u64 {
        self.next_id - 1
    }
}
