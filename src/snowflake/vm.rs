use crate::snowflake::effects::{self, EffectError, EffectRequest, HostResult, TerminalEffect};
use crate::snowflake::runtime::{CANCEL_LISP, LISP_ACTIVE, PAUSE};
use crate::snowflake::value::{Capture, CellId, ChunkId, Value};
use crate::snowflake::world::{Binding, Transaction, World};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("invalid bytecode: {0}")]
    Invalid(String),
    #[error("evaluation failed: {0}")]
    Evaluation(String),
    #[error("external effect failed: {0}")]
    Effect(#[from] EffectError),
}

#[derive(Clone)]
enum Local {
    Direct(Value),
    Boxed(Option<CellId>),
}

struct CallFrame {
    chunk: ChunkId,
    ip: usize,
    base: usize,
    captures: Vec<CellId>,
    locals: Vec<Local>,
}

pub struct Task {
    source: String,
    chunk: ChunkId,
    ip: usize,
    base: usize,
    stack: Vec<Value>,
    captures: Vec<CellId>,
    locals: Vec<Local>,
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
    pub fn start(world: &World, entry: ChunkId) -> Result<Self, VmError> {
        let chunk = world
            .chunk(entry)
            .ok_or_else(|| VmError::Invalid("entry chunk does not exist".into()))?;
        if chunk.arity != 0 {
            return Err(VmError::Evaluation(format!(
                "entry requires {} arguments",
                chunk.arity
            )));
        }
        if !chunk.captures.is_empty() {
            return Err(VmError::Invalid("entry chunk has captures".into()));
        }
        Ok(Self {
            source: chunk.source.clone(),
            chunk: entry,
            ip: 0,
            base: 0,
            stack: Vec::new(),
            captures: Vec::new(),
            locals: Self::local_layout(chunk),
            calls: Vec::new(),
            transaction: Transaction::begin(world),
            awaiting_effect: false,
        })
    }

    pub fn poll(&mut self, world: &mut World, control: &AtomicU8) -> TaskPoll {
        if self.awaiting_effect {
            self.transaction.rollback(world);
            self.awaiting_effect = false;
            return TaskPoll::Failed(VmError::Invalid(
                "task was polled before its effect was resumed".into(),
            ));
        }
        loop {
            let signal = control.load(Ordering::Acquire);
            if signal & CANCEL_LISP != 0 {
                self.transaction.rollback(world);
                return TaskPoll::Cancelled;
            }
            if signal & PAUSE != 0 {
                return TaskPoll::Paused;
            }
            if signal & !(CANCEL_LISP | PAUSE | LISP_ACTIVE) != 0 {
                self.transaction.rollback(world);
                return TaskPoll::Failed(VmError::Invalid("unknown task control bits".into()));
            }
            match self.step(world) {
                Ok(Some(TaskPoll::Complete(value))) => {
                    self.transaction.commit(world);
                    return TaskPoll::Complete(value);
                }
                Ok(Some(TaskPoll::Terminal(effect))) => return TaskPoll::Terminal(effect),
                Ok(Some(poll)) => return poll,
                Ok(None) => {}
                Err(error) => {
                    self.transaction.rollback(world);
                    return TaskPoll::Failed(error);
                }
            }
        }
    }

    pub fn resume(&mut self, result: Result<Value, EffectError>) -> Result<(), VmError> {
        if !self.awaiting_effect {
            return Err(VmError::Invalid(
                "task is not awaiting an external effect".into(),
            ));
        }
        self.awaiting_effect = false;
        self.stack.push(result?);
        Ok(())
    }

    pub fn commit_boundary(&mut self, world: &World) {
        self.transaction.commit(world);
    }

    pub fn transaction(&mut self) -> &mut Transaction {
        &mut self.transaction
    }

    pub fn replace_checkpoint(&mut self, state: crate::snowflake::world::State) {
        self.transaction.replace_committed(state);
    }

    pub fn merge_runtime_state(&mut self, world: &World) {
        self.transaction.merge_runtime_state(world);
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn abort(self, world: &mut World) {
        self.transaction.abort(world);
    }

    fn local_layout(chunk: &crate::snowflake::value::Chunk) -> Vec<Local> {
        (0..chunk.locals)
            .map(|slot| {
                if chunk.boxed.binary_search(&slot).is_ok() {
                    Local::Boxed(None)
                } else {
                    Local::Direct(Value::Nil)
                }
            })
            .collect()
    }

    fn initialize_locals(world: &mut World, locals: &mut [Local]) {
        for local in locals {
            if let Local::Boxed(cell @ None) = local {
                *cell = Some(world.allocate_cell(Value::Nil));
            }
        }
    }

    fn read_local(&self, world: &World, index: u16) -> Result<Value, VmError> {
        match self.locals.get(usize::from(index)) {
            Some(Local::Direct(value)) => Ok(value.clone()),
            Some(Local::Boxed(Some(cell))) => Self::read_cell(world, *cell),
            Some(Local::Boxed(None)) => Err(VmError::Invalid("uninitialized boxed local".into())),
            None => Err(VmError::Invalid("local index out of range".into())),
        }
    }

    fn write_local(&mut self, world: &mut World, index: u16, value: Value) -> Result<(), VmError> {
        match self.locals.get_mut(usize::from(index)) {
            Some(Local::Direct(slot)) => *slot = value,
            Some(Local::Boxed(Some(cell))) => Self::write_cell(world, *cell, value)?,
            Some(Local::Boxed(None)) => {
                return Err(VmError::Invalid("uninitialized boxed local".into()));
            }
            None => return Err(VmError::Invalid("local index out of range".into())),
        }
        Ok(())
    }

    fn read_cell(world: &World, cell: CellId) -> Result<Value, VmError> {
        world
            .state
            .cells
            .get(cell.0 as usize)
            .and_then(Option::as_ref)
            .cloned()
            .ok_or_else(|| VmError::Invalid("capture cell does not exist".into()))
    }

    fn write_cell(world: &mut World, cell: CellId, value: Value) -> Result<(), VmError> {
        let slot = world
            .state
            .cells
            .get_mut(cell.0 as usize)
            .and_then(Option::as_mut)
            .ok_or_else(|| VmError::Invalid("capture cell does not exist".into()))?;
        *slot = value;
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, VmError> {
        if self.stack.len() <= self.base {
            return Err(VmError::Invalid("operand stack underflow".into()));
        }
        self.stack
            .pop()
            .ok_or_else(|| VmError::Invalid("operand stack underflow".into()))
    }

    fn step(&mut self, world: &mut World) -> Result<Option<TaskPoll>, VmError> {
        Self::initialize_locals(world, &mut self.locals);
        let chunk = world
            .chunk(self.chunk)
            .ok_or_else(|| VmError::Invalid("current chunk does not exist".into()))?;
        let op = chunk
            .code
            .get(self.ip)
            .cloned()
            .ok_or_else(|| VmError::Invalid("instruction pointer out of range".into()))?;
        self.ip = self
            .ip
            .checked_add(1)
            .ok_or_else(|| VmError::Invalid("instruction pointer overflow".into()))?;
        use crate::snowflake::value::Op;
        match op {
            Op::Const(index) => {
                let value = world
                    .chunk(self.chunk)
                    .and_then(|chunk| chunk.constants.get(index as usize))
                    .cloned()
                    .ok_or_else(|| VmError::Invalid("constant index out of range".into()))?;
                self.stack.push(value);
            }
            Op::GetGlobal(symbol) => {
                let value = world
                    .state
                    .globals
                    .get(&symbol)
                    .map(|binding| binding.value.clone())
                    .ok_or_else(|| VmError::Evaluation("undefined global".into()))?;
                self.stack.push(value);
            }
            Op::DefGlobal(symbol) => {
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| VmError::Invalid("operand stack underflow".into()))?;
                if world
                    .state
                    .globals
                    .get(&symbol)
                    .is_some_and(|binding| !binding.mutable)
                {
                    return Err(VmError::Evaluation(
                        "cannot redefine immutable global".into(),
                    ));
                }
                world.state.globals.insert(
                    symbol,
                    Binding {
                        value,
                        source: Some(self.chunk),
                        mutable: true,
                    },
                );
            }
            Op::SetGlobal(symbol) => {
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| VmError::Invalid("operand stack underflow".into()))?;
                let binding = world
                    .state
                    .globals
                    .get_mut(&symbol)
                    .ok_or_else(|| VmError::Evaluation("cannot set undefined global".into()))?;
                if !binding.mutable {
                    return Err(VmError::Evaluation("cannot set immutable global".into()));
                }
                binding.value = value;
                binding.source = Some(self.chunk);
            }
            Op::GetLocal(index) => {
                let value = self.read_local(world, index)?;
                self.stack.push(value);
            }
            Op::SetLocal(index) => {
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| VmError::Invalid("operand stack underflow".into()))?;
                self.write_local(world, index, value)?;
            }
            Op::GetCapture(index) => {
                let cell = *self
                    .captures
                    .get(usize::from(index))
                    .ok_or_else(|| VmError::Invalid("capture index out of range".into()))?;
                self.stack.push(Self::read_cell(world, cell)?);
            }
            Op::SetCapture(index) => {
                let value = self
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| VmError::Invalid("operand stack underflow".into()))?;
                let cell = *self
                    .captures
                    .get(usize::from(index))
                    .ok_or_else(|| VmError::Invalid("capture index out of range".into()))?;
                Self::write_cell(world, cell, value)?;
            }
            Op::Closure(child) => {
                let captures = world
                    .chunk(child)
                    .ok_or_else(|| VmError::Invalid("closure chunk does not exist".into()))?
                    .captures
                    .clone();
                let mut cells = Vec::with_capacity(captures.len());
                for capture in captures {
                    cells.push(match capture {
                        Capture::Local(index) => match self.locals.get(usize::from(index)) {
                            Some(Local::Boxed(Some(cell))) => *cell,
                            Some(Local::Boxed(None)) => {
                                return Err(VmError::Invalid("uninitialized boxed local".into()));
                            }
                            Some(Local::Direct(_)) => {
                                return Err(VmError::Invalid("captured local is not boxed".into()));
                            }
                            None => {
                                return Err(VmError::Invalid(
                                    "closure local capture is out of range".into(),
                                ));
                            }
                        },
                        Capture::Parent(index) => {
                            *self.captures.get(usize::from(index)).ok_or_else(|| {
                                VmError::Invalid("closure parent capture is out of range".into())
                            })?
                        }
                    });
                }
                self.stack.push(Value::Closure {
                    chunk: child,
                    captures: cells,
                });
            }
            Op::Pop => {
                self.pop()?;
            }
            Op::Jump(target) => self.jump(world, target)?,
            Op::JumpFalse(target) => {
                let condition = self.pop()?;
                if matches!(condition, Value::Nil | Value::Bool(false)) {
                    self.jump(world, target)?;
                } else {
                    self.check_target(world, target)?;
                }
            }
            Op::Call(arguments) => return self.call(world, arguments),
            Op::TailCall(arguments) => return self.tail_call(world, arguments),
            Op::Return => {
                let value = self.pop()?;
                return self.return_value(value);
            }
        }
        self.check_stack(world)?;
        Ok(None)
    }

    fn check_target(&self, world: &World, target: u32) -> Result<(), VmError> {
        let valid = world
            .chunk(self.chunk)
            .is_some_and(|chunk| (target as usize) < chunk.code.len());
        valid
            .then_some(())
            .ok_or_else(|| VmError::Invalid("jump target out of range".into()))
    }

    fn jump(&mut self, world: &World, target: u32) -> Result<(), VmError> {
        self.check_target(world, target)?;
        self.ip = target as usize;
        Ok(())
    }

    fn check_stack(&self, world: &World) -> Result<(), VmError> {
        let maximum = world
            .chunk(self.chunk)
            .ok_or_else(|| VmError::Invalid("current chunk does not exist".into()))?
            .max_stack as usize;
        (self.stack.len().saturating_sub(self.base) <= maximum)
            .then_some(())
            .ok_or_else(|| VmError::Invalid("operand stack exceeds chunk maximum".into()))
    }

    fn take_operands(&mut self, count: u16) -> Result<Vec<Value>, VmError> {
        let start = self
            .stack
            .len()
            .checked_sub(usize::from(count))
            .filter(|start| *start >= self.base)
            .ok_or_else(|| VmError::Invalid("operand stack underflow".into()))?;
        Ok(self.stack.drain(start..).collect())
    }

    fn operands(&self, arguments: u16) -> Result<(usize, Value, Vec<Value>), VmError> {
        let count = usize::from(arguments);
        let callable = self
            .stack
            .len()
            .checked_sub(count + 1)
            .filter(|index| *index >= self.base)
            .ok_or_else(|| VmError::Invalid("call operand stack underflow".into()))?;
        Ok((
            callable,
            self.stack[callable].clone(),
            self.stack[callable + 1..].to_vec(),
        ))
    }

    fn call(&mut self, world: &mut World, arguments: u16) -> Result<Option<TaskPoll>, VmError> {
        let (callable, function, values) = self.operands(arguments)?;
        match function {
            Value::Closure { chunk, captures } => {
                let locals = self.prepare_call(world, chunk, &captures, values)?;
                self.stack.truncate(callable);
                let frame = CallFrame {
                    chunk: self.chunk,
                    ip: self.ip,
                    base: self.base,
                    captures: std::mem::take(&mut self.captures),
                    locals: std::mem::take(&mut self.locals),
                };
                self.calls.push(frame);
                self.chunk = chunk;
                self.ip = 0;
                self.base = callable;
                self.captures = captures;
                self.locals = locals;
                Ok(None)
            }
            Value::Host(host) => {
                self.stack.truncate(callable);
                Ok(self.accept_host_result(effects::call(world, host, values)?))
            }
            _ => Err(VmError::Evaluation(
                "attempted to call a non-function".into(),
            )),
        }
    }

    fn accept_host_result(&mut self, result: HostResult) -> Option<TaskPoll> {
        match result {
            HostResult::Value(value) => {
                self.stack.push(value);
                None
            }
            HostResult::Effect(effect) => {
                self.awaiting_effect = true;
                Some(TaskPoll::Effect(effect))
            }
            HostResult::Terminal(effect) => Some(TaskPoll::Terminal(effect)),
        }
    }

    fn tail_call(
        &mut self,
        world: &mut World,
        arguments: u16,
    ) -> Result<Option<TaskPoll>, VmError> {
        let (_, function, values) = self.operands(arguments)?;
        match function {
            Value::Closure { chunk, captures } => {
                let locals = self.prepare_call(world, chunk, &captures, values)?;
                self.stack.truncate(self.base);
                self.chunk = chunk;
                self.ip = 0;
                self.captures = captures;
                self.locals = locals;
                Ok(None)
            }
            Value::Host(host) => {
                self.stack.truncate(self.base);
                Ok(self.accept_host_result(effects::call(world, host, values)?))
            }
            _ => Err(VmError::Evaluation(
                "attempted to call a non-function".into(),
            )),
        }
    }

    fn prepare_call(
        &self,
        world: &mut World,
        chunk_id: ChunkId,
        captures: &[CellId],
        arguments: Vec<Value>,
    ) -> Result<Vec<Local>, VmError> {
        let chunk = world
            .chunk(chunk_id)
            .ok_or_else(|| VmError::Invalid("called chunk does not exist".into()))?;
        if usize::from(chunk.arity) != arguments.len() {
            return Err(VmError::Evaluation(format!(
                "expected {} arguments, got {}",
                chunk.arity,
                arguments.len()
            )));
        }
        if captures.len() != chunk.captures.len() {
            return Err(VmError::Invalid("closure capture count mismatch".into()));
        }
        for cell in captures {
            Self::read_cell(world, *cell)?;
        }
        let mut locals = Self::local_layout(chunk);
        Self::initialize_locals(world, &mut locals);
        for (index, value) in arguments.into_iter().enumerate() {
            match &mut locals[index] {
                Local::Direct(slot) => *slot = value,
                Local::Boxed(Some(cell)) => Self::write_cell(world, *cell, value)?,
                Local::Boxed(None) => unreachable!(),
            }
        }
        Ok(locals)
    }

    fn return_value(&mut self, value: Value) -> Result<Option<TaskPoll>, VmError> {
        self.stack.truncate(self.base);
        if let Some(frame) = self.calls.pop() {
            self.chunk = frame.chunk;
            self.ip = frame.ip;
            self.base = frame.base;
            self.captures = frame.captures;
            self.locals = frame.locals;
            self.stack.push(value);
            Ok(None)
        } else {
            Ok(Some(TaskPoll::Complete(value)))
        }
    }
}
