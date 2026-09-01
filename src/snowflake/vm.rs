use crate::snowflake::effects::{EffectError, EffectRequest, TerminalEffect};
use crate::snowflake::value::{CellId, ChunkId, RootSet, Value};
use crate::snowflake::world::{Transaction, World};
use std::sync::atomic::AtomicU8;

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("invalid bytecode: {0}")]
    Invalid(String),
    #[error("evaluation failed: {0}")]
    Evaluation(String),
    #[error("external effect failed: {0}")]
    Effect(#[from] EffectError),
}

struct CallFrame {
    chunk: ChunkId,
    ip: usize,
    base: usize,
    captures: Vec<CellId>,
}

pub struct Task {
    chunk: ChunkId,
    ip: usize,
    base: usize,
    stack: Vec<Value>,
    captures: Vec<CellId>,
    calls: Vec<CallFrame>,
    transaction: Transaction,
    awaiting_effect: bool,
}

pub enum TaskPoll {
    Complete(Value),
    Effect(EffectRequest),
    Terminal(TerminalEffect),
    Paused,
    Cancelled,
    Failed(VmError),
}

impl Task {
    pub fn start(_world: &World, _entry: ChunkId) -> Result<Self, VmError> {
        todo!("create a zero-argument task and transaction checkpoint")
    }

    pub fn poll(&mut self, _world: &mut World, _control: &AtomicU8) -> TaskPoll {
        todo!("execute until completion, effect, pause, cancellation, or error")
    }

    pub fn resume(&mut self, _result: Result<Value, EffectError>) -> Result<(), VmError> {
        todo!("inject exactly one effect result into an awaiting call")
    }

    pub fn commit_boundary(&mut self, _world: &World) {
        todo!("commit mutations before dispatching an accepted external effect")
    }

    pub fn abort(self, _world: &mut World) {
        todo!("restore the task's current transaction segment")
    }

    pub fn roots(&self) -> RootSet {
        todo!("collect value, cell, and chunk roots from the full call stack")
    }

    fn step(&mut self, _world: &mut World) -> Result<Option<TaskPoll>, VmError> {
        todo!("execute one checked bytecode operation")
    }

    fn call(&mut self, _world: &mut World, _arguments: u16) -> Result<Option<TaskPoll>, VmError> {
        todo!("call a closure or host value without losing the call site")
    }

    fn return_value(&mut self, _value: Value) -> Result<Option<TaskPoll>, VmError> {
        todo!("restore a caller or complete the task")
    }
}
