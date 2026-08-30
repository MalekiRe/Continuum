use crate::kernel::Kernel;
use crate::kernel::native::{argument, integer_argument};
use crate::vm::value::{NativeError, Value};

impl Kernel {
    pub(crate) fn register_kernel_builtins(&mut self) {
        self.define_native("system/version", 0, |_kernel, _args| {
            Ok(Value::string(concat!(
                "persistent-lisp-harness/",
                env!("CARGO_PKG_VERSION")
            )))
        });
        self.define_native("system/clock", 0, |_kernel, _args| {
            Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
        });
        self.define_native("transcript/recent", 1, |kernel, args| {
            let count = match &args[0] {
                Value::Int(n) if *n >= 0 => *n as usize,
                _ => return Err("transcript/recent: expected a non-negative integer".into()),
            };
            let Some(frame) = kernel.frames.last() else {
                return Ok(Value::Nil);
            };
            let start = frame.state.transcript.len().saturating_sub(count);
            Ok(Value::List(
                frame.state.transcript[start..]
                    .iter()
                    .map(|entry| {
                        Value::list(vec![
                            Value::string(&entry.timestamp),
                            Value::string(&entry.source),
                            Value::string(&entry.result),
                        ])
                    })
                    .collect(),
            ))
        });
        self.define_native("source/get", 1, |kernel, args| {
            let name = match &args[0] {
                Value::Symbol(s) | Value::String(s) => s.clone(),
                other => {
                    return Err(NativeError::Failed(format!(
                        "source/get: expected name, got {}",
                        other
                    )));
                }
            };
            let qualified = if name.contains('/') {
                name
            } else {
                format!("user/{}", name)
            };
            Ok(kernel
                .env
                .source(&qualified)
                .map(Value::string)
                .unwrap_or(Value::Nil))
        });
        self.define_native("source/list", 0, |kernel, _args| {
            let mut names = Vec::new();
            for (ns_name, ns) in kernel.env.namespaces.iter() {
                for name in ns.sources.keys() {
                    names.push(Value::symbol(&format!("{}/{}", ns_name, name)));
                }
            }
            names.sort_by_key(|v| format!("{}", v));
            Ok(Value::List(names))
        });
        self.define_native("context/add-hook", 1, |kernel, args| {
            let hook = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            if hook.chars().count() > 2_000 {
                return Err("context/add-hook: hook exceeds 2000 characters".into());
            }
            let frame = kernel
                .frames
                .last_mut()
                .ok_or_else(|| "context/add-hook: no frame".to_string())?;
            if frame.state.context_hooks.len() >= 16 {
                return Err("context/add-hook: at most 16 hooks are allowed".into());
            }
            frame.state.context_hooks.push(hook);
            Ok(Value::keyword("ok"))
        });
        self.define_native("context/clear-hooks", 0, |kernel, _args| {
            if let Some(frame) = kernel.frames.last_mut() {
                frame.state.context_hooks.clear();
            }
            Ok(Value::keyword("ok"))
        });
        self.define_native("memory/remember", 2, |kernel, args| {
            let key = match &args[0] {
                Value::String(s) | Value::Symbol(s) => s.clone(),
                other => format!("{}", other),
            };
            let value = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            if key.chars().count() > 200 || value.chars().count() > 2_000 {
                return Err("memory/remember: key or value exceeds context limits".into());
            }
            let frame = kernel
                .frames
                .last_mut()
                .ok_or_else(|| "memory/remember: no frame".to_string())?;
            if frame.state.memory.len() >= 64
                && !frame.state.memory.iter().any(|entry| entry.key == key)
            {
                return Err("memory/remember: at most 64 entries are allowed".into());
            }
            if let Some(entry) = frame.state.memory.iter_mut().find(|entry| entry.key == key) {
                entry.value = value;
                entry.updated_at = chrono::Utc::now().to_rfc3339();
            } else {
                frame.state.memory.push(crate::kernel::MemoryEntry {
                    key,
                    value,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                });
            }
            Ok(Value::keyword("ok"))
        });
        self.define_native("memory/forget", 1, |kernel, args| {
            let key = match &args[0] {
                Value::String(s) | Value::Symbol(s) => s.clone(),
                other => format!("{}", other),
            };
            if let Some(frame) = kernel.frames.last_mut() {
                frame.state.memory.retain(|entry| entry.key != key);
            }
            Ok(Value::keyword("ok"))
        });
        self.define_native("memory/list", 0, |kernel, _args| {
            let Some(frame) = kernel.frames.last() else {
                return Ok(Value::Nil);
            };
            Ok(Value::List(
                frame
                    .state
                    .memory
                    .iter()
                    .map(|entry| {
                        Value::list(vec![
                            Value::string(&entry.key),
                            Value::string(&entry.value),
                            Value::string(&entry.updated_at),
                        ])
                    })
                    .collect(),
            ))
        });
        self.define_native("inspect/namespaces", 0, |_kernel, _args| {
            let names: Vec<Value> = _kernel
                .env
                .namespace_names()
                .iter()
                .map(|n| {
                    let count = _kernel
                        .env
                        .namespaces
                        .get(n)
                        .map(|ns| ns.list_bindings().len())
                        .unwrap_or(0);
                    Value::list(vec![Value::symbol(n), Value::int(count as i64)])
                })
                .collect();
            Ok(Value::List(names))
        });
        self.define_native("inspect/bindings", 1, |_kernel, args| {
            let ns_name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                _ => return Err("inspect/bindings: expected symbol".into()),
            };
            let bindings = _kernel.inspect_namespace(&ns_name).unwrap_or_default();
            let items: Vec<Value> = bindings.iter().map(|b| Value::symbol(b)).collect();
            Ok(Value::List(items))
        });
        self.define_native("inspect/history", 1, |_kernel, args| {
            let Value::Symbol(name) = &args[0] else {
                return Err("inspect/history: expected symbol".into());
            };
            let qualified = if name.contains('/') {
                name.clone()
            } else {
                format!("user/{}", name)
            };
            let (namespace, binding) = qualified.split_once('/').unwrap();
            let ns =
                _kernel.env.namespaces.get(namespace).ok_or_else(|| {
                    NativeError::InvalidArgument(format!("no history for {}", name))
                })?;
            let records = ns.history(binding);
            if records.is_empty() {
                return Ok(Value::string("no history"));
            }
            Ok(Value::List(
                records
                    .iter()
                    .map(|record| {
                        Value::list(vec![
                            Value::string(&record.timestamp),
                            Value::int(record.version as i64),
                            Value::string(&format!("{}", record.value)),
                        ])
                    })
                    .collect(),
            ))
        });
        self.define_native("wake", 2, |kernel, args| {
            let duration_ms = integer_argument(&args, 0, "wake")?;
            if duration_ms < 0 {
                return Err("wake: duration must be non-negative".into());
            }
            let action = format!("{}", argument(&args, 1, "wake")?);
            let wake_at = chrono::Utc::now()
                .checked_add_signed(chrono::Duration::milliseconds(duration_ms))
                .ok_or_else(|| "wake: duration is out of range".to_string())?;
            let frame_id = kernel
                .frames
                .last()
                .map(|frame| frame.id.clone())
                .ok_or_else(|| "wake: no active frame".to_string())?;
            kernel.wake_timers.push(crate::kernel::WakeEntry {
                wake_at,
                action,
                frame_id,
            });
            Ok(Value::keyword("scheduled"))
        });
        self.define_native("inspect/find", 1, |_kernel, args| {
            let query = match &args[0] {
                Value::String(s) => s.clone(),
                other => {
                    return Err(NativeError::Failed(format!(
                        "inspect/find: expected string, got {}",
                        other
                    )));
                }
            };

            let results = _kernel.find_bindings(&query);
            // Return compact summaries: just the qualified names
            let items: Vec<Value> = results.iter().map(|r| Value::string(r)).collect();
            Ok(Value::List(items))
        });
    }
}
