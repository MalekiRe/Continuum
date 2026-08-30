use crate::kernel::Kernel;
use crate::vm::env::{DataFamily, DataVariant, EnvRef};
use crate::vm::reader;
use crate::vm::value::*;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
/// Global interrupt flag — set by kernel to request interruption of Lisp evaluation.
pub static EVAL_INTERRUPTED: AtomicBool = AtomicBool::new(false);
pub static EVAL_RUNNING: AtomicBool = AtomicBool::new(false);

pub fn request_interrupt() -> bool {
    EVAL_RUNNING.load(Ordering::Acquire) && !EVAL_INTERRUPTED.swap(true, Ordering::AcqRel)
}

type PrintHook = Option<fn(&str)>;

/// Global print hook — set by main.rs to capture all output.
pub static PRINT_HOOK: std::sync::LazyLock<Mutex<PrintHook>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Turn counter — incremented on every evaluated expression for safepoint checks.
pub static TURN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Max turns before automatic safepoint check.
pub const SAFEPOINT_INTERVAL: u64 = 1000;

/// Check safepoint — if interrupted, return an error.
#[inline]
pub fn check_safepoint() -> Result<(), EvalError> {
    let count = TURN_COUNTER.fetch_add(1, Ordering::Relaxed);
    if count.is_multiple_of(SAFEPOINT_INTERVAL) && EVAL_INTERRUPTED.load(Ordering::Relaxed) {
        EVAL_INTERRUPTED.store(false, Ordering::Relaxed);
        return Err(EvalError::KernelError(
            "evaluation interrupted by kernel".into(),
        ));
    }
    Ok(())
}

/// Result of a single evaluation step.
/// Step(expr) means evaluate expr next (tail call).
/// Done(val) means evaluation completed.
pub enum StepResult {
    Done(Value),
    Step(Value),
}

#[derive(Debug)]
pub enum EvalError {
    UndefinedSymbol(String),
    InvalidForm(String),
    NotAFunction(Value),
    ArityMismatch {
        name: String,
        expected: u32,
        got: usize,
    },
    SyntaxError(String),
    UserError(String),
    KernelError(String),
    InvalidPattern(String),
    TailCall(Value),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UndefinedSymbol(s) => write!(f, "undefined symbol: {}", s),
            EvalError::InvalidForm(s) => write!(f, "invalid form: {}", s),
            EvalError::NotAFunction(v) => write!(f, "not a function: {}", v),
            EvalError::ArityMismatch {
                name,
                expected,
                got,
            } => write!(
                f,
                "arity mismatch: {} expects {} arguments, got {}",
                name, expected, got
            ),
            EvalError::SyntaxError(s) => write!(f, "syntax error: {}", s),
            EvalError::UserError(s) => write!(f, "error: {}", s),
            EvalError::KernelError(s) => write!(f, "kernel error: {}", s),
            EvalError::InvalidPattern(s) => write!(f, "invalid pattern: {}", s),
            EvalError::TailCall(_) => write!(f, "tail call"),
        }
    }
}

impl std::error::Error for EvalError {}

/// Evaluate one step, converting TailCall to StepResult.
fn eval_step(value: Value, kernel: &mut Kernel) -> Result<StepResult, EvalError> {
    match eval_value_inner(value, kernel, true) {
        Ok(value) => Ok(StepResult::Done(value)),
        Err(EvalError::TailCall(expression)) => Ok(StepResult::Step(expression)),
        Err(error) => Err(error),
    }
}

pub fn eval(input: &str, kernel: &mut Kernel) -> Result<Value, EvalError> {
    let exprs = reader::read_all(input).map_err(|e| EvalError::SyntaxError(e.to_string()))?;
    let mut result = Value::Nil;
    for expr in exprs {
        // Tail-call trampoline steps may move the active arena cursor.
        // A completed top-level form must always restore its caller scope.
        let caller_environment = kernel.env.current_environment();
        let mut current = expr;
        loop {
            match eval_step(current, kernel) {
                Ok(StepResult::Done(value)) => {
                    result = value;
                    kernel
                        .env
                        .activate_environment(caller_environment)
                        .map_err(EvalError::KernelError)?;
                    break;
                }
                Ok(StepResult::Step(next)) => current = next,
                Err(error) => {
                    kernel
                        .env
                        .activate_environment(caller_environment)
                        .map_err(EvalError::KernelError)?;
                    return Err(error);
                }
            }
        }
    }
    Ok(result)
}

fn eval_value_inner(val: Value, kernel: &mut Kernel, tail_pos: bool) -> Result<Value, EvalError> {
    // Safepoint: check if kernel wants to interrupt
    check_safepoint()?;
    match val {
        Value::Symbol(ref name) => kernel
            .env
            .lookup(name)
            .cloned()
            .ok_or_else(|| EvalError::UndefinedSymbol(name.clone())),
        Value::List(ref items) if items.is_empty() => Ok(Value::Nil),
        Value::List(ref items) => {
            let head = &items[0];
            let args = &items[1..];
            match head {
                Value::Symbol(s) => {
                    match s.as_str() {
                        "define" => eval_define(args, kernel),
                        "undefine" => eval_undefine(args, kernel),
                        "lambda" => eval_lambda(args, kernel),
                        "if" => eval_if_tail(args, kernel, tail_pos),
                        "begin" => eval_begin_tail(args, kernel, tail_pos),
                        "let" => eval_let_tail(args, kernel, tail_pos),
                        "let*" => eval_let_star_tail(args, kernel, tail_pos),
                        "letrec" => eval_letrec_tail(args, kernel, tail_pos),
                        "set!" => eval_set(args, kernel),
                        "quote" => eval_quote(args),
                        "quasiquote" => eval_quasiquote(args, kernel),
                        "define-syntax" => eval_define_syntax(args, kernel),
                        "define-data" => eval_define_data(args, kernel),
                        "match" => eval_match(args, kernel),
                        _ => {
                            // Check if this symbol is a macro
                            let expanded = try_expand_macro(s, args, &kernel.env)?;
                            if let Some(expanded) = expanded {
                                eval_value_inner(expanded, kernel, tail_pos)
                            } else {
                                let fun =
                                    eval_value_inner(Value::Symbol(s.clone()), kernel, false)?;
                                apply_tail(fun, eval_args(args, kernel)?, kernel, tail_pos)
                            }
                        }
                    }
                }
                _ => {
                    let fun = eval_value_inner(head.clone(), kernel, false)?;
                    apply_tail(fun, eval_args(args, kernel)?, kernel, tail_pos)
                }
            }
        }
        Value::Vector(items) => {
            let mut evaled = Vec::with_capacity(items.len());
            for item in items {
                evaled.push(eval_value_inner(item, kernel, false)?);
            }
            Ok(Value::Vector(evaled))
        }
        Value::Map(map) => {
            let mut evaled = IndexMap::new();
            for (k, v) in map {
                evaled.insert(
                    eval_value_inner(k, kernel, false)?,
                    eval_value_inner(v, kernel, false)?,
                );
            }
            Ok(Value::Map(evaled))
        }
        other => Ok(other),
    }
}

pub fn eval_value(val: Value, kernel: &mut Kernel) -> Result<Value, EvalError> {
    eval_value_inner(val, kernel, false)
}

// ---- Special forms ----

/// Evaluate a value, handling TailCall by following the trampoline.
fn eval_any(val: Value, kernel: &mut Kernel) -> Result<Value, EvalError> {
    let mut current = val;
    loop {
        match eval_step(current, kernel) {
            Ok(StepResult::Done(v)) => return Ok(v),
            Ok(StepResult::Step(next)) => {
                current = next;
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

fn eval_define(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("define requires arguments".into()));
    }

    match &args[0] {
        Value::Symbol(name) => {
            if args.len() != 2 {
                return Err(EvalError::InvalidForm(format!(
                    "define: expected (define name value), got {} args",
                    args.len()
                )));
            }
            let qualified = if name.contains('/') {
                name.clone()
            } else {
                format!("user/{}", name)
            };
            let retained = if kernel.current_form_is("define") {
                kernel.current_source.clone().unwrap_or_default()
            } else {
                format!("(define {} {})", name, args[1])
            };
            let val = eval_any(args[1].clone(), kernel)?;
            kernel
                .env
                .define(&qualified, val)
                .map_err(EvalError::SyntaxError)?;
            kernel.store_source(&qualified, &retained);
            Ok(Value::Symbol(name.clone()))
        }
        Value::List(params) => {
            if params.is_empty() {
                return Err(EvalError::InvalidForm(
                    "define: function definition needs a name".into(),
                ));
            }
            let name = match &params[0] {
                Value::Symbol(n) => n.clone(),
                other => {
                    return Err(EvalError::InvalidForm(format!(
                        "define: expected symbol for function name, got {}",
                        other
                    )));
                }
            };
            let param_names: Vec<String> = params[1..]
                .iter()
                .map(|parameter| match parameter {
                    Value::Symbol(name) => Ok(name.clone()),
                    other => Err(EvalError::InvalidForm(format!(
                        "define: expected symbol parameter, got {}",
                        other
                    ))),
                })
                .collect::<Result<_, _>>()?;
            let body = args[1..].to_vec();
            let reconstructed = format!(
                "(define ({} {}) {})",
                name,
                param_names.join(" "),
                body.iter()
                    .map(|v| format!("{}", v))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let retained = if kernel.current_form_is("define") {
                kernel.current_source.clone().unwrap_or(reconstructed)
            } else {
                reconstructed
            };
            let lambda = eval_lambda_simple(param_names, body, kernel)?;
            let qualified = if name.contains('/') {
                name.clone()
            } else {
                format!("user/{}", name)
            };
            kernel
                .env
                .define(&qualified, lambda)
                .map_err(EvalError::SyntaxError)?;
            kernel.store_source(&qualified, &retained);
            Ok(Value::Symbol(name))
        }
        other => Err(EvalError::InvalidForm(format!(
            "define: expected symbol or list, got {}",
            other
        ))),
    }
}

fn eval_undefine(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::InvalidForm(
            "undefine requires exactly one symbol argument".into(),
        ));
    }
    let name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "undefine: expected symbol, got {}",
                other
            )));
        }
    };

    // Check if this is a data family — if so, remove all constructors atomically
    // The family name is the full path (e.g., "my/Foo" not just "Foo")
    if kernel.env.is_data_family(&name) {
        // Undefine the data family, removing all constructors atomically
        kernel
            .env
            .undefine_data_family(&name)
            .map_err(EvalError::SyntaxError)?;
        return Ok(Value::Symbol(name));
    }

    // Regular undefine
    let qualified = if name.contains('/') {
        name
    } else {
        format!("user/{}", name)
    };
    kernel
        .env
        .undefine(&qualified)
        .map_err(EvalError::SyntaxError)?;
    Ok(Value::Nil)
}

fn eval_lambda(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm(
            "lambda requires parameters and body".into(),
        ));
    }
    let param_list = &args[0];
    let body = args[1..].to_vec();

    let param_names = match param_list {
        Value::List(items) => {
            let mut names = Vec::new();
            for p in items {
                match p {
                    Value::Symbol(s) => names.push(s.clone()),
                    other => {
                        return Err(EvalError::InvalidForm(format!(
                            "lambda: expected symbol parameter, got {}",
                            other
                        )));
                    }
                }
            }
            names
        }
        _ => {
            return Err(EvalError::InvalidForm(format!(
                "lambda: expected parameter list, got {}",
                param_list
            )));
        }
    };

    let env_id = kernel.capture_lexical_env();
    Ok(Value::Function(Function::Interpreted {
        params: param_names,
        body,
        env_id,
    }))
}

fn eval_lambda_simple(
    params: Vec<String>,
    body: Vec<Value>,
    kernel: &mut Kernel,
) -> Result<Value, EvalError> {
    let env_id = kernel.capture_lexical_env();
    Ok(Value::Function(Function::Interpreted {
        params,
        body,
        env_id,
    }))
}

fn eval_if_tail(args: &[Value], kernel: &mut Kernel, tail_pos: bool) -> Result<Value, EvalError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::InvalidForm("if expects 2 or 3 arguments".into()));
    }
    let cond = eval_value_inner(args[0].clone(), kernel, false)?;
    if cond.is_truthy() {
        eval_value_inner(args[1].clone(), kernel, tail_pos)
    } else if args.len() == 3 {
        eval_value_inner(args[2].clone(), kernel, tail_pos)
    } else {
        Ok(Value::Nil)
    }
}

fn eval_begin_tail(
    args: &[Value],
    kernel: &mut Kernel,
    tail_pos: bool,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Ok(Value::Nil);
    }
    // All expressions except the last: no tail position
    for arg in args.iter().take(args.len() - 1) {
        let _ = eval_value_inner(arg.clone(), kernel, false)?;
    }
    // Last expression: pass through tail_pos
    let result = eval_value_inner(args.last().unwrap().clone(), kernel, tail_pos)?;
    Ok(result)
}

fn parse_bindings(value: &Value, form: &str) -> Result<Vec<(String, Value)>, EvalError> {
    let Value::List(bindings) = value else {
        return Err(EvalError::InvalidForm(format!(
            "{}: expected binding list",
            form
        )));
    };
    bindings
        .iter()
        .map(|binding| match binding {
            Value::List(pair) if pair.len() == 2 => match &pair[0] {
                Value::Symbol(name) => Ok((name.clone(), pair[1].clone())),
                other => Err(EvalError::InvalidForm(format!(
                    "{}: expected symbol, got {}",
                    form, other
                ))),
            },
            other => Err(EvalError::InvalidForm(format!(
                "{}: expected (name value), got {}",
                form, other
            ))),
        })
        .collect()
}

fn finish_scoped_body(
    body: &[Value],
    kernel: &mut Kernel,
    tail_pos: bool,
) -> Result<Value, EvalError> {
    let result = eval_begin_tail(body, kernel, tail_pos);
    if !matches!(result, Err(EvalError::TailCall(_))) {
        kernel.env.pop_frame();
    }
    result
}

fn eval_let_tail(args: &[Value], kernel: &mut Kernel, tail_pos: bool) -> Result<Value, EvalError> {
    let bindings = parse_bindings(
        args.first()
            .ok_or_else(|| EvalError::InvalidForm("let requires bindings and body".into()))?,
        "let",
    )?;
    // Plain let initializers all observe the outer environment.
    let values = bindings
        .iter()
        .map(|(_, expr)| eval_value_inner(expr.clone(), kernel, false))
        .collect::<Result<Vec<_>, _>>()?;
    kernel.env.push_frame();
    for ((name, _), value) in bindings.into_iter().zip(values) {
        kernel.env.set_lexical(&name, value);
    }
    finish_scoped_body(&args[1..], kernel, tail_pos)
}

fn eval_let_star_tail(
    args: &[Value],
    kernel: &mut Kernel,
    tail_pos: bool,
) -> Result<Value, EvalError> {
    let bindings = parse_bindings(
        args.first()
            .ok_or_else(|| EvalError::InvalidForm("let* requires bindings and body".into()))?,
        "let*",
    )?;
    kernel.env.push_frame();
    for (name, expr) in bindings {
        match eval_value_inner(expr, kernel, false) {
            Ok(value) => kernel.env.set_lexical(&name, value),
            Err(error) => {
                kernel.env.pop_frame();
                return Err(error);
            }
        }
    }
    finish_scoped_body(&args[1..], kernel, tail_pos)
}

fn eval_letrec_tail(
    args: &[Value],
    kernel: &mut Kernel,
    tail_pos: bool,
) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm(
            "letrec requires bindings and body".into(),
        ));
    }

    let bindings = match &args[0] {
        Value::List(items) => items,
        _ => {
            return Err(EvalError::InvalidForm(
                "letrec: expected binding list".into(),
            ));
        }
    };

    kernel.env.push_frame();
    for binding in bindings {
        match binding {
            Value::List(items) if items.len() == 2 => {
                let name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => {
                        return Err(EvalError::InvalidForm(format!(
                            "letrec: expected symbol, got {}",
                            other
                        )));
                    }
                };
                kernel.env.set_lexical(&name, Value::Nil);
            }
            other => {
                return Err(EvalError::InvalidForm(format!(
                    "letrec: expected (name value) pair, got {}",
                    other
                )));
            }
        }
    }
    for binding in bindings {
        match binding {
            Value::List(items) if items.len() == 2 => {
                let name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => {
                        return Err(EvalError::InvalidForm(format!(
                            "letrec: expected symbol, got {}",
                            other
                        )));
                    }
                };
                let val = eval_value_inner(items[1].clone(), kernel, false)?;
                kernel.env.set_lexical(&name, val);
            }
            _ => unreachable!(),
        }
    }

    finish_scoped_body(&args[1..], kernel, tail_pos)
}

fn eval_set(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::InvalidForm("set! expects 2 arguments".into()));
    }
    let name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "set!: expected symbol, got {}",
                other
            )));
        }
    };
    let val = eval_any(args[1].clone(), kernel)?;

    // Mutate the shared cell found along the active lexical chain.
    if kernel.env.set_existing_lexical(&name, val.clone()) {
        return Ok(Value::Nil);
    }

    // Check namespaces
    if name.contains('/') {
        if kernel.env.lookup(&name).is_some() {
            kernel
                .env
                .define(&name, val)
                .map_err(EvalError::SyntaxError)?;
            return Ok(Value::Nil);
        }
    } else {
        let qualified = format!("user/{}", name);
        if kernel.env.lookup(&qualified).is_some() {
            kernel
                .env
                .define(&qualified, val)
                .map_err(EvalError::SyntaxError)?;
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

fn eval_quasiquote(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm(
            "quasiquote requires an argument".into(),
        ));
    }
    expand_quasiquote(&args[0], kernel)
}

fn expand_quasiquote(val: &Value, kernel: &mut Kernel) -> Result<Value, EvalError> {
    match val {
        Value::List(items) if !items.is_empty() => {
            let head = &items[0];
            if let Value::Symbol(s) = head
                && s == "unquote"
            {
                if items.len() != 2 {
                    return Err(EvalError::InvalidForm("unquote expects 1 argument".into()));
                }
                return eval_value(items[1].clone(), kernel);
            }
            expand_quasiquote_seq(items, kernel, Value::List)
        }
        Value::Vector(items) => expand_quasiquote_seq(items, kernel, Value::Vector),
        _ => Ok(val.clone()),
    }
}

fn expand_quasiquote_seq(
    items: &[Value],
    kernel: &mut Kernel,
    wrap: fn(Vec<Value>) -> Value,
) -> Result<Value, EvalError> {
    let mut result = Vec::new();
    for item in items {
        match item {
            Value::List(sub) if !sub.is_empty() => {
                if let Value::Symbol(s) = &sub[0]
                    && s == "unquote-splicing"
                {
                    if sub.len() != 2 {
                        return Err(EvalError::InvalidForm(
                            "unquote-splicing expects 1 argument".into(),
                        ));
                    }
                    let spliced = eval_value(sub[1].clone(), kernel)?;
                    match spliced {
                        Value::List(v) => result.extend(v),
                        Value::Vector(v) => result.extend(v),
                        _ => result.push(spliced),
                    }
                    continue;
                }
                result.push(expand_quasiquote(item, kernel)?);
            }
            other => {
                result.push(expand_quasiquote(other, kernel)?);
            }
        }
    }
    Ok(wrap(result))
}

// ---- define-syntax (simple) ----

fn eval_define_syntax(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::InvalidForm(
            "define-syntax requires name and transformer".into(),
        ));
    }
    let name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "define-syntax: expected symbol, got {}",
                other
            )));
        }
    };

    let transformer = &args[1];

    match transformer {
        Value::List(items)
            if items.len() >= 2 && matches!(&items[0], Value::Symbol(s) if s == "syntax-rules") =>
        {
            let literals = match &items[1] {
                Value::List(lits) => lits
                    .iter()
                    .map(|v| match v {
                        Value::Symbol(s) => s.clone(),
                        _ => "".into(),
                    })
                    .collect(),
                _ => vec![],
            };

            let mut rules = Vec::new();
            for rule in items[2..].iter() {
                match rule {
                    Value::List(rule_items) if rule_items.len() >= 2 => {
                        let pattern = match &rule_items[0] {
                            Value::List(items) => items.clone(),
                            _ => {
                                return Err(EvalError::InvalidForm(
                                    "syntax-rules: pattern must be a list".into(),
                                ));
                            }
                        };
                        let template = rule_items[rule_items.len() - 1].clone();
                        rules.push((pattern, template));
                    }
                    _ => {
                        return Err(EvalError::InvalidForm(
                            "syntax-rules: expected (pattern template)".into(),
                        ));
                    }
                }
            }

            let m = Value::Macro(Macro::SyntaxRules { literals, rules });

            if !name.contains('/') {
                kernel
                    .env
                    .define(&format!("user/{}", name), m)
                    .map_err(EvalError::SyntaxError)?;
            } else {
                kernel
                    .env
                    .define(&name, m)
                    .map_err(EvalError::SyntaxError)?;
            }
            Ok(Value::Symbol(name))
        }
        _ => Err(EvalError::InvalidForm(
            "define-syntax: unsupported transformer (only syntax-rules supported)".into(),
        )),
    }
}

// ---- define-data ----

fn eval_define_data(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm(
            "define-data requires a family name and variants".into(),
        ));
    }

    let family_name = match &args[0] {
        Value::Symbol(s) => s.clone(),
        Value::Keyword(k) => k.clone(),
        other => {
            return Err(EvalError::InvalidForm(format!(
                "define-data: expected family name, got {}",
                other
            )));
        }
    };

    let qualified_family = if family_name.contains('/') {
        family_name.clone()
    } else {
        format!("user/{}", family_name)
    };
    let mut variants = Vec::new();
    for variant_def in &args[1..] {
        match variant_def {
            Value::List(items) if !items.is_empty() => {
                let variant_name = match &items[0] {
                    Value::Symbol(s) => s.clone(),
                    other => {
                        return Err(EvalError::InvalidForm(format!(
                            "define-data: expected variant name, got {}",
                            other
                        )));
                    }
                };
                let field_names: Vec<String> = items[1..]
                    .iter()
                    .map(|v| match v {
                        Value::Symbol(s) => s.clone(),
                        other => format!("{}", other),
                    })
                    .collect();
                variants.push(DataVariant {
                    name: variant_name,
                    fields: field_names,
                });
            }
            _ => {
                return Err(EvalError::InvalidForm(
                    "define-data: expected variant definition list".into(),
                ));
            }
        }
    }

    let generated_bindings: Vec<_> = variants
        .iter()
        .map(|variant| format!("{}/{}", qualified_family, variant.name))
        .collect();
    kernel
        .env
        .set_data_family(DataFamily {
            name: qualified_family.clone(),
            variants: variants.clone(),
            generated_bindings: generated_bindings.clone(),
        })
        .map_err(EvalError::SyntaxError)?;

    for (variant, constructor_name) in variants.iter().zip(generated_bindings) {
        let constructor = Value::Function(Function::Constructor {
            family: qualified_family.clone(),
            variant: variant.name.clone(),
            arity: variant.fields.len() as u32,
        });
        kernel
            .env
            .define(&constructor_name, constructor)
            .map_err(EvalError::SyntaxError)?;
    }

    Ok(Value::Symbol(family_name))
}

// ---- match ----

fn eval_match(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::InvalidForm(
            "match requires a value and at least one clause".into(),
        ));
    }

    let value = eval_value(args[0].clone(), kernel)?;

    for clause in &args[1..] {
        match clause {
            Value::List(items) if items.len() >= 2 => {
                let pattern = &items[0];
                let body = &items[1..];
                let mut bindings = HashMap::new();
                if match_pattern(&value, pattern, &mut bindings) {
                    kernel.env.push_frame();
                    for (k, v) in &bindings {
                        kernel.env.set_lexical(k, v.clone());
                    }
                    let result = eval_begin_tail(body, kernel, false);
                    kernel.env.pop_frame();
                    return result;
                }
            }
            _ => {
                return Err(EvalError::InvalidForm(
                    "match: expected (pattern body ...) clause".into(),
                ));
            }
        }
    }

    Err(EvalError::InvalidForm(
        "match: no clause matched the value".into(),
    ))
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
        // Constructor pattern: (ctor-name field-pattern ...) matching tagged values
        Value::List(items) if !items.is_empty() && matches!(&items[0], Value::Symbol(_)) => {
            if let Value::Tagged {
                family: f,
                variant: v,
                fields: vals,
            } = value
            {
                // Check if the pattern head matches the constructor
                let ctor_name = format!("{}/{}", f, v);
                if let Value::Symbol(ref s) = items[0]
                    && (s == &ctor_name
                        || s == v
                        || s == &format!("{}/{}", f.split('/').next_back().unwrap_or(f), v))
                {
                    let pat_fields = &items[1..];
                    if pat_fields.len() == vals.len() {
                        return pat_fields
                            .iter()
                            .zip(vals.iter())
                            .all(|(pattern, value)| match_pattern(value, pattern, bindings));
                    }
                }
                false
            } else {
                // Regular list matching
                if let Value::List(vals) = value {
                    vals.len() == items.len()
                        && items
                            .iter()
                            .zip(vals.iter())
                            .all(|(p, v)| match_pattern(v, p, bindings))
                } else {
                    false
                }
            }
        }
        Value::List(items) => {
            if let Value::List(vals) = value {
                vals.len() == items.len()
                    && items
                        .iter()
                        .zip(vals.iter())
                        .all(|(p, v)| match_pattern(v, p, bindings))
            } else {
                false
            }
        }
        Value::Tagged {
            family,
            variant,
            fields,
        } => {
            if let Value::Tagged {
                family: f,
                variant: v,
                fields: vals,
            } = value
            {
                f == family
                    && v == variant
                    && fields.len() == vals.len()
                    && fields
                        .iter()
                        .zip(vals.iter())
                        .all(|(p, v)| match_pattern(v, p, bindings))
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
        Some(Value::Macro(Macro::SyntaxRules {
            literals, rules, ..
        })) => {
            for (pattern, template) in rules {
                let form = Value::List(
                    std::iter::once(Value::Symbol(name.to_string()))
                        .chain(args.iter().cloned())
                        .collect(),
                );
                let mut bindings = HashMap::new();
                if match_pattern_syntax(&form, pattern, &mut bindings, literals) {
                    let expanded = apply_template(template, &bindings)?;
                    return Ok(Some(expanded));
                }
            }
            Err(EvalError::SyntaxError(format!(
                "no matching syntax-rules clause for {}",
                name
            )))
        }
        _ => Ok(None),
    }
}

fn match_pattern_syntax(
    value: &Value,
    pattern: &[Value],
    bindings: &mut HashMap<String, Value>,
    literals: &[String],
) -> bool {
    // The whole form must be a list
    let form_items = match value {
        Value::List(items) => items,
        _ => return false,
    };

    if pattern.is_empty() {
        return form_items.is_empty();
    }

    // Pattern element 0 is the macro name — must match literally as a symbol
    // Pattern elements 1..N are the arguments
    let pat_head = &pattern[0];
    let pat_args = &pattern[1..];

    // Match the macro name
    let name_match = match pat_head {
        Value::Symbol(s) => {
            // In syntax-rules, the first symbol of the pattern is the macro name
            // It must match as a literal (not a binding variable)
            form_items
                .first()
                .is_some_and(|first| first == &Value::Symbol(s.clone()))
        }
        _ => false,
    };

    if !name_match {
        return false;
    }

    // Now match the remaining arguments
    if pat_args.is_empty() {
        return form_items.len() == 1; // just the name
    }

    // Handle ellipsis in the last pattern element
    let has_ellipsis =
        pat_args.len() >= 2 && pat_args[pat_args.len() - 1] == Value::Symbol("...".to_string());

    if has_ellipsis {
        // The pattern element before ... captures all remaining form items as a list
        let repeated_var = &pat_args[pat_args.len() - 2];
        let fixed_args = &pat_args[..pat_args.len() - 2];

        // Match fixed arguments
        let fixed_count = fixed_args.len();
        let form_args = &form_items[1..]; // skip the name

        if form_args.len() < fixed_count {
            return false;
        }

        // Match fixed args
        for (p, v) in fixed_args.iter().zip(form_args.iter()) {
            if !match_syntax_pattern(p, v, bindings, literals) {
                return false;
            }
        }

        // Bind the repeated variable to the remaining form items as a list
        let rest: Vec<Value> = form_args[fixed_count..].to_vec();
        match repeated_var {
            Value::Symbol(s) => {
                bindings.insert(s.clone(), Value::List(rest));
            }
            _ => return false,
        }

        true
    } else {
        // No ellipsis: exact match
        let form_args = &form_items[1..]; // skip the name
        if form_args.len() != pat_args.len() {
            return false;
        }
        pat_args
            .iter()
            .zip(form_args.iter())
            .all(|(p, v)| match_syntax_pattern(p, v, bindings, literals))
    }
}

fn match_syntax_pattern(
    pattern: &Value,
    value: &Value,
    bindings: &mut HashMap<String, Value>,
    literals: &[String],
) -> bool {
    match pattern {
        Value::Symbol(s) if s == "_" => true,
        Value::Symbol(s) if literals.contains(s) => value == &Value::Symbol(s.clone()),
        Value::Symbol(s) => {
            bindings.insert(s.clone(), value.clone());
            true
        }
        Value::List(items) => match value {
            Value::List(vals) if vals.len() == items.len() => items
                .iter()
                .zip(vals.iter())
                .all(|(p, v)| match_syntax_pattern(p, v, bindings, literals)),
            _ => false,
        },
        _ => value == pattern,
    }
}

fn apply_template(template: &Value, bindings: &HashMap<String, Value>) -> Result<Value, EvalError> {
    match template {
        Value::Symbol(s) => Ok(bindings
            .get(s)
            .cloned()
            .unwrap_or_else(|| Value::Symbol(s.clone()))),
        Value::List(items) => {
            let mut result = Vec::new();
            let mut i = 0;
            while i < items.len() {
                // Check for ellipsis: pattern_var followed by ...
                if i + 1 < items.len()
                    && items[i + 1] == Value::Symbol("...".to_string())
                    && let Value::Symbol(var_name) = &items[i]
                {
                    if let Some(Value::List(values)) = bindings.get(var_name) {
                        result.extend(values.clone());
                    }
                    i += 2;
                    continue;
                }
                result.push(apply_template(&items[i], bindings)?);
                i += 1;
            }
            Ok(Value::List(result))
        }
        other => Ok(other.clone()),
    }
}

// ---- Application ----

fn apply_tail(
    fun: Value,
    args: Vec<Value>,
    kernel: &mut Kernel,
    tail_ok: bool,
) -> Result<Value, EvalError> {
    match fun {
        Value::Function(Function::Native {
            name, arity, func, ..
        }) => {
            if arity != VARIADIC_ARITY && args.len() as u32 != arity {
                return Err(EvalError::ArityMismatch {
                    name,
                    expected: arity,
                    got: args.len(),
                });
            }
            (func)(kernel, args).map_err(EvalError::UserError)
        }
        Value::Function(Function::Constructor {
            family,
            variant,
            arity,
        }) => {
            if arity != VARIADIC_ARITY && args.len() as u32 != arity {
                return Err(EvalError::ArityMismatch {
                    name: format!("{}/{}", family, variant),
                    expected: arity,
                    got: args.len(),
                });
            }
            Ok(Value::Tagged {
                family: family.clone(),
                variant: variant.clone(),
                fields: args,
            })
        }
        Value::Function(Function::Interpreted {
            params,
            body,
            env_id,
        }) => {
            if args.len() != params.len() {
                return Err(EvalError::ArityMismatch {
                    name: "lambda".into(),
                    expected: u32::try_from(params.len()).unwrap_or(u32::MAX),
                    got: args.len(),
                });
            }

            let caller_environment = kernel.env.current_environment();
            kernel
                .env
                .push_call_frame(env_id)
                .map_err(EvalError::KernelError)?;
            for (parameter, argument) in params.iter().zip(args) {
                kernel.env.set_lexical(parameter, argument);
            }

            // The trampoline owns the active cursor after a tail call. The call
            // frame remains parented to the closure environment, not the caller.
            if tail_ok && body.len() == 1 {
                let next_expr = body.into_iter().next().unwrap();
                return Err(EvalError::TailCall(next_expr));
            }

            let result = match eval_begin_tail(&body, kernel, true) {
                Err(EvalError::TailCall(expr)) => eval_any(expr, kernel),
                other => other,
            };
            kernel
                .env
                .activate_environment(caller_environment)
                .map_err(EvalError::KernelError)?;
            result
        }

        other => Err(EvalError::NotAFunction(other)),
    }
}

fn eval_args(args: &[Value], kernel: &mut Kernel) -> Result<Vec<Value>, EvalError> {
    args.iter().map(|a| eval_value(a.clone(), kernel)).collect()
}
