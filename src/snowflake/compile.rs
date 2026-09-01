use crate::snowflake::value::{Capture, Chunk, ChunkId, Op, SymbolId, Value};
use crate::snowflake::world::World;
use std::sync::atomic::AtomicU8;

#[derive(Debug, thiserror::Error)]
#[error("compile error at byte {offset}: {message}")]
pub struct CompileError {
    pub offset: usize,
    pub message: String,
}

enum ReadFrame {
    List(Vec<Value>),
    Vector(Vec<Value>),
    Map {
        entries: Vec<(Value, Value)>,
        key: Option<Value>,
    },
    Prefix(SymbolId),
}

struct Reader<'a> {
    source: &'a str,
    offset: usize,
    frames: Vec<ReadFrame>,
}

impl<'a> Reader<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            frames: Vec::new(),
        }
    }

    fn one(&mut self, _world: &mut World) -> Result<Value, CompileError> {
        todo!("iteratively read exactly one form and reject trailing input")
    }

    fn atom(&mut self, _world: &mut World) -> Result<Value, CompileError> {
        todo!("read a string, number, keyword, or interned symbol")
    }

    fn accept(&mut self, _value: Value) -> Result<Option<Value>, CompileError> {
        todo!("feed a value through prefix and collection frames")
    }

    fn finish(&mut self, _closing: char) -> Result<Option<Value>, CompileError> {
        todo!("close the matching collection and reject odd maps")
    }
}

#[derive(Clone, Copy)]
enum Place {
    Local(u16),
    Capture(u16),
    Global(SymbolId),
}

struct Local {
    name: SymbolId,
    depth: u16,
    captured: bool,
}

struct Builder {
    name: SymbolId,
    source: String,
    arity: u16,
    code: Vec<Op>,
    constants: Vec<Value>,
    captures: Vec<Capture>,
    locals: Vec<Local>,
    scope: u16,
    stack: i32,
    max_stack: u16,
}

impl Builder {
    fn emit(&mut self, _op: Op) {
        todo!("append an operation and update checked stack depth")
    }

    fn finish(self) -> Result<Chunk, CompileError> {
        todo!("finish with Return and produce a self-consistent chunk")
    }
}

struct Compiler<'a> {
    world: &'a mut World,
    control: &'a AtomicU8,
    functions: Vec<Builder>,
}

impl Compiler<'_> {
    fn expression(&mut self, _form: &Value, _tail: bool) -> Result<(), CompileError> {
        todo!("compile literals, symbols, calls, or a recognized special form")
    }

    fn special(
        &mut self,
        _name: SymbolId,
        _arguments: &[Value],
        _tail: bool,
    ) -> Result<bool, CompileError> {
        todo!("compile quote/if/begin/lambda/define/set!/let variants")
    }

    fn lambda(&mut self, _parameters: &[Value], _body: &[Value]) -> Result<ChunkId, CompileError> {
        todo!("push a child builder, compile it, and propagate captures")
    }

    fn resolve(&mut self, _name: SymbolId) -> Place {
        todo!("walk function builders outward and register capture links inward")
    }

    fn finish(self) -> Result<ChunkId, CompileError> {
        todo!("validate cancellation, finish the root, and insert all chunks")
    }
}

pub fn compile(
    _world: &mut World,
    _source: &str,
    _control: &AtomicU8,
) -> Result<ChunkId, CompileError> {
    todo!("read, direct-compile, closure-convert, and transactionally insert")
}
