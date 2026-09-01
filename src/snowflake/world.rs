use crate::snowflake::value::{
    Capture, CellId, Chunk, ChunkId, HostId, MessageId, Op, SymbolId, Symbols, Value,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub value: Value,
    pub source: Option<ChunkId>,
    pub mutable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptEntry {
    pub source: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub text: String,
    pub created_at: String,
    pub reply: Option<String>,
    pub reply_at: Option<String>,
    pub reply_order: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub name: String,
    pub instructions: String,
    pub transcript: Vec<TranscriptEntry>,
    pub inbox: Vec<MessageId>,
}

impl Agent {
    pub fn new(name: String, instructions: String) -> Self {
        Self {
            name,
            instructions,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub symbols: Symbols,
    pub globals: IndexMap<SymbolId, Binding>,
    pub cells: Vec<Option<Value>>,
    pub code: Vec<Option<Chunk>>,
    pub agents: Vec<Agent>,
    pub messages: IndexMap<MessageId, Message>,
    pub next_message: u32,
}

#[derive(Debug, Clone, Default)]
pub struct World {
    pub state: State,
}

#[derive(Debug, Clone)]
pub struct Transaction(State);

impl Transaction {
    pub fn begin(world: &World) -> Self {
        Self(world.state.clone())
    }

    pub fn commit(&mut self, world: &World) {
        self.0 = world.state.clone();
    }

    pub fn committed(&self) -> &State {
        &self.0
    }

    pub fn replace_committed(&mut self, state: State) {
        self.0 = state;
    }

    pub fn merge_runtime_state(&mut self, world: &World) {
        self.0.agents = world.state.agents.clone();
        self.0.messages = world.state.messages.clone();
        self.0.next_message = world.state.next_message;
    }

    pub fn abort(self, world: &mut World) {
        world.state = self.0;
    }

    pub fn rollback(&mut self, world: &mut World) {
        world.state = self.0.clone();
    }
}

impl World {
    pub fn install_host(&mut self, name: &str, host: HostId) {
        let symbol = self.state.symbols.intern(name);
        if self
            .state
            .globals
            .get(&symbol)
            .is_some_and(|binding| binding.value == Value::Host(host) && !binding.mutable)
        {
            return;
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

    pub(crate) fn validate_chunk(&self, chunk: &Chunk) -> Result<(), String> {
        if self.state.symbols.name(chunk.name).is_none() {
            return Err("unknown chunk name".into());
        }
        if chunk.arity > chunk.locals || chunk.captures.len() > usize::from(u16::MAX) {
            return Err("arity, locals, or captures exceed VM indexes".into());
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
        let mut pending = Vec::new();
        let mut maximum = 0_i32;
        for root in 0..chunk.code.len() {
            if depths[root].is_some() {
                continue;
            }
            pending.push((root, 0_i32));
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
                    Op::Const(index) if (*index as usize) >= chunk.constants.len() => {
                        return Err("constant index out of range".into());
                    }
                    Op::GetGlobal(symbol) | Op::DefGlobal(symbol) | Op::SetGlobal(symbol)
                        if self.state.symbols.name(*symbol).is_none() =>
                    {
                        return Err("unknown global symbol".into());
                    }
                    Op::GetLocal(index) | Op::SetLocal(index) if *index >= chunk.locals => {
                        return Err("local index out of range".into());
                    }
                    Op::GetCapture(index) | Op::SetCapture(index)
                        if (*index as usize) >= chunk.captures.len() =>
                    {
                        return Err("capture index out of range".into());
                    }
                    Op::Const(_) | Op::GetGlobal(_) | Op::GetLocal(_) | Op::GetCapture(_) => (0, 1),
                    Op::DefGlobal(_) | Op::SetGlobal(_) | Op::SetLocal(_) | Op::SetCapture(_) => {
                        (1, 0)
                    }
                    Op::Closure(id) => {
                        let child = self
                            .chunk(*id)
                            .ok_or_else(|| "closure chunk does not exist".to_owned())?;
                        for capture in &child.captures {
                            match capture {
                                Capture::Local(index)
                                    if *index >= chunk.locals
                                        || chunk.boxed.binary_search(index).is_err() =>
                                {
                                    return Err("closure local capture is not boxed".into());
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
                };
                if before < needed || matches!(op, Op::Return) && before != 1 {
                    return Err(format!("invalid stack depth at instruction {ip}"));
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
        }
        if maximum != i32::from(chunk.max_stack) {
            return Err("incorrect maximum stack depth".into());
        }
        Ok(())
    }
}
