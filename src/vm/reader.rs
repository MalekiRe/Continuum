use crate::vm::value::Value;

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("unexpected EOF: {0}")]
    UnexpectedEof(String),
    #[error("invalid syntax: {0}")]
    InvalidSyntax(String),
    #[error("unmatched '('")]
    UnmatchedParen,
    #[error("unmatched '['")]
    UnmatchedBracket,
    #[error("unmatched '{{'")]
    UnmatchedBrace,
    #[error("unknown dispatch: {0}")]
    UnknownDispatch(String),
}

enum ParserState {
    List(Vec<Value>),
    Vector(Vec<Value>),
    Map {
        pairs: Vec<(Value, Value)>,
        pending_key: Option<Value>,
    },
    Prefix(&'static str),
}

pub fn read_one(input: &str) -> Result<(Value, &str), ReadError> {
    let input = skip_whitespace_and_comments(input);
    if input.is_empty() {
        return Err(ReadError::UnexpectedEof("expected a value".into()));
    }

    let mut stack = Vec::new();
    let mut rest = input;
    loop {
        rest = skip_whitespace_and_comments(rest);
        if rest.is_empty() {
            return match stack.last() {
                Some(ParserState::List(_)) => Err(ReadError::UnmatchedParen),
                Some(ParserState::Vector(_)) => Err(ReadError::UnmatchedBracket),
                Some(ParserState::Map { .. }) => Err(ReadError::UnmatchedBrace),
                Some(ParserState::Prefix(_)) | None => {
                    Err(ReadError::UnexpectedEof("expected a value".into()))
                }
            };
        }

        let ch = rest.chars().next().unwrap();
        match ch {
            '(' => {
                stack.push(ParserState::List(Vec::new()));
                rest = &rest[1..];
                continue;
            }
            '[' => {
                stack.push(ParserState::Vector(Vec::new()));
                rest = &rest[1..];
                continue;
            }
            '{' => {
                stack.push(ParserState::Map {
                    pairs: Vec::new(),
                    pending_key: None,
                });
                rest = &rest[1..];
                continue;
            }
            ')' => match stack.pop() {
                Some(ParserState::List(items)) => {
                    rest = &rest[1..];
                    if let Some(value) = complete_value(&mut stack, Value::List(items)) {
                        return Ok((value, rest));
                    }
                    continue;
                }
                _ => return Err(ReadError::InvalidSyntax("unexpected ')'".into())),
            },
            ']' => match stack.pop() {
                Some(ParserState::Vector(items)) => {
                    rest = &rest[1..];
                    if let Some(value) = complete_value(&mut stack, Value::Vector(items)) {
                        return Ok((value, rest));
                    }
                    continue;
                }
                _ => return Err(ReadError::InvalidSyntax("unexpected ']'".into())),
            },
            '}' => match stack.pop() {
                Some(ParserState::Map {
                    pairs,
                    pending_key: None,
                }) => {
                    let value = Value::Map(pairs.into_iter().collect());
                    rest = &rest[1..];
                    if let Some(value) = complete_value(&mut stack, value) {
                        return Ok((value, rest));
                    }
                    continue;
                }
                Some(ParserState::Map { .. }) => {
                    return Err(ReadError::InvalidSyntax(
                        "map literal requires an even number of forms".into(),
                    ));
                }
                _ => return Err(ReadError::InvalidSyntax("unexpected '}'".into())),
            },
            '\'' => {
                stack.push(ParserState::Prefix("quote"));
                rest = &rest[1..];
                continue;
            }
            '`' => {
                stack.push(ParserState::Prefix("quasiquote"));
                rest = &rest[1..];
                continue;
            }
            ',' => {
                let splicing = rest.as_bytes().get(1) == Some(&b'@');
                stack.push(ParserState::Prefix(if splicing {
                    "unquote-splicing"
                } else {
                    "unquote"
                }));
                rest = &rest[if splicing { 2 } else { 1 }..];
                continue;
            }
            _ => {}
        }

        let (value, new_rest) = match ch {
            '#' => read_dispatch(&rest[1..])?,
            ':' => {
                let (name, rest) = read_raw_symbol(&rest[1..])?;
                (Value::Keyword(name), rest)
            }
            '"' => read_string(&rest[1..])?,
            _ => read_atom(rest)?,
        };
        rest = new_rest;
        if let Some(value) = complete_value(&mut stack, value) {
            return Ok((value, rest));
        }
    }
}

fn complete_value(stack: &mut Vec<ParserState>, mut value: Value) -> Option<Value> {
    loop {
        match stack.last_mut() {
            Some(ParserState::List(items)) => {
                items.push(value);
                return None;
            }
            Some(ParserState::Vector(items)) => {
                items.push(value);
                return None;
            }
            Some(ParserState::Map { pairs, pending_key }) => {
                if let Some(key) = pending_key.take() {
                    pairs.push((key, value));
                } else {
                    *pending_key = Some(value);
                }
                return None;
            }
            Some(ParserState::Prefix(_)) => {
                let Some(ParserState::Prefix(prefix)) = stack.pop() else {
                    unreachable!()
                };
                value = Value::list(vec![Value::symbol(prefix), value]);
            }
            None => return Some(value),
        }
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

fn skip_whitespace_and_comments(mut input: &str) -> &str {
    loop {
        input = input.trim_start_matches(char::is_whitespace);
        if !input.starts_with(';') {
            return input;
        }
        input = input.find('\n').map_or("", |newline| &input[newline + 1..]);
    }
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
        'x' => read_radix(&input[1..], 16, "hex", "#x"),
        'o' => read_radix(&input[1..], 8, "octal", "#o"),
        'b' => read_radix(&input[1..], 2, "binary", "#b"),
        _ => Err(ReadError::UnknownDispatch(format!("#{}", ch))),
    }
}

fn read_radix<'a>(
    input: &'a str,
    radix: u32,
    name: &str,
    prefix: &str,
) -> Result<(Value, &'a str), ReadError> {
    let (raw, rest) = read_raw_symbol(input)?;
    i64::from_str_radix(&raw, radix)
        .map(|value| (Value::Int(value), rest))
        .map_err(|_| ReadError::InvalidSyntax(format!("invalid {name}: {prefix}{raw}")))
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
        && value.is_finite()
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
        if ch.is_whitespace() || "()[]{}'\";,`#".contains(ch) {
            if s.is_empty() {
                return Err(ReadError::InvalidSyntax(
                    "expected a symbol or number".into(),
                ));
            }
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
