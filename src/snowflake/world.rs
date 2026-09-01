use crate::snowflake::value::{
    AgentId, CellId, Chunk, ChunkId, HostId, MessageId, RootSet, SymbolId, Symbols, Value,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

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

    pub fn with_committed<R>(
        &mut self,
        _world: &mut World,
        _operation: impl FnOnce(&World) -> R,
    ) -> R {
        todo!("RAII-swap committed state into the world, then restore working state")
    }
}

impl World {
    pub fn install_host(&mut self, _name: &str, _host: HostId) {
        todo!("intern and install an immutable host binding")
    }

    pub fn allocate_cell(&mut self, _value: Value) -> CellId {
        todo!("allocate a checked stable cell id")
    }

    pub fn insert_chunk(&mut self, _chunk: Chunk) -> ChunkId {
        todo!("validate and transactionally insert a chunk")
    }

    pub fn chunk(&self, _id: ChunkId) -> Option<&Chunk> {
        todo!("look up a chunk")
    }

    pub fn compact(&mut self, _runtime_roots: &RootSet) {
        todo!("sweep cells/chunks unreachable from durable state and live tasks")
    }
}
