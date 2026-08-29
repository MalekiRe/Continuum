use crate::vm::value::Value;
use std::collections::HashMap;

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

pub fn read_one(input: &str) -> Result<(Value, &str), ReadError> {
    let input = skip_whitespace_and_comments(input);
    if input.is_empty() {
        return Err(ReadError::UnexpectedEof("expected a value".into()));
    }

    let ch = input.chars().next().unwrap();
    if ch == '(' {
        read_list(&input[1..])
    } else if ch == ')' {
        Err(ReadError::InvalidSyntax("unexpected ')'".into()))
    } else if ch == '\'' {
        let (val, rest) = read_one(&input[1..])?;
        Ok((Value::list(vec![Value::symbol("quote"), val]), rest))
    } else if ch == '`' {
        let (val, rest) = read_one(&input[1..])?;
        Ok((Value::list(vec![Value::symbol("quasiquote"), val]), rest))
    } else if ch == ',' {
        if input.len() > 1 && input.as_bytes()[1] == b'@' {
            let (val, rest) = read_one(&input[2..])?;
            Ok((Value::list(vec![Value::symbol("unquote-splicing"), val]), rest))
        } else {
            let (val, rest) = read_one(&input[1..])?;
            Ok((Value::list(vec![Value::symbol("unquote"), val]), rest))
        }
    } else if ch == '#' {
        read_dispatch(&input[1..])
    } else if ch == ':' {
        let (name, rest) = read_raw_symbol(&input[1..])?;
        Ok((Value::Keyword(name), rest))
    } else if ch == '"' {
        read_string(&input[1..])
    } else if ch == '{' {
        read_map(&input[1..])
    } else if ch == '[' {
        read_vector(&input[1..])
    } else {
        read_atom(input)
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

fn skip_whitespace_and_comments<'a>(input: &'a str) -> &'a str {
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

fn read_list(input: &str) -> Result<(Value, &str), ReadError> {
    let mut items = Vec::new();
    let mut rest = input;
    loop {
        rest = skip_whitespace_and_comments(rest);
        if rest.is_empty() {
            return Err(ReadError::UnmatchedParen);
        }
        if rest.starts_with(')') {
            return Ok((Value::List(items), &rest[1..]));
        }
        if rest.starts_with('.') && rest.len() > 1 && rest.as_bytes()[1].is_ascii_whitespace() {
            return Err(ReadError::InvalidSyntax(
                "dotted pairs not supported; use proper list syntax".into()));
        }
        let (val, new_rest) = read_one(rest)?;
        items.push(val);
        rest = new_rest;
    }
}

fn read_vector(input: &str) -> Result<(Value, &str), ReadError> {
    let mut items = Vec::new();
    let mut rest = input;
    loop {
        rest = skip_whitespace_and_comments(rest);
        if rest.is_empty() {
            return Err(ReadError::UnmatchedBracket);
        }
        if rest.starts_with(']') {
            return Ok((Value::Vector(items), &rest[1..]));
        }
        let (val, new_rest) = read_one(rest)?;
        items.push(val);
        rest = new_rest;
    }
}

fn read_map(input: &str) -> Result<(Value, &str), ReadError> {
    let mut map = HashMap::new();
    let mut rest = input;
    let mut expect_key = true;
    let mut key = None;
    loop {
        rest = skip_whitespace_and_comments(rest);
        if rest.is_empty() {
            return Err(ReadError::UnmatchedBrace);
        }
        if rest.starts_with('}') {
            return Ok((Value::Map(map), &rest[1..]));
        }
        let (val, new_rest) = read_one(rest)?;
        if expect_key {
            key = Some(val);
        } else {
            map.insert(key.take().unwrap(), val);
        }
        expect_key = !expect_key;
        rest = new_rest;
    }
}

fn read_dispatch(input: &str) -> Result<(Value, &str), ReadError> {
    if input.is_empty() {
        return Err(ReadError::UnexpectedEof("expected dispatch character".into()));
    }
    let ch = input.chars().next().unwrap();
    match ch {
        't' => Ok((Value::Bool(true), &input[1..])),
        'f' => Ok((Value::Bool(false), &input[1..])),
        '\\' => {
            if input.len() >= 2 {
                let char_lit = input.as_bytes()[1] as char;
                Ok((Value::string(&char_lit.to_string()), &input[2..]))
            } else {
                Err(ReadError::UnexpectedEof("expected character after #\\".into()))
            }
        }
        'x' => {
            let (raw, rest) = read_raw_symbol(&input[1..])?;
            i64::from_str_radix(&raw, 16).map(|n| (Value::Int(n), rest))
                .map_err(|_| ReadError::InvalidSyntax(format!("invalid hex number: #x{}", raw)))
        }
        'o' => {
            let (raw, rest) = read_raw_symbol(&input[1..])?;
            i64::from_str_radix(&raw, 8).map(|n| (Value::Int(n), rest))
                .map_err(|_| ReadError::InvalidSyntax(format!("invalid octal number: #o{}", raw)))
        }
        'b' => {
            let (raw, rest) = read_raw_symbol(&input[1..])?;
            i64::from_str_radix(&raw, 2).map(|n| (Value::Int(n), rest))
                .map_err(|_| ReadError::InvalidSyntax(format!("invalid binary number: #b{}", raw)))
        }
        _ => Err(ReadError::UnknownDispatch(format!("#{}", ch))),
    }
}

fn read_string(input: &str) -> Result<(Value, &str), ReadError> {
    let mut s = String::new();
    let mut chars = input.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '"' {
            return Ok((Value::String(s), &input[i+1..]));
        }
        if ch == '\\' {
            match chars.next() {
                None => return Err(ReadError::UnexpectedEof("unterminated string escape".into())),
                Some((_, 'n')) => s.push('\n'),
                Some((_, 't')) => s.push('\t'),
                Some((_, 'r')) => s.push('\r'),
                Some((_, '"')) => s.push('"'),
                Some((_, '\\')) => s.push('\\'),
                Some((_, c)) => { s.push('\\'); s.push(c); }
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

    if raw.contains('.') || raw.contains('e') || raw.contains('E') {
        if let Ok(f) = raw.parse::<f64>() {
            return Ok((Value::Float(f), rest));
        }
    }

    match raw.as_str() {
        "nil" => return Ok((Value::Nil, rest)),
        "#t" | "true" => return Ok((Value::Bool(true), rest)),
        "#f" | "false" => return Ok((Value::Bool(false), rest)),
        _ => {}
    }

    Ok((Value::Symbol(raw), rest))
}

fn read_raw_symbol<'a>(input: &'a str) -> Result<(String, &'a str), ReadError> {
    let mut s = String::new();
    for (i, ch) in input.char_indices() {
        if ch.is_ascii_whitespace() || "()[]{}'\";,`#".contains(ch) {
            return Ok((s, &input[i..]));
        }
        s.push(ch);
    }
    if s.is_empty() {
        return Err(ReadError::UnexpectedEof("expected a symbol or number".into()));
    }
    Ok((s, ""))
}
