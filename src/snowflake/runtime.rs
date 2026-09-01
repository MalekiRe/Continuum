use crate::snowflake::compile::compile;
use crate::snowflake::effects::{self, EffectError, EffectRequest, ExternalRun, TerminalEffect};
use crate::snowflake::image::{ImageError, ImageStore};
use crate::snowflake::value::{MessageId, Value};
use crate::snowflake::vm::{Task, TaskPoll, VmError};
use crate::snowflake::world::{Agent, Message, TranscriptEntry, World};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub const PAUSE: u8 = 1;
pub const CANCEL_LISP: u8 = 2;
pub(crate) const LISP_ACTIVE: u8 = 4;
const STOPPING: u8 = 8;

pub enum Active {
    Idle,
    Lisp(Task),
    External(Task, ExternalRun),
    Thinking(ExternalRun),
}

pub enum Command {
    Snapshot,
    CancelLisp,
    CancelExternal,
    HumanMessage(String),
    Shutdown,
}

#[derive(Clone)]
pub struct RuntimeHandle(
    mpsc::UnboundedSender<Command>,
    Arc<AtomicU8>,
    Arc<Mutex<()>>,
);

impl RuntimeHandle {
    pub fn send(&self, command: Command) -> Result<(), RuntimeError> {
        let _admission = self.2.lock().expect("admission lock poisoned");
        if matches!(&command, Command::CancelLisp) {
            let _ = self
                .1
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                    (state & LISP_ACTIVE != 0).then_some(state | CANCEL_LISP)
                });
            return (!self.0.is_closed())
                .then_some(())
                .ok_or(RuntimeError::Invariant("runtime stopped"));
        }
        let human = matches!(&command, Command::HumanMessage(_));
        if human && self.1.load(Ordering::Acquire) & STOPPING != 0 {
            return Err(RuntimeError::Invariant("runtime stopping"));
        }
        let flags = match &command {
            Command::Shutdown => PAUSE | STOPPING,
            Command::Snapshot | Command::HumanMessage(_) => PAUSE,
            _ => 0,
        };
        self.1.fetch_or(flags, Ordering::AcqRel);
        self.0
            .send(command)
            .map_err(|_| RuntimeError::Invariant("runtime stopped"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Vm(#[from] VmError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("runtime invariant: {0}")]
    Invariant(&'static str),
}

type Starter = Arc<dyn Fn(&EffectRequest) -> ExternalRun + Send + Sync>;
pub type RuntimeObserver = Arc<dyn Fn(&str, &TranscriptEntry, bool) + Send + Sync>;
pub type HumanObserver = Arc<dyn Fn(&Message) + Send + Sync>;

struct RunGuard(tokio::task::JoinHandle<()>, Arc<AtomicU8>);

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.abort();
        self.1.fetch_or(CANCEL_LISP, Ordering::AcqRel);
    }
}

pub struct Runtime {
    pub world: World,
    active: Active,
    parked: Vec<Task>,
    control: Arc<AtomicU8>,
    admission: Arc<Mutex<()>>,
    images: ImageStore,
    starter: Starter,
    observer: Option<RuntimeObserver>,
    human_observer: Option<HumanObserver>,
    sender: mpsc::UnboundedSender<Command>,
    commands: mpsc::UnboundedReceiver<Command>,
    shutdown: Option<tokio::time::Instant>,
    model_failures: u8,
    model_retry: Option<tokio::time::Instant>,
}

impl Runtime {
    pub fn with_starter(
        mut world: World,
        images: ImageStore,
        starter: impl Fn(&EffectRequest) -> ExternalRun + Send + Sync + 'static,
    ) -> Self {
        effects::install(&mut world);
        if world.state.agents.is_empty() {
            let root = Agent::new("Continuum".into(), String::new());
            world.state.agents.push(root);
        }
        let (sender, commands) = mpsc::unbounded_channel();
        Self {
            world,
            active: Active::Idle,
            parked: Vec::new(),
            control: Arc::new(AtomicU8::new(0)),
            admission: Arc::new(Mutex::new(())),
            images,
            starter: Arc::new(starter),
            observer: None,
            human_observer: None,
            sender,
            commands,
            shutdown: None,
            model_failures: 0,
            model_retry: None,
        }
    }

    pub fn observe(&mut self, observer: RuntimeObserver) {
        self.observer = Some(observer);
    }

    pub fn observe_humans(&mut self, observer: HumanObserver) {
        self.human_observer = Some(observer);
    }

    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle(
            self.sender.clone(),
            self.control.clone(),
            self.admission.clone(),
        )
    }

    pub async fn run(&mut self) -> Result<(), RuntimeError> {
        let handle = self.handle();
        let _guard = RunGuard(
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60 * 60)).await;
                    if handle.send(Command::Snapshot).is_err() {
                        break;
                    }
                }
            }),
            self.control.clone(),
        );
        loop {
            if self
                .shutdown
                .is_some_and(|deadline| deadline <= tokio::time::Instant::now())
            {
                if let Active::Lisp(task) | Active::External(task, _) =
                    std::mem::replace(&mut self.active, Active::Idle)
                {
                    task.abort(&mut self.world);
                }
                self.images.save(&self.world, None)?;
                break;
            }
            if let Ok(command) = self.commands.try_recv() {
                self.command(command)?;
                continue;
            }
            if self.shutdown.is_some() && matches!(self.active, Active::Idle) {
                self.images.save(&self.world, None)?;
                break;
            }
            if matches!(self.active, Active::Idle)
                && (self.model_failures >= 3 || self.model_retry.is_some())
            {
                tokio::select! {
                    command = self.commands.recv() => self.command(command.ok_or(
                        RuntimeError::Invariant("command channel closed"))?)?,
                    _ = tokio::time::sleep_until(self.model_retry.expect("failed models retry")),
                        if self.model_failures < 3 => self.model_retry = None,
                }
                continue;
            }
            match std::mem::replace(&mut self.active, Active::Idle) {
                Active::Idle => {
                    let prompt = self.context()?;
                    self.active = Active::Thinking((self.starter)(&EffectRequest::Model(prompt)));
                }
                Active::Lisp(task) => self.drive_task(task).await?,
                Active::External(task, mut run) => {
                    tokio::select! {
                        command = self.commands.recv() => {
                            self.active = Active::External(task, run);
                            self.command(command.ok_or(RuntimeError::Invariant("command channel closed"))?)?;
                        }
                        result = &mut run.future => self.finish_external(task, result)?,
                        _ = tokio::time::sleep_until(self.shutdown.unwrap_or_else(tokio::time::Instant::now)), if self.shutdown.is_some() => {
                            task.abort(&mut self.world);
                            self.active = Active::Idle;
                        }
                    }
                }
                Active::Thinking(mut run) => {
                    tokio::select! {
                        command = self.commands.recv() => {
                            self.active = Active::Thinking(run);
                            self.command(command.ok_or(RuntimeError::Invariant("command channel closed"))?)?;
                        }
                        result = &mut run.future => self.finish_thinking(result)?,
                        _ = tokio::time::sleep_until(self.shutdown.unwrap_or_else(tokio::time::Instant::now)), if self.shutdown.is_some() => {
                            self.active = Active::Idle;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn resume_lisp(&mut self, task: Task) {
        self.control.fetch_or(LISP_ACTIVE, Ordering::AcqRel);
        self.active = Active::Lisp(task);
    }

    async fn drive_task(&mut self, mut task: Task) -> Result<(), RuntimeError> {
        let mut world = self.world.clone();
        let control = self.control.clone();
        let (task, world, poll) = tokio::task::spawn_blocking(move || {
            let poll = task.poll(&mut world, &control);
            (task, world, poll)
        })
        .await
        .map_err(|_| RuntimeError::Invariant("Lisp worker panicked"))?;
        self.world = world;
        self.control.fetch_and(!LISP_ACTIVE, Ordering::AcqRel);
        if !matches!(poll, TaskPoll::Paused | TaskPoll::Cancelled) {
            self.control.fetch_and(!CANCEL_LISP, Ordering::AcqRel);
        }
        let source = task.source().to_owned();
        match poll {
            TaskPoll::Complete(value) => self.finish(false, source, effects::display(&value))?,
            TaskPoll::Effect(request) => self.start_effect(task, request)?,
            TaskPoll::Terminal(effect) => self.finish_terminal(task, source, effect)?,
            TaskPoll::Paused => self.resume_lisp(task),
            TaskPoll::Cancelled => {
                self.finish(false, source, "error: Lisp cancelled".into())?;
                self.control.fetch_and(!CANCEL_LISP, Ordering::AcqRel);
            }
            TaskPoll::Failed(error) => self.finish(false, source, format!("error: {error}"))?,
        }
        Ok(())
    }

    fn start_effect(&mut self, mut task: Task, request: EffectRequest) -> Result<(), RuntimeError> {
        task.commit_boundary(&self.world);
        if let EffectRequest::Agent { name, request } = request {
            self.world.state.agents.push(Agent::new(name, request));
            self.parked.push(task);
            self.active = Active::Idle;
        } else {
            let run = (self.starter)(&request);
            self.active = Active::External(task, run);
        }
        Ok(())
    }

    fn finish_external(
        &mut self,
        mut task: Task,
        result: Result<Value, EffectError>,
    ) -> Result<(), RuntimeError> {
        if let Err(error) = task.resume(result) {
            let source = task.source().to_owned();
            task.abort(&mut self.world);
            if self.shutdown.is_some() {
                self.active = Active::Idle;
            } else {
                self.finish(false, source, format!("error: {error}"))?;
            }
        } else {
            self.resume_lisp(task);
        }
        Ok(())
    }

    fn finish_thinking(&mut self, result: Result<Value, EffectError>) -> Result<(), RuntimeError> {
        let source = match result {
            Ok(Value::String(source)) => {
                self.model_failures = 0;
                self.model_retry = None;
                source
            }
            Ok(_) => return Err(RuntimeError::Invariant("model returned a non-string value")),
            Err(error) => {
                if self.shutdown.is_some() {
                    self.active = Active::Idle;
                } else {
                    if error.0 == "model request interrupted by human input" {
                        self.model_retry = None;
                    } else {
                        self.model_failures = self.model_failures.saturating_add(1);
                        let seconds = 1_u64 << self.model_failures.min(6);
                        self.model_retry =
                            Some(tokio::time::Instant::now() + Duration::from_secs(seconds));
                    }
                    self.finish(false, "(model)".into(), format!("error: {error}"))?;
                }
                return Ok(());
            }
        };
        let committed = self.world.state.clone();
        match compile(&mut self.world, &source, &self.control) {
            Ok(entry) => {
                let mut task = Task::start(&self.world, entry)?;
                task.replace_checkpoint(committed);
                self.resume_lisp(task);
            }
            Err(error) => self.finish(false, source, format!("error: {error}"))?,
        }
        Ok(())
    }

    fn finish_terminal(
        &mut self,
        mut task: Task,
        source: String,
        effect: TerminalEffect,
    ) -> Result<(), RuntimeError> {
        match effect {
            TerminalEffect::Reply { message, text } => {
                let current = self.current()?;
                let inbox = &mut self.world.state.agents[current].inbox;
                let owned = inbox
                    .iter()
                    .position(|id| *id == message)
                    .ok_or(RuntimeError::Invariant("reply message is not owned"))?;
                inbox.remove(owned);
                let messages = &self.world.state.messages;
                let order = messages.values().filter(|m| m.reply.is_some()).count() as u32;
                let after = self.world.state.next_message;
                let message = self
                    .world
                    .state
                    .messages
                    .get_mut(&message)
                    .ok_or(RuntimeError::Invariant("reply message disappeared"))?;
                message.reply = Some(text.clone());
                message.reply_at = Some(chrono::Utc::now().to_rfc3339());
                message.reply_order = Some((after, order));
                task.commit_boundary(&self.world);
                self.finish(true, source, text)
            }
            TerminalEffect::ReturnAgent(result) => {
                if self.world.state.agents.len() == 1 {
                    task.abort(&mut self.world);
                    return self.finish(false, source, "error: root agent cannot return".into());
                }
                task.commit_boundary(&self.world);
                let child = self
                    .world
                    .state
                    .agents
                    .pop()
                    .ok_or(RuntimeError::Invariant("returned agent disappeared"))?;
                let unanswered: Vec<_> = child
                    .inbox
                    .into_iter()
                    .filter(|id| {
                        self.world
                            .state
                            .messages
                            .get(id)
                            .is_some_and(|message| message.reply.is_none())
                    })
                    .collect();
                self.world
                    .state
                    .agents
                    .last_mut()
                    .ok_or(RuntimeError::Invariant("returned agent has no parent"))?
                    .inbox
                    .extend(unanswered);
                if let Some(mut parent) = self.parked.pop() {
                    parent.commit_boundary(&self.world);
                    parent.resume(Ok(Value::String(result)))?;
                    self.resume_lisp(parent);
                } else {
                    self.finish(false, format!("(agent/result {})", child.name), result)?;
                }
                Ok(())
            }
        }
    }

    pub fn command(&mut self, command: Command) -> Result<(), RuntimeError> {
        let admission = self.admission.clone();
        let _admission = admission.lock().expect("admission lock poisoned");
        match command {
            Command::Snapshot => {
                self.control.fetch_or(PAUSE, Ordering::AcqRel);
                match &mut self.active {
                    Active::Lisp(task) => {
                        self.images.save(&self.world, Some(task.transaction()))?
                    }
                    _ => self.images.save(&self.world, None)?,
                }
                self.control.fetch_and(!PAUSE, Ordering::AcqRel);
            }
            Command::CancelLisp => {
                if matches!(self.active, Active::Lisp(_)) {
                    self.control.fetch_or(CANCEL_LISP, Ordering::AcqRel);
                } else {
                    self.control.fetch_and(!CANCEL_LISP, Ordering::AcqRel);
                }
            }
            Command::CancelExternal => match &self.active {
                Active::External(_, run) | Active::Thinking(run) => run.cancel(),
                _ => {}
            },
            Command::HumanMessage(text) => {
                if self.shutdown.is_some() || self.control.load(Ordering::Acquire) & STOPPING != 0 {
                    return Err(RuntimeError::Invariant("runtime stopping"));
                }
                self.add_message(text)?;
                self.model_failures = 0;
                self.model_retry = None;
                match &mut self.active {
                    Active::Lisp(task) | Active::External(task, _) => {
                        task.merge_runtime_state(&self.world);
                    }
                    _ => {}
                }
                self.control.fetch_and(!PAUSE, Ordering::AcqRel);
            }
            Command::Shutdown => {
                self.control.fetch_or(PAUSE | STOPPING, Ordering::AcqRel);
                self.shutdown
                    .get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_secs(2));
                self.control.fetch_or(CANCEL_LISP, Ordering::AcqRel);
                match &self.active {
                    Active::External(_, run) | Active::Thinking(run) => run.cancel(),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn context(&self) -> Result<String, RuntimeError> {
        let current = self.current()?;
        let agent = &self.world.state.agents[current];
        let mut text = format!(
            "{}\nEmit exactly one raw Lisp form and no prose. Continue taking actions after replies. Supported special forms are quote, if, begin, lambda, define, set!, let, let*, and letrec; use begin, never progn. Effects are ordinary expressions: (bash STRING), (model STRING), (agent NAME REQUEST), (reply INTEGER-ID TEXT), and child-only (return TEXT). Core values are integers, strings, booleans, nil, and lists. Never invent a human-message ID or call reply unless a PENDING HUMAN MESSAGE line is present.\n",
            agent.instructions
        );
        if agent.inbox.is_empty() {
            text.push_str("This agent owns no pending messages; do not call reply.\n");
        }
        for (id, message) in &self.world.state.messages {
            if agent.inbox.contains(id) {
                text.push_str(&format!("PENDING HUMAN MESSAGE {}: {}\nAnswer it exactly as (reply {} \"your response\").\n", id.0, message.text, id.0));
            } else if current == 0 {
                let status = message
                    .reply
                    .as_ref()
                    .map_or("pending elsewhere", |_| "answered");
                text.push_str(&format!(
                    "HUMAN MESSAGE HISTORY {}: {} ({status})\n",
                    id.0, message.text
                ));
            }
        }
        for entry in &agent.transcript {
            text.push_str(&format!("> {}\n{}\n", entry.source, entry.result));
        }
        Ok(text)
    }

    fn record(&mut self, current: usize, entry: TranscriptEntry, replied: bool) {
        if let Some(observer) = &self.observer {
            observer(&self.world.state.agents[current].name, &entry, replied);
        }
        self.world.state.agents[current].transcript.push(entry);
    }

    fn finish(
        &mut self,
        replied: bool,
        source: String,
        result: String,
    ) -> Result<(), RuntimeError> {
        let current = self.current()?;
        self.record(current, TranscriptEntry { source, result }, replied);
        self.active = Active::Idle;
        Ok(())
    }

    fn current(&self) -> Result<usize, RuntimeError> {
        self.world
            .state
            .agents
            .len()
            .checked_sub(1)
            .ok_or(RuntimeError::Invariant("no runnable agent"))
    }

    fn add_message(&mut self, text: String) -> Result<(), RuntimeError> {
        let id = MessageId(self.world.state.next_message);
        let next = self
            .world
            .state
            .next_message
            .checked_add(1)
            .ok_or(RuntimeError::Invariant("message id exhausted"))?;
        self.world.state.next_message = next;
        self.world.state.messages.insert(
            id,
            Message {
                text,
                created_at: chrono::Utc::now().to_rfc3339(),
                reply: None,
                reply_at: None,
                reply_order: None,
            },
        );
        let current = self.current()?;
        self.world.state.agents[current].inbox.push(id);
        if let Some(observer) = &self.human_observer {
            observer(&self.world.state.messages[&id]);
        }
        Ok(())
    }
}
