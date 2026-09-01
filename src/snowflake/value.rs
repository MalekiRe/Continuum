use serde::{Deserialize, Serialize};
use std::collections::HashMap;

macro_rules! id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub u32);
    };
}

id!(CellId);
id!(ChunkId);
id!(HostId);
id!(MessageId);
id!(SymbolId);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    String(String),
    Symbol(SymbolId),
    List(Vec<Value>),
    Closure {
        chunk: ChunkId,
        captures: Vec<CellId>,
    },
    Host(HostId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Capture {
    Local(u16),
    Parent(u16),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub name: SymbolId,
    pub source: String,
    pub arity: u16,
    pub locals: u16,
    pub max_stack: u16,
    pub boxed: Vec<u16>,
    pub captures: Vec<Capture>,
    pub constants: Vec<Value>,
    pub code: Vec<Op>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symbols {
    names: Vec<String>,
    #[serde(skip)]
    index: HashMap<String, SymbolId>,
}

impl Symbols {
    pub fn intern(&mut self, name: &str) -> SymbolId {
        if let Some(symbol) = self.index.get(name) {
            return *symbol;
        }
        let id = u32::try_from(self.names.len()).expect("symbol table exhausted");
        let symbol = SymbolId(id);
        self.names.push(name.to_owned());
        self.index.insert(name.to_owned(), symbol);
        symbol
    }

    pub fn get(&self, name: &str) -> Option<SymbolId> {
        self.index.get(name).copied()
    }

    pub fn name(&self, symbol: SymbolId) -> Option<&str> {
        self.names
            .get(usize::try_from(symbol.0).ok()?)
            .map(String::as_str)
    }

    pub fn rebuild_index(&mut self) {
        self.index.clear();
        self.index
            .extend(self.names.iter().enumerate().map(|(id, name)| {
                (
                    name.clone(),
                    SymbolId(u32::try_from(id).expect("symbol table exhausted")),
                )
            }));
    }
}
