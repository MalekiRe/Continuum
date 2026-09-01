use crate::snowflake::value::{Capture, Chunk, ChunkId, Op, SymbolId, Value};
use crate::snowflake::world::World;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, thiserror::Error)]
#[error("compile error at byte {offset}: {message}")]
pub struct CompileError {
    pub offset: usize,
    pub message: String,
}

fn error(offset: usize, message: impl Into<String>) -> CompileError {
    CompileError {
        offset,
        message: message.into(),
    }
}

enum ReadFrame {
    List(Vec<Value>),
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

    fn one(&mut self, world: &mut World) -> Result<Value, CompileError> {
        let mut result = None;
        loop {
            self.space();
            if self.offset == self.source.len() {
                if let Some(value) = result {
                    return Ok(value);
                }
                return Err(error(
                    self.offset,
                    if self.frames.is_empty() {
                        "expected one form"
                    } else {
                        "unterminated form"
                    },
                ));
            }
            if result.is_some() {
                return Err(error(self.offset, "trailing input after first form"));
            }
            let start = self.offset;
            let character = self.bump().expect("offset is not at end");
            let accepted = match character {
                '(' => {
                    self.frames.push(ReadFrame::List(Vec::new()));
                    None
                }
                '[' => return Err(error(start, "vectors are not supported")),
                '{' => {
                    self.frames.push(ReadFrame::Map {
                        entries: Vec::new(),
                        key: None,
                    });
                    None
                }
                ')' | ']' | '}' => self.finish(character)?,
                '\'' => {
                    let quote = world.state.symbols.intern("quote");
                    self.frames.push(ReadFrame::Prefix(quote));
                    None
                }
                '`' | ',' => return Err(error(start, "unsupported reader prefix")),
                _ => {
                    self.offset = start;
                    let value = self.atom(world)?;
                    self.accept(value)?
                }
            };
            if accepted.is_some() {
                result = accepted;
            }
        }
    }

    fn atom(&mut self, world: &mut World) -> Result<Value, CompileError> {
        let start = self.offset;
        if self.peek() == Some('"') {
            self.bump();
            let mut escaped = false;
            while let Some(character) = self.bump() {
                if character == '"' && !escaped {
                    let token = &self.source[start..self.offset];
                    return serde_json::from_str(token)
                        .map(Value::String)
                        .map_err(|failure| error(start, format!("invalid string: {failure}")));
                }
                if character == '\n' || character == '\r' {
                    return Err(error(start, "newline in string"));
                }
                escaped = character == '\\' && !escaped;
            }
            return Err(error(start, "unterminated string"));
        }
        while self
            .peek()
            .is_some_and(|character| !Self::delimiter(character))
        {
            self.bump();
        }
        let token = &self.source[start..self.offset];
        if token.is_empty() {
            return Err(error(start, "expected an atom"));
        }
        match token {
            "nil" => Ok(Value::Nil),
            "true" | "#t" => Ok(Value::Bool(true)),
            "false" | "#f" => Ok(Value::Bool(false)),
            _ => {
                if let Ok(integer) = token.parse::<i64>() {
                    return Ok(Value::Int(integer));
                }
                if (token.contains('.') || token.contains('e') || token.contains('E'))
                    && let Ok(float) = token.parse::<f64>()
                    && float.is_finite()
                {
                    return Ok(Value::Float(float));
                }
                Ok(Value::Symbol(world.state.symbols.intern(token)))
            }
        }
    }

    fn accept(&mut self, mut value: Value) -> Result<Option<Value>, CompileError> {
        loop {
            match self.frames.last_mut() {
                None => return Ok(Some(value)),
                Some(ReadFrame::List(values)) => {
                    values.push(value);
                    return Ok(None);
                }
                Some(ReadFrame::Map { entries, key }) => {
                    if let Some(key) = key.take() {
                        entries.push((key, value));
                    } else {
                        *key = Some(value);
                    }
                    return Ok(None);
                }
                Some(ReadFrame::Prefix(_)) => {
                    let Some(ReadFrame::Prefix(prefix)) = self.frames.pop() else {
                        unreachable!()
                    };
                    value = Value::List(vec![Value::Symbol(prefix), value]);
                }
            }
        }
    }

    fn finish(&mut self, closing: char) -> Result<Option<Value>, CompileError> {
        let Some(frame) = self.frames.pop() else {
            return Err(error(
                self.offset - closing.len_utf8(),
                "unexpected closing delimiter",
            ));
        };
        let value = match (closing, frame) {
            (')', ReadFrame::List(values)) => Value::List(values),
            ('}', ReadFrame::Map { entries, key: None }) => Value::Map(entries),
            ('}', ReadFrame::Map { key: Some(_), .. }) => {
                return Err(error(
                    self.offset - 1,
                    "map requires an even number of forms",
                ));
            }
            (_, ReadFrame::Prefix(_)) => {
                return Err(error(self.offset - 1, "prefix is missing its form"));
            }
            _ => return Err(error(self.offset - 1, "mismatched closing delimiter")),
        };
        self.accept(value)
    }

    fn space(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            if self.peek() != Some(';') {
                break;
            }
            while self.peek().is_some_and(|character| character != '\n') {
                self.bump();
            }
        }
    }

    fn delimiter(character: char) -> bool {
        character.is_whitespace()
            || matches!(character, '(' | ')' | '[' | ']' | '{' | '}' | '\'' | ';')
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
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
    slot: u16,
}

struct Builder {
    name: SymbolId,
    source: String,
    arity: u16,
    code: Vec<Op>,
    constants: Vec<Value>,
    captures: Vec<Capture>,
    boxed: Vec<u16>,
    locals: Vec<Local>,
    local_slots: u16,
    scope: u16,
    stack: i32,
    max_stack: u16,
}

impl Builder {
    fn new(name: SymbolId, source: &str) -> Self {
        Self {
            name,
            source: source.to_owned(),
            arity: 0,
            code: Vec::new(),
            constants: Vec::new(),
            captures: Vec::new(),
            boxed: Vec::new(),
            locals: Vec::new(),
            local_slots: 0,
            scope: 0,
            stack: 0,
            max_stack: 0,
        }
    }

    fn emit(&mut self, op: Op) -> Result<(), CompileError> {
        let (needed, delta) = match &op {
            Op::Const(_)
            | Op::GetGlobal(_)
            | Op::GetLocal(_)
            | Op::GetCapture(_)
            | Op::Closure(_) => (0, 1),
            Op::DefGlobal(_) | Op::SetGlobal(_) | Op::SetLocal(_) | Op::SetCapture(_) => (1, 0),
            Op::Pop | Op::JumpFalse(_) | Op::Return => (1, -1),
            Op::Jump(_) => (0, 0),
            Op::Call(count) | Op::TailCall(count) => (i32::from(*count) + 1, -i32::from(*count)),
            Op::Map(count) => (i32::from(*count) * 2, 1 - i32::from(*count) * 2),
        };
        if self.stack < needed {
            return Err(error(0, "compiler stack underflow"));
        }
        self.stack = self
            .stack
            .checked_add(delta)
            .ok_or_else(|| error(0, "compiler stack overflow"))?;
        self.max_stack = self
            .max_stack
            .max(u16::try_from(self.stack).map_err(|_| error(0, "bytecode stack exceeds u16"))?);
        self.code.push(op);
        Ok(())
    }

    fn constant(&mut self, value: Value) -> Result<(), CompileError> {
        let index = u32::try_from(self.constants.len())
            .map_err(|_| error(0, "constant table exceeds u32"))?;
        self.constants.push(value);
        self.emit(Op::Const(index))
    }

    fn add_local(&mut self, name: SymbolId) -> Result<u16, CompileError> {
        let slot = self.local_slots;
        self.local_slots = self
            .local_slots
            .checked_add(1)
            .ok_or_else(|| error(0, "too many locals"))?;
        self.locals.push(Local { name, slot });
        Ok(slot)
    }

    fn finish(mut self) -> Result<Chunk, CompileError> {
        if self.stack != 1 {
            return Err(error(0, "function body does not produce exactly one value"));
        }
        self.emit(Op::Return)?;
        Ok(Chunk {
            name: self.name,
            source: self.source,
            arity: self.arity,
            locals: self.local_slots,
            max_stack: self.max_stack,
            boxed: self.boxed,
            captures: self.captures,
            constants: self.constants,
            code: self.code,
        })
    }
}

struct Compiler<'a> {
    world: &'a mut World,
    control: &'a AtomicU8,
    functions: Vec<Builder>,
    pending: Vec<Chunk>,
}

impl Compiler<'_> {
    fn current(&mut self) -> &mut Builder {
        self.functions
            .last_mut()
            .expect("compiler always has a function")
    }

    fn expression(&mut self, form: &Value, tail: bool) -> Result<(), CompileError> {
        if self.control.load(Ordering::Acquire) != 0 {
            return Err(error(0, "compilation cancelled"));
        }
        match form {
            Value::Symbol(name) => {
                let place = self.resolve(*name)?;
                self.current().emit(match place {
                    Place::Local(index) => Op::GetLocal(index),
                    Place::Capture(index) => Op::GetCapture(index),
                    Place::Global(symbol) => Op::GetGlobal(symbol),
                })
            }
            Value::List(forms) if forms.is_empty() => {
                self.current().constant(Value::List(Vec::new()))
            }
            Value::List(forms) => {
                if let Value::Symbol(name) = forms[0]
                    && self.special(name, &forms[1..], tail)?
                {
                    return Ok(());
                }
                let count = u16::try_from(forms.len() - 1)
                    .map_err(|_| error(0, "too many call arguments"))?;
                for form in forms {
                    self.expression(form, false)?;
                }
                self.current().emit(if tail {
                    Op::TailCall(count)
                } else {
                    Op::Call(count)
                })
            }
            Value::Map(entries) => {
                let count =
                    u16::try_from(entries.len()).map_err(|_| error(0, "map is too large"))?;
                for (key, value) in entries {
                    self.expression(key, false)?;
                    self.expression(value, false)?;
                }
                self.current().emit(Op::Map(count))
            }
            value => self.current().constant(value.clone()),
        }
    }

    fn special(
        &mut self,
        name: SymbolId,
        arguments: &[Value],
        tail: bool,
    ) -> Result<bool, CompileError> {
        let Some(name_text) = self.world.state.symbols.name(name).map(str::to_owned) else {
            return Err(error(0, "unknown symbol id"));
        };
        match name_text.as_str() {
            "quote" => {
                if arguments.len() != 1 {
                    return Err(error(0, "quote expects one argument"));
                }
                self.current().constant(arguments[0].clone())?;
            }
            "if" => self.compile_if(arguments, tail)?,
            "begin" => self.sequence(arguments, tail)?,
            "lambda" => {
                let Some((parameters, body)) = arguments.split_first() else {
                    return Err(error(0, "lambda expects parameters and a body"));
                };
                let Value::List(parameters) = parameters else {
                    return Err(error(0, "lambda parameters must be a list"));
                };
                let anonymous = self.world.state.symbols.intern("<lambda>");
                let id = self.lambda(anonymous, parameters, body)?;
                self.current().emit(Op::Closure(id))?;
            }
            "define" => self.define(arguments)?,
            "set!" => self.set(arguments)?,
            "let" | "let*" | "letrec" => self.compile_let(&name_text, arguments, tail)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn compile_if(&mut self, arguments: &[Value], tail: bool) -> Result<(), CompileError> {
        if !(2..=3).contains(&arguments.len()) {
            return Err(error(0, "if expects two or three arguments"));
        }
        self.expression(&arguments[0], false)?;
        let branch = self.current().code.len();
        self.current().emit(Op::JumpFalse(0))?;
        let base = self.current().stack;
        self.expression(&arguments[1], tail)?;
        let then_stack = self.current().stack;
        let jump = self.current().code.len();
        self.current().emit(Op::Jump(0))?;
        let alternative = u32::try_from(self.current().code.len())
            .map_err(|_| error(0, "instruction stream exceeds u32"))?;
        self.current().stack = base;
        if let Some(otherwise) = arguments.get(2) {
            self.expression(otherwise, tail)?;
        } else {
            self.current().constant(Value::Nil)?;
        }
        if self.current().stack != then_stack {
            return Err(error(0, "if branches have inconsistent stack depth"));
        }
        let end = u32::try_from(self.current().code.len())
            .map_err(|_| error(0, "instruction stream exceeds u32"))?;
        self.current().code[branch] = Op::JumpFalse(alternative);
        self.current().code[jump] = Op::Jump(end);
        Ok(())
    }

    fn sequence(&mut self, forms: &[Value], tail: bool) -> Result<(), CompileError> {
        if forms.is_empty() {
            return self.current().constant(Value::Nil);
        }
        for form in &forms[..forms.len() - 1] {
            self.expression(form, false)?;
            self.current().emit(Op::Pop)?;
        }
        self.expression(&forms[forms.len() - 1], tail)
    }

    fn define(&mut self, arguments: &[Value]) -> Result<(), CompileError> {
        match arguments {
            [Value::Symbol(name), value] => {
                self.expression(value, false)?;
                self.current().emit(Op::DefGlobal(*name))
            }
            [Value::List(signature), body @ ..] if !body.is_empty() => {
                let Some((Value::Symbol(name), parameters)) = signature.split_first() else {
                    return Err(error(0, "function definition requires a name"));
                };
                let id = self.lambda(*name, parameters, body)?;
                self.current().emit(Op::Closure(id))?;
                self.current().emit(Op::DefGlobal(*name))
            }
            _ => Err(error(0, "invalid define")),
        }
    }

    fn set(&mut self, arguments: &[Value]) -> Result<(), CompileError> {
        let [Value::Symbol(name), value] = arguments else {
            return Err(error(0, "set! expects a name and value"));
        };
        self.expression(value, false)?;
        let op = match self.resolve(*name)? {
            Place::Local(index) => Op::SetLocal(index),
            Place::Capture(index) => Op::SetCapture(index),
            Place::Global(symbol) => Op::SetGlobal(symbol),
        };
        self.current().emit(op)
    }

    fn compile_let(
        &mut self,
        kind: &str,
        arguments: &[Value],
        tail: bool,
    ) -> Result<(), CompileError> {
        let Some((Value::List(bindings), body)) = arguments.split_first() else {
            return Err(error(0, format!("{kind} expects a binding list")));
        };
        let mut parsed = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let Value::List(pair) = binding else {
                return Err(error(0, "binding must be a two-form list"));
            };
            let [Value::Symbol(name), initializer] = pair.as_slice() else {
                return Err(error(0, "binding must contain a name and initializer"));
            };
            if parsed.iter().any(|(existing, _)| existing == name) && kind != "let*" {
                return Err(error(0, "duplicate binding"));
            }
            parsed.push((*name, initializer));
        }
        let previous_len = self.current().locals.len();
        self.current().scope = self
            .current()
            .scope
            .checked_add(1)
            .ok_or_else(|| error(0, "scope depth exceeds u16"))?;
        match kind {
            "let" => {
                for (_, initializer) in &parsed {
                    self.expression(initializer, false)?;
                }
                let mut slots = Vec::with_capacity(parsed.len());
                for (name, _) in &parsed {
                    slots.push(self.current().add_local(*name)?);
                }
                for slot in slots.into_iter().rev() {
                    self.current().emit(Op::SetLocal(slot))?;
                    self.current().emit(Op::Pop)?;
                }
            }
            "let*" => {
                for (name, initializer) in &parsed {
                    self.expression(initializer, false)?;
                    let slot = self.current().add_local(*name)?;
                    self.current().emit(Op::SetLocal(slot))?;
                    self.current().emit(Op::Pop)?;
                }
            }
            "letrec" => {
                let mut slots = Vec::with_capacity(parsed.len());
                for (name, _) in &parsed {
                    slots.push(self.current().add_local(*name)?);
                }
                for ((_, initializer), slot) in parsed.iter().zip(slots) {
                    self.expression(initializer, false)?;
                    self.current().emit(Op::SetLocal(slot))?;
                    self.current().emit(Op::Pop)?;
                }
            }
            _ => unreachable!(),
        }
        self.sequence(body, tail)?;
        let builder = self.current();
        builder.locals.truncate(previous_len);
        builder.scope -= 1;
        Ok(())
    }

    fn lambda(
        &mut self,
        name: SymbolId,
        parameters: &[Value],
        body: &[Value],
    ) -> Result<ChunkId, CompileError> {
        let arity = u16::try_from(parameters.len()).map_err(|_| error(0, "too many parameters"))?;
        let mut builder = Builder::new(name, self.current().source.as_str());
        builder.arity = arity;
        for parameter in parameters {
            let Value::Symbol(parameter) = parameter else {
                return Err(error(0, "parameter must be a symbol"));
            };
            if builder.locals.iter().any(|local| local.name == *parameter) {
                return Err(error(0, "duplicate parameter"));
            }
            builder.add_local(*parameter)?;
        }
        self.functions.push(builder);
        let compiled = self.sequence(body, true).and_then(|()| {
            self.functions
                .pop()
                .expect("lambda builder exists")
                .finish()
        });
        if compiled.is_err() && self.functions.len() > 1 {
            self.functions.pop();
        }
        let chunk = compiled?;
        let raw = self
            .world
            .state
            .code
            .len()
            .checked_add(self.pending.len())
            .ok_or_else(|| error(0, "chunk arena exhausted"))?;
        let id = ChunkId(u32::try_from(raw).map_err(|_| error(0, "chunk arena exceeds u32"))?);
        self.pending.push(chunk);
        Ok(id)
    }

    fn resolve(&mut self, name: SymbolId) -> Result<Place, CompileError> {
        let current = self.functions.len() - 1;
        if let Some(local) = self.functions[current]
            .locals
            .iter()
            .rev()
            .find(|local| local.name == name)
        {
            return Ok(Place::Local(local.slot));
        }
        for owner in (0..current).rev() {
            if let Some(local) = self.functions[owner]
                .locals
                .iter()
                .rposition(|local| local.name == name)
            {
                let local = self.functions[owner].locals[local].slot;
                if let Err(position) = self.functions[owner].boxed.binary_search(&local) {
                    self.functions[owner].boxed.insert(position, local);
                }
                let mut parent_capture = None;
                for child in owner + 1..=current {
                    let link = if child == owner + 1 {
                        Capture::Local(local)
                    } else {
                        Capture::Parent(parent_capture.expect("parent link exists"))
                    };
                    let index = if let Some(index) = self.functions[child]
                        .captures
                        .iter()
                        .position(|capture| *capture == link)
                    {
                        index
                    } else {
                        self.functions[child].captures.push(link);
                        self.functions[child].captures.len() - 1
                    };
                    parent_capture =
                        Some(u16::try_from(index).map_err(|_| error(0, "too many captures"))?);
                }
                return Ok(Place::Capture(parent_capture.expect("capture link exists")));
            }
        }
        Ok(Place::Global(name))
    }

    fn finish(mut self) -> Result<ChunkId, CompileError> {
        if self.control.load(Ordering::Acquire) != 0 {
            return Err(error(0, "compilation cancelled"));
        }
        let root = self
            .functions
            .pop()
            .expect("root builder exists")
            .finish()?;
        if !self.functions.is_empty() {
            return Err(error(0, "unfinished function builder"));
        }
        let root_raw = self
            .world
            .state
            .code
            .len()
            .checked_add(self.pending.len())
            .ok_or_else(|| error(0, "chunk arena exhausted"))?;
        let root_id =
            ChunkId(u32::try_from(root_raw).map_err(|_| error(0, "chunk arena exceeds u32"))?);
        for chunk in self.pending {
            self.world.insert_chunk(chunk);
        }
        let inserted = self.world.insert_chunk(root);
        if inserted != root_id {
            return Err(error(0, "chunk insertion was not stable"));
        }
        Ok(root_id)
    }
}

pub fn compile(
    world: &mut World,
    source: &str,
    control: &AtomicU8,
) -> Result<ChunkId, CompileError> {
    let original = world.state.clone();
    let result = (|| {
        let form = Reader::new(source).one(world)?;
        let name = world.state.symbols.intern("<toplevel>");
        let root = Builder::new(name, source);
        let mut compiler = Compiler {
            world,
            control,
            functions: vec![root],
            pending: Vec::new(),
        };
        compiler.expression(&form, true)?;
        compiler.finish()
    })();
    if result.is_err() {
        world.state = original;
    }
    result
}
