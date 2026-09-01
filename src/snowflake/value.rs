use serde::{Deserialize, Serialize};
use std::collections::HashMap;

macro_rules! id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub u32);
    };
}

id!(AgentId);
id!(CellId);
id!(ChunkId);
id!(HostId);
id!(MessageId);
id!(SymbolId);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Symbol(SymbolId),
    Keyword(SymbolId),
    List(Vec<Value>),
    Vector(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Closure {
        chunk: ChunkId,
        captures: Vec<CellId>,
    },
    Host(HostId),
    Data {
        tag: SymbolId,
        fields: Vec<Value>,
    },
}

#[derive(Debug, Default)]
pub struct RootSet {
    pub values: Vec<Value>,
    pub cells: Vec<CellId>,
    pub chunks: Vec<ChunkId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capture {
    Local(u16),
    Parent(u16),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Op {
    Const(u32),
    GetGlobal(SymbolId),
    DefGlobal(SymbolId),
    SetGlobal(SymbolId),
    GetLocal(u16),
    SetLocal(u16),
    GetCapture(u16),
    SetCapture(u16),
    Closure(ChunkId),
    Pop,
    Jump(u32),
    JumpFalse(u32),
    Call(u16),
    TailCall(u16),
    Return,
    List(u16),
    Vector(u16),
    Map(u16),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub name: SymbolId,
    pub source: String,
    pub arity: u16,
    pub locals: u16,
    pub max_stack: u16,
    pub captures: Vec<Capture>,
    pub constants: Vec<Value>,
    pub code: Vec<Op>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Symbols {
    names: Vec<String>,
    #[serde(skip)]
    index: HashMap<String, SymbolId>,
}

impl Symbols {
    pub fn intern(&mut self, _name: &str) -> SymbolId {
        todo!("intern or return an existing symbol")
    }

    pub fn name(&self, _symbol: SymbolId) -> Option<&str> {
        todo!("resolve a symbol without panicking")
    }

    pub fn rebuild_index(&mut self) {
        todo!("rebuild the skipped reverse index after image load")
    }
}
