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
        retry_after: Option<std::time::Duration>,
    },
    #[error("model response contained no content ({0})")]
    MissingContent(String),
    #[error("{0}")]
    Client(String),
}
impl ModelError {
    fn retry_delay(&self, attempt: u32) -> Option<std::time::Duration> {
        match self {
            Self::Request(error) if error.is_connect() => {
                Some(std::time::Duration::from_millis(200))
            }
            Self::Http {
                status,
                retry_after,
                ..
            } if *status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() => {
                Some(retry_after.unwrap_or(std::time::Duration::from_secs(1 << attempt)))
            }
            _ => None,
        }
    }
}

fn parse_retry_after(value: &reqwest::header::HeaderValue) -> Option<std::time::Duration> {
    let value = value.to_str().ok()?;
    let delay = if let Ok(seconds) = value.parse() {
        std::time::Duration::from_secs(seconds)
    } else {
        (chrono::DateTime::parse_from_rfc2822(value)
            .ok()?
            .with_timezone(&chrono::Utc)
            - chrono::Utc::now())
        .to_std()
        .unwrap_or_default()
    };
    Some(delay)
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
pub struct ModelCompletion {
    pub content: String,
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OpenRouterModel {
    pub model: String,
    pub reasoning_effort: String,
    pub timeout: std::time::Duration,
    client: reqwest::Client,
}

impl Default for OpenRouterModel {
    fn default() -> Self {
        Self {
            model: std::env::var("CONTINUUM_MODEL")
                .unwrap_or_else(|_| "deepseek/deepseek-v4-flash".into()),
            reasoning_effort: std::env::var("CONTINUUM_REASONING_EFFORT")
                .unwrap_or_else(|_| "high".into()),
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
    ) -> Result<ModelCompletion, ModelError> {
        let mut response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .timeout(self.timeout)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": self.model,
                "messages": [
                    {"role": "system", "content": request.system},
                    {"role": "user", "content": request.context}
                ],
                "temperature": 0.4,
                "reasoning": {"effort": self.reasoning_effort, "exclude": false},
                "provider": {"sort": "throughput", "allow_fallbacks": true, "require_parameters": true}
            }))
            .send()
            .await
            .map_err(ModelError::Request)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(parse_retry_after);
        let mut raw = Vec::new();
        let transport_error = loop {
            match response.chunk().await {
                Ok(Some(chunk)) => raw.extend_from_slice(&chunk),
                Ok(None) => break None,
                Err(error) => break Some(error),
            }
        };
        let parsed = serde_json::from_slice::<serde_json::Value>(&raw);
        if !status.is_success() {
            let message = parsed
                .as_ref()
                .ok()
                .and_then(|body| body.pointer("/error/message"))
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| std::str::from_utf8(&raw).unwrap_or("non-JSON response"))
                .to_string();
            return Err(ModelError::Http {
                status,
                message,
                retry_after,
            });
        }
        let body = match parsed {
            Ok(body) => body,
            Err(source) => {
                if let Some(error) = transport_error {
                    return Err(ModelError::ResponseBody(error));
                }
                return Err(ModelError::InvalidJson { status, source });
            }
        };
        if let Some(content) = body
            .pointer("/choices/0/message/content")
            .and_then(|value| value.as_str())
            .filter(|content| !content.trim().is_empty())
        {
            let message = &body["choices"][0]["message"];
            return Ok(ModelCompletion {
                content: content.to_owned(),
                reasoning: extract_reasoning(message),
            });
        }
        let finish_reason = body
            .pointer("/choices/0/finish_reason")
            .and_then(|value| value.as_str())
            .unwrap_or("missing");
        let completion_tokens = body
            .pointer("/usage/completion_tokens")
            .map_or_else(|| "missing".into(), ToString::to_string);
        Err(ModelError::MissingContent(format!(
            "finish reason: {finish_reason}, completion tokens: {completion_tokens}"
        )))
    }
}

fn extract_reasoning(message: &serde_json::Value) -> Option<String> {
    for field in ["reasoning", "reasoning_content"] {
        if let Some(reasoning) = message.get(field).and_then(|value| value.as_str())
            && !reasoning.trim().is_empty()
        {
            return Some(reasoning.to_owned());
        }
    }
    let reasoning = message
        .get("reasoning_details")?
        .as_array()?
        .iter()
        .filter_map(|detail| {
            detail
                .get("text")
                .or_else(|| detail.get("summary"))?
                .as_str()
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    (!reasoning.trim().is_empty()).then_some(reasoning)
}

impl OpenRouterModel {
    pub async fn complete_detailed(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelCompletion, ModelError> {
        if cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let started = std::time::Instant::now();
        let api_key = match std::env::var("OPENROUTER_API_KEY") {
            Ok(key) => key,
            Err(_) => {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(ModelError::Cancelled),
                    () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
                return Err(ModelError::MissingApiKey);
            }
        };
        for attempt in 0..2 {
            let result = tokio::select! {
                biased;
                result = self.openrouter_request(api_key.clone(), request.clone()) => result,
                () = cancellation.cancelled() => return Err(ModelError::Cancelled),
            };
            let retry = result
                .as_ref()
                .err()
                .and_then(|error| error.retry_delay(attempt));
            if attempt == 1 || retry.is_none() {
                if result.is_err() {
                    let delay = std::time::Duration::from_secs(2).saturating_sub(started.elapsed());
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return Err(ModelError::Cancelled),
                        () = tokio::time::sleep(delay) => {}
                    }
                }
                return result;
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(ModelError::Cancelled),
                () = tokio::time::sleep(retry.unwrap()) => {}
            }
        }
        unreachable!("two model attempts either returned or retried")
    }
}

#[async_trait]
impl ModelClient for OpenRouterModel {
    async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<String, ModelError> {
        self.complete_detailed(request, cancellation)
            .await
            .map(|completion| completion.content)
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
            return self
                .handle_trap(kernel, frame_id, pending, ":resumed".into())
                .await;
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
                let executor = self.executor.clone();
                let execution = tokio::task::spawn_blocking(move || executor.run(&command))
                    .await
                    .map_err(|_| SchedulerError::Invariant("Bash executor task panicked"))?
                    .inspect_err(|error| {
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
                let crate::vm::value::Value::String(result) = value else {
                    return Err(SchedulerError::Invariant(
                        "agent returned a non-string value",
                    ));
                };
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
        let directive = "\nEmit exactly one Lisp form. No prose, tags, or Markdown. Keep taking useful actions indefinitely, including after replying to a human. Use (begin ...) only for synchronous Lisp operations. bash, model/call, agent/call, agent/return, and message/reply must be top-level forms.\n";
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

        let recent = render_recent_transcript(&frame.state.transcript, 24_000);
        section("Recent Lisp actions and results", &recent, 24_000);
        let compacted = render_recent_compacted(&frame.state.compacted_context, 6_000);
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

fn render_recent_transcript(entries: &[TranscriptEntry], budget: usize) -> String {
    let mut selected = Vec::new();
    let mut used = 0;
    for entry in entries.iter().rev() {
        let rendered = format!(
            "> {}\n{}\n",
            truncate(&entry.source, 600),
            truncate(&entry.result, 1_200)
        );
        if used + rendered.len() > budget {
            if selected.is_empty() {
                selected.push(truncate(&rendered, budget));
            }
            break;
        }
        used += rendered.len();
        selected.push(rendered);
    }
    selected.reverse();
    selected.concat()
}

fn render_recent_compacted(context: &crate::kernel::CompactedContext, budget: usize) -> String {
    let mut selected = Vec::new();
    let mut used = 0;
    for entry in context.entries.iter().rev() {
        let rendered = format!(
            "[{}] {} => {}\n",
            entry.timestamp, entry.source, entry.result
        );
        if used + rendered.len() > budget {
            break;
        }
        used += rendered.len();
        selected.push(rendered);
    }
    selected.reverse();
    let omitted = context.omitted_turns
        + u64::try_from(context.entries.len().saturating_sub(selected.len())).unwrap_or(u64::MAX);
    let mut rendered = String::new();
    if omitted > 0 {
        let _ = writeln!(rendered, "[{omitted} older turns omitted]");
    }
    rendered.push_str(&selected.concat());
    rendered
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

#[cfg(test)]
mod model_response_tests {
    use super::extract_reasoning;

    #[test]
    fn extracts_plain_and_structured_reasoning() {
        let plain = serde_json::json!({"reasoning": "think"});
        assert_eq!(extract_reasoning(&plain).as_deref(), Some("think"));
        let details = serde_json::json!({"reasoning_details": [
            {"type": "reasoning.summary", "summary": "summary"},
            {"type": "reasoning.text", "text": "details"},
            {"type": "reasoning.encrypted", "data": "secret"}
        ]});
        assert_eq!(
            extract_reasoning(&details).as_deref(),
            Some("summary\ndetails")
        );
    }

    #[test]
    fn retry_after_is_honored_only_for_explicit_retryable_responses() {
        let limited = super::ModelError::Http {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            message: "slow down".into(),
            retry_after: Some(std::time::Duration::from_secs(7)),
        };
        assert_eq!(
            limited.retry_delay(0),
            Some(std::time::Duration::from_secs(7))
        );
        let bad = super::ModelError::Http {
            status: reqwest::StatusCode::BAD_REQUEST,
            message: "bad".into(),
            retry_after: None,
        };
        assert_eq!(bad.retry_delay(0), None);
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        let seconds = reqwest::header::HeaderValue::from_static("7");
        assert_eq!(
            super::parse_retry_after(&seconds),
            Some(std::time::Duration::from_secs(7))
        );
        let date = (chrono::Utc::now() + chrono::Duration::minutes(2)).to_rfc2822();
        let date = reqwest::header::HeaderValue::from_str(&date).unwrap();
        let delay = super::parse_retry_after(&date).unwrap();
        assert!(delay >= std::time::Duration::from_secs(118));
        assert!(delay <= std::time::Duration::from_secs(120));
    }

    #[test]
    fn empty_or_encrypted_reasoning_is_not_fabricated() {
        assert_eq!(
            extract_reasoning(&serde_json::json!({"reasoning": "  "})),
            None
        );
        assert_eq!(
            extract_reasoning(&serde_json::json!({
                "reasoning_details": [{"type": "reasoning.encrypted", "data": "opaque"}]
            })),
            None
        );
    }
}
