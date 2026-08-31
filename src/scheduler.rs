use crate::executor::{ExecutionResult, Executor, ExecutorError};
use crate::ids::{FrameId, MessageId};
use crate::kernel::{AllocationError, FrameStatus, Kernel, MessageError, TranscriptEntry, VmTrap};
use crate::vm::reader::{self, ReadError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub system: String,
    pub context: String,
}

const MODEL_CONTEXT_LIMIT: usize = 62_000;

fn append_section(text: &mut String, limit: usize, heading: &str, body: &str, budget: usize) {
    let mut remaining = limit.saturating_sub(text.len());
    if body.is_empty() || remaining == 0 {
        return;
    }
    let prefix = format!("\n# {heading}\n");
    if prefix.len() >= remaining {
        return;
    }
    text.push_str(&prefix);
    remaining -= prefix.len();
    text.push_str(&truncate(body, budget.min(remaining)));
    if text.len() < limit && !text.ends_with('\n') {
        text.push('\n');
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
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error(transparent)]
    Message(#[from] MessageError),
    #[error(transparent)]
    Allocation(#[from] AllocationError),
    #[error("Lisp evaluation interrupted by human input")]
    EvaluationInterrupted,
    #[error("scheduler invariant violated: {0}")]
    Invariant(&'static str),
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("model output must be raw Lisp without Markdown or tags")]
    Wrapped,
    #[error("model emitted invalid Lisp: {0}")]
    Read(#[from] ReadError),
    #[error("model must emit exactly one Lisp form, got {0}")]
    FormCount(usize),
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
    active: Arc<Mutex<Option<CancellationToken>>>,
    pending: Arc<std::sync::atomic::AtomicBool>,
}

impl ModelInterruptHandle {
    /// Records an intervention and cancels this scheduler's active request.
    pub fn request_interrupt(&self) -> bool {
        let active = self.active.lock().unwrap();
        let newly_pending = !self.pending.swap(true, Ordering::AcqRel);
        if let Some(cancellation) = active.as_ref() {
            cancellation.cancel();
        }
        newly_pending
    }

    pub fn clear_pending(&self) {
        let active = self.active.lock().unwrap();
        if active.is_none() {
            self.pending.store(false, Ordering::Release);
        }
    }
}

#[derive(Default)]
struct ModelRuntime {
    gate: tokio::sync::Mutex<()>,
    active: Arc<Mutex<Option<CancellationToken>>>,
    pending: Arc<std::sync::atomic::AtomicBool>,
}

impl ModelRuntime {
    fn interrupt_handle(&self) -> ModelInterruptHandle {
        ModelInterruptHandle {
            active: Arc::clone(&self.active),
            pending: Arc::clone(&self.pending),
        }
    }

    fn activate(&self) -> ActiveRequestGuard {
        let cancellation = CancellationToken::new();
        let mut active = self.active.lock().unwrap();
        if self.pending.load(Ordering::Acquire) {
            cancellation.cancel();
        }
        *active = Some(cancellation.clone());
        ActiveRequestGuard {
            active: Arc::clone(&self.active),
            pending: Arc::clone(&self.pending),
            cancellation,
            finished: false,
        }
    }

    fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

struct ActiveRequestGuard {
    active: Arc<Mutex<Option<CancellationToken>>>,
    pending: Arc<std::sync::atomic::AtomicBool>,
    cancellation: CancellationToken,
    finished: bool,
}

impl ActiveRequestGuard {
    fn finish<T>(mut self, result: Result<T, ModelError>) -> Result<T, ModelError> {
        let mut active = self.active.lock().unwrap();
        let interrupted =
            self.pending.swap(false, Ordering::AcqRel) || self.cancellation.is_cancelled();
        *active = None;
        self.finished = true;
        drop(active);
        if interrupted {
            Err(ModelError::Cancelled)
        } else {
            result
        }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        if !self.finished {
            *self.active.lock().unwrap() = None;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnOutcome {
    Evaluated {
        frame_id: FrameId,
        source: String,
        result: String,
    },
    ToolCompleted {
        frame_id: FrameId,
        source: String,
        result: String,
    },
    Spawned {
        parent_id: FrameId,
        child_id: FrameId,
    },
    Returned {
        parent_id: FrameId,
        result: String,
    },
    Replied {
        message_id: MessageId,
        text: String,
    },
    Idle,
}

pub struct Scheduler<M: ModelClient> {
    model: M,
    model_runtime: ModelRuntime,
    executor: Executor,
}

impl<M: ModelClient> Scheduler<M> {
    pub fn new(model: M, executor: Executor) -> Self {
        Self {
            model,
            model_runtime: ModelRuntime::default(),
            executor,
        }
    }

    pub fn model_interrupt_handle(&self) -> ModelInterruptHandle {
        self.model_runtime.interrupt_handle()
    }

    async fn complete_model(&self, request: ModelRequest) -> Result<String, ModelError> {
        // Declared first so the active guard clears its slot before this gate unlocks.
        let _request_gate = self.model_runtime.gate.lock().await;
        let active = self.model_runtime.activate();
        let cancellation = active.cancellation.clone();
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(ModelError::Cancelled),
            result = self.model.complete(request, cancellation.clone()) => result,
        };
        active.finish(result)
    }

    pub async fn run_turn(&self, kernel: &mut Kernel) -> Result<TurnOutcome, SchedulerError> {
        let Some(frame) = kernel.frames.last() else {
            return Ok(TurnOutcome::Idle);
        };
        let frame_id = frame.id.clone();

        // Complete an in-memory top-level suspension before asking for a new action.
        if let Some(pending) = kernel.pending_trap() {
            if pending.operation == VmTrap::AwaitHuman {
                if kernel.notices_for_frame(&frame_id).is_empty() {
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
        let (request, notice_watermark) = self.build_request_with_notices(kernel);
        let raw = self.complete_model(request).await?;
        if let Some(watermark) = notice_watermark {
            kernel.mark_notices_seen_through(&frame_id, watermark);
        }
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
        if self.model_runtime.take_pending() {
            return Err(ModelError::Cancelled.into());
        }
        let displayed = match kernel.eval(&source) {
            Ok(value) => format!("{}", value),
            Err(crate::vm::eval::EvalError::Interrupted) => {
                return Err(SchedulerError::EvaluationInterrupted);
            }
            Err(error) => format!("error: {}", error),
        };
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
        frame_id: FrameId,
        pending: crate::kernel::PendingTrap,
        scheduled: String,
    ) -> Result<TurnOutcome, SchedulerError> {
        let source = pending.source;
        match pending.operation {
            VmTrap::RunBash { command } => {
                let execution = self.executor.run(&command).inspect_err(|error| {
                    kernel.clear_trap();
                    kernel.append_transcript_to(&frame_id, &source, &format!("error: {error}"));
                })?;
                let result = format_execution(&execution);
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
                    .inspect_err(|error| {
                        kernel.clear_trap();
                        kernel.append_transcript_to(&frame_id, &source, &format!("error: {error}"));
                    })?;
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
                let child_id = kernel
                    .spawn_subagent(&name, &request)
                    .inspect_err(|error| {
                        kernel.append_transcript_to(&frame_id, &source, &format!("error: {error}"));
                    })?;
                kernel.append_transcript_to(&frame_id, &source, &scheduled);
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
                    .ok_or(SchedulerError::Invariant("agent returned without a parent"))?;
                kernel.append_transcript_to(&parent_id, "(agent/result)", &result);
                Ok(TurnOutcome::Returned { parent_id, result })
            }
            VmTrap::Reply { message_id, text } => {
                kernel.complete_message(&message_id)?;
                kernel.clear_trap();
                kernel.append_transcript_to(&frame_id, &source, &text);
                Ok(TurnOutcome::Replied { message_id, text })
            }
            VmTrap::AwaitHuman => {
                let frame = kernel.frames.last_mut().ok_or(SchedulerError::Invariant(
                    "human wait without an active frame",
                ))?;
                frame.status = FrameStatus::Waiting;
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
        self.build_request_with_notices(kernel).0
    }

    fn build_request_with_notices(&self, kernel: &Kernel) -> (ModelRequest, Option<u64>) {
        let frame = kernel
            .frames
            .last()
            .expect("build_request requires a frame");
        let directive = "\nEmit exactly one Lisp form. No prose, tags, or Markdown. Use (begin ...) only for synchronous Lisp operations. bash, model/call, agent/call, agent/return, human/wait, and message/reply must be top-level forms.\n";
        let system = if frame.state.instructions.is_empty() {
            "You are Continuum, a persistent agent inhabiting a Lisp world. Choose one useful Lisp action.".into()
        } else {
            truncate(&frame.state.instructions, 16_000)
        };
        let body_budget = MODEL_CONTEXT_LIMIT.saturating_sub(system.len() + directive.len());
        let mut context = String::with_capacity(body_budget + directive.len());
        let mut section = |heading, body, budget| {
            append_section(&mut context, body_budget, heading, body, budget)
        };

        let visible_notices = kernel.notices_for_frame(&frame.id);
        let mut notices = String::new();
        let mut notice_watermark = None;
        for (index, message) in visible_notices.iter().enumerate() {
            let slots = visible_notices.len() - index;
            let allowance = (12_000usize.saturating_sub(notices.len()) / slots).max(1);
            let heading = match (&message.id, message.handled) {
                (Some(id), false) => format!("- Human message [{}]: ", id),
                (Some(id), true) => format!(
                    "- Answered human notice [{}] (informational; do not call message/reply): ",
                    id
                ),
                (None, _) => "- ".to_string(),
            };
            let body_budget = allowance.saturating_sub(heading.len() + 1);
            let _ = writeln!(
                notices,
                "{}{}",
                heading,
                truncate(&message.text, body_budget)
            );
            notice_watermark = Some(message.sequence);
        }
        section("Current human messages and notices", &notices, 12_000);

        let mut stack = String::new();
        for active in &kernel.frames {
            let _ = writeln!(
                stack,
                "- {} [{}] {:?}",
                active.name, active.id, active.status
            );
        }
        section("Active frame stack", &stack, 4_000);

        let mut guidance = String::new();
        for hook in &frame.state.context_hooks {
            let _ = writeln!(guidance, "Hook: {}", truncate(hook, 2_000));
        }
        for entry in &frame.state.memory {
            let _ = writeln!(
                guidance,
                "{}: {}",
                truncate(&entry.key, 200),
                truncate(&entry.value, 1_000)
            );
        }
        section("Context hooks and selected memory", &guidance, 12_000);

        let mut recent = String::new();
        for entry in &frame.state.transcript {
            let _ = writeln!(
                recent,
                "> {}\n{}",
                truncate(&entry.source, 600),
                truncate(&entry.result, 1_200)
            );
        }
        section("Recent Lisp actions and results", &recent, 24_000);
        let compacted = frame.state.compacted_context.render();
        section("Earlier compacted context", &compacted, 6_000);

        let mut library = String::new();
        for name in kernel.env.namespace_names() {
            let bindings = kernel.inspect_namespace(&name).unwrap_or_default();
            let _ = writeln!(library, "{}: {}", name, bindings.join(", "));
        }
        let _ = writeln!(library, "\nDefinitions with retained source:");
        for (namespace, values) in kernel.env.namespaces.iter() {
            let mut names: Vec<_> = values.sources.keys().collect();
            names.sort();
            for name in names {
                let _ = writeln!(library, "- {}/{}", namespace, name);
            }
        }
        section("Library discovery", &library, 4_000);

        context.push_str(directive);
        (ModelRequest { system, context }, notice_watermark)
    }

    fn compact_current_frame(&self, kernel: &mut Kernel) {
        const RECENT_TRANSCRIPT_BUDGET: usize = 24_000;
        const COMPACTED_BUDGET: usize = 32_000;
        let Some(frame) = kernel.frames.last_mut() else {
            return;
        };
        let occupancy = |entry: &TranscriptEntry| entry.source.len() + entry.result.len() + 8;
        let mut recent_bytes: usize = frame.state.transcript.iter().map(occupancy).sum();
        while recent_bytes > RECENT_TRANSCRIPT_BUDGET && frame.state.transcript.len() > 1 {
            let entry = frame.state.transcript.remove(0);
            recent_bytes = recent_bytes.saturating_sub(occupancy(&entry));
            frame
                .state
                .compacted_context
                .entries
                .push_back(crate::kernel::CompactedEntry {
                    timestamp: entry.timestamp,
                    source: truncate(&entry.source, 240),
                    result: truncate(&entry.result, 480),
                });
        }
        while frame.state.compacted_context.rendered_len() > COMPACTED_BUDGET {
            if frame.state.compacted_context.entries.pop_front().is_none() {
                break;
            }
            frame.state.compacted_context.omitted_turns += 1;
        }
    }
}

pub fn normalize_one_form(raw: &str) -> Result<String, NormalizeError> {
    let trimmed = raw.trim();
    if trimmed.starts_with("```") || trimmed.contains("<lisp>") {
        return Err(NormalizeError::Wrapped);
    }
    let forms = reader::read_all(trimmed)?;
    if forms.len() != 1 {
        return Err(NormalizeError::FormCount(forms.len()));
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
    const ELLIPSIS: &str = "…";
    if max < ELLIPSIS.len() {
        let mut end = max.min(value.len());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        return value[..end].to_string();
    }
    let mut end = (max - ELLIPSIS.len()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &value[..end], ELLIPSIS)
}
