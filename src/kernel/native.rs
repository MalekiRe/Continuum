use crate::ids::{JobId, MessageId};
use crate::kernel::{HookPhase, HookSpec, Kernel, VmTrap};
use crate::vm::env::BindingOrigin;
use crate::vm::value::{Arity, Function, NativeError, Value};

#[derive(Debug, thiserror::Error)]
pub enum BuiltinError {
    #[error(transparent)]
    Native(#[from] NativeError),
    #[error("external operation")]
    Trap(VmTrap),
}

impl From<&str> for BuiltinError {
    fn from(value: &str) -> Self {
        NativeError::from(value).into()
    }
}

impl From<String> for BuiltinError {
    fn from(value: String) -> Self {
        NativeError::from(value).into()
    }
}

macro_rules! count_args {
    () => { 0u32 };
    ($head:ident $(, $tail:ident)*) => { 1u32 + count_args!($($tail),*) };
}

macro_rules! builtin_arity {
    (exact [$($argument:ident),*]) => { Arity::Exact(count_args!($($argument),*)) };
    (variadic $arguments:ident) => { Arity::Variadic };
}

macro_rules! call_builtin {
    ($name:literal, exact [$($argument:ident),*], $context:ident, $kernel:ident, $arguments:ident, $body:block) => {{
        let $context = $kernel;
        match $arguments.as_slice() {
            [$($argument),*] => $body,
            _ => Err(NativeError::InvalidArgument(format!(
                "{}: expected {} arguments",
                $name,
                count_args!($($argument),*)
            )).into()),
        }
    }};
    ($name:literal, variadic $values:ident, $context:ident, $kernel:ident, $arguments:ident, $body:block) => {{
        let $context = $kernel;
        let $values = $arguments;
        $body
    }};
}

macro_rules! builtins {
    ($(
        $variant:ident => $name:literal, $kind:ident $arguments:tt, |$context:ident| $body:block;
    )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Builtin {
            $($variant),*
        }

        impl Builtin {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),*
                }
            }

            pub const fn arity(self) -> Arity {
                match self {
                    $(Self::$variant => builtin_arity!($kind $arguments)),*
                }
            }

            pub fn call(
                self,
                kernel: &mut Kernel,
                arguments: Vec<Value>,
            ) -> Result<Value, BuiltinError> {
                match self {
                    $(Self::$variant => call_builtin!(
                        $name,
                        $kind $arguments,
                        $context,
                        kernel,
                        arguments,
                        $body
                    )),*
                }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

fn numbers(left: &Value, right: &Value, name: &str) -> Result<(f64, f64), NativeError> {
    Ok((
        left.require_number(name, 1)?,
        right.require_number(name, 2)?,
    ))
}

fn arithmetic(
    left: &Value,
    right: &Value,
    name: &str,
    integers: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<Value, NativeError> {
    if let (Value::Int(left), Value::Int(right)) = (left, right) {
        return integers(*left, *right)
            .map(Value::Int)
            .ok_or_else(|| NativeError::InvalidArgument(format!("{name}: integer overflow")));
    }
    let (left, right) = numbers(left, right, name)?;
    let result = floats(left, right);
    if result.is_finite() {
        Ok(Value::Float(result))
    } else {
        Err(NativeError::InvalidArgument(format!(
            "{name}: non-finite result"
        )))
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::List(_) => "list",
        Value::Vector(_) => "vector",
        Value::Map(_) => "map",
        Value::Function(_) => "function",
        Value::Macro(_) => "macro",
        Value::Tagged { .. } => "tagged",
    }
}

fn pair_payload<'a>(
    value: &'a Value,
    operation: &str,
) -> Result<(&'a Value, &'a Value), NativeError> {
    let values = value.as_vector().ok_or_else(|| {
        NativeError::InvalidArgument(format!("{operation}: expected vector payload"))
    })?;
    match values {
        [left, right] => Ok((left, right)),
        _ => Err(NativeError::InvalidArgument(format!(
            "{operation}: expected two payload values"
        ))),
    }
}

builtins! {
    Add => "kernel/+", exact [left, right], |_kernel| {
        Ok(arithmetic(left, right, "+", i64::checked_add, |a, b| a + b)?)
    };
    Subtract => "kernel/-", exact [left, right], |_kernel| {
        Ok(arithmetic(left, right, "-", i64::checked_sub, |a, b| a - b)?)
    };
    Multiply => "kernel/*", exact [left, right], |_kernel| {
        Ok(arithmetic(left, right, "*", i64::checked_mul, |a, b| a * b)?)
    };
    Divide => "kernel//", exact [left, right], |_kernel| {
        let (left, right) = numbers(left, right, "/")?;
        if right == 0.0 {
            Err("/: division by zero".into())
        } else {
let result = left / right;
if result.is_finite() {
    Ok(Value::Float(result))
} else {
    Err("/: non-finite result".into())
}
        }
    };
    Equal => "kernel/=", exact [left, right], |_kernel| {
        Ok(Value::Bool(left == right))
    };
    Less => "kernel/<", exact [left, right], |_kernel| {
        let (left, right) = numbers(left, right, "<")?;
        Ok(Value::Bool(left < right))
    };
    Greater => "kernel/>", exact [left, right], |_kernel| {
        let (left, right) = numbers(left, right, ">")?;
        Ok(Value::Bool(left > right))
    };
    Cons => "kernel/cons", exact [car, cdr], |_kernel| {
        match cdr {
            Value::List(items) => Ok(Value::List(
                std::iter::once((*car).clone())
                    .chain(items.iter().cloned())
                    .collect(),
            )),
            Value::Nil => Ok(Value::List(vec![(*car).clone()])),
            _ => Err("cons: second argument must be a list".into()),
        }
    };
    Car => "kernel/car", exact [value], |_kernel| {
        match value {
            Value::List(items) => items
                .first()
                .cloned()
                .ok_or_else(|| NativeError::from("car: empty list"))
                .map_err(Into::into),
            _ => Err("car: expected list".into()),
        }
    };
    Cdr => "kernel/cdr", exact [value], |_kernel| {
        match value {
            Value::List(items) if items.len() >= 2 => Ok(Value::List(items[1..].to_vec())),
            Value::List(_) => Ok(Value::Nil),
            _ => Err("cdr: expected list".into()),
        }
    };
    List => "kernel/list", variadic values, |_kernel| {
        Ok(Value::List(values))
    };
    Display => "kernel/display", exact [value], |kernel| {
        kernel.write_output(&value.to_string());
        Ok((*value).clone())
    };
    Println => "kernel/println", exact [value], |kernel| {
        kernel.write_output(&format!("{value}\n"));
        Ok((*value).clone())
    };
    TypeOf => "kernel/type-of", exact [value], |_kernel| {
        Ok(Value::keyword(type_name(value)))
    };
    StringAppend => "string-append", exact [left, right], |_kernel| {
        Ok(Value::string(&format!("{}{}", left.coerce_text(), right.coerce_text())))
    };
    Nth => "nth", exact [index, value], |_kernel| {
        let index = index.require_nonnegative_usize("nth", 1)?;
        let items = value
            .as_list()
            .ok_or_else(|| NativeError::from("nth: argument 2 must be a list"))?;
        items.get(index).cloned().ok_or_else(|| NativeError::InvalidArgument(format!(
            "nth: index {index} out of bounds (len {})",
            items.len()
        ))).map_err(Into::into)
    };
    Length => "length", exact [value], |_kernel| {
        let length = match value {
            Value::List(items) => items.len(),
            Value::String(value) => value.chars().count(),
            _ => return Err("length: expected list or string".into()),
        };
        i64::try_from(length)
            .map(Value::Int)
            .map_err(|_| NativeError::from("length: value is too large"))
            .map_err(Into::into)
    };
    MapGet => "map/get", exact [value, key], |_kernel| {
        let map = value
            .as_map()
            .ok_or_else(|| NativeError::from("map/get: argument 1 must be a map"))?;
        Ok(map.get(key).cloned().unwrap_or(Value::Nil))
    };
    VectorGet => "vector/get", exact [value, index], |_kernel| {
        let vector = value
            .as_vector()
            .ok_or_else(|| NativeError::from("vector/get: argument 1 must be a vector"))?;
        let index = index.require_nonnegative_usize("vector/get", 2)?;
        vector.get(index).cloned().ok_or_else(|| NativeError::InvalidArgument(format!(
            "vector/get: index {index} out of bounds (len {})",
            vector.len()
        ))).map_err(Into::into)
    };
    Append => "append", variadic values, |_kernel| {
        let mut result = Vec::new();
        for value in values {
            match value {
                Value::List(items) => result.extend(items),
                value => result.push(value),
            }
        }
        Ok(Value::List(result))
    };
    Error => "kernel/error", exact [value], |_kernel| {
        Err(NativeError::Failed(value.to_string()).into())
    };
    StringSearch => "string-search", exact [needle, haystack], |_kernel| {
        let needle = needle.require_string("string-search", 1)?;
        let haystack = haystack.require_string("string-search", 2)?;
        match haystack.find(needle) {
            Some(index) => i64::try_from(haystack[..index].chars().count())
                .map(Value::Int)
                .map_err(|_| NativeError::from("string-search: index is too large"))
                .map_err(Into::into),
            None => Ok(Value::Bool(false)),
        }
    };
    Substring => "substring", exact [value, start, end], |_kernel| {
        let value = value.require_string("substring", 1)?;
        let start = start.require_nonnegative_usize("substring", 2)?;
        let end = end.require_nonnegative_usize("substring", 3)?;
        let count = value.chars().count();
        if start > end || end > count {
            return Err(NativeError::InvalidArgument(format!(
                "substring: invalid range {start}..{end} for length {count}"
            )).into());
        }
        let offset = |index| value.char_indices().nth(index).map_or(value.len(), |(index, _)| index);
        Ok(Value::string(&value[offset(start)..offset(end)]))
    };
    SystemVersion => "system/version", exact [], |_kernel| {
        Ok(Value::string(concat!("persistent-lisp-harness/", env!("CARGO_PKG_VERSION"))))
    };
    SystemClock => "system/clock", exact [], |_kernel| {
        Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
    };
    TranscriptRecent => "transcript/recent", exact [count], |kernel| {
        let count = count.require_nonnegative_usize("transcript/recent", 1)?;
        let Some(frame) = kernel.frames.last() else {
            return Ok(Value::Nil);
        };
        let start = frame.state.transcript.len().saturating_sub(count);
        Ok(Value::List(frame.state.transcript[start..].iter().map(|entry| Value::list(vec![
            Value::string(&entry.timestamp),
            Value::string(&entry.source),
            Value::string(&entry.result),
        ])).collect()))
    };
    SourceGet => "source/get", exact [name], |kernel| {
        let name = name.as_symbol().or_else(|| name.as_str()).ok_or_else(|| {
            NativeError::InvalidArgument("source/get: expected symbol or string".into())
        })?;
        Ok(kernel.env.source(&super::qualify_user_name(name)).map(Value::string).unwrap_or(Value::Nil))
    };
    SourceList => "source/list", exact [], |kernel| {
        let mut names = Vec::new();
        for (namespace, values) in kernel.env.namespaces.iter() {
            names.extend(values.sources.keys().map(|name| Value::symbol(&format!("{namespace}/{name}"))));
        }
        names.sort_by_key(ToString::to_string);
        Ok(Value::List(names))
    };
HookAdd => "hook/add", exact [id, target, phase, function], |kernel| {
    let text = |value: &Value, operation: &str| {
        value
            .as_symbol()
            .or_else(|| value.as_str())
            .map(str::to_owned)
            .ok_or_else(|| NativeError::InvalidArgument(format!(
                "{operation}: expected symbol or string"
            )))
    };
    let phase = match phase.as_keyword() {
        Some("before") => HookPhase::Before,
        Some("after") => HookPhase::After,
        _ => return Err("hook/add: phase must be :before or :after".into()),
    };
    kernel.add_hook(HookSpec {
        id: text(id, "hook/add id")?,
        target: text(target, "hook/add target")?,
        phase,
        function: text(function, "hook/add function")?,
    });
    Ok(Value::keyword("ok"))
};
HookRemove => "hook/remove", exact [id], |kernel| {
    let id = id.as_symbol().or_else(|| id.as_str()).ok_or(
        "hook/remove: expected symbol or string",
    )?;
    Ok(Value::Bool(kernel.remove_hook(id)))
};
HookList => "hook/list", exact [target], |kernel| {
    let target = match target {
        Value::Nil => None,
        value => Some(value.as_symbol().or_else(|| value.as_str()).ok_or(
            "hook/list: target must be nil, symbol, or string",
        )?),
    };
    Ok(Value::List(kernel.list_hooks(target).into_iter().map(|hook| {
        Value::list(vec![
            Value::string(&hook.id),
            Value::string(&hook.target),
            Value::keyword(match hook.phase {
                HookPhase::Before => "before",
                HookPhase::After => "after",
            }),
            Value::string(&hook.function),
        ])
    }).collect()))
};
HookRun => "hook/run", exact [target, value], |kernel| {
    let target = target.as_symbol().or_else(|| target.as_str()).ok_or(
        "hook/run: expected symbol or string target",
    )?;
    crate::vm::eval::run_explicit_hooks(target, (*value).clone(), kernel)
        .map_err(|error| NativeError::Failed(error.to_string()).into())
};
ContextInject => "context/inject", exact [key, lifetime, text], |kernel| {
    let key = key.require_string("context/inject", 1)?.to_owned();
    let lifetime = match lifetime.as_keyword() {
        Some("next") => crate::kernel::ContextLifetime::Next,
        Some("frame") => crate::kernel::ContextLifetime::Frame,
        _ => return Err("context/inject: lifetime must be :next or :frame".into()),
    };
    let text = text.require_string("context/inject", 3)?.to_owned();
    if !kernel.inject_context(key, lifetime, text) {
        return Err("context/inject: no active frame".into());
    }
    Ok(Value::keyword("ok"))
};
ContextRemove => "context/remove", exact [key], |kernel| {
    let key = key.require_string("context/remove", 1)?;
    Ok(Value::Bool(kernel.remove_context(key)))
};
ContextList => "context/list", exact [], |kernel| {
    let Some(frame) = kernel.frames.last() else { return Ok(Value::Nil); };
    Ok(Value::List(frame.state.context_entries.iter().map(|entry| Value::list(vec![
        Value::string(&entry.key),
        Value::keyword(match entry.lifetime {
            crate::kernel::ContextLifetime::Next => "next",
            crate::kernel::ContextLifetime::Frame => "frame",
        }),
        Value::string(&entry.text),
    ])).collect()))
};
MemoryRemember => "memory/remember", exact [key, value], |kernel| {
    let id = kernel.remember(key.coerce_text(), value.coerce_text());
    Ok(Value::string(id.as_str()))
};
MemoryNote => "memory/note", exact [value], |kernel| {
    let id = kernel.note_memory(value.coerce_text());
    Ok(Value::string(id.as_str()))
};
MemoryForget => "memory/forget", exact [selector], |kernel| {
    Ok(Value::Bool(kernel.forget_memory(&selector.coerce_text())))
};
MemoryView => "memory/view", exact [], |kernel| {
    Ok(Value::List(kernel.visible_memory().into_iter().map(|entry| Value::list(vec![
        Value::string(entry.id.as_str()),
        Value::string(&entry.key),
        Value::string(&entry.value),
        Value::string(&entry.updated_at),
    ])).collect()))
};
MemoryRecall => "memory/recall", exact [query], |kernel| {
    let query = query.require_string("memory/recall", 1)?.to_lowercase();
    Ok(Value::List(kernel.visible_memory().into_iter().filter(|entry| {
        entry.key.to_lowercase().contains(&query)
            || entry.value.to_lowercase().contains(&query)
    }).map(|entry| Value::list(vec![
        Value::string(entry.id.as_str()),
        Value::string(&entry.key),
        Value::string(&entry.value),
    ])).collect()))
};
MemoryList => "memory/list", exact [], |kernel| {
    Ok(Value::List(kernel.visible_memory().into_iter().map(|entry| Value::list(vec![
        Value::string(entry.id.as_str()),
        Value::string(&entry.key),
        Value::string(&entry.value),
        Value::string(&entry.updated_at),
    ])).collect()))
};
HistoryRead => "history/read", exact [id], |kernel| {
    let id = id.require_int("history/read", 1)?;
    let id = u64::try_from(id).map_err(|_| NativeError::from("history/read: ID must be non-negative"))?;
    let Some(event) = kernel.history.iter().find(|event| event.id == id) else {
        return Ok(Value::Nil);
    };
    Ok(Value::list(vec![
        Value::int(event.id as i64),
        Value::string(&event.timestamp),
        event.frame_id.as_ref().map(|id| Value::string(id.as_str())).unwrap_or(Value::Nil),
        Value::string(&event.kind),
        Value::string(&event.text),
    ]))
};
HistoryFind => "history/find", exact [query], |kernel| {
    let query = query.require_string("history/find", 1)?.to_lowercase();
    Ok(Value::List(kernel.history.iter().filter(|event| {
        event.kind.to_lowercase().contains(&query)
            || event.text.to_lowercase().contains(&query)
    }).map(|event| Value::list(vec![
        Value::int(event.id as i64),
        Value::string(&event.timestamp),
        Value::string(&event.kind),
        Value::string(&event.text),
    ])).collect()))
};
HistorySpine => "history/spine", exact [], |kernel| {
    let Some(frame) = kernel.frames.last() else { return Ok(Value::Nil); };
    Ok(Value::List(frame.state.spine.iter().map(|node| Value::list(vec![
        Value::int(node.level as i64),
        Value::int(node.first_event as i64),
        Value::int(node.last_event as i64),
        Value::string(&node.summary),
    ])).collect()))
};
    InspectNamespaces => "inspect/namespaces", exact [], |kernel| {
        Ok(Value::List(kernel.env.namespace_names().into_iter().map(|name| {
            let count = kernel.env.namespaces.get(&name).map_or(0, |namespace| namespace.bindings.len());
            Value::list(vec![Value::symbol(&name), Value::int(count as i64)])
        }).collect()))
    };
    InspectBindings => "inspect/bindings", exact [namespace], |kernel| {
        let namespace = namespace.as_symbol().ok_or("inspect/bindings: expected symbol")?;
        Ok(Value::List(kernel.inspect_namespace(namespace).unwrap_or_default().iter().map(|name| Value::symbol(name)).collect()))
    };
    InspectHistory => "inspect/history", exact [name], |kernel| {
        let name = name.as_symbol().ok_or("inspect/history: expected symbol")?;
        let qualified = super::qualify_user_name(name);
        let (namespace, binding) = qualified.split_once('/').expect("qualified name");
        let records = kernel
            .env
            .namespaces
            .get(namespace)
            .and_then(|namespace| namespace.history(binding));
        let Some(records) = records else {
            return Ok(Value::string("no history"));
        };
        Ok(Value::List(records.iter().map(|record| {
            let detail = match &record.value.change {
                crate::vm::env::BindingChange::Defined { source, preview } => {
                    format!("defined {preview} from {}", source.as_deref().unwrap_or("<unknown>"))
                }
                crate::vm::env::BindingChange::Undefined => "undefined".into(),
            };
            Value::list(vec![
                Value::string(&record.at.to_rfc3339()),
                Value::int(record.value.version as i64),
                Value::string(&detail),
            ])
        }).collect()))
    };
    Wake => "wake", exact [duration, action], |kernel| {
        let duration = duration.require_int("wake", 1)?;
        if duration < 0 {
            return Err("wake: duration must be non-negative".into());
        }
        let wake_at = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::milliseconds(duration))
            .ok_or("wake: duration is out of range")?;
        let frame_id = kernel.frames.last().map(|frame| frame.id.clone()).ok_or("wake: no active frame")?;
        kernel.schedule_wake_at(frame_id, wake_at, action.to_string()).map_err(|error| error.to_string())?;
        Ok(Value::keyword("scheduled"))
    };
    InspectFind => "inspect/find", exact [query], |kernel| {
        let query = query.require_string("inspect/find", 1)?;
        Ok(Value::List(kernel.find_bindings(query).iter().map(|name| Value::string(name)).collect()))
    };
    Trap => "kernel/trap", exact [operation, payload], |kernel| {
        let operation = operation.as_keyword().ok_or("kernel/trap: operation must be a keyword")?;
        let trap = match operation {
            "bash" => VmTrap::RunBash {
                command: payload.require_string("bash", 1)?.into(),
            },
"bash-start" => VmTrap::StartBash {
    command: payload.require_string("bash/start", 1)?.into(),
},
"bash-status" => VmTrap::BashStatus {
    id: JobId::new(payload.require_string("bash/status", 1)?),
},
"bash-cancel" => VmTrap::BashCancel {
    id: JobId::new(payload.require_string("bash/cancel", 1)?),
},
"bash-collect" => VmTrap::BashCollect {
    id: JobId::new(payload.require_string("bash/collect", 1)?),
},
"bash-list" if matches!(payload, Value::Nil) => VmTrap::BashList,
"bash-list" => return Err("bash/list: payload must be nil".into()),
"model-call" => VmTrap::CallModel {
                prompt: payload.require_string("model/call", 1)?.into(),
            },
            "agent-call" => {
                let (name, request) = pair_payload(payload, "agent/call")?;
                VmTrap::CallAgent {
                    name: name.require_string("agent/call", 1)?.into(),
                    request: request.require_string("agent/call", 2)?.into(),
                }
            }
            "agent-return" => {
                if kernel.frames.len() <= 1 {
                    return Err("agent/return: root frame has no parent".into());
                }
                VmTrap::ReturnAgent {
                    value: payload.require_string("agent/return", 1)?.into(),
                }
            }
            "message-reply" => {
                let (id, text) = pair_payload(payload, "message/reply")?;
                let id = MessageId::new(id.require_string("message/reply", 1)?);
                if !kernel.has_pending_message(&id) {
                    return Err(format!("message/reply: unknown or completed message '{id}'").into());
                }
                VmTrap::Reply {
                    message_id: id,
                    text: text.require_string("message/reply", 2)?.into(),
                }
            }
            "human-wait" if matches!(payload, Value::Nil) => VmTrap::AwaitHuman,
            "human-wait" => return Err("human/wait: payload must be nil".into()),
            _ => return Err(format!("kernel/trap: unknown operation :{operation}").into()),
        };
        Err(BuiltinError::Trap(trap))
    };
}

pub(crate) fn install(kernel: &mut Kernel) {
    for builtin in Builtin::ALL {
        let dispatch = builtin.name();
        let binding = if dispatch.contains('/') {
            dispatch.to_string()
        } else {
            format!("kernel/{dispatch}")
        };
        kernel.env.force_define(
            &binding,
            Value::Function(Function::Native {
                name: dispatch.into(),
                arity: builtin.arity(),
            }),
            None,
            BindingOrigin::Kernel,
        );
    }
}
