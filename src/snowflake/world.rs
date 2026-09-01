use crate::snowflake::value::{
    AgentId, Capture, CellId, Chunk, ChunkId, HostId, MessageId, Op, RootSet, SymbolId, Symbols,
    Value,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub value: Value,
    pub source: Option<ChunkId>,
    pub mutable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub source: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub text: String,
    pub answered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub instructions: String,
    pub transcript: Vec<TranscriptEntry>,
    pub inbox: Vec<MessageId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub symbols: Symbols,
    pub globals: IndexMap<SymbolId, Binding>,
    pub cells: Vec<Option<Value>>,
    pub code: Vec<Option<Chunk>>,
    pub agents: Vec<Agent>,
    pub messages: IndexMap<MessageId, Message>,
    pub next_agent: u32,
    pub next_message: u32,
}

#[derive(Debug, Clone, Default)]
pub struct World {
    pub state: State,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    committed: State,
}

impl Transaction {
    pub fn begin(world: &World) -> Self {
        Self {
            committed: world.state.clone(),
        }
    }

    pub fn commit(&mut self, world: &World) {
        self.committed = world.state.clone();
    }

    pub fn abort(self, world: &mut World) {
        world.state = self.committed;
    }

    pub fn rollback(&mut self, world: &mut World) {
        world.state = self.committed.clone();
    }

    pub fn with_committed<R>(
        &mut self,
        world: &mut World,
        operation: impl FnOnce(&World) -> R,
    ) -> R {
        struct Restore<'a> {
            world: &'a mut World,
            committed: &'a mut State,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                std::mem::swap(&mut self.world.state, self.committed);
            }
        }

        std::mem::swap(&mut world.state, &mut self.committed);
        let view = Restore {
            world,
            committed: &mut self.committed,
        };
        operation(view.world)
    }
}

impl World {
    pub fn install_host(&mut self, name: &str, host: HostId) {
        let symbol = self.state.symbols.intern(name);
        match self.state.globals.get(&symbol) {
            Some(binding) if binding.value == Value::Host(host) && !binding.mutable => return,
            Some(_) => panic!("cannot replace global `{name}` with a host"),
            None => {}
        }
        self.state.globals.insert(
            symbol,
            Binding {
                value: Value::Host(host),
                source: None,
                mutable: false,
            },
        );
    }

    pub fn allocate_cell(&mut self, value: Value) -> CellId {
        let id = u32::try_from(self.state.cells.len()).expect("cell arena exhausted");
        self.state.cells.push(Some(value));
        CellId(id)
    }

    pub fn insert_chunk(&mut self, chunk: Chunk) -> ChunkId {
        self.validate_chunk(&chunk)
            .unwrap_or_else(|message| panic!("invalid chunk: {message}"));
        let id = u32::try_from(self.state.code.len()).expect("chunk arena exhausted");
        self.state.code.push(Some(chunk));
        ChunkId(id)
    }

    pub fn chunk(&self, id: ChunkId) -> Option<&Chunk> {
        self.state.code.get(usize::try_from(id.0).ok()?)?.as_ref()
    }

    pub fn compact(&mut self, runtime_roots: &RootSet) {
        let mut values = runtime_roots.values.clone();
        let mut cells: Vec<_> = runtime_roots.cells.clone();
        let mut chunks: Vec<_> = runtime_roots.chunks.clone();
        for binding in self.state.globals.values() {
            values.push(binding.value.clone());
            chunks.extend(binding.source);
        }
        let mut live_cells = HashSet::new();
        let mut live_chunks = HashSet::new();
        loop {
            while let Some(value) = values.pop() {
                match value {
                    Value::Closure { chunk, captures } => {
                        chunks.push(chunk);
                        cells.extend(captures);
                    }
                    Value::List(items) | Value::Vector(items) => values.extend(items),
                    Value::Map(entries) => {
                        for (key, value) in entries {
                            values.push(key);
                            values.push(value);
                        }
                    }
                    Value::Data { fields, .. } => values.extend(fields),
                    _ => {}
                }
            }
            while let Some(id) = cells.pop() {
                if live_cells.insert(id)
                    && let Some(Some(value)) = self.state.cells.get(id.0 as usize)
                {
                    values.push(value.clone());
                }
            }
            while let Some(id) = chunks.pop() {
                if live_chunks.insert(id)
                    && let Some(Some(chunk)) = self.state.code.get(id.0 as usize)
                {
                    values.extend(chunk.constants.iter().cloned());
                    chunks.extend(chunk.code.iter().filter_map(|op| match op {
                        Op::Closure(id) => Some(*id),
                        _ => None,
                    }));
                }
            }
            if values.is_empty() && cells.is_empty() && chunks.is_empty() {
                break;
            }
        }
        for (id, cell) in self.state.cells.iter_mut().enumerate() {
            if !live_cells.contains(&CellId(id as u32)) {
                *cell = None;
            }
        }
        for (id, chunk) in self.state.code.iter_mut().enumerate() {
            if !live_chunks.contains(&ChunkId(id as u32)) {
                *chunk = None;
            }
        }
    }

    fn validate_chunk(&self, chunk: &Chunk) -> Result<(), String> {
        if self.state.symbols.name(chunk.name).is_none() {
            return Err("unknown chunk name".into());
        }
        if chunk.arity > chunk.locals {
            return Err("arity exceeds local slots".into());
        }
        if chunk.boxed.iter().any(|slot| *slot >= chunk.locals)
            || chunk.boxed.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("invalid boxed local slots".into());
        }
        if chunk.code.is_empty() {
            return Err("empty instruction stream".into());
        }
        let mut depths = vec![None; chunk.code.len()];
        let mut pending = vec![(0_usize, 0_i32)];
        let mut maximum = 0_i32;
        while let Some((ip, before)) = pending.pop() {
            if ip >= chunk.code.len() {
                return Err("jump outside instruction stream".into());
            }
            if let Some(known) = depths[ip] {
                if known != before {
                    return Err("inconsistent stack depth at join".into());
                }
                continue;
            }
            depths[ip] = Some(before);
            let op = &chunk.code[ip];
            let (needed, delta) = match op {
                Op::Const(index) => {
                    if (*index as usize) >= chunk.constants.len() {
                        return Err("constant index out of range".into());
                    }
                    (0, 1)
                }
                Op::GetGlobal(symbol) => {
                    if self.state.symbols.name(*symbol).is_none() {
                        return Err("unknown global symbol".into());
                    }
                    (0, 1)
                }
                Op::DefGlobal(symbol) | Op::SetGlobal(symbol) => {
                    if self.state.symbols.name(*symbol).is_none() {
                        return Err("unknown global symbol".into());
                    }
                    (1, 0)
                }
                Op::GetLocal(index) => {
                    if *index >= chunk.locals {
                        return Err("local index out of range".into());
                    }
                    (0, 1)
                }
                Op::SetLocal(index) => {
                    if *index >= chunk.locals {
                        return Err("local index out of range".into());
                    }
                    (1, 0)
                }
                Op::GetCapture(index) => {
                    if (*index as usize) >= chunk.captures.len() {
                        return Err("capture index out of range".into());
                    }
                    (0, 1)
                }
                Op::SetCapture(index) => {
                    if (*index as usize) >= chunk.captures.len() {
                        return Err("capture index out of range".into());
                    }
                    (1, 0)
                }
                Op::Closure(id) => {
                    let child = self
                        .chunk(*id)
                        .ok_or_else(|| "closure chunk does not exist".to_owned())?;
                    for capture in &child.captures {
                        match capture {
                            Capture::Local(index) if *index >= chunk.locals => {
                                return Err("closure local capture is out of range".into());
                            }
                            Capture::Parent(index)
                                if usize::from(*index) >= chunk.captures.len() =>
                            {
                                return Err("closure parent capture is out of range".into());
                            }
                            _ => {}
                        }
                    }
                    (0, 1)
                }
                Op::Pop | Op::JumpFalse(_) | Op::Return => (1, -1),
                Op::Jump(_) => (0, 0),
                Op::Call(count) | Op::TailCall(count) => {
                    (i32::from(*count) + 1, -i32::from(*count))
                }
                Op::List(count) | Op::Vector(count) => (i32::from(*count), 1 - i32::from(*count)),
                Op::Map(count) => (i32::from(*count) * 2, 1 - i32::from(*count) * 2),
            };
            if before < needed {
                return Err(format!("stack underflow at instruction {ip}"));
            }
            let after = before + delta;
            maximum = maximum.max(after);
            let target = match op {
                Op::Jump(target) => Some(*target),
                Op::JumpFalse(target) => {
                    pending.push((ip + 1, after));
                    Some(*target)
                }
                Op::Return => None,
                _ => {
                    pending.push((ip + 1, after));
                    None
                }
            };
            if let Some(target) = target {
                pending.push((target as usize, after));
            }
        }
        if maximum != i32::from(chunk.max_stack) {
            return Err("incorrect maximum stack depth".into());
        }
        Ok(())
    }
}
