
use crate::kernel::{Frame, FrameStatus, Kernel, FrameState};
use crate::vm::eval;
use crate::vm::value::Value;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub fn spawn_child(kernel: &mut Kernel, name: &str) -> String {
        let id = format!("frame-{}", kernel.next_frame_id);
        kernel.next_frame_id += 1;

        let parent_id = kernel.frames.last().map(|f| f.id.clone());

        let frame = Frame {
            id: id.clone(),
            name: name.to_string(),
            parent_id,
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

        kernel.frames.push(frame);
        id
    }

    /// Execute one cognition turn.
    /// Returns the outcome and whether to schedule another turn.
    pub fn execute_turn(kernel: &mut Kernel, source: &str) -> Result<TurnOutcome, eval::EvalError> {
        let result = kernel.eval(source)?;

        let outcome = match &result {
            Value::Keyword(s) if s == "Continue" => {
                TurnOutcome::Continued
            }

            Value::Tagged { family, variant, .. } if family == "control" && variant == "CancelCurrent" => {
                Self::frame_return(kernel, result);
                TurnOutcome::Cancelled
            }
            _ => {
                TurnOutcome::Continued
            }
        };

        Ok(outcome)
    }

    fn frame_return(kernel: &mut Kernel, result: Value) {
        if let Some(mut frame) = kernel.frames.pop() {
            frame.status = FrameStatus::Completed;
            // Deliver to parent frame's pending_subagent_result
            if let Some(parent) = kernel.frames.last_mut() {
                parent.state.pending_subagent_result = Some(result);
                if parent.status == FrameStatus::Waiting {
                    parent.status = FrameStatus::Running;
                }
            }
        }
    }

    /// Deliver a human interrupt.
    pub fn human_interrupt(kernel: &mut Kernel, message: &str) {
        if let Some(frame) = kernel.frames.last_mut() {
            frame.pending_message = Some(message.to_string());
            frame.message_queue.push(message.to_string());
            if frame.status == FrameStatus::Waiting {
                frame.status = FrameStatus::Running;
            }
        }
    }



    /// Fifteen-minute review: check if the current top-level call has been
    /// running too long.
    pub fn fifteen_minute_review(
        kernel: &Kernel,
        start_time: chrono::DateTime<chrono::Utc>,
        threshold_ms: u64,
    ) -> ReviewDecision {
        let elapsed = (chrono::Utc::now() - start_time).num_milliseconds() as u64;

        if elapsed >= threshold_ms {
            // Check the call stack for the current frame
            if let Some(frame) = kernel.frames.last() {
                // Simple heuristic: cancel if we see repetitive patterns
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

#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    Continued,
    Completed,
    Cancelled,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum ReviewDecision {
    NoAction,
    Continue,
    Cancel(String),
    Advice(String),
}

/// A review request for the supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub frame_id: String,
    pub elapsed_ms: u64,
    pub current_source: Option<String>,
    pub tool_calls: Vec<String>,
    pub tokens_generated: u64,
    pub tokens_waiting: u64,
}
