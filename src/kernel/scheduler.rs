use crate::kernel::{Frame, FrameStatus, Kernel, FrameState};
use crate::vm::eval;
use crate::vm::value::Value;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scheduler {
    pub max_turns_per_frame: u32,
    pub fifteen_minute_ms: u64,
    pub supervise_efficiency: bool,
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler {
            max_turns_per_frame: 1000,
            fifteen_minute_ms: 15 * 60 * 1000,
            supervise_efficiency: true,
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
            },
        };

        kernel.frames.push(frame);
        id
    }

    pub fn execute_turn(kernel: &mut Kernel, source: &str) -> Result<TurnOutcome, eval::EvalError> {
        let result = kernel.eval(source)?;

        let outcome = match &result {
            Value::Keyword(s) if s == "Continue" => {
                let _ = Self::schedule_next(kernel);
                TurnOutcome::Continued
            }
            Value::Keyword(s) if s == "Wait" => {
                if let Some(frame) = kernel.frames.last_mut() {
                    frame.status = FrameStatus::Waiting;
                }
                TurnOutcome::Waiting
            }
            Value::Keyword(s) if s == "Return" => {
                Self::frame_return(kernel, Value::Nil);
                TurnOutcome::Completed
            }
            Value::Tagged { family, variant, .. } if family == "control" && variant == "CancelCurrent" => {
                Self::frame_return(kernel, result);
                TurnOutcome::Cancelled
            }
            _ => {
                let _ = Self::schedule_next(kernel);
                TurnOutcome::Continued
            }
        };

        Ok(outcome)
    }

    fn schedule_next(_kernel: &mut Kernel) -> Result<(), String> {
        Ok(())
    }

    fn frame_return(kernel: &mut Kernel, result: Value) {
        if let Some(mut frame) = kernel.frames.pop() {
            frame.status = FrameStatus::Completed;
            if let Some(parent_id) = &frame.parent_id {
                let msg = format!("(child/returned {:?} {:?})", frame.name, result);
                let _ = msg;
                let _ = parent_id;
            }
        }
    }

    pub fn human_interrupt(kernel: &mut Kernel, message: &str) {
        if let Some(frame) = kernel.frames.last_mut() {
            frame.pending_message = Some(message.to_string());
            frame.message_queue.push(message.to_string());
            if frame.status == FrameStatus::Waiting {
                frame.status = FrameStatus::Running;
            }
        }
    }

    pub fn check_fifteen_minute_review(
        start_time: chrono::DateTime<chrono::Utc>,
        threshold_ms: u64,
    ) -> bool {
        let elapsed = (chrono::Utc::now() - start_time).num_milliseconds() as u64;
        elapsed >= threshold_ms
    }

    pub fn supervisor_advice(text: &str) -> Value {
        Value::Tagged {
            family: "supervisor".into(),
            variant: "Advice".into(),
            fields: vec![Value::string(text)],
        }
    }

    pub fn supervisor_cancel(reason: &str) -> Value {
        Value::Tagged {
            family: "supervisor".into(),
            variant: "Cancel".into(),
            fields: vec![Value::string(reason)],
        }
    }

    pub fn supervisor_no_action() -> Value {
        Value::Tagged {
            family: "supervisor".into(),
            variant: "NoAction".into(),
            fields: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    Continued,
    Waiting,
    Completed,
    Cancelled,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub frame_id: String,
    pub elapsed_ms: u64,
    pub current_source: Option<String>,
    pub tool_calls: Vec<String>,
    pub tokens_generated: u64,
    pub tokens_waiting: u64,
}
