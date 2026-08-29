use crate::vm::value::*;
use crate::vm::env::EnvRef;
use crate::vm::reader;
use std::collections::HashMap;

#[derive(Debug)]
pub enum EvalError {
    UndefinedSymbol(String),
    InvalidForm(String),
    NotAFunction(Value),
    ArityMismatch { name: String, expected: u32, got: usize },
    SyntaxError(String),
    UserError(String),
    KernelError(String),
    NotADataFamily(String),
    InvalidPattern(String),
    AuthorityDenied(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UndefinedSymbol(s) => write!(f, "undefined symbol: {}", s),
            EvalError::InvalidForm(s) => write!(f, "invalid form: {}", s),
            EvalError::NotAFunction(v) => write!(f, "not a function: {}", v),
            EvalError::ArityMismatch { name, expected, got } => write!(f, "arity mismatch: {} expects {} arguments, got {}", name, expected, got),
            EvalError::SyntaxError(s) => write!(f, "syntax error: {}", s),
            EvalError::UserError(s) => write!(f, "error: {}", s),
            EvalError::KernelError(s) => write!(f, "kernel error: {}", s),
            EvalError::NotADataFamily(s) => write!(f, "not a data family: {}", s),
            EvalError::InvalidPattern(s) => write!(f, "invalid pattern: {}", s),
            EvalError::AuthorityDenied(s) => write!(f, "authority denied: {}", s),
        }
    }
}

impl std::error::Error for EvalError {}

pub fn eval(input: &str, env: &mut EnvRef) -> Result<Value, EvalError> {
    let exprs = reader::read_all(input).map_err(|e| EvalError::SyntaxError(e.to_string()))?;
    let mut result = Value::Nil;
    for expr in exprs {
        result = eval_value(expr, env)?;
    }
    Ok(result)
}

pub fn eval_value(val: Value, env: &mut EnvRef) -> Result<Value, EvalError> {
    match val {
        Value::Symbol(ref name) => {
            env.lookup(name).cloned().ok_or_else(|| EvalError::UndefinedSymbol(name.clone()))
        }
        Value::List(ref items) if items.is_empty() => {
            Ok(Value::Nil)
        }
        Value::List(ref items) => {
            let head = &items[0];
            let args = &items[1..];
            match head {
                Value::Symbol(s) => {
                    match s.as_str() {
                        "define"  => eval_define(args, env),
                        "undefine" => eval_undefine(args, env),
                        "lambda"  => eval_lambda(args, env),
                        "if"      => eval_if(args, env),
                        "begin"   => eval_begin(args, env),
                        "let"     => eval_let(args, env),
                        "let*"    => eval_let_star(args, env),
                        "letrec"  => eval_letrec(args, env),
                        "set!"    => eval_set(args, env),
                        "quote"   => eval_quote(args),
                        "quasiquote" => eval_quasiquote(args, env),
                        "define-syntax" => eval_define_syntax(args, env),
                        "define-data" => eval_define_data(args, env),
                        "match"   => eval_match(args, env),
                        _ => {
                            // Check if this symbol is a macro
                            let expanded = try_expand_macro(s, args, env)?;
                            if let Some(expanded) = expanded {
                                eval_value(expanded, env)
                            } else {
                                let fun = eval_value(Value::Symbol(s.clone()), env)?;
                                apply(fun, eval_args(args, env)?, env)
                            }
                        }
                    }
                }
                _ => {
                    let fun = eval_value(head.clone(), env)?;
                    apply(fun, eval_args(args, env)?, env)
                }
            }
        }
        Value::Vector(items) => {
            let mut evaled = Vec::with_capacity(items.len());
            for item in items {
                evaled.push(eval_value(item, env)?);
            }
            Ok(Value::Vector(evaled))
        }
        Value::Map(map) => {
            let mut evaled = HashMap::new();
            for (k, v) in map {
                evaled.insert(eval_value(k, env)?, eval_value(v, env)?);
            }
            Ok(Value::Map(evaled))
        }
        other => Ok(other),
    }
}

// ---- Special forms ----

fn eval_define(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("define requires arguments".into()));
    }

    match &args[0] {
        Value::Symbol(name) => {
            if args.len() != 2 {
                return Err(EvalError::InvalidForm(format!("define: expected (define name value), got {} args", args.len())));
            }
            let val = eval_value(args[1].clone(), env)?;
            if !name.contains('/') {
                env.define(&format!("user/{}", name), val.clone()).map_err(|e| EvalError::SyntaxError(e))?;
            } else {
                env.define(name, val.clone()).map_err(|e| EvalError::SyntaxError(e))?;
            }
            Ok(Value::Symbol(name.clone()))
        }
        Value::List(params) => {
            if params.is_empty() {
                return Err(EvalError::InvalidForm("define: function definition needs a name".into()));
            }
            let name = match &params[0] {
                Value::Symbol(n) => n.clone(),
                other => return Err(EvalError::InvalidForm(format!("define: expected symbol for function name, got {}", other))),
            };
            let param_names: Vec<String> = params[1..].iter().map(|p| {
                match p {
                    Value::Symbol(s) => s.clone(),
                    other => other.to_string(),
                }
            }).collect();
            let body: Vec<Value> = args[1..].to_vec();
            let lambda = eval_lambda_simple(param_names, body, env)?;
            if !name.contains('/') {
                env.define(&format!("user/{}", name), lambda).map_err(|e| EvalError::SyntaxError(e))?;
            } else {
                env.define(&name, lambda).map_err(|e| EvalError::SyntaxError(e))?;
            }
            Ok(Value::Symbol(name))
        }
        other => Err(EvalError::InvalidForm(format!("define: expected symbol or list, got {}", other))),
    }
}

fn eval_undefine(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::InvalidForm("undefine requires exactly one symbol argument".into()));
    }
    let name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        other => return Err(EvalError::InvalidForm(format!("undefine: expected symbol, got {}", other))),
    };
    let qualified = if name.contains('/') { name } else { format!("user/{}", name) };
    env.undefine(&qualified).map_err(|e| EvalError::SyntaxError(e))?;
    Ok(Value::Nil)
}

fn eval_lambda(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("lambda requires parameters and body".into()));
    }
    let param_list = &args[0];
    let body = args[1..].to_vec();

    let param_names = match param_list {
        Value::List(items) => {
            let mut names = Vec::new();
            for p in items {
                match p {
                    Value::Symbol(s) => names.push(s.clone()),
                    other => return Err(EvalError::InvalidForm(format!("lambda: expected symbol parameter, got {}", other))),
                }
            }
            names
        }
        Value::Symbol(s) if s == "args" => vec!["args".into()],
        _ => return Err(EvalError::InvalidForm(format!("lambda: expected parameter list, got {}", param_list))),
    };

    env.serialize_env_for_closure();
    let serialized = env.serialized.clone();
    Ok(Value::Function(Function::Interpreted {
        params: param_names,
        body,
        env_serialized: serialized,
    }))
}

fn eval_lambda_simple(params: Vec<String>, body: Vec<Value>, env: &EnvRef) -> Result<Value, EvalError> {
    // We need serialized env. Clone env and serialize.
    let serialized = serde_json::to_string(env).unwrap_or_default();
    Ok(Value::Function(Function::Interpreted {
        params,
        body,
        env_serialized: serialized,
    }))
}

fn eval_if(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::InvalidForm("if expects 2 or 3 arguments".into()));
    }
    let cond = eval_value(args[0].clone(), env)?;
    if cond.is_truthy() {
        eval_value(args[1].clone(), env)
    } else if args.len() == 3 {
        eval_value(args[2].clone(), env)
    } else {
        Ok(Value::Nil)
    }
}

fn eval_begin(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    let mut result = Value::Nil;
    for arg in args {
        result = eval_value(arg.clone(), env)?;
    }
    Ok(result)
}

fn eval_let(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("let requires bindings and body".into()));
    }

    let bindings = match &args[0] {
        Value::List(items) => items,
        _ => return Err(EvalError::InvalidForm("let: expected binding list".into())),
    };

    env.push_frame();
    for binding in bindings {
        match binding {
            Value::List(items) if items.len() == 2 => {
                let name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => return Err(EvalError::InvalidForm(format!("let: expected symbol, got {}", other))),
                };
                let val = eval_value(items[1].clone(), env)?;
                env.set_lexical(&name, val);
            }
            other => return Err(EvalError::InvalidForm(format!("let: expected (name value) pair, got {}", other))),
        }
    }

    let body: Vec<Value> = args[1..].to_vec();
    let result = eval_begin(&body, env);
    env.pop_frame();
    result
}

fn eval_let_star(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("let* requires bindings and body".into()));
    }

    let bindings = match &args[0] {
        Value::List(items) => items,
        _ => return Err(EvalError::InvalidForm("let*: expected binding list".into())),
    };

    env.push_frame();
    for binding in bindings {
        match binding {
            Value::List(items) if items.len() == 2 => {
                let name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => return Err(EvalError::InvalidForm(format!("let*: expected symbol, got {}", other))),
                };
                let val = eval_value(items[1].clone(), env)?;
                env.set_lexical(&name, val);
            }
            other => return Err(EvalError::InvalidForm(format!("let*: expected (name value) pair, got {}", other))),
        }
    }

    let body: Vec<Value> = args[1..].to_vec();
    let result = eval_begin(&body, env);
    env.pop_frame();
    result
}

fn eval_letrec(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("letrec requires bindings and body".into()));
    }

    let bindings = match &args[0] {
        Value::List(items) => items,
        _ => return Err(EvalError::InvalidForm("letrec: expected binding list".into())),
    };

    env.push_frame();
    for binding in bindings {
        match binding {
            Value::List(items) if items.len() == 2 => {
                let name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => return Err(EvalError::InvalidForm(format!("letrec: expected symbol, got {}", other))),
                };
                env.set_lexical(&name, Value::Nil);
            }
            other => return Err(EvalError::InvalidForm(format!("letrec: expected (name value) pair, got {}", other))),
        }
    }
    for binding in bindings {
        match binding {
            Value::List(items) if items.len() == 2 => {
                let name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => return Err(EvalError::InvalidForm(format!("letrec: expected symbol, got {}", other))),
                };
                let val = eval_value(items[1].clone(), env)?;
                env.set_lexical(&name, val);
            }
            _ => unreachable!(),
        }
    }

    let body: Vec<Value> = args[1..].to_vec();
    let result = eval_begin(&body, env);
    env.pop_frame();
    result
}

fn eval_set(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::InvalidForm("set! expects 2 arguments".into()));
    }
    let name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        other => return Err(EvalError::InvalidForm(format!("set!: expected symbol, got {}", other))),
    };
    let val = eval_value(args[1].clone(), env)?;

    // Check lexical frames first
    for frame in env.frames.iter_mut().rev() {
        if frame.contains_key(&name) {
            frame.insert(name.clone(), val);
            return Ok(Value::Nil);
        }
    }

    // Check namespaces
    if name.contains('/') {
        if env.lookup(&name).is_some() {
            env.define(&name, val).map_err(|e| EvalError::SyntaxError(e))?;
            return Ok(Value::Nil);
        }
    } else {
        let qualified = format!("user/{}", name);
        if env.lookup(&qualified).is_some() {
            env.define(&qualified, val).map_err(|e| EvalError::SyntaxError(e))?;
            return Ok(Value::Nil);
        }
    }

    Err(EvalError::UndefinedSymbol(name))
}

fn eval_quote(args: &[Value]) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("quote requires an argument".into()));
    }
    Ok(args[0].clone())
}

fn eval_quasiquote(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("quasiquote requires an argument".into()));
    }
    expand_quasiquote(&args[0], env)
}

fn expand_quasiquote(val: &Value, env: &mut EnvRef) -> Result<Value, EvalError> {
    match val {
        Value::List(items) if !items.is_empty() => {
            let head = &items[0];

            if let Value::Symbol(s) = head {
                if s == "unquote" {
                    if items.len() != 2 {
                        return Err(EvalError::InvalidForm("unquote expects 1 argument".into()));
                    }
                    return eval_value(items[1].clone(), env);
                }
            }

            let mut result = Vec::new();
            for item in items {
                match item {
                    Value::List(sub) if !sub.is_empty() => {
                        if let Value::Symbol(s) = &sub[0] {
                            if s == "unquote-splicing" {
                                if sub.len() != 2 {
                                    return Err(EvalError::InvalidForm("unquote-splicing expects 1 argument".into()));
                                }
                                let spliced = eval_value(sub[1].clone(), env)?;
                                match spliced {
                                    Value::List(v) => result.extend(v),
                                    Value::Vector(v) => result.extend(v),
                                    _ => result.push(spliced),
                                }
                                continue;
                            }
                        }
                        result.push(expand_quasiquote(item, env)?);
                    }
                    other => {
                        result.push(expand_quasiquote(other, env)?);
                    }
                }
            }
            Ok(Value::List(result))
        }
        Value::Vector(items) => {
            let mut result = Vec::new();
            for item in items {
                match item {
                    Value::List(sub) if !sub.is_empty() => {
                        if let Value::Symbol(s) = &sub[0] {
                            if s == "unquote-splicing" {
                                if sub.len() != 2 {
                                    return Err(EvalError::InvalidForm("unquote-splicing expects 1 argument".into()));
                                }
                                let spliced = eval_value(sub[1].clone(), env)?;
                                match spliced {
                                    Value::List(v) => result.extend(v),
                                    Value::Vector(v) => result.extend(v),
                                    _ => result.push(spliced),
                                }
                                continue;
                            }
                        }
                        result.push(expand_quasiquote(item, env)?);
                    }
                    other => {
                        result.push(expand_quasiquote(other, env)?);
                    }
                }
            }
            Ok(Value::Vector(result))
        }
        _ => Ok(val.clone()),
    }
}

// ---- define-syntax (simple) ----

fn eval_define_syntax(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::InvalidForm("define-syntax requires name and transformer".into()));
    }
    let name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        other => return Err(EvalError::InvalidForm(format!("define-syntax: expected symbol, got {}", other))),
    };

    let transformer = &args[1];

    match transformer {
        Value::List(items) if items.len() >= 2 && matches!(&items[0], Value::Symbol(s) if s == "syntax-rules") => {
            let literals = match &items[1] {
                Value::List(lits) => lits.iter().map(|v| match v { Value::Symbol(s) => s.clone(), _ => "".into() }).collect(),
                _ => vec![],
            };

            let mut rules = Vec::new();
            for rule in items[2..].iter() {
                match rule {
                    Value::List(rule_items) if rule_items.len() >= 2 => {
                        let pattern = rule_items[0..rule_items.len()-1].to_vec();
                        let template = rule_items[rule_items.len()-1].clone();
                        rules.push((pattern, template));
                    }
                    _ => return Err(EvalError::InvalidForm("syntax-rules: expected (pattern template)".into())),
                }
            }

            let serialized = serde_json::to_string(env).unwrap_or_default();
            let m = Value::Macro(Macro::SyntaxRules {
                literals,
                rules,
                env_serialized: serialized,
            });

            if !name.contains('/') {
                env.define(&format!("user/{}", name), m).map_err(|e| EvalError::SyntaxError(e))?;
            } else {
                env.define(&name, m).map_err(|e| EvalError::SyntaxError(e))?;
            }
            Ok(Value::Symbol(name))
        }
        _ => Err(EvalError::InvalidForm("define-syntax: unsupported transformer (only syntax-rules supported)".into())),
    }
}

// ---- define-data ----

fn eval_define_data(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("define-data requires a family name and variants".into()));
    }

    let family_name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        Value::Keyword(k) => k.clone(),
        other => return Err(EvalError::InvalidForm(format!("define-data: expected family name, got {}", other))),
    };

    // We create constructors that return tagged values via a factory approach.
    // Store the data family definition.
    let mut variants = Vec::new();
    for variant_def in &args[1..] {
        match variant_def {
            Value::List(items) if items.len() >= 1 => {
                let variant_name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => return Err(EvalError::InvalidForm(format!("define-data: expected variant name, got {}", other))),
                };
                let field_names: Vec<String> = items[1..].iter().map(|v| match v {
                    Value::Symbol(s) => s.clone(),
                    other => format!("{}", other),
                }).collect();

                // Create constructor via inline tagged value
                let fam = family_name.clone();
                let var = variant_name.clone();
                let constructor_name = format!("{}/{}", family_name, variant_name);

                env.set_data_constructor(&constructor_name, fam.clone(), var.clone(), field_names.clone());

                variants.push(crate::vm::env::DataVariant {
                    name: variant_name,
                    fields: field_names,
                });
            }
            _ => return Err(EvalError::InvalidForm("define-data: expected variant definition list".into())),
        }
    }

    // Store data family in env
    let fam_name = family_name.clone();
    env.set_data_family(&fam_name, crate::vm::env::DataFamily {
        name: fam_name.clone(),
        variants,
    });

    Ok(Value::Symbol(family_name))
}

// ---- match ----

fn eval_match(args: &[Value], env: &mut EnvRef) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::InvalidForm("match requires a value and at least one clause".into()));
    }

    let value = eval_value(args[0].clone(), env)?;

    for clause in &args[1..] {
        match clause {
            Value::List(items) if items.len() >= 2 => {
                let pattern = &items[0];
                let body = &items[1..];
                let mut bindings = HashMap::new();
                if match_pattern(&value, pattern, &mut bindings) {
                    env.push_frame();
                    for (k, v) in &bindings {
                        env.set_lexical(k, v.clone());
                    }
                    let result = eval_begin(body, env);
                    env.pop_frame();
                    return result;
                }
            }
            _ => return Err(EvalError::InvalidForm("match: expected (pattern body ...) clause".into())),
        }
    }

    Err(EvalError::InvalidForm("match: no clause matched the value".into()))
}

fn match_pattern(value: &Value, pattern: &Value, bindings: &mut HashMap<String, Value>) -> bool {
    match pattern {
        Value::Symbol(s) if s == "_" => true,
        Value::Symbol(s) => {
            bindings.insert(s.clone(), value.clone());
            true
        }
        Value::Nil => matches!(value, Value::Nil),
        Value::Bool(b) => matches!(value, Value::Bool(v) if v == b),
        Value::Int(n) => matches!(value, Value::Int(v) if v == n),
        Value::String(s) => matches!(value, Value::String(v) if v == s),
        Value::Keyword(k) => matches!(value, Value::Keyword(v) if v == k),
        Value::List(items) => {
            if let Value::List(vals) = value {
                vals.len() == items.len() && items.iter().zip(vals.iter()).all(|(p, v)| match_pattern(v, p, bindings))
            } else {
                false
            }
        }
        Value::Tagged { family, variant, fields } => {
            if let Value::Tagged { family: f, variant: v, fields: vals } = value {
                f == family && v == variant && fields.len() == vals.len()
                    && fields.iter().zip(vals.iter()).all(|(p, v)| match_pattern(v, p, bindings))
            } else {
                false
            }
        }
        _ => value == pattern,
    }
}

// ---- Macro expansion ----

fn try_expand_macro(name: &str, args: &[Value], env: &EnvRef) -> Result<Option<Value>, EvalError> {
    let val = env.lookup(name);
    match val {
        Some(Value::Macro(Macro::SyntaxRules { literals, rules, .. })) => {
            for (pattern, template) in rules {
                let form = Value::List(
                    std::iter::once(Value::Symbol(name.to_string()))
                        .chain(args.iter().cloned())
                        .collect()
                );
                let mut bindings = HashMap::new();
                if match_pattern_syntax(&form, pattern, &mut bindings, literals) {
                    let expanded = apply_template(template, &bindings)?;
                    return Ok(Some(expanded));
                }
            }
            Err(EvalError::SyntaxError(format!("no matching syntax-rules clause for {}", name)))
        }
        _ => Ok(None),
    }
}

fn match_pattern_syntax(value: &Value, pattern: &[Value], bindings: &mut HashMap<String, Value>, literals: &[String]) -> bool {
    if pattern.is_empty() {
        return matches!(value, Value::Nil);
    }

    let pat_head = &pattern[0];
    let pat_tail = &pattern[1..];

    match pat_head {
        Value::Symbol(s) if s == "_" => {
            match value {
                Value::List(items) if items.len() == pat_tail.len() + 1 => {
                    pat_tail.iter().zip(items[1..].iter()).all(|(p, v)| match_syntax_pattern(p, v, bindings, literals))
                }
                _ => false,
            }
        }
        Value::Symbol(s) if literals.contains(s) => {
            match value {
                Value::List(items) if !items.is_empty() => {
                    if let Value::Symbol(name) = &items[0] {
                        name == s && pat_tail.iter().zip(items[1..].iter()).all(|(p, v)| match_syntax_pattern(p, v, bindings, literals))
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }
        Value::Symbol(s) => {
            bindings.insert(s.clone(), value.clone());
            true
        }
        _ => {
            match value {
                Value::List(items) if !items.is_empty() => {
                    match_syntax_pattern(pat_head, &items[0], bindings, literals)
                        && pat_tail.iter().zip(items[1..].iter()).all(|(p, v)| match_syntax_pattern(p, v, bindings, literals))
                }
                _ => false,
            }
        }
    }
}

fn match_syntax_pattern(pattern: &Value, value: &Value, bindings: &mut HashMap<String, Value>, literals: &[String]) -> bool {
    match pattern {
        Value::Symbol(s) if s == "_" => true,
        Value::Symbol(s) if literals.contains(s) => {
            value == &Value::Symbol(s.clone())
        }
        Value::Symbol(s) => {
            bindings.insert(s.clone(), value.clone());
            true
        }
        Value::List(items) => {
            match value {
                Value::List(vals) if vals.len() == items.len() => {
                    items.iter().zip(vals.iter()).all(|(p, v)| match_syntax_pattern(p, v, bindings, literals))
                }
                _ => false,
            }
        }
        _ => value == pattern,
    }
}

fn apply_template(template: &Value, bindings: &HashMap<String, Value>) -> Result<Value, EvalError> {
    match template {
        Value::Symbol(s) => {
            Ok(bindings.get(s).cloned().unwrap_or_else(|| Value::Symbol(s.clone())))
        }
        Value::List(items) => {
            let evaled: Result<Vec<Value>, _> = items.iter().map(|item| apply_template(item, bindings)).collect();
            Ok(Value::List(evaled?))
        }
        other => Ok(other.clone()),
    }
}

// ---- Application ----

fn apply(fun: Value, args: Vec<Value>, env: &mut EnvRef) -> Result<Value, EvalError> {
    match fun {
        Value::Function(Function::Native { name, arity, func, .. }) => {
            if arity > 0 && args.len() as u32 != arity {
                return Err(EvalError::ArityMismatch { name, expected: arity, got: args.len() });
            }
            (func)(args).map_err(|e| EvalError::UserError(e))
        }
        Value::Function(Function::Interpreted { params, body, env_serialized }) => {
            // Deserialize closure env, but restore kernel natives from current env
            let mut local: EnvRef = serde_json::from_str(&env_serialized).unwrap_or_else(|_| env.clone());

            // Restore native function pointers from current env (they can't survive serialization)
            if let Some(current_kernel) = env.namespaces.get("kernel") {
                if let Some(local_kernel) = local.namespaces.get_mut("kernel") {
                    for (name, val) in &current_kernel.bindings {
                        if matches!(val, Value::Function(Function::Native { .. })) {
                            local_kernel.bindings.insert(name.clone(), val.clone());
                        }
                    }
                }
            }

            local.push_frame();
            for (p, a) in params.iter().zip(args.into_iter()) {
                local.set_lexical(p, a);
            }
            let result = eval_begin(&body, &mut local);
            local.pop_frame();
            result
        }
        Value::Macro(Macro::Native { name: _, func, .. }) => {
            (func)(args).map_err(|e| EvalError::UserError(e))
        }
        other => Err(EvalError::NotAFunction(other)),
    }
}

fn eval_args(args: &[Value], env: &mut EnvRef) -> Result<Vec<Value>, EvalError> {
    args.iter().map(|a| eval_value(a.clone(), env)).collect()
}
