use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;

/// Stable ID for a Value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ValueId(pub u64);

/// A Lisp function, either native (Rust) or interpreted.
#[derive(Debug, Clone)]
pub enum Function {
    Native {
        name: String,
        arity: u32,
        func: fn(Vec<Value>) -> Result<Value, String>,
    },
    Interpreted {
        params: Vec<String>,
        body: Vec<Value>,
        env_serialized: String,
    },
    Constructor {
        family: String,
        variant: String,
        arity: u32,
    },
}

impl Serialize for Function {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Function::Native { name, arity, .. } => {
                let m = serde_json::json!({
                    "type": "native",
                    "name": name,
                    "arity": arity,
                });
                serde_json::Value::serialize(&m, serializer)
            }
            Function::Interpreted { params, body, env_serialized } => {
                let m = serde_json::json!({
                    "type": "interpreted",
                    "params": params,
                    "body": body,
                    "env_serialized": env_serialized,
                });
                serde_json::Value::serialize(&m, serializer)
            }
            Function::Constructor { family, variant, arity } => {
                let m = serde_json::json!({
                    "type": "constructor",
                    "family": family,
                    "variant": variant,
                    "arity": arity,
                });
                serde_json::Value::serialize(&m, serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Function {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let m = serde_json::Value::deserialize(deserializer)?;
        let type_ = m["type"].as_str().unwrap_or("");
        match type_ {
            "native" => {
                // Native functions can't be deserialized from a snapshot;
                // they must be re-registered by the kernel on recovery.
                // We create a stub that returns an error if called.
                let name = m["name"].as_str().unwrap_or("unknown").to_string();
                let arity = m["arity"].as_u64().unwrap_or(0) as u32;
                Ok(Function::Native {
                    name,
                    arity,
                    func: |_| Err("native function not available after deserialization; re-register it".into()),
                })
            }
            "interpreted" => {
                let params: Vec<String> = m["params"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let body: Vec<Value> = m["body"].as_array()
                    .map(|a| a.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
                    .unwrap_or_default();
                let env_serialized = m["env_serialized"].as_str().unwrap_or("{}").to_string();
                Ok(Function::Interpreted { params, body, env_serialized })
            }
            "constructor" => {
                let family = m["family"].as_str().unwrap_or("?").to_string();
                let variant = m["variant"].as_str().unwrap_or("?").to_string();
                let arity = m["arity"].as_u64().unwrap_or(0) as u32;
                Ok(Function::Constructor { family, variant, arity })
            }
            _ => Err(serde::de::Error::custom("unknown function type")),
        }
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Function::Native { name, arity, .. } => write!(f, "#<native-fn {} arity {}>", name, arity),
            Function::Interpreted { params, .. } => write!(f, "#<fn ({})>", params.join(" ")),
            Function::Constructor { family, variant, .. } => write!(f, "#<constructor {}/{}>", family, variant),
        }
    }
}

/// A Lisp macro.
#[derive(Debug, Clone)]
pub enum Macro {
    Native {
        name: String,
        func: fn(Vec<Value>) -> Result<Value, String>,
    },
    SyntaxRules {
        literals: Vec<String>,
        rules: Vec<(Vec<Value>, Value)>,
        env_serialized: String,
    },
}

impl Serialize for Macro {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Macro::Native { name, .. } => {
                let m = serde_json::json!({
                    "type": "native",
                    "name": name,
                });
                serde_json::Value::serialize(&m, serializer)
            }
            Macro::SyntaxRules { literals, rules, env_serialized } => {
                let m = serde_json::json!({
                    "type": "syntax-rules",
                    "literals": literals,
                    "rules": rules,
                    "env_serialized": env_serialized,
                });
                serde_json::Value::serialize(&m, serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Macro {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let m = serde_json::Value::deserialize(deserializer)?;
        let type_ = m["type"].as_str().unwrap_or("");
        match type_ {
            "native" => {
                let name = m["name"].as_str().unwrap_or("unknown").to_string();
                Ok(Macro::Native {
                    name,
                    func: |_| Err("native macro not available after deserialization; re-register it".into()),
                })
            }
            "syntax-rules" => {
                let literals: Vec<String> = m["literals"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let rules_raw: Vec<serde_json::Value> = m["rules"].as_array()
                    .map(|a| a.iter().cloned().collect())
                    .unwrap_or_default();
                let mut rules = Vec::new();
                for rule in rules_raw {
                    if let (Some(pattern), Some(template)) = (
                        rule[0].as_array().map(|a| a.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect()),
                        serde_json::from_value(rule[1].clone()).ok(),
                    ) {
                        rules.push((pattern, template));
                    }
                }
                let env_serialized = m["env_serialized"].as_str().unwrap_or("{}").to_string();
                Ok(Macro::SyntaxRules { literals, rules, env_serialized })
            }
            _ => Err(serde::de::Error::custom("unknown macro type")),
        }
    }
}

/// Opaque kernel reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelRef {
    pub kind: String,
    pub id: String,
    pub metadata: HashMap<String, String>,
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
    Map(HashMap<Value, Value>),
    Function(Function),
    Macro(Macro),
    Tagged {
        family: String,
        variant: String,
        fields: Vec<Value>,
    },
    KernelRef(KernelRef),
    Opaque(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{}", if *b { "#t" } else { "#f" }),
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::String(s) => write!(f, "\"{}\"", s),
            Value::Symbol(s) => write!(f, "{}", s),
            Value::Keyword(k) => write!(f, ":{}", k),
            Value::List(items) => {
                write!(f, "(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 { write!(f, " ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Vector(items) => {
                write!(f, "#(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 { write!(f, " ")?; }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            }
            Value::Map(map) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in map.iter() {
                    if !first { write!(f, " ")?; }
                    write!(f, "{} {}", k, v)?;
                    first = false;
                }
                write!(f, "}}")
            }
            Value::Function(fun) => write!(f, "{}", fun),
            Value::Macro(m) => match m {
                Macro::Native { name, .. } => write!(f, "#<macro {}>", name),
                Macro::SyntaxRules { .. } => write!(f, "#<macro>"),
            },
            Value::Tagged { family, variant, .. } => write!(f, "({}/{} ...)", family, variant),
            Value::KernelRef(kr) => write!(f, "#<{} {}>", kr.kind, kr.id),
            Value::Opaque(s) => write!(f, "#<opaque {}>", s),
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
            _ => false,
        }
    }
}

impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Value::Nil => 0u8.hash(state),
            Value::Bool(b) => (1u8, b).hash(state),
            Value::Int(n) => (2u8, n).hash(state),
            Value::Float(n) => (3u8, n.to_bits()).hash(state),
            Value::String(s) => (4u8, s).hash(state),
            Value::Symbol(s) => (5u8, s).hash(state),
            Value::Keyword(k) => (6u8, k).hash(state),
            Value::List(items) => (7u8, items).hash(state),
            Value::Vector(items) => (8u8, items).hash(state),
            Value::Map(map) => {
                9u8.hash(state);
                for (k, v) in map {
                    k.hash(state);
                    v.hash(state);
                }
            }
            _ => 255u8.hash(state),
        }
    }
}

impl Value {
    pub fn symbol(s: &str) -> Self { Value::Symbol(s.to_string()) }
    pub fn keyword(s: &str) -> Self { Value::Keyword(s.to_string()) }
    pub fn string(s: &str) -> Self { Value::String(s.to_string()) }
    pub fn int(n: i64) -> Self { Value::Int(n) }
    pub fn list(items: Vec<Value>) -> Self { Value::List(items) }
    pub fn nil() -> Self { Value::Nil }
    pub fn bool(b: bool) -> Self { Value::Bool(b) }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn tagged(family: &str, variant: &str, fields: Vec<Value>) -> Self {
        Value::Tagged {
            family: family.to_string(),
            variant: variant.to_string(),
            fields,
        }
    }

    pub fn car(&self) -> Option<&Value> {
        match self {
            Value::List(items) => items.first(),
            _ => None,
        }
    }

    pub fn cdr(&self) -> Value {
        match self {
            Value::List(items) if items.len() >= 2 => Value::List(items[1..].to_vec()),
            Value::List(_) => Value::Nil,
            _ => Value::Nil,
        }
    }

    pub fn is_pair(&self) -> bool {
        matches!(self, Value::List(items) if items.len() >= 1)
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Value::List(_) | Value::Nil)
    }
}

/// The `lisp_fn!` macro — exported at crate root level.
#[macro_export]
macro_rules! lisp_fn {
    ($name:expr, $arity:expr, $func:expr) => {
        $crate::vm::value::Value::Function($crate::vm::value::Function::Native {
            name: $name.to_string(),
            arity: $arity,
            func: $func,
        })
    };
}
