use crate::snowflake::compile::compile;
use crate::snowflake::effects::{self, EffectError, EffectRequest, ExternalRun, TerminalEffect};
use crate::snowflake::image::{ImageError, ImageStore};
use crate::snowflake::value::{MessageId, Value};
use crate::snowflake::vm::{Task, TaskPoll, VmError};
use crate::snowflake::world::{Agent, Message, TranscriptEntry, World};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

pub const PAUSE: u8 = 1;
pub const CANCEL_LISP: u8 = 2;

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
pub struct RuntimeHandle(mpsc::UnboundedSender<Command>, Arc<AtomicU8>);

impl RuntimeHandle {
    pub fn send(&self, command: Command) -> Result<(), RuntimeError> {
        let signal = match command {
            Command::Snapshot | Command::HumanMessage(_) | Command::Shutdown => PAUSE,
            Command::CancelLisp => CANCEL_LISP,
            _ => 0,
        };
        self.1.fetch_or(signal, Ordering::AcqRel);
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
    images: ImageStore,
    starter: Starter,
    sender: mpsc::UnboundedSender<Command>,
    commands: mpsc::UnboundedReceiver<Command>,
    shutdown: Option<tokio::time::Instant>,
}

impl Runtime {
    pub fn new(world: World, images: ImageStore) -> Self {
        Self::with_starter(world, images, effects::start)
    }

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
            images,
            starter: Arc::new(starter),
            sender,
            commands,
            shutdown: None,
        }
    }

    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle(self.sender.clone(), self.control.clone())
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
            if self.shutdown.is_some() && matches!(self.active, Active::Idle) {
                self.images.save(&self.world, None)?;
                break;
            }
            if let Ok(command) = self.commands.try_recv() {
                self.command(command)?;
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
        let source = task.source().to_owned();
        match poll {
            TaskPoll::Complete(value) => self.finish(source, effects::display(&value))?,
            TaskPoll::Effect(request) => self.start_effect(task, request)?,
            TaskPoll::Terminal(effect) => self.finish_terminal(task, source, effect)?,
            TaskPoll::Paused => self.active = Active::Lisp(task),
            TaskPoll::Cancelled => {
                self.finish(source, "error: Lisp cancelled".into())?;
                self.control.fetch_and(!CANCEL_LISP, Ordering::AcqRel);
            }
            TaskPoll::Failed(error) => self.finish(source, format!("error: {error}"))?,
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
                self.finish(source, format!("error: {error}"))?;
            }
        } else {
            self.active = Active::Lisp(task);
        }
        Ok(())
    }

    fn finish_thinking(&mut self, result: Result<Value, EffectError>) -> Result<(), RuntimeError> {
        let source = match result {
            Ok(Value::String(source)) => source,
            Ok(_) => return Err(RuntimeError::Invariant("model returned a non-string value")),
            Err(error) => {
                if self.shutdown.is_some() {
                    self.active = Active::Idle;
                } else {
                    self.finish("(model)".into(), format!("error: {error}"))?;
                }
                return Ok(());
            }
        };
        let committed = self.world.state.clone();
        match compile(&mut self.world, &source, &self.control) {
            Ok(entry) => {
                let mut task = Task::start(&self.world, entry)?;
                task.replace_checkpoint(committed);
                self.active = Active::Lisp(task);
            }
            Err(error) => self.finish(source, format!("error: {error}"))?,
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
                let message = self
                    .world
                    .state
                    .messages
                    .get_mut(&message)
                    .ok_or(RuntimeError::Invariant("reply message disappeared"))?;
                message.answered = true;
                task.commit_boundary(&self.world);
                self.finish(source, text)
            }
            TerminalEffect::ReturnAgent(result) => {
                if self.world.state.agents.len() == 1 {
                    task.abort(&mut self.world);
                    return self.finish(source, "error: root agent cannot return".into());
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
                            .is_some_and(|message| !message.answered)
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
                    self.active = Active::Lisp(parent);
                } else {
                    self.world
                        .state
                        .agents
                        .last_mut()
                        .ok_or(RuntimeError::Invariant("returned agent has no parent"))?
                        .transcript
                        .push(TranscriptEntry {
                            source: format!("(agent/result {})", child.name),
                            result,
                        });
                    self.active = Active::Idle;
                }
                Ok(())
            }
        }
    }

    pub fn command(&mut self, command: Command) -> Result<(), RuntimeError> {
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
                self.add_message(text)?;
                match &mut self.active {
                    Active::Lisp(task) | Active::External(task, _) => {
                        task.merge_runtime_state(&self.world);
                    }
                    _ => {}
                }
                self.control.fetch_and(!PAUSE, Ordering::AcqRel);
            }
            Command::Shutdown => {
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
        let agent = &self.world.state.agents[self.current()?];
        let mut text = format!(
            "{}\nEmit exactly one raw Lisp form. Continue taking actions after replies.\n",
            agent.instructions
        );
        for id in &agent.inbox {
            if let Some(message) = self.world.state.messages.get(id) {
                text.push_str(&format!(
                    "Human message [{}]: {}{}\n",
                    id.0,
                    message.text,
                    if message.answered { " (answered)" } else { "" }
                ));
            }
        }
        for entry in &agent.transcript {
            text.push_str(&format!("> {}\n{}\n", entry.source, entry.result));
        }
        Ok(text)
    }

    fn finish(&mut self, source: String, result: String) -> Result<(), RuntimeError> {
        let current = self.current()?;
        self.world.state.agents[current]
            .transcript
            .push(TranscriptEntry { source, result });
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
                answered: false,
            },
        );
        let current = self.current()?;
        self.world.state.agents[current].inbox.push(id);
        Ok(())
    }
}
