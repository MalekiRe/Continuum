use crate::vm::value::Value;
use indexmap::IndexMap;

#[derive(Debug)]
pub enum ReadError {
    UnexpectedEof(String),
    InvalidSyntax(String),
    UnmatchedParen,
    UnmatchedBracket,
    UnmatchedBrace,
    UnknownDispatch(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::UnexpectedEof(s) => write!(f, "unexpected EOF: {}", s),
            ReadError::InvalidSyntax(s) => write!(f, "invalid syntax: {}", s),
            ReadError::UnmatchedParen => write!(f, "unmatched '('"),
            ReadError::UnmatchedBracket => write!(f, "unmatched '['"),
            ReadError::UnmatchedBrace => write!(f, "unmatched '{{'"),
            ReadError::UnknownDispatch(s) => write!(f, "unknown dispatch: {}", s),
        }
    }
}

impl std::error::Error for ReadError {}

enum ParserState {
    List(Vec<Value>),
    Vector(Vec<Value>),
    Map {
        pairs: Vec<(Value, Value)>,
        expect_key: bool,
        current_key: Option<Value>,
    },
}

pub fn read_one(input: &str) -> Result<(Value, &str), ReadError> {
    let input = skip_whitespace_and_comments(input);
    if input.is_empty() {
        return Err(ReadError::UnexpectedEof("expected a value".into()));
    }

    let mut stack: Vec<ParserState> = Vec::new();
    let mut rest = input;

    loop {
        rest = skip_whitespace_and_comments(rest);
        if rest.is_empty() {
            return match stack.last() {
                Some(ParserState::List(_)) => Err(ReadError::UnmatchedParen),
                Some(ParserState::Vector(_)) => Err(ReadError::UnmatchedBracket),
                Some(ParserState::Map { .. }) => Err(ReadError::UnmatchedBrace),
                None => Err(ReadError::UnexpectedEof("expected a value".into())),
            };
        }

        let ch = rest.chars().next().unwrap();

        if ch == '(' {
            stack.push(ParserState::List(Vec::new()));
            rest = &rest[1..];
            continue;
        }

        if ch == '[' {
            stack.push(ParserState::Vector(Vec::new()));
            rest = &rest[1..];
            continue;
        }

        if ch == '{' {
            stack.push(ParserState::Map {
                pairs: Vec::new(),
                expect_key: true,
                current_key: None,
            });
            rest = &rest[1..];
            continue;
        }

        if ch == ')' {
            match stack.pop() {
                Some(ParserState::List(items)) => {
                    let val = Value::List(items);
                    rest = &rest[1..];
                    if stack.is_empty() {
                        return Ok((val, rest));
                    }
                    append_to_parent(&mut stack, val)?;
                    continue;
                }
                _ => return Err(ReadError::InvalidSyntax("unexpected ')'".into())),
            }
        }

        if ch == ']' {
            match stack.pop() {
                Some(ParserState::Vector(items)) => {
                    let val = Value::Vector(items);
                    rest = &rest[1..];
                    if stack.is_empty() {
                        return Ok((val, rest));
                    }
                    append_to_parent(&mut stack, val)?;
                    continue;
                }
                _ => return Err(ReadError::InvalidSyntax("unexpected ']'".into())),
            }
        }

        if ch == '}' {
            match stack.pop() {
                Some(ParserState::Map { pairs, .. }) => {
                    let mut map = IndexMap::new();
                    for (k, v) in pairs {
                        map.insert(k, v);
                    }
                    let val = Value::Map(map);
                    rest = &rest[1..];
                    if stack.is_empty() {
                        return Ok((val, rest));
                    }
                    append_to_parent(&mut stack, val)?;
                    continue;
                }
                _ => return Err(ReadError::InvalidSyntax("unexpected '}'".into())),
            }
        }

        if ch == '\'' {
            let (val, new_rest) = read_one(&rest[1..])?;
            let quoted = Value::list(vec![Value::symbol("quote"), val]);
            rest = new_rest;
            if stack.is_empty() {
                return Ok((quoted, rest));
            }
            append_to_parent(&mut stack, quoted)?;
            continue;
        }

        if ch == '`' {
            let (val, new_rest) = read_one(&rest[1..])?;
            let qq = Value::list(vec![Value::symbol("quasiquote"), val]);
            rest = new_rest;
            if stack.is_empty() {
                return Ok((qq, rest));
            }
            append_to_parent(&mut stack, qq)?;
            continue;
        }

        if ch == ',' {
            if rest.len() > 1 && rest.as_bytes()[1] == b'@' {
                let (val, new_rest) = read_one(&rest[2..])?;
                let unq = Value::list(vec![Value::symbol("unquote-splicing"), val]);
                rest = new_rest;
                if stack.is_empty() {
                    return Ok((unq, rest));
                }
                append_to_parent(&mut stack, unq)?;
            } else {
                let (val, new_rest) = read_one(&rest[1..])?;
                let unq = Value::list(vec![Value::symbol("unquote"), val]);
                rest = new_rest;
                if stack.is_empty() {
                    return Ok((unq, rest));
                }
                append_to_parent(&mut stack, unq)?;
            }
            continue;
        }

        if ch == '#' {
            let (val, new_rest) = read_dispatch(&rest[1..])?;
            rest = new_rest;
            if stack.is_empty() {
                return Ok((val, rest));
            }
            append_to_parent(&mut stack, val)?;
            continue;
        }

        if ch == ':' {
            let (name, new_rest) = read_raw_symbol(&rest[1..])?;
            let val = Value::Keyword(name);
            rest = new_rest;
            if stack.is_empty() {
                return Ok((val, rest));
            }
            append_to_parent(&mut stack, val)?;
            continue;
        }

        if ch == '"' {
            let (val, new_rest) = read_string(&rest[1..])?;
            rest = new_rest;
            if stack.is_empty() {
                return Ok((val, rest));
            }
            append_to_parent(&mut stack, val)?;
            continue;
        }

        let (val, new_rest) = read_atom(rest)?;
        rest = new_rest;
        if stack.is_empty() {
            return Ok((val, rest));
        }
        append_to_parent(&mut stack, val)?;
    }
}

fn append_to_parent(stack: &mut [ParserState], val: Value) -> Result<(), ReadError> {
    match stack.last_mut() {
        Some(ParserState::List(items)) => {
            items.push(val);
            Ok(())
        }
        Some(ParserState::Vector(items)) => {
            items.push(val);
            Ok(())
        }
        Some(ParserState::Map {
            pairs,
            expect_key,
            current_key,
        }) => {
            if *expect_key {
                *current_key = Some(val);
                *expect_key = false;
            } else {
                if let Some(key) = current_key.take() {
                    pairs.push((key, val));
                }
                *expect_key = true;
            }
            Ok(())
        }
        None => Ok(()),
    }
}

pub fn read_all(input: &str) -> Result<Vec<Value>, ReadError> {
    let mut results = Vec::new();
    let mut rest = input;
    loop {
        rest = skip_whitespace_and_comments(rest);
        if rest.is_empty() {
            break;
        }
        let (val, new_rest) = read_one(rest)?;
        results.push(val);
        rest = new_rest;
    }
    Ok(results)
}

fn skip_whitespace_and_comments(input: &str) -> &str {
    let mut i = 0;
    let bytes = input.as_bytes();
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if bytes[i] == b';' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            break;
        }
    }
    &input[i..]
}

fn read_dispatch(input: &str) -> Result<(Value, &str), ReadError> {
    if input.is_empty() {
        return Err(ReadError::UnexpectedEof(
            "expected dispatch character".into(),
        ));
    }
    let ch = input.chars().next().unwrap();
    match ch {
        't' => Ok((Value::Bool(true), &input[1..])),
        'f' => Ok((Value::Bool(false), &input[1..])),
        '\\' => {
            let chars = input[1..]
                .chars()
                .next()
                .ok_or_else(|| ReadError::UnexpectedEof("expected character after #\\".into()))?;
            let end = 1 + chars.len_utf8();
            Ok((Value::string(&chars.to_string()), &input[end..]))
        }
        'x' => {
            let (raw, rest) = read_raw_symbol(&input[1..])?;
            i64::from_str_radix(&raw, 16)
                .map(|n| (Value::Int(n), rest))
                .map_err(|_| ReadError::InvalidSyntax(format!("invalid hex: #x{}", raw)))
        }
        'o' => {
            let (raw, rest) = read_raw_symbol(&input[1..])?;
            i64::from_str_radix(&raw, 8)
                .map(|n| (Value::Int(n), rest))
                .map_err(|_| ReadError::InvalidSyntax(format!("invalid octal: #o{}", raw)))
        }
        'b' => {
            let (raw, rest) = read_raw_symbol(&input[1..])?;
            i64::from_str_radix(&raw, 2)
                .map(|n| (Value::Int(n), rest))
                .map_err(|_| ReadError::InvalidSyntax(format!("invalid binary: #b{}", raw)))
        }
        _ => Err(ReadError::UnknownDispatch(format!("#{}", ch))),
    }
}

fn read_string(input: &str) -> Result<(Value, &str), ReadError> {
    let mut s = String::new();
    let mut chars = input.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '"' {
            return Ok((Value::String(s), &input[i + 1..]));
        }
        if ch == '\\' {
            match chars.next() {
                None => {
                    return Err(ReadError::UnexpectedEof(
                        "unterminated string escape".into(),
                    ));
                }
                Some((_, 'n')) => s.push('\n'),
                Some((_, 't')) => s.push('\t'),
                Some((_, 'r')) => s.push('\r'),
                Some((_, '"')) => s.push('"'),
                Some((_, '\\')) => s.push('\\'),
                Some((_, c)) => {
                    s.push('\\');
                    s.push(c);
                }
            }
        } else {
            s.push(ch);
        }
    }
    Err(ReadError::UnexpectedEof("unterminated string".into()))
}

fn read_atom(input: &str) -> Result<(Value, &str), ReadError> {
    let (raw, rest) = read_raw_symbol(input)?;
    if let Ok(n) = raw.parse::<i64>() {
        return Ok((Value::Int(n), rest));
    }
    if (raw.contains('.') || raw.contains('e') || raw.contains('E'))
        && let Ok(value) = raw.parse::<f64>()
    {
        return Ok((Value::Float(value), rest));
    }
    match raw.as_str() {
        "nil" => return Ok((Value::Nil, rest)),
        "#t" | "true" => return Ok((Value::Bool(true), rest)),
        "#f" | "false" => return Ok((Value::Bool(false), rest)),
        _ => {}
    }
    Ok((Value::Symbol(raw), rest))
}

fn read_raw_symbol(input: &str) -> Result<(String, &str), ReadError> {
    let mut s = String::new();
    for (i, ch) in input.char_indices() {
        if ch.is_ascii_whitespace() || "()[]{}'\";,`#".contains(ch) {
            return Ok((s, &input[i..]));
        }
        s.push(ch);
    }
    if s.is_empty() {
        return Err(ReadError::UnexpectedEof(
            "expected a symbol or number".into(),
        ));
    }
    Ok((s, ""))
}
