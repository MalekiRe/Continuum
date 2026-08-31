mod context;

use crate::executor::{ExecutionResult, Executor, ExecutorError};
use crate::ids::{FrameId, MessageId};
use crate::kernel::{AllocationError, EvalOutcome, Kernel, MessageError, TrapRequest, VmTrap};
use crate::vm::reader::{self, ReadError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub system: String,
    pub context: String,
}

const MODEL_CONTEXT_LIMIT: usize = 62_000;

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

        if frame.waiting_for_human {
            if kernel.notices_for_frame(&frame_id).is_empty() {
                return Ok(TurnOutcome::Idle);
            }
            kernel
                .frames
                .last_mut()
                .expect("active frame")
                .waiting_for_human = false;
        }

        context::compact_current_frame(kernel);
        let (request, notice_watermark) = context::build_request(kernel);
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
        let outcome = match kernel.eval(&source) {
            Ok(outcome) => outcome,
            Err(crate::vm::eval::EvalError::Interrupted) => {
                return Err(SchedulerError::EvaluationInterrupted);
            }
            Err(error) => {
                EvalOutcome::Value(crate::vm::value::Value::string(&format!("error: {error}")))
            }
        };
        let displayed = match outcome {
            EvalOutcome::Value(value) => value.to_string(),
            EvalOutcome::Trap(request) => {
                return self
                    .handle_trap(kernel, frame_id, request, ":scheduled".into())
                    .await;
            }
        };
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
        pending: TrapRequest,
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
                        kernel.append_transcript_to(&frame_id, &source, &format!("error: {error}"));
                    })?;
                let result = format_execution(&execution);
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
                        kernel.append_transcript_to(&frame_id, &source, &format!("error: {error}"));
                    })?;
                kernel.append_transcript_to(&frame_id, &source, &result);
                Ok(TurnOutcome::ToolCompleted {
                    frame_id,
                    source,
                    result,
                })
            }
            VmTrap::CallAgent { name, request } => {
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
                let result = value;
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
                kernel.append_transcript_to(&frame_id, &source, &text);
                Ok(TurnOutcome::Replied { message_id, text })
            }
            VmTrap::AwaitHuman => {
                let frame = kernel.frames.last_mut().ok_or(SchedulerError::Invariant(
                    "human wait without an active frame",
                ))?;
                frame.waiting_for_human = true;
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
        context::build_request(kernel).0
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
