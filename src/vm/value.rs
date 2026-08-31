use crate::vm::env::EnvironmentId;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

mod map_entries {
    use super::Value;
    use indexmap::IndexMap;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(map: &IndexMap<Value, Value>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<IndexMap<Value, Value>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<(Value, Value)>::deserialize(deserializer)
            .map(|entries| entries.into_iter().collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arity {
    Exact(u32),
    Variadic,
}

impl Serialize for Arity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Exact(count) => serializer.serialize_u32(*count),
            Self::Variadic => serializer.serialize_str("variadic"),
        }
    }
}

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

impl fmt::Display for Arity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(count) => count.fmt(formatter),
            Self::Variadic => formatter.write_str("variadic"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("native operation failed: {0}")]
    Failed(String),
}

impl From<String> for NativeError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for NativeError {
    fn from(message: &str) -> Self {
        Self::Failed(message.into())
    }
}

pub type NativeFn = fn(&mut crate::kernel::Kernel, Vec<Value>) -> Result<Value, NativeError>;

/// A Lisp function. Native implementations live in the kernel registry;
/// serializable values retain only their identity and arity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Function {
    Native {
        name: String,
        arity: Arity,
    },
    Interpreted {
        params: Vec<String>,
        body: Vec<Value>,
        env_id: EnvironmentId,
    },
    Constructor {
        family: String,
        variant: String,
        arity: usize,
    },
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Function::Native { name, arity } => {
                write!(f, "#<native-fn {} arity {}>", name, arity)
            }
            Function::Interpreted { params, .. } => write!(f, "#<fn ({})>", params.join(" ")),
            Function::Constructor {
                family, variant, ..
            } => write!(f, "#<constructor {}/{}>", family, variant),
        }
    }
}

/// A declarative Lisp macro.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Macro {
    #[serde(rename = "syntax-rules")]
    SyntaxRules {
        literals: Vec<String>,
        rules: Vec<(Vec<Value>, Value)>,
    },
}

impl fmt::Display for Macro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#<macro>")
    }
}

/// The core Lisp value type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Symbol(String),
    Keyword(String),
    List(Vec<Value>),
    Vector(Vec<Value>),
    Map(#[serde(with = "map_entries")] IndexMap<Value, Value>),
    Function(Function),
    Macro(Macro),
    Tagged {
        family: String,
        variant: String,
        fields: Vec<Value>,
    },
}

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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", if *b { "#t" } else { "#f" }),
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::String(s) => write!(f, "{:?}", s),
            Value::Symbol(s) => write!(f, "{}", s),
            Value::Keyword(k) => write!(f, ":{}", k),
            Value::List(items) => write_values(f, "(", items, ")"),
            Value::Vector(items) => write_values(f, "#(", items, ")"),
            Value::Map(map) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in map.iter() {
                    if !first {
                        write!(f, " ")?;
                    }
                    write!(f, "{} {}", k, v)?;
                    first = false;
                }
                write!(f, "}}")
            }
            Value::Function(fun) => write!(f, "{}", fun),
            Value::Macro(m) => write!(f, "{}", m),
            Value::Tagged {
                family,
                variant,
                fields,
            } => {
                write!(f, "({}/{}", family, variant)?;
                for field in fields {
                    write!(f, " {}", field)?;
                }
                write!(f, ")")
            }
        }
    }
}

impl Eq for Value {}
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Symbol(a), Value::Symbol(b)) => a == b,
            (Value::Keyword(a), Value::Keyword(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Vector(a), Value::Vector(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => a == b,
            (Value::Macro(a), Value::Macro(b)) => a == b,
            (
                Value::Tagged {
                    family: fa,
                    variant: va,
                    fields: fia,
                },
                Value::Tagged {
                    family: fb,
                    variant: vb,
                    fields: fib,
                },
            ) => fa == fb && va == vb && fia == fib,
            _ => false,
        }
    }
}

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

pub(crate) fn collect_captured_environments<'a>(
    roots: impl IntoIterator<Item = &'a Value>,
    captured: &mut HashSet<EnvironmentId>,
) {
    let mut pending: Vec<_> = roots.into_iter().collect();
    while let Some(value) = pending.pop() {
        match value {
            Value::Function(Function::Interpreted { env_id, body, .. }) => {
                captured.insert(*env_id);
                pending.extend(body);
            }
            Value::List(values) | Value::Vector(values) => pending.extend(values),
            Value::Map(values) => {
                for (key, value) in values {
                    pending.extend([key, value]);
                }
            }
            Value::Macro(Macro::SyntaxRules { rules, .. }) => {
                for (pattern, template) in rules {
                    pending.extend(pattern);
                    pending.push(template);
                }
            }
            Value::Tagged { fields, .. } => pending.extend(fields),
            _ => {}
        }
    }
}

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
    pub fn symbol(s: &str) -> Self {
        Value::Symbol(s.to_string())
    }
    pub fn keyword(s: &str) -> Self {
        Value::Keyword(s.to_string())
    }
    pub fn string(s: &str) -> Self {
        Value::String(s.to_string())
    }
    pub fn int(n: i64) -> Self {
        Value::Int(n)
    }
    pub fn list(items: Vec<Value>) -> Self {
        Value::List(items)
    }

    pub(crate) fn coerce_text(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            other => other.to_string(),
        }
    }

    fn required<T>(
        value: Option<T>,
        function: &str,
        position: usize,
        expected: &str,
    ) -> Result<T, NativeError> {
        value.ok_or_else(|| {
            NativeError::InvalidArgument(format!(
                "{}: argument {} must be {}",
                function, position, expected
            ))
        })
    }

    pub fn require_int(&self, function: &str, position: usize) -> Result<i64, NativeError> {
        Self::required(self.as_int(), function, position, "an integer")
    }

    pub fn require_number(&self, function: &str, position: usize) -> Result<f64, NativeError> {
        Self::required(self.as_number(), function, position, "a number")
    }

    pub fn require_string<'a>(
        &'a self,
        function: &str,
        position: usize,
    ) -> Result<&'a str, NativeError> {
        Self::required(self.as_str(), function, position, "a string")
    }

    pub fn require_nonnegative_usize(
        &self,
        function: &str,
        position: usize,
    ) -> Result<usize, NativeError> {
        usize::try_from(self.require_int(function, position)?).map_err(|_| {
            NativeError::InvalidArgument(format!(
                "{}: argument {} must be a non-negative index",
                function, position
            ))
        })
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Int(value) => Some(*value as f64),
            Value::Float(value) => Some(*value),
            _ => None,
        }
    }

    value_ref!(as_str, String, str);
    value_ref!(as_symbol, Symbol, str);
    value_ref!(as_list, List, [Value]);
    value_ref!(as_vector, Vector, [Value]);
    value_ref!(as_map, Map, IndexMap<Value, Value>);

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Value::List(_) | Value::Nil)
    }
}
