
use serde::{Deserialize, Serialize};

/// A summary of a range of events, rebuildable from raw history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSummary {
    /// First event ID covered.
    pub from_id: u64,
    /// Last event ID covered.
    pub to_id: u64,
    /// Human-readable summary text.
    pub summary: String,
    /// Source event IDs that this summary was built from.
    pub source_ids: Vec<u64>,
    /// Level of detail (0 = coarse, 1 = fine).
    pub detail_level: u8,
}

/// The compaction manager builds and caches summary trees over event history.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CompactionManager {
    /// Cached summaries, ordered by event range.
    pub summaries: Vec<EventSummary>,
    /// Raw events are authoritative even if summaries exist.
    pub raw_events_path: String,
    /// Threshold for coarse summary (number of events).
    pub coarse_threshold: u64,
    /// Threshold for fine summary.
    pub fine_threshold: u64,
}

impl CompactionManager {
    pub fn new(raw_events_path: &str) -> Self {
        CompactionManager {
            summaries: Vec::new(),
            raw_events_path: raw_events_path.to_string(),
            coarse_threshold: 1000,
            fine_threshold: 100,
        }
    }

    /// Build or update summaries for recent events.
    pub fn compact(&mut self, latest_id: u64, _events_text: &str) -> EventSummary {
        let from_id = self.summaries.last()
            .map(|s| s.to_id + 1)
            .unwrap_or(1);

        if from_id > latest_id {
            // Nothing new to compact
            return EventSummary {
                from_id,
                to_id: latest_id,
                summary: "no new events".into(),
                source_ids: vec![],
                detail_level: 0,
            };
        }

        // Generate a summary from the raw event text
        // In production, this would call the model to generate a summary
        let summary_text = format!(
            "{} events from ID {} to {}",
            latest_id - from_id + 1,
            from_id,
            latest_id
        );

        let source_ids: Vec<u64> = (from_id..=latest_id).collect();
        let detail_level = if latest_id - from_id < self.fine_threshold { 1 } else { 0 };

        let summary = EventSummary {
            from_id,
            to_id: latest_id,
            summary: summary_text,
            source_ids,
            detail_level,
        };

        self.summaries.push(summary.clone());
        summary
    }

    /// Find which summary covers a given event ID.
    pub fn find_summary_for(&self, event_id: u64) -> Option<&EventSummary> {
        self.summaries.iter().find(|s| s.from_id <= event_id && event_id <= s.to_id)
    }

    /// Zoom into a specific summary, returning its source IDs.
    pub fn source_ids_for(&self, summary_from: u64) -> Option<&[u64]> {
        self.summaries.iter()
            .find(|s| s.from_id == summary_from)
            .map(|s| s.source_ids.as_slice())
    }

    /// Delete all summaries (safe — raw history is never lost).
    pub fn clear_summaries(&mut self) {
        self.summaries.clear();
    }

    pub fn summary_count(&self) -> usize {
        self.summaries.len()
    }
}
