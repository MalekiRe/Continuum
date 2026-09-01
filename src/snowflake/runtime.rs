use crate::snowflake::effects::{EffectError, EffectRequest, ExternalRun, TerminalEffect};
use crate::snowflake::image::ImageStore;
use crate::snowflake::vm::{Task, VmError};
use crate::snowflake::world::World;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub const RUN: u8 = 0;
pub const PAUSE: u8 = 1;
pub const CANCEL_LISP: u8 = 2;

pub enum Active {
    Idle,
    Lisp(Task),
    External {
        task: Task,
        request: EffectRequest,
        run: ExternalRun,
    },
    Thinking(ExternalRun),
}

pub enum Command {
    Snapshot,
    CancelLisp,
    CancelExternal,
    HumanMessage(String),
    Shutdown,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Vm(#[from] VmError),
    #[error(transparent)]
    Effect(#[from] EffectError),
    #[error("runtime invariant: {0}")]
    Invariant(&'static str),
}

pub struct Runtime {
    pub world: World,
    active: Active,
    parked: Vec<Task>,
    control: Arc<AtomicU8>,
    images: ImageStore,
}

impl Runtime {
    pub fn new(_world: World, _images: ImageStore) -> Self {
        todo!("install hosts and create an idle runtime")
    }

    pub async fn run(&mut self) -> Result<(), RuntimeError> {
        todo!("drive model generation, Lisp tasks, effects, controls, and agents")
    }

    fn drive_task(&mut self, _task: Task) -> Result<(), RuntimeError> {
        todo!("poll and transition on every TaskPoll variant")
    }

    fn start_effect(&mut self, _task: Task, _request: EffectRequest) {
        todo!("retain the task, cancellable future, and World independently")
    }

    fn finish_terminal(&mut self, _effect: TerminalEffect) -> Result<(), RuntimeError> {
        todo!("complete message reply or child return, resuming a live parent if present")
    }

    fn command(&mut self, _command: Command) -> Result<(), RuntimeError> {
        todo!("separate pause, Lisp cancellation, and external cancellation")
    }

    fn request_pause(&self) {
        self.control.store(PAUSE, Ordering::Release);
    }
}
