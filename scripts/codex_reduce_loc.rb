# frozen_string_literal: true


def replace(path, before, after)
  text = File.read(path)
  count = text.scan(Regexp.new(Regexp.escape(before))).length
  abort "#{path}: expected one match, found #{count}" unless count == 1
  File.write(path, text.sub(before, after))
end


def replace_between(path, first, last, replacement)
  text = File.read(path)
  start = text.index(first) or abort "#{path}: missing start marker #{first.inspect}"
  finish = text.index(last, start + first.length) or abort "#{path}: missing end marker #{last.inspect}"
  File.write(path, text[0...start] + replacement + text[finish..])
end

# Centralize evaluator mechanics and parsing helpers.
replace(
  "src/vm/eval.rs",
  "use crate::kernel::Kernel;",
  "use crate::kernel::{Kernel, qualify_user_name};"
)

replace_between(
  "src/vm/eval.rs",
  "pub(crate) fn eval_forms(exprs: Vec<Value>, kernel: &mut Kernel) -> Result<Value, EvalError> {",
  "\nfn eval_value_inner",
  <<~'RUST'.chomp
    pub(crate) fn eval_forms(exprs: Vec<Value>, kernel: &mut Kernel) -> Result<Value, EvalError> {
        exprs
            .into_iter()
            .try_fold(Value::Nil, |_, expression| eval_any(expression, kernel))
    }
  RUST
)

replace(
  "src/vm/eval.rs",
  <<~'OLD',
    // ---- Special forms ----

    /// Evaluate a value, handling TailCall by following the trampoline.
OLD
  <<~'NEW'
    // ---- Special forms ----

    fn expect_symbol(value: &Value, form: &str, expected: &str) -> Result<String, EvalError> {
        let Value::Symbol(name) = value else {
            return Err(EvalError::InvalidForm(format!(
                "{form}: expected {expected}, got {value}"
            )));
        };
        Ok(name.clone())
    }

    fn expect_symbols(
        values: &[Value],
        form: &str,
        expected: &str,
    ) -> Result<Vec<String>, EvalError> {
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
NEW
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_define(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {",
  "\nfn eval_undefine",
  <<~'RUST'.chomp
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
                (
                    name,
                    interpreted_function(params, body, kernel),
                    retained,
                )
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
  RUST
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_undefine(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {",
  "\nfn eval_lambda",
  <<~'RUST'.chomp
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
  RUST
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_lambda(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {",
  "\nfn eval_lambda_simple",
  <<~'RUST'.chomp
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
  RUST
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_lambda_simple(",
  "fn eval_if_tail",
  ""
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_begin_tail(",
  "\nfn parse_bindings",
  <<~'RUST'.chomp
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
  RUST
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_letrec_tail(",
  "\nfn eval_set",
  <<~'RUST'.chomp
    fn eval_letrec_tail(
        args: &[Value],
        kernel: &mut Kernel,
        tail_pos: bool,
    ) -> Result<Value, EvalError> {
        let bindings = parse_bindings(
            args.first().ok_or_else(|| {
                EvalError::InvalidForm("letrec requires bindings and body".into())
            })?,
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
  RUST
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_set(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {",
  "\nfn eval_quote",
  <<~'RUST'.chomp
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
  RUST
)

replace_between(
  "src/vm/eval.rs",
  "fn eval_quote(args: &[Value]) -> Result<Value, EvalError> {",
  "\nfn expand_quasiquote",
  <<~'RUST'.chomp
    fn eval_quote(args: &[Value]) -> Result<Value, EvalError> {
        args.first()
            .cloned()
            .ok_or_else(|| EvalError::InvalidForm("quote requires an argument".into()))
    }

    fn eval_quasiquote(args: &[Value], kernel: &mut Kernel) -> Result<Value, EvalError> {
        expand_quasiquote(
            args.first().ok_or_else(|| {
                EvalError::InvalidForm("quasiquote requires an argument".into())
            })?,
            kernel,
        )
    }
  RUST
)

replace(
  "src/vm/eval.rs",
  <<~'OLD',
                        Value::List(v) => result.extend(v),
                        Value::Vector(v) => result.extend(v),
OLD
  <<~'NEW'
                        Value::List(values) | Value::Vector(values) => result.extend(values),
NEW
)

replace(
  "src/vm/eval.rs",
  <<~'OLD',
            if !name.contains('/') {
                kernel.env.define(&format!("user/{}", name), m)?;
            } else {
                kernel.env.define(&name, m)?;
            }
OLD
  <<~'NEW'
            kernel.env.define(&qualify_user_name(&name), m)?;
NEW
)

replace(
  "src/vm/eval.rs",
  <<~'OLD',
    let qualified_family = if family_name.contains('/') {
        family_name.clone()
    } else {
        format!("user/{}", family_name)
    };
OLD
  <<~'NEW'
    let qualified_family = qualify_user_name(&family_name);
NEW
)

# Reduce Value boilerplate while retaining the same wire format and equality/hash rules.
replace_between(
  "src/vm/value.rs",
  "impl<'de> Deserialize<'de> for Arity {",
  "impl fmt::Display for Arity",
  <<~'RUST'
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredArity {
        Exact(u32),
        Named(String),
    }

    impl<'de> Deserialize<'de> for Arity {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            match StoredArity::deserialize(deserializer)? {
                StoredArity::Exact(u32::MAX) => Ok(Self::Variadic),
                StoredArity::Exact(count) => Ok(Self::Exact(count)),
                StoredArity::Named(name) if name == "variadic" => Ok(Self::Variadic),
                StoredArity::Named(_) => Err(serde::de::Error::custom("invalid arity")),
            }
        }
    }

  RUST
)

replace(
  "src/vm/value.rs",
  "impl fmt::Display for Value {",
  <<~'RUST'.chomp
    fn write_values(
        formatter: &mut fmt::Formatter<'_>,
        open: &str,
        values: &[Value],
        close: &str,
    ) -> fmt::Result {
        formatter.write_str(open)?;
        if let Some((first, rest)) = values.split_first() {
            write!(formatter, "{first}")?;
            for value in rest {
                write!(formatter, " {value}")?;
            }
        }
        formatter.write_str(close)
    }

    impl fmt::Display for Value {
  RUST
)

replace(
  "src/vm/value.rs",
  <<~'OLD',
            Value::List(items) => {
                write!(f, "(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Vector(items) => {
                write!(f, "#(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
OLD
  <<~'NEW'
            Value::List(items) => write_values(f, "(", items, ")"),
            Value::Vector(items) => write_values(f, "#(", items, ")"),
NEW
)

replace_between(
  "src/vm/value.rs",
  "impl std::hash::Hash for Value {",
  "pub(crate) fn collect_captured_environments",
  <<~'RUST'
    impl std::hash::Hash for Value {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            std::mem::discriminant(self).hash(state);
            match self {
                Value::Nil => {}
                Value::Bool(value) => value.hash(state),
                Value::Int(value) => value.hash(state),
                Value::Float(value) => value.to_bits().hash(state),
                Value::String(value) | Value::Symbol(value) | Value::Keyword(value) => {
                    value.hash(state)
                }
                Value::List(values) | Value::Vector(values) => values.hash(state),
                Value::Map(map) => {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::Hasher;
                    let mut entries: Vec<_> = map
                        .iter()
                        .map(|(key, value)| {
                            let mut entry = DefaultHasher::new();
                            key.hash(&mut entry);
                            value.hash(&mut entry);
                            entry.finish()
                        })
                        .collect();
                    entries.sort_unstable();
                    entries.hash(state);
                }
                Value::Function(function) => function.hash(state),
                Value::Macro(macro_) => macro_.hash(state),
                Value::Tagged {
                    family,
                    variant,
                    fields,
                } => (family, variant, fields).hash(state),
            }
        }
    }

  RUST
)

replace(
  "src/vm/value.rs",
  "impl Value {",
  <<~'RUST'.chomp
    macro_rules! value_ref {
        ($name:ident, $variant:ident, $type:ty) => {
            pub fn $name(&self) -> Option<&$type> {
                match self {
                    Self::$variant(value) => Some(value),
                    _ => None,
                }
            }
        };
    }

    impl Value {
  RUST
)

replace_between(
  "src/vm/value.rs",
  "    pub fn as_str(&self) -> Option<&str> {",
  "    pub fn is_truthy",
  <<~'RUST'
        value_ref!(as_str, String, str);
        value_ref!(as_symbol, Symbol, str);
        value_ref!(as_list, List, [Value]);
        value_ref!(as_vector, Vector, [Value]);
        value_ref!(as_map, Map, IndexMap<Value, Value>);

  RUST
)

# Collapse repetitive native predicates.
replace_between(
  "src/vm/primitives.rs",
  "        exact_native!(self, \"kernel/nil?\"",
  "        exact_native!(self, \"string-append\"",
  <<~'RUST'
        macro_rules! predicate {
            ($name:literal, $pattern:pat) => {
                exact_native!(self, $name, |_kernel, [value]| Ok(Value::Bool(matches!(value, $pattern))));
            };
        }
        predicate!("kernel/nil?", Value::Nil);
        predicate!("kernel/number?", Value::Int(_) | Value::Float(_));
        predicate!("kernel/symbol?", Value::Symbol(_));
        predicate!("kernel/string?", Value::String(_));
        exact_native!(self, "kernel/list?", |_kernel, [value]| Ok(Value::Bool(value.is_list())));
        predicate!("kernel/function?", Value::Function(_));
        predicate!("kernel/keyword?", Value::Keyword(_));
  RUST
)

# Simplify namespace fallback lookup.
replace_between(
  "src/vm/env.rs",
  "    pub fn lookup(&self, symbol: &str) -> Option<&Value> {",
  "\n    pub fn define",
  <<~'RUST'.chomp
        pub fn lookup(&self, symbol: &str) -> Option<&Value> {
            if symbol.contains('/') {
                let (namespace, name) = qualified_parts(symbol).ok()?;
                return self.namespaces.get(namespace)?.get(name);
            }
            self.lexical
                .find_cell(self.current_environment, symbol)
                .and_then(|cell| self.lexical.cells.get(&cell))
                .or_else(|| {
                    ["user", "kernel"]
                        .into_iter()
                        .find_map(|namespace| self.namespaces.get(namespace)?.get(symbol))
                })
        }
  RUST
)

# Keep UTF-8 truncation semantics with one boundary search.
replace_between(
  "src/scheduler.rs",
  "fn truncate(value: &str, max: usize) -> String {",
  "\n}",
  <<~'RUST'.chomp
    fn truncate(value: &str, max: usize) -> String {
        if value.len() <= max {
            return value.into();
        }
        const ELLIPSIS: &str = "…";
        let suffix = if max >= ELLIPSIS.len() { ELLIPSIS } else { "" };
        let limit = max.saturating_sub(suffix.len()).min(value.len());
        let end = (0..=limit)
            .rfind(|&index| value.is_char_boundary(index))
            .unwrap_or_default();
        format!("{}{suffix}", &value[..end])
    }
  RUST
)
