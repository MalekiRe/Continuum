use super::*;
use crate::executor::ExecutorStatus;
use crate::vm::value::Value;
use std::time::Duration;
use tokio::sync::mpsc;

const WALL_REVIEW: Duration = Duration::from_secs(15 * 60);
const PAUSE_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlTrigger {
    Human(String),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlReply {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlDecision {
    Continue {
        reply: Option<String>,
    },
    Advice {
        advice: String,
        reply: Option<String>,
    },
    Cancel {
        reason: String,
        reply: Option<String>,
    },
}

impl ControlDecision {
    fn reply(&self) -> Option<&str> {
        match self {
            Self::Continue { reply } | Self::Advice { reply, .. } | Self::Cancel { reply, .. } => {
                reply.as_deref()
            }
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Continue { .. } => "continue current work".into(),
            Self::Advice { advice, .. } => format!("continue with advice: {advice}"),
            Self::Cancel { reason, .. } => format!("cancel current work: {reason}"),
        }
    }
}

#[derive(Debug, Clone)]
enum ReviewTrigger {
    Human(String),
    WallClock,
    Efficiency {
        generated_tokens: usize,
        waiting: Duration,
    },
}

impl ReviewTrigger {
    fn render(&self) -> String {
        match self {
            Self::Human(text) => format!("Human message:\n{text}"),
            Self::WallClock => "The current operation has run for another fifteen minutes.".into(),
            Self::Efficiency {
                generated_tokens,
                waiting,
            } => format!(
                "Efficiency review: the preceding action used approximately {generated_tokens} model tokens and has waited {:.1} seconds.",
                waiting.as_secs_f64()
            ),
        }
    }
}

#[derive(Debug, Clone)]
enum ActiveWork {
    Idle,
    Model { context: String },
    Lisp { source: String },
    Bash { command: String },
}

struct BlockingBash {
    frame_id: FrameId,
    source: String,
    command: String,
    generated_tokens: usize,
}

impl ActiveWork {
    fn render(&self, status: Option<&ExecutorStatus>) -> String {
        match self {
            Self::Idle => "No external operation is active.".into(),
            Self::Model { context } => format!(
                "The active frame is generating its next Lisp action. Context tail:\n{}",
                tail(context, 4_000)
            ),
            Self::Lisp { source } => format!("Lisp is evaluating:\n{source}"),
            Self::Bash { command } => {
                let progress = status.map_or_else(
                    || "The process has not published status.".into(),
                    |status| {
                        format!(
                            "elapsed={:.1}s stdout={} bytes stderr={} bytes",
                            status.elapsed.as_secs_f64(),
                            status.stdout_bytes,
                            status.stderr_bytes
                        )
                    },
                );
                format!("Blocking Bash command:\n{command}\n\nProgress: {progress}")
            }
        }
    }
}

#[derive(Debug)]
enum ControlForm {
    Decide(ControlDecision),
    Bash(String),
}

fn tail(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut start = value.len() - max;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

fn parse_string(value: &Value, name: &str) -> Result<String, SchedulerError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or(SchedulerError::Control(format!("{name} must be a string")))
}

fn optional_reply(values: &[Value], index: usize) -> Result<Option<String>, SchedulerError> {
    match values.get(index) {
        None | Some(Value::Nil) => Ok(None),
        Some(value) => parse_string(value, "control reply").map(Some),
    }
}

fn parse_control(raw: &str) -> Result<ControlForm, SchedulerError> {
    let forms =
        reader::read_all(raw).map_err(|error| SchedulerError::Control(error.to_string()))?;
    let [Value::List(items)] = forms.as_slice() else {
        return Err(SchedulerError::Control(
            "control model must emit exactly one list form".into(),
        ));
    };
    let Some(Value::Symbol(head)) = items.first() else {
        return Err(SchedulerError::Control(
            "control form needs a symbolic head".into(),
        ));
    };
    let args = &items[1..];
    match head.as_str() {
        "control/continue" if args.len() <= 1 => {
            Ok(ControlForm::Decide(ControlDecision::Continue {
                reply: optional_reply(args, 0)?,
            }))
        }
        "control/advice" if (1..=2).contains(&args.len()) => {
            Ok(ControlForm::Decide(ControlDecision::Advice {
                advice: parse_string(&args[0], "control advice")?,
                reply: optional_reply(args, 1)?,
            }))
        }
        "control/cancel" if (1..=2).contains(&args.len()) => {
            Ok(ControlForm::Decide(ControlDecision::Cancel {
                reason: parse_string(&args[0], "control cancellation reason")?,
                reply: optional_reply(args, 1)?,
            }))
        }
        "control/bash" if args.len() == 1 => Ok(ControlForm::Bash(parse_string(
            &args[0],
            "control Bash command",
        )?)),
        _ => Err(SchedulerError::Control(format!(
            "invalid control form: {raw}"
        ))),
    }
}

fn estimate_tokens(value: &str) -> usize {
    value.chars().count().div_ceil(4).max(1)
}

fn efficiency_delay(tokens: usize) -> Duration {
    Duration::from_secs((tokens as u64).clamp(60, WALL_REVIEW.as_secs()))
}

impl<M: ModelClient> Scheduler<M> {
    async fn control_turn(
        &self,
        trigger: ReviewTrigger,
        work: ActiveWork,
        status: Option<ExecutorStatus>,
        stack: &str,
    ) -> Result<ControlDecision, SchedulerError> {
        let mut observations = String::new();
        loop {
            let context = format!(
                "{}\n\nActive logical stack:\n{}\n\nCurrent work:\n{}\n\nControl observations:\n{}",
                trigger.render(),
                stack,
                work.render(status.as_ref()),
                observations
            );
            let raw = self
                .complete_model(ModelRequest {
                    system: concat!(
                        "You supervise a persistent local agent. Investigate intelligently; do not cancel merely because work is old. ",
                        "Emit one form: (control/continue [reply]), (control/advice advice [reply]), ",
                        "(control/cancel reason [reply]), or (control/bash command). ",
                        "A human trigger should receive a useful reply. control/bash may inspect processes, files, logs, and progress before deciding."
                    )
                    .into(),
                    context,
                })
                .await?;
            match parse_control(raw.trim()) {
                Ok(ControlForm::Decide(decision)) => return Ok(decision),
                Ok(ControlForm::Bash(command)) => {
                    let executor = self.control_executor.clone();
                    let executed = command.clone();
                    let displayed = tokio::task::spawn_blocking(move || executor.run(&executed))
                        .await
                        .map_err(|_| SchedulerError::Invariant("control Bash task panicked"))??;
                    observations.push_str("\n> ");
                    observations.push_str(&command);
                    observations.push('\n');
                    observations.push_str(&format_execution(&displayed));
                    if observations.len() > 16_000 {
                        observations = tail(&observations, 16_000).to_owned();
                    }
                }
                Err(error) => {
                    observations.push_str("\nInvalid control output: ");
                    observations.push_str(&error.to_string());
                    observations.push_str("\nRaw output: ");
                    observations.push_str(raw.trim());
                }
            }
        }
    }

    fn stack_context(kernel: &Kernel) -> String {
        kernel
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                let active = if index + 1 == kernel.frames.len() {
                    "active"
                } else {
                    "blocked on child"
                };
                format!("- {} [{}] {active}", frame.name, frame.id)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn apply_control(
        kernel: &mut Kernel,
        trigger: &ReviewTrigger,
        decision: &ControlDecision,
        replies: &mpsc::UnboundedSender<ControlReply>,
        send_reply: bool,
    ) -> Result<(), SchedulerError> {
        if let ReviewTrigger::Human(text) = trigger {
            let id = kernel.human_message(text)?;
            kernel.complete_message(&id)?;
            let reply = decision
                .reply()
                .unwrap_or("I reviewed the current work and will continue monitoring it.")
                .to_owned();
            if send_reply {
                let _ = replies.send(ControlReply { text: reply });
            }
        }
        if !matches!(decision, ControlDecision::Continue { .. }) {
            kernel.control_notice(format!("Supervisor decision: {}", decision.description()))?;
        }
        Ok(())
    }

    async fn review_with_kernel(
        &self,
        kernel: &mut Kernel,
        trigger: ReviewTrigger,
        work: ActiveWork,
        status: Option<ExecutorStatus>,
        replies: &mpsc::UnboundedSender<ControlReply>,
    ) -> Result<ControlDecision, SchedulerError> {
        let stack = Self::stack_context(kernel);
        let decision = self
            .control_turn(trigger.clone(), work, status, &stack)
            .await?;
        Self::apply_control(kernel, &trigger, &decision, replies, true)?;
        Ok(decision)
    }

    async fn complete_model_supervised(
        &self,
        kernel: &mut Kernel,
        request: ModelRequest,
        controls: &mut mpsc::UnboundedReceiver<ControlTrigger>,
        replies: &mpsc::UnboundedSender<ControlReply>,
    ) -> Result<Option<String>, SchedulerError> {
        loop {
            let mut completion = Box::pin(self.complete_model(request.clone()));
            let wall = tokio::time::sleep(WALL_REVIEW);
            tokio::pin!(wall);
            let trigger = tokio::select! {
                result = &mut completion => return result.map(Some).map_err(Into::into),
                trigger = controls.recv(), if !controls.is_closed() => match trigger {
                    Some(ControlTrigger::Human(text)) => ReviewTrigger::Human(text),
                    Some(ControlTrigger::Shutdown) | None => {
                        self.model_interrupt_handle().request_interrupt();
                        let _ = completion.await;
                        return Ok(None);
                    }
                },
                () = &mut wall => ReviewTrigger::WallClock,
            };
            self.model_interrupt_handle().request_interrupt();
            let _ = completion.await;
            let decision = self
                .review_with_kernel(
                    kernel,
                    trigger,
                    ActiveWork::Model {
                        context: request.context.clone(),
                    },
                    None,
                    replies,
                )
                .await?;
            if matches!(decision, ControlDecision::Cancel { .. }) {
                return Ok(None);
            }
        }
    }

    async fn evaluate_supervised(
        &self,
        kernel: Kernel,
        source: String,
        controls: &mut mpsc::UnboundedReceiver<ControlTrigger>,
        replies: &mpsc::UnboundedSender<ControlReply>,
    ) -> (Kernel, Result<Option<EvalOutcome>, SchedulerError>) {
        let interrupt = kernel.eval_interrupt_handle();
        let stack = Self::stack_context(&kernel);
        let eval_source = source.clone();
        let mut evaluation = Box::pin(tokio::task::spawn_blocking(move || {
            let mut kernel = kernel;
            let result = kernel.eval(&eval_source);
            (kernel, result)
        }));
        let mut deferred = Vec::new();
        let mut shutdown = false;
        loop {
            let wall = tokio::time::sleep(WALL_REVIEW);
            tokio::pin!(wall);
            let trigger = tokio::select! {
                joined = &mut evaluation => {
                    let (mut kernel, result) = match joined {
                        Ok(value) => value,
                        Err(_) => std::process::abort(),
                    };
                    for (trigger, decision) in deferred {
                        if let Err(error) = Self::apply_control(&mut kernel, &trigger, &decision, replies, false) {
                            return (kernel, Err(error));
                        }
                    }
                    if shutdown {
                        return (kernel, Ok(None));
                    }
                    return (kernel, match result {
                        Ok(value) => Ok(Some(value)),
                        Err(crate::vm::eval::EvalError::Interrupted) => Ok(None),
                        Err(error) => Ok(Some(EvalOutcome::Value(Value::string(&format!("error: {error}"))))),
                    });
                }
                trigger = controls.recv(), if !controls.is_closed() => match trigger {
                    Some(ControlTrigger::Human(text)) => ReviewTrigger::Human(text),
                    Some(ControlTrigger::Shutdown) | None => {
                        shutdown = true;
                        interrupt.request_interrupt();
                        continue;
                    }
                },
                () = &mut wall => ReviewTrigger::WallClock,
            };

            if !interrupt.request_pause() {
                continue;
            }
            let paused = {
                let wait = interrupt.clone();
                tokio::task::spawn_blocking(move || wait.wait_until_paused(PAUSE_WAIT))
                    .await
                    .unwrap_or(false)
            };
            if !paused {
                continue;
            }
            let decision = match self
                .control_turn(
                    trigger.clone(),
                    ActiveWork::Lisp {
                        source: source.clone(),
                    },
                    None,
                    &stack,
                )
                .await
            {
                Ok(decision) => decision,
                Err(error) => {
                    interrupt.resume();
                    return match evaluation.await {
                        Ok((kernel, _)) => (kernel, Err(error)),
                        Err(_) => std::process::abort(),
                    };
                }
            };
            if let Some(reply) = decision.reply() {
                let _ = replies.send(ControlReply {
                    text: reply.to_owned(),
                });
            }
            match decision {
                ControlDecision::Cancel { .. } => {
                    interrupt.request_interrupt();
                }
                _ => interrupt.resume(),
            }
            deferred.push((trigger, decision));
        }
    }

    async fn blocking_bash_supervised(
        &self,
        kernel: &mut Kernel,
        request: BlockingBash,
        controls: &mut mpsc::UnboundedReceiver<ControlTrigger>,
        replies: &mpsc::UnboundedSender<ControlReply>,
    ) -> Result<Option<TurnOutcome>, SchedulerError> {
        let BlockingBash {
            frame_id,
            source,
            command,
            generated_tokens,
        } = request;
        let display_command = command.clone();
        let executor = self.executor.clone();
        let mut execution = Box::pin(tokio::task::spawn_blocking(move || executor.run(&command)));
        let started = tokio::time::Instant::now();
        let mut cancelled = false;
        let mut shutdown = false;
        loop {
            let wall = tokio::time::sleep(WALL_REVIEW);
            let efficiency = tokio::time::sleep(efficiency_delay(generated_tokens));
            tokio::pin!(wall, efficiency);
            let trigger = tokio::select! {
                joined = &mut execution => {
                    let execution = joined
                        .map_err(|_| SchedulerError::Invariant("Bash executor task panicked"))??;
                    let result = format_execution(&execution);
                    kernel.append_transcript_to(&frame_id, &source, &result);
                    if shutdown {
                        return Ok(None);
                    }
                    return Ok(Some(TurnOutcome::ToolCompleted { frame_id, source, result }));
                }
                trigger = controls.recv(), if !controls.is_closed() => match trigger {
                    Some(ControlTrigger::Human(text)) => ReviewTrigger::Human(text),
                    Some(ControlTrigger::Shutdown) | None => {
                        shutdown = true;
                        self.executor.cancel()?;
                        continue;
                    }
                },
                () = &mut wall => ReviewTrigger::WallClock,
                () = &mut efficiency => ReviewTrigger::Efficiency {
                    generated_tokens,
                    waiting: started.elapsed(),
                },
            };
            let decision = self
                .review_with_kernel(
                    kernel,
                    trigger,
                    ActiveWork::Bash {
                        command: display_command.clone(),
                    },
                    self.executor.active_status(),
                    replies,
                )
                .await?;
            if matches!(decision, ControlDecision::Cancel { .. }) && !cancelled {
                cancelled = self.executor.cancel()?;
            }
        }
    }

    pub async fn run_supervised_turn(
        &self,
        mut kernel: Kernel,
        controls: &mut mpsc::UnboundedReceiver<ControlTrigger>,
        replies: &mpsc::UnboundedSender<ControlReply>,
    ) -> (Kernel, Result<TurnOutcome, SchedulerError>) {
        while let Ok(trigger) = controls.try_recv() {
            match trigger {
                ControlTrigger::Shutdown => return (kernel, Ok(TurnOutcome::Shutdown)),
                ControlTrigger::Human(text) => {
                    let decision = self
                        .review_with_kernel(
                            &mut kernel,
                            ReviewTrigger::Human(text),
                            ActiveWork::Idle,
                            None,
                            replies,
                        )
                        .await;
                    if let Err(error) = decision {
                        return (kernel, Err(error));
                    }
                }
            }
        }

        let Some(frame) = kernel.frames.last() else {
            return (kernel, Ok(TurnOutcome::Idle));
        };
        let frame_id = frame.id.clone();
        if frame.waiting_for_human && kernel.notices_for_frame(&frame_id).is_empty() {
            return (kernel, Ok(TurnOutcome::Idle));
        }
        if let Some(frame) = kernel.frames.last_mut() {
            frame.waiting_for_human = false;
        }

        if let Err(error) = kernel.run_stage("stage/before-context", Value::Nil) {
            return (kernel, Err(SchedulerError::Control(error.to_string())));
        }
        context::compact_current_frame(&mut kernel);
        let (request, notice_watermark) = context::build_request(&kernel);
        let Some(raw) = (match self
            .complete_model_supervised(&mut kernel, request, controls, replies)
            .await
        {
            Ok(raw) => raw,
            Err(error) => return (kernel, Err(error)),
        }) else {
            return (
                kernel,
                Ok(TurnOutcome::Cancelled {
                    frame_id,
                    reason: "control decision".into(),
                }),
            );
        };
        if let Err(error) = kernel.run_stage("stage/after-generation", Value::string(&raw)) {
            return (kernel, Err(SchedulerError::Control(error.to_string())));
        }
        let source = match normalize_one_form(&raw) {
            Ok(source) => source,
            Err(error) => {
                let result = format!("error: {error}");
                kernel.append_transcript_to(&frame_id, raw.trim(), &result);
                return (
                    kernel,
                    Ok(TurnOutcome::Evaluated {
                        frame_id,
                        source: raw,
                        result,
                    }),
                );
            }
        };
        if let Some(watermark) = notice_watermark {
            kernel.mark_notices_seen_through(&frame_id, watermark);
        }
        if let Some(frame) = kernel.frames.last_mut() {
            frame
                .state
                .context_entries
                .retain(|entry| entry.lifetime == crate::kernel::ContextLifetime::Frame);
        }
        let generated_tokens = estimate_tokens(&raw);
        let (mut kernel, evaluated) = self
            .evaluate_supervised(kernel, source.clone(), controls, replies)
            .await;
        let Some(outcome) = (match evaluated {
            Ok(outcome) => outcome,
            Err(error) => return (kernel, Err(error)),
        }) else {
            return (
                kernel,
                Ok(TurnOutcome::Cancelled {
                    frame_id,
                    reason: "control decision".into(),
                }),
            );
        };
        match outcome {
            EvalOutcome::Value(value) => {
                let result = value.to_string();
                kernel.append_transcript_to(&frame_id, &source, &result);
                kernel.collect_garbage();
                (
                    kernel,
                    Ok(TurnOutcome::Evaluated {
                        frame_id,
                        source,
                        result,
                    }),
                )
            }
            EvalOutcome::Trap(TrapRequest {
                source,
                operation: VmTrap::RunBash { command },
            }) => {
                let result = self
                    .blocking_bash_supervised(
                        &mut kernel,
                        BlockingBash {
                            frame_id,
                            source,
                            command,
                            generated_tokens,
                        },
                        controls,
                        replies,
                    )
                    .await;
                kernel.collect_garbage();
                match result {
                    Ok(Some(outcome)) => (kernel, Ok(outcome)),
                    Ok(None) => (kernel, Ok(TurnOutcome::Shutdown)),
                    Err(error) => (kernel, Err(error)),
                }
            }
            EvalOutcome::Trap(request) => {
                let result = self
                    .handle_trap(&mut kernel, frame_id, request, ":scheduled".into())
                    .await;
                kernel.collect_garbage();
                (kernel, result)
            }
        }
    }
}
