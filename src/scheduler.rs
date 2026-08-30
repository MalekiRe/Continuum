use crate::executor::{ExecutionResult, Executor};
use crate::kernel::{FrameStatus, Kernel, TranscriptEntry, VmTrap};
use crate::vm::reader;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub system: String,
    pub context: String,
}

const MODEL_CONTEXT_LIMIT: usize = 62_000;

struct ContextBuilder {
    text: String,
    remaining: usize,
}

impl ContextBuilder {
    fn new(limit: usize) -> Self {
        Self { text: String::with_capacity(limit), remaining: limit }
    }

    fn section(&mut self, heading: &str, body: &str, budget: usize) {
        if body.is_empty() || self.remaining == 0 { return; }
        let prefix = format!("\n# {heading}\n");
        if prefix.len() >= self.remaining { return; }
        self.text.push_str(&prefix);
        self.remaining -= prefix.len();
        let allowed = budget.min(self.remaining);
        let rendered = truncate(body, allowed);
        self.text.push_str(&rendered);
        self.remaining -= rendered.len();
        if self.remaining > 0 && !self.text.ends_with('\n') {
            self.text.push('\n');
            self.remaining -= 1;
        }
    }

    fn finish(mut self, directive: &str) -> String {
        let rendered = truncate(directive, self.remaining);
        self.text.push_str(&rendered);
        self.text
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model request interrupted by human input")]
    Cancelled,
    #[error("OPENROUTER_API_KEY is not set")]
    MissingApiKey,
    #[error("model request: {0}")]
    Request(#[source] reqwest::Error),
    #[error("model response body: {0}")]
    ResponseBody(#[source] reqwest::Error),
    #[error("model HTTP {status} returned invalid JSON: {source}")]
    InvalidJson {
        status: reqwest::StatusCode,
        #[source]
        source: serde_json::Error,
    },
    #[error("model HTTP {status}: {message}")]
    Http {
        status: reqwest::StatusCode,
        message: String,
    },
    #[error("model response contained no content")]
    MissingContent,
    #[error("{0}")]
    Client(String),
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<String, ModelError>;
}

#[derive(Debug, Clone)]
pub struct OpenRouterModel {
    pub model: String,
    pub max_tokens: u32,
    pub timeout: std::time::Duration,
    client: reqwest::Client,
}

impl Default for OpenRouterModel {
    fn default() -> Self {
        Self {
            model: std::env::var("CONTINUUM_MODEL")
                .unwrap_or_else(|_| "deepseek/deepseek-v4-flash".into()),
            max_tokens: 600,
            timeout: std::time::Duration::from_secs(60),
            client: reqwest::Client::new(),
        }
    }
}

impl OpenRouterModel {
    async fn openrouter_request(
        &self,
        api_key: String,
        request: ModelRequest,
    ) -> Result<String, ModelError> {
        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .timeout(self.timeout)
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "messages": [
                    {"role": "system", "content": request.system},
                    {"role": "user", "content": request.context}
                ],
                "max_tokens": self.max_tokens,
                "temperature": 0.4
            }))
            .send()
            .await
            .map_err(ModelError::Request)?;
        let status = response.status();
        let raw = response.text().await.map_err(ModelError::ResponseBody)?;
        let body: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|source| ModelError::InvalidJson { status, source })?;
        if !status.is_success() {
            let message = body
                .pointer("/error/message")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown error")
                .to_string();
            return Err(ModelError::Http { status, message });
        }
        body.pointer("/choices/0/message/content")
            .and_then(|value| value.as_str())
            .map(str::to_owned)
            .ok_or(ModelError::MissingContent)
    }
}

#[async_trait]
impl ModelClient for OpenRouterModel {
    async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<String, ModelError> {
        if cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| ModelError::MissingApiKey)?;
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(ModelError::Cancelled),
            result = self.openrouter_request(api_key, request) => result,
        }
    }
}

#[derive(Clone)]
pub struct ModelInterruptHandle {
    active: Arc<Mutex<Option<ActiveModelRequest>>>,
}

impl ModelInterruptHandle {
    /// Cancels the request currently owned by this scheduler, if any.
    pub fn request_interrupt(&self) -> bool {
        let active = self.active.lock().unwrap();
        let Some(active) = active.as_ref() else {
            return false;
        };
        if active.cancellation.is_cancelled() {
            false
        } else {
            active.cancellation.cancel();
            true
        }
    }
}

struct ActiveModelRequest {
    generation: u64,
    cancellation: CancellationToken,
}

struct ModelRuntime {
    gate: tokio::sync::Mutex<()>,
    active: Arc<Mutex<Option<ActiveModelRequest>>>,
    next_generation: AtomicU64,
}

impl ModelRuntime {
    fn new() -> Self {
        Self {
            gate: tokio::sync::Mutex::new(()),
            active: Arc::new(Mutex::new(None)),
            next_generation: AtomicU64::new(0),
        }
    }

    fn interrupt_handle(&self) -> ModelInterruptHandle {
        ModelInterruptHandle {
            active: Arc::clone(&self.active),
        }
    }

    fn activate(&self) -> ActiveRequestGuard {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let cancellation = CancellationToken::new();
        *self.active.lock().unwrap() = Some(ActiveModelRequest {
            generation,
            cancellation: cancellation.clone(),
        });
        ActiveRequestGuard {
            active: Arc::clone(&self.active),
            generation,
            cancellation,
        }
    }
}

struct ActiveRequestGuard {
    active: Arc<Mutex<Option<ActiveModelRequest>>>,
    generation: u64,
    cancellation: CancellationToken,
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        let mut active = self.active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|request| request.generation == self.generation)
        {
            *active = None;
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
    model: M,
    model_runtime: ModelRuntime,
    pub executor: Executor,
    pub transcript_limit: usize,
    pub compact_batch: usize,
}

impl<M: ModelClient> Scheduler<M> {
    pub fn new(model: M, executor: Executor) -> Self {
        Self {
            model,
            model_runtime: ModelRuntime::new(),
            executor,
            transcript_limit: 20,
            compact_batch: 20,
        }
    }

    pub fn model_interrupt_handle(&self) -> ModelInterruptHandle {
        self.model_runtime.interrupt_handle()
    }

    async fn complete_model(&self, request: ModelRequest) -> Result<String, ModelError> {
        let _generation_gate = self.model_runtime.gate.lock().await;
        let active = self.model_runtime.activate();
        let cancellation = active.cancellation.clone();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(ModelError::Cancelled),
            result = self.model.complete(request, cancellation.clone()) => result,
        }
    }

    pub async fn run_turn(&self, kernel: &mut Kernel) -> Result<TurnOutcome, String> {
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
                return self
                    .handle_trap(kernel, frame_id, pending, ":resumed".into())
                    .await;
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
        let raw = self
            .complete_model(self.build_request(kernel))
            .await
            .map_err(|error| error.to_string())?;
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
            return self.handle_trap(kernel, frame_id, pending, displayed).await;
        }
        kernel.append_transcript_to(&frame_id, &source, &displayed);
        Ok(TurnOutcome::Evaluated {
            frame_id,
            source,
            result: displayed,
        })
    }

    async fn handle_trap(
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
                    .complete_model(ModelRequest {
                        system: "Return a concise string result for the calling Lisp agent.".into(),
                        context: prompt,
                    })
                    .await
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
        let frame = kernel.frames.last().expect("build_request requires a frame");
        let directive = "\nEmit exactly one Lisp form. No prose, tags, or Markdown. Use (begin ...) only for synchronous Lisp operations. bash, model/call, agent/call, agent/return, human/wait, and message/reply must be top-level forms.\n";
        let mut context = ContextBuilder::new(MODEL_CONTEXT_LIMIT - directive.len());

        let mut notices = String::new();
        for message in &frame.messages {
            match &message.id {
                Some(id) => {
                    let _ = writeln!(notices, "- Human message [{}]: {}", id, truncate(&message.text, 2_000));
                }
                None => {
                    let _ = writeln!(notices, "- {}", truncate(&message.text, 2_000));
                }
            }
        }
        context.section("Current human messages and notices", &notices, 12_000);

        let mut stack = String::new();
        for active in &kernel.frames {
            let _ = writeln!(stack, "- {} [{}] {:?}", active.name, active.id, active.status);
        }
        context.section("Active frame stack", &stack, 4_000);

        let mut guidance = String::new();
        for hook in &frame.state.context_hooks {
            let _ = writeln!(guidance, "Hook: {}", truncate(hook, 2_000));
        }
        for entry in &frame.state.memory {
            let _ = writeln!(guidance, "{}: {}", truncate(&entry.key, 200), truncate(&entry.value, 1_000));
        }
        context.section("Context hooks and selected memory", &guidance, 12_000);

        let mut recent = String::new();
        for entry in &frame.state.transcript {
            let _ = writeln!(recent, "> {}\n{}", truncate(&entry.source, 600), truncate(&entry.result, 1_200));
        }
        context.section("Recent Lisp actions and results", &recent, 24_000);
        context.section("Earlier compacted context", &frame.state.compacted_context, 6_000);

        let mut library = String::new();
        for name in kernel.env.namespace_names() {
            let bindings = kernel.inspect_namespace(&name).unwrap_or_default();
            let _ = writeln!(library, "{}: {}", name, bindings.join(", "));
        }
        let _ = writeln!(library, "\nDefinitions with retained source:");
        for (namespace, values) in &kernel.env.namespaces {
            let mut names: Vec<_> = values.sources.keys().collect();
            names.sort();
            for name in names {
                let _ = writeln!(library, "- {}/{}", namespace, name);
            }
        }
        context.section("Library discovery", &library, 4_000);

        ModelRequest {
            system: if frame.state.instructions.is_empty() {
                "You are Continuum, a persistent agent inhabiting a Lisp world. Choose one useful Lisp action.".into()
            } else {
                truncate(&frame.state.instructions, 16_000)
            },
            context: context.finish(directive),
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
