use crate::kernel::{Kernel, qualify_user_name};
use crate::vm::env::{DataFamily, DataVariant, EnvRef};
use crate::vm::value::*;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const RUNNING: u8 = 1;
const PENDING: u8 = 2;

#[derive(Clone, Debug, Default)]
pub(crate) struct EvalControl {
    state: Arc<std::sync::atomic::AtomicU8>,
    turns: Arc<AtomicU64>,
}

#[derive(Clone, Debug)]
pub struct EvalInterruptHandle(EvalControl);

impl EvalControl {
    pub(crate) fn interrupt_handle(&self) -> EvalInterruptHandle {
        EvalInterruptHandle(self.clone())
    }

    pub(crate) fn begin(&self) -> EvalRunGuard {
        self.state.fetch_or(RUNNING, Ordering::AcqRel);
        EvalRunGuard {
            control: self.clone(),
            finished: false,
        }
    }

    pub(crate) fn check_safepoint(&self) -> Result<(), EvalError> {
        let count = self.turns.fetch_add(1, Ordering::Relaxed);
        if count.is_multiple_of(SAFEPOINT_INTERVAL)
            && self.state.fetch_and(!PENDING, Ordering::AcqRel) & PENDING != 0
        {
            return Err(EvalError::Interrupted);
        }
        Ok(())
    }
}

impl EvalInterruptHandle {
    pub fn request_interrupt(&self) -> bool {
        self.0.state.fetch_or(PENDING, Ordering::AcqRel) & RUNNING != 0
    }

    pub fn clear_pending(&self) {
        let _ = self
            .0
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & RUNNING == 0).then_some(state & !PENDING)
            });
    }

    pub fn is_running(&self) -> bool {
        self.0.state.load(Ordering::Acquire) & RUNNING != 0
    }
}

pub(crate) struct EvalRunGuard {
    control: EvalControl,
    finished: bool,
}

impl EvalRunGuard {
    pub(crate) fn finish<T>(mut self, result: Result<T, EvalError>) -> Result<T, EvalError> {
        let interrupted = self
            .control
            .state
            .fetch_and(!(RUNNING | PENDING), Ordering::AcqRel)
            & PENDING
            != 0;
        self.finished = true;
        if interrupted && result.is_ok() {
            Err(EvalError::Interrupted)
        } else {
            result
        }
    }
}

impl Drop for EvalRunGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.control.state.fetch_and(!RUNNING, Ordering::AcqRel);
        }
    }
}

/// Max turns before automatic safepoint check.
pub const SAFEPOINT_INTERVAL: u64 = 1000;

/// Result of a single evaluation step.
/// Step(expr) means evaluate expr next (tail call).
/// Done(val) means evaluation completed.
enum StepResult {
    Done(Value),
    Step(Value),
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("undefined symbol: {0}")]
    UndefinedSymbol(String),
    #[error("invalid form: {0}")]
    InvalidForm(String),
    #[error("not a function: {0}")]
    NotAFunction(Value),
    #[error("arity mismatch: {name} expects {expected} arguments, got {got}")]
    ArityMismatch {
        name: String,
        expected: usize,
        got: usize,
    },
    #[error("syntax error: {0}")]
    SyntaxError(String),
    #[error("error: {0}")]
    UserError(String),
    #[error(transparent)]
    Native(#[from] NativeError),
    #[error("kernel error: {0}")]
    KernelError(String),
    #[error(transparent)]
    Environment(#[from] crate::vm::env::EnvError),
    #[error("invalid pattern: {0}")]
    InvalidPattern(String),
    #[error("evaluation interrupted")]
    Interrupted,
    #[error("tail call")]
    TailCall(Value),
}

/// Evaluate one step, converting TailCall to StepResult.
fn eval_step(value: Value, kernel: &mut Kernel) -> Result<StepResult, EvalError> {
    match eval_value_inner(value, kernel, true) {
        Ok(value) => Ok(StepResult::Done(value)),
        Err(EvalError::TailCall(expression)) => Ok(StepResult::Step(expression)),
        Err(error) => Err(error),
    }
}

pub fn eval(input: &str, kernel: &mut Kernel) -> Result<Value, EvalError> {
    kernel.eval(input)
}

pub(crate) fn eval_forms(exprs: Vec<Value>, kernel: &mut Kernel) -> Result<Value, EvalError> {
    exprs
        .into_iter()
        .try_fold(Value::Nil, |_, expression| eval_any(expression, kernel))
}
fn eval_value_inner(val: Value, kernel: &mut Kernel, tail_pos: bool) -> Result<Value, EvalError> {
    // Safepoint: check if kernel wants to interrupt
    kernel.eval_control.check_safepoint()?;
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

fn expect_symbol(value: &Value, form: &str, expected: &str) -> Result<String, EvalError> {
    let Value::Symbol(name) = value else {
        return Err(EvalError::InvalidForm(format!(
            "{form}: expected {expected}, got {value}"
        )));
    };
    Ok(name.clone())
}

fn expect_symbols(values: &[Value], form: &str, expected: &str) -> Result<Vec<String>, EvalError> {
    values
        .iter()
        .map(|value| expect_symbol(value, form, expected))
        .collect()
}

fn interpreted_function(params: Vec<String>, body: Vec<Value>, kernel: &Kernel) -> Value {
    Value::Function(Function::Interpreted {
        params,
        body,
        env_id: kernel.capture_lexical_env(),
    })
}

/// Evaluate a value, handling TailCall by following the trampoline.
fn eval_any(val: Value, kernel: &mut Kernel) -> Result<Value, EvalError> {
    let caller_environment = kernel.env.current_environment();
    let mut current = val;
    let result = loop {
        match eval_step(current, kernel) {
            Ok(StepResult::Done(value)) => break Ok(value),
            Ok(StepResult::Step(next)) => current = next,
            Err(error) => break Err(error),
        }
    };
    kernel.env.activate_environment(caller_environment)?;
    result
}

fn eval_define(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    let Some(target) = args.first() else {
        return Err(EvalError::InvalidForm("define requires arguments".into()));
    };
    let (name, value, retained) = match target {
        Value::Symbol(name) => {
            if args.len() != 2 {
                return Err(EvalError::InvalidForm(format!(
                    "define: expected (define name value), got {} args",
                    args.len()
                )));
            }
            let retained = if kernel.current_form_is("define") {
                kernel.current_source().unwrap_or_default().to_owned()
            } else {
                format!("(define {name} {})", args[1])
            };
            (name.clone(), eval_any(args[1].clone(), kernel)?, retained)
        }
        Value::List(signature) => {
            let Some(name) = signature.first() else {
                return Err(EvalError::InvalidForm(
                    "define: function definition needs a name".into(),
                ));
            };
            let name = expect_symbol(name, "define", "symbol for function name")?;
            let params = expect_symbols(&signature[1..], "define", "symbol parameter")?;
            let body = args[1..].to_vec();
            let reconstructed = format!(
                "(define ({} {}) {})",
                name,
                params.join(" "),
                body.iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let retained = if kernel.current_form_is("define") {
                kernel
                    .current_source()
                    .map(str::to_owned)
                    .unwrap_or_else(|| reconstructed.clone())
            } else {
                reconstructed
            };
            (name, interpreted_function(params, body, kernel), retained)
        }
        other => {
            return Err(EvalError::InvalidForm(format!(
                "define: expected symbol or list, got {other}"
            )));
        }
    };
    let qualified = qualify_user_name(&name);
    kernel.env.define(&qualified, value)?;
    kernel.store_source(&qualified, &retained);
    Ok(Value::Symbol(name))
}
fn eval_undefine(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::InvalidForm(
            "undefine requires exactly one symbol argument".into(),
        ));
    }
    let name = expect_symbol(&args[0], "undefine", "symbol")?;
    if kernel.env.is_data_family(&name) {
        kernel.env.undefine_data_family(&name)?;
        return Ok(Value::Symbol(name));
    }
    kernel.env.undefine(&qualify_user_name(&name))?;
    Ok(Value::Nil)
}
fn eval_lambda(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    let Some(parameter_list) = args.first() else {
        return Err(EvalError::InvalidForm(
            "lambda requires parameters and body".into(),
        ));
    };
    let Value::List(parameters) = parameter_list else {
        return Err(EvalError::InvalidForm(format!(
            "lambda: expected parameter list, got {parameter_list}"
        )));
    };
    Ok(interpreted_function(
        expect_symbols(parameters, "lambda", "symbol parameter")?,
        args[1..].to_vec(),
        kernel,
    ))
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
    let Some((last, preceding)) = args.split_last() else {
        return Ok(Value::Nil);
    };
    for expression in preceding {
        eval_value_inner(expression.clone(), kernel, false)?;
    }
    eval_value_inner(last.clone(), kernel, tail_pos)
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

fn eval_in_new_frame(
    kernel: &mut Kernel,
    evaluate: impl FnOnce(&mut Kernel) -> Result<Value, EvalError>,
) -> Result<Value, EvalError> {
    let caller = kernel.env.current_environment();
    kernel.env.push_frame();
    let result = evaluate(kernel);
    if !matches!(result, Err(EvalError::TailCall(_))) {
        kernel.env.activate_environment(caller)?;
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
    eval_in_new_frame(kernel, |kernel| {
        for ((name, _), value) in bindings.into_iter().zip(values) {
            kernel.env.set_lexical(&name, value);
        }
        eval_begin_tail(&args[1..], kernel, tail_pos)
    })
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
    eval_in_new_frame(kernel, |kernel| {
        for (name, expr) in bindings {
            let value = eval_value_inner(expr, kernel, false)?;
            kernel.env.set_lexical(&name, value);
        }
        eval_begin_tail(&args[1..], kernel, tail_pos)
    })
}

fn eval_letrec_tail(
    args: &[Value],
    kernel: &mut Kernel,
    tail_pos: bool,
) -> Result<Value, EvalError> {
    let bindings = parse_bindings(
        args.first()
            .ok_or_else(|| EvalError::InvalidForm("letrec requires bindings and body".into()))?,
        "letrec",
    )?;
    eval_in_new_frame(kernel, |kernel| {
        for (name, _) in &bindings {
            kernel.env.set_lexical(name, Value::Nil);
        }
        for (name, expression) in bindings {
            let value = eval_value_inner(expression, kernel, false)?;
            kernel.env.set_lexical(&name, value);
        }
        eval_begin_tail(&args[1..], kernel, tail_pos)
    })
}
fn eval_set(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::InvalidForm("set! expects 2 arguments".into()));
    }
    let name = expect_symbol(&args[0], "set!", "symbol")?;
    let value = eval_any(args[1].clone(), kernel)?;
    if kernel.env.set_existing_lexical(&name, value.clone()) {
        return Ok(Value::Nil);
    }
    let qualified = qualify_user_name(&name);
    if kernel.env.lookup(&qualified).is_some() {
        kernel.env.define(&qualified, value)?;
        return Ok(Value::Nil);
    }
    Err(EvalError::UndefinedSymbol(name))
}
fn eval_quote(args: &[Value]) -> Result<Value, EvalError> {
    args.first()
        .cloned()
        .ok_or_else(|| EvalError::InvalidForm("quote requires an argument".into()))
}

fn eval_quasiquote(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
    expand_quasiquote(
        args.first()
            .ok_or_else(|| EvalError::InvalidForm("quasiquote requires an argument".into()))?,
        kernel,
    )
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
                kernel.env.define(&format!("user/{}", name), m)?;
            } else {
                kernel.env.define(&name, m)?;
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
    kernel.env.set_data_family(DataFamily {
        name: crate::ids::QualifiedName::new(qualified_family.clone()),
        variants: variants.clone(),
        generated_bindings: generated_bindings
            .iter()
            .cloned()
            .map(crate::ids::QualifiedName::new)
            .collect(),
    })?;

    for (variant, constructor_name) in variants.iter().zip(generated_bindings) {
        let constructor = Value::Function(Function::Constructor {
            family: qualified_family.clone(),
            variant: variant.name.clone(),
            arity: variant.fields.len(),
        });
        kernel.env.define(&constructor_name, constructor)?;
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
                    return eval_in_new_frame(kernel, |kernel| {
                        for (name, value) in bindings {
                            kernel.env.set_lexical(&name, value);
                        }
                        eval_begin_tail(body, kernel, false)
                    });
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
        Value::Function(Function::Native { name, arity }) => {
            if let Arity::Exact(expected) = arity
                && args.len() != expected as usize
            {
                return Err(EvalError::ArityMismatch {
                    name,
                    expected: expected as usize,
                    got: args.len(),
                });
            }
            let native = kernel.native(&name).ok_or_else(|| {
                NativeError::Failed(format!("native function '{}' is not registered", name))
            })?;
            native(kernel, args).map_err(EvalError::Native)
        }
        Value::Function(Function::Constructor {
            family,
            variant,
            arity,
        }) => {
            if args.len() != arity {
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
                    expected: params.len(),
                    got: args.len(),
                });
            }

            let caller_environment = kernel.env.current_environment();
            kernel.env.push_call_frame(env_id)?;
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
            kernel.env.activate_environment(caller_environment)?;
            result
        }

        other => Err(EvalError::NotAFunction(other)),
    }
}

fn eval_args(args: &[Value], kernel: &mut Kernel) -> Result<Vec<Value>, EvalError> {
    args.iter().map(|a| eval_value(a.clone(), kernel)).collect()
}
