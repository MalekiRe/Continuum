use crate::executor::{ExecutionResult, Executor};
use crate::kernel::{FrameStatus, Kernel, TranscriptEntry, VmTrap};
use crate::vm::reader;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, mpsc};

static MODEL_HTTP: LazyLock<reqwest::blocking::Client> =
    LazyLock::new(reqwest::blocking::Client::new);

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub system: String,
    pub context: String,
}

pub trait ModelClient: Send + Sync {
    fn complete(&self, request: ModelRequest) -> Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct OpenRouterModel {
    pub model: String,
    pub max_tokens: u32,
    pub timeout: std::time::Duration,
}

impl Default for OpenRouterModel {
    fn default() -> Self {
        Self {
            model: std::env::var("CONTINUUM_MODEL")
                .unwrap_or_else(|_| "deepseek/deepseek-v4-flash".into()),
            max_tokens: 600,
            timeout: std::time::Duration::from_secs(60),
        }
    }
}

static MODEL_RUNNING: AtomicBool = AtomicBool::new(false);
static MODEL_INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn request_model_interrupt() -> bool {
    MODEL_RUNNING.load(Ordering::Acquire) && !MODEL_INTERRUPTED.swap(true, Ordering::AcqRel)
}

fn openrouter_request(
    model: String,
    max_tokens: u32,
    timeout: std::time::Duration,
    api_key: String,
    request: ModelRequest,
) -> Result<String, String> {
    let response = MODEL_HTTP
        .post("https://openrouter.ai/api/v1/chat/completions")
        .timeout(timeout)
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": request.system},
                {"role": "user", "content": request.context}
            ],
            "max_tokens": max_tokens,
            "temperature": 0.4
        }))
        .send()
        .map_err(|error| format!("model request: {}", error))?;
    let status = response.status();
    let raw = response
        .text()
        .map_err(|error| format!("model response body: {}", error))?;
    let body: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("model HTTP {} returned invalid JSON: {}", status, error))?;
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown error");
        return Err(format!("model HTTP {}: {}", status, message));
    }
    body.pointer("/choices/0/message/content")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "model response contained no content".into())
}

impl ModelClient for OpenRouterModel {
    fn complete(&self, request: ModelRequest) -> Result<String, String> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| "OPENROUTER_API_KEY is not set".to_string())?;
        MODEL_INTERRUPTED.store(false, Ordering::Release);
        MODEL_RUNNING.store(true, Ordering::Release);
        let (sender, receiver) = mpsc::sync_channel(1);
        let (model, max_tokens, timeout) = (self.model.clone(), self.max_tokens, self.timeout);
        std::thread::spawn(move || {
            let _ = sender.send(openrouter_request(
                model, max_tokens, timeout, api_key, request,
            ));
        });
        loop {
            if MODEL_INTERRUPTED.swap(false, Ordering::AcqRel) {
                MODEL_RUNNING.store(false, Ordering::Release);
                return Err("model request interrupted by human input".into());
            }
            match receiver.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(result) => {
                    MODEL_RUNNING.store(false, Ordering::Release);
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    MODEL_RUNNING.store(false, Ordering::Release);
                    return Err("model request worker stopped unexpectedly".into());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnOutcome {
    Evaluated {
        frame_id: String,
        source: String,
        result: String,
    },
    ToolCompleted {
        frame_id: String,
        source: String,
        result: String,
    },
    Spawned {
        parent_id: String,
        child_id: String,
    },
    Returned {
        parent_id: String,
        result: String,
    },
    Replied {
        message_id: String,
        text: String,
    },
    Idle,
}

pub struct Scheduler<M: ModelClient> {
    pub model: M,
    pub executor: Executor,
    pub transcript_limit: usize,
    pub compact_batch: usize,
}

impl<M: ModelClient> Scheduler<M> {
    pub fn new(model: M, executor: Executor) -> Self {
        Self {
            model,
            executor,
            transcript_limit: 20,
            compact_batch: 20,
        }
    }

    pub fn run_turn(&self, kernel: &mut Kernel) -> Result<TurnOutcome, String> {
        let Some(frame) = kernel.frames.last() else {
            return Ok(TurnOutcome::Idle);
        };
        let frame_id = frame.id.clone();

        // Resume a snapshotted top-level operation before asking for a new action.
        if let Some(pending) = kernel.pending_trap() {
            if pending.operation == VmTrap::AwaitHuman {
                if frame.messages.is_empty() {
                    return Ok(TurnOutcome::Idle);
                }
                kernel.clear_trap();
                kernel.frames.last_mut().unwrap().status = FrameStatus::Running;
            } else {
                return self.handle_trap(kernel, frame_id, pending, ":resumed".into());
            }
        }
        if kernel
            .frames
            .last()
            .is_none_or(|frame| frame.status != FrameStatus::Running)
        {
            return Ok(TurnOutcome::Idle);
        }

        self.compact_current_frame(kernel);
        let raw = self.model.complete(self.build_request(kernel))?;
        let source = match normalize_one_form(&raw) {
            Ok(source) => source,
            Err(error) => {
                let result = format!("error: {}", error);
                kernel.append_transcript_to(&frame_id, raw.trim(), &result);
                return Ok(TurnOutcome::Evaluated {
                    frame_id,
                    source: raw,
                    result,
                });
            }
        };
        let displayed = match kernel.eval(&source) {
            Ok(value) => format!("{}", value),
            Err(error) => format!("error: {}", error),
        };
        if let Some(frame) = kernel.frames.last_mut() {
            frame.messages.retain(|message| message.id.is_some());
        }

        if let Some(pending) = kernel.pending_trap() {
            return self.handle_trap(kernel, frame_id, pending, displayed);
        }
        kernel.append_transcript_to(&frame_id, &source, &displayed);
        Ok(TurnOutcome::Evaluated {
            frame_id,
            source,
            result: displayed,
        })
    }

    fn handle_trap(
        &self,
        kernel: &mut Kernel,
        frame_id: String,
        pending: crate::kernel::PendingTrap,
        scheduled: String,
    ) -> Result<TurnOutcome, String> {
        let source = pending.source;
        match pending.operation {
            VmTrap::RunBash { command } => {
                let result = self
                    .executor
                    .run(&command)
                    .map(|execution| format_execution(&execution))
                    .unwrap_or_else(|error| format!("error: {}", error));
                kernel.clear_trap();
                kernel.append_transcript_to(&frame_id, &source, &result);
                Ok(TurnOutcome::ToolCompleted {
                    frame_id,
                    source,
                    result,
                })
            }
            VmTrap::CallModel { prompt } => {
                let result = self
                    .model
                    .complete(ModelRequest {
                        system: "Return a concise string result for the calling Lisp agent.".into(),
                        context: prompt,
                    })
                    .unwrap_or_else(|error| format!("error: {}", error));
                kernel.clear_trap();
                kernel.append_transcript_to(&frame_id, &source, &result);
                Ok(TurnOutcome::ToolCompleted {
                    frame_id,
                    source,
                    result,
                })
            }
            VmTrap::CallAgent { name, request } => {
                kernel.clear_trap();
                kernel.append_transcript_to(&frame_id, &source, &scheduled);
                let child_id = kernel.spawn_subagent(&name, &request);
                Ok(TurnOutcome::Spawned {
                    parent_id: frame_id,
                    child_id,
                })
            }
            VmTrap::ReturnAgent { value } => {
                let result = format!("{}", value);
                kernel.clear_trap();
                kernel.append_transcript_to(&frame_id, &source, &result);
                kernel.return_from_subagent();
                let parent_id = kernel
                    .frames
                    .last()
                    .map(|frame| frame.id.clone())
                    .ok_or_else(|| "agent returned without a parent".to_string())?;
                kernel.append_transcript_to(&parent_id, "(agent/result)", &result);
                Ok(TurnOutcome::Returned { parent_id, result })
            }
            VmTrap::Reply { message_id, text } => {
                kernel.clear_trap();
                kernel.complete_message(&message_id);
                kernel.append_transcript_to(&frame_id, &source, &text);
                Ok(TurnOutcome::Replied { message_id, text })
            }
            VmTrap::AwaitHuman => {
                kernel.frames.last_mut().unwrap().status = FrameStatus::Waiting;
                kernel.append_transcript_to(&frame_id, &source, "waiting for human input");
                Ok(TurnOutcome::ToolCompleted {
                    frame_id,
                    source,
                    result: "waiting for human input".into(),
                })
            }
        }
    }

    pub fn build_request(&self, kernel: &Kernel) -> ModelRequest {
        let frame = kernel
            .frames
            .last()
            .expect("build_request requires a frame");
        let mut context = String::from(
            "# Active frame stack
",
        );
        for active in &kernel.frames {
            context.push_str(&format!(
                "- {} [{}] {:?}
",
                active.name, active.id, active.status
            ));
        }
        context.push_str(
            "
# Current task and notices
",
        );
        for message in &frame.messages {
            match &message.id {
                Some(id) => context.push_str(&format!(
                    "- Human message [{}]: {}
",
                    id,
                    truncate(&message.text, 2_000)
                )),
                None => context.push_str(&format!(
                    "- {}
",
                    truncate(&message.text, 2_000)
                )),
            }
        }
        if !frame.state.compacted_context.is_empty() {
            context.push_str(
                "
# Earlier compacted context
",
            );
            context.push_str(&truncate(&frame.state.compacted_context, 16_000));
            context.push('\n');
        }
        context.push_str(
            "
# Recent Lisp actions and results
",
        );
        for entry in &frame.state.transcript {
            context.push_str(&format!(
                "> {}
{}
",
                truncate(&entry.source, 600),
                truncate(&entry.result, 1_200),
            ));
        }

        let mut visible = String::new();
        for name in kernel.env.namespace_names() {
            let bindings = kernel.inspect_namespace(&name).unwrap_or_default();
            visible.push_str(&format!(
                "{}: {}
",
                name,
                bindings.join(", ")
            ));
        }
        context.push_str(
            "
# Visible namespaces and bindings
",
        );
        context.push_str(&truncate(&visible, 8_000));

        let mut definitions = String::new();
        for (namespace, values) in kernel.env.namespaces.iter() {
            let mut names: Vec<_> = values.sources.keys().collect();
            names.sort();
            for name in names {
                definitions.push_str(&format!(
                    "- {}/{}
",
                    namespace, name
                ));
            }
        }
        context.push_str(
            "
# Definitions with retained source
",
        );
        context.push_str(&truncate(&definitions, 4_000));

        if !frame.state.memory.is_empty() {
            context.push_str(
                "
# Selected persistent memory
",
            );
            for entry in &frame.state.memory {
                context.push_str(&format!(
                    "{}: {}
",
                    truncate(&entry.key, 200),
                    truncate(&entry.value, 1_000)
                ));
            }
        }
        for hook in &frame.state.context_hooks {
            context.push_str(
                "
# Context hook
",
            );
            context.push_str(&truncate(hook, 2_000));
        }
        context = truncate(&context, 62_000);
        context.push_str("
Emit exactly one Lisp form. No prose, tags, or Markdown. Use (begin ...) only for synchronous Lisp operations. bash, model/call, agent/call, agent/return, human/wait, and message/reply must be top-level forms.
");
        ModelRequest {
            system: if frame.state.instructions.is_empty() {
                "You are Continuum, a persistent agent inhabiting a Lisp world. Choose one useful Lisp action.".into()
            } else {
                frame.state.instructions.clone()
            },
            context,
        }
    }

    fn compact_current_frame(&self, kernel: &mut Kernel) {
        let Some(frame) = kernel.frames.last_mut() else {
            return;
        };
        if frame.state.transcript.len() <= self.transcript_limit {
            return;
        }
        let count = self.compact_batch.min(frame.state.transcript.len());
        let drained: Vec<TranscriptEntry> = frame.state.transcript.drain(..count).collect();
        if !frame.state.compacted_context.is_empty() {
            frame.state.compacted_context.push('\n');
        }
        for entry in drained {
            frame.state.compacted_context.push_str(&format!(
                "[{}] {} => {}\n",
                entry.timestamp,
                truncate(&entry.source, 240),
                truncate(&entry.result, 480),
            ));
        }
        if frame.state.compacted_context.len() > 32_000 {
            let mut drop = frame.state.compacted_context.len() - 32_000;
            while !frame.state.compacted_context.is_char_boundary(drop) {
                drop += 1;
            }
            let boundary = frame.state.compacted_context[drop..]
                .find('\n')
                .map_or(drop, |offset| drop + offset + 1);
            frame.state.compacted_context.drain(..boundary);
        }
    }
}

pub fn normalize_one_form(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("```") || trimmed.contains("<lisp>") {
        return Err("model output must be raw Lisp without Markdown or tags".into());
    }
    let forms =
        reader::read_all(trimmed).map_err(|e| format!("model emitted invalid Lisp: {}", e))?;
    if forms.len() != 1 {
        return Err(format!(
            "model must emit exactly one Lisp form, got {}",
            forms.len()
        ));
    }
    Ok(trimmed.to_string())
}

fn format_execution(result: &ExecutionResult) -> String {
    serde_json::to_string(result)
        .unwrap_or_else(|_| "{\"error\":\"cannot serialize execution result\"}".into())
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
