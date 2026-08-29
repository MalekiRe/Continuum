use crate::kernel::{FrameStatus, Kernel};
use chrono;

#[derive(Debug, Clone)]
pub struct Scheduler {
    pub max_turns_per_frame: u32,
    pub fifteen_minute_ms: u64,
    pub supervise_efficiency: bool,
    pub check_interval_ms: u64,
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler {
            max_turns_per_frame: 1000,
            fifteen_minute_ms: 15 * 60 * 1000,
            supervise_efficiency: true,
            check_interval_ms: 1000,
        }
    }
}

impl Scheduler {
    /// Fifteen-minute review: check if the current top-level call has been
    /// running too long.
    pub fn fifteen_minute_review(
        kernel: &Kernel,
        start_time: chrono::DateTime<chrono::Utc>,
        threshold_ms: u64,
    ) -> ReviewDecision {
        let elapsed = (chrono::Utc::now() - start_time).num_milliseconds() as u64;

        if elapsed >= threshold_ms {
            if let Some(frame) = kernel.frames.last() {
                let queue_len = frame.message_queue.len();
                if queue_len > 5 {
                    return ReviewDecision::Cancel(
                        format!("Frame '{}' has been running for {}ms with {} queued messages. Polling without progress?",
                            frame.name, elapsed, queue_len)
                    );
                }
            }
            ReviewDecision::Continue
        } else {
            ReviewDecision::NoAction
        }
    }
}

#[derive(Debug, Clone)]
pub enum ReviewDecision {
    NoAction,
    Continue,
    Cancel(String),
    Advice(String),
}
