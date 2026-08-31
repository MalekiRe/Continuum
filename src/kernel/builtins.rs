use crate::kernel::Kernel;
use crate::kernel::native::exact_native;
use crate::vm::value::{NativeError, Value};

impl Kernel {
    pub(crate) fn register_kernel_builtins(&mut self) {
        exact_native!(self, "system/version", |_kernel, []| {
            Ok(Value::string(concat!(
                "persistent-lisp-harness/",
                env!("CARGO_PKG_VERSION")
            )))
        });
        exact_native!(self, "system/clock", |_kernel, []| {
            Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
        });
        exact_native!(self, "transcript/recent", |kernel, [count_value]| {
            let count = match count_value {
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
        exact_native!(self, "source/get", |kernel, [name_value]| {
            let name = match name_value {
                Value::Symbol(s) | Value::String(s) => s.clone(),
                other => {
                    return Err(NativeError::Failed(format!(
                        "source/get: expected name, got {}",
                        other
                    )));
                }
            };
            let qualified = super::qualify_user_name(&name);
            Ok(kernel
                .env
                .source(&qualified)
                .map(Value::string)
                .unwrap_or(Value::Nil))
        });
        exact_native!(self, "source/list", |kernel, []| {
            let mut names = Vec::new();
            for (ns_name, ns) in kernel.env.namespaces.iter() {
                for name in ns.sources.keys() {
                    names.push(Value::symbol(&format!("{}/{}", ns_name, name)));
                }
            }
            names.sort_by_key(|v| format!("{}", v));
            Ok(Value::List(names))
        });
        exact_native!(self, "context/add-hook", |kernel, [hook_value]| {
            let hook = hook_value.coerce_text();
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
        exact_native!(self, "context/clear-hooks", |kernel, []| {
            if let Some(frame) = kernel.frames.last_mut() {
                frame.state.context_hooks.clear();
            }
            Ok(Value::keyword("ok"))
        });
        exact_native!(
            self,
            "memory/remember",
            |kernel, [key_value, memory_value]| {
                let key = key_value.coerce_text();
                let value = memory_value.coerce_text();
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
            }
        );
        exact_native!(self, "memory/forget", |kernel, [key_value]| {
            let key = key_value.coerce_text();
            if let Some(frame) = kernel.frames.last_mut() {
                frame.state.memory.retain(|entry| entry.key != key);
            }
            Ok(Value::keyword("ok"))
        });
        exact_native!(self, "memory/list", |kernel, []| {
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
        exact_native!(self, "inspect/namespaces", |kernel, []| {
            let names: Vec<Value> = kernel
                .env
                .namespace_names()
                .iter()
                .map(|n| {
                    let count = kernel
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
        exact_native!(self, "inspect/bindings", |kernel, [namespace_value]| {
            let ns_name = match namespace_value {
                Value::Symbol(s) => s.clone(),
                _ => return Err("inspect/bindings: expected symbol".into()),
            };
            let bindings = kernel.inspect_namespace(&ns_name).unwrap_or_default();
            let items: Vec<Value> = bindings.iter().map(|b| Value::symbol(b)).collect();
            Ok(Value::List(items))
        });
        exact_native!(self, "inspect/history", |kernel, [name_value]| {
            let Value::Symbol(name) = name_value else {
                return Err("inspect/history: expected symbol".into());
            };
            let qualified = super::qualify_user_name(name);
            let (namespace, binding) = qualified.split_once('/').unwrap();
            let ns =
                kernel.env.namespaces.get(namespace).ok_or_else(|| {
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
        exact_native!(self, "wake", |kernel, [duration_value, action_value]| {
            let duration_ms = duration_value.require_int("wake", 1)?;
            if duration_ms < 0 {
                return Err("wake: duration must be non-negative".into());
            }
            let action = format!("{}", action_value);
            let wake_at = chrono::Utc::now()
                .checked_add_signed(chrono::Duration::milliseconds(duration_ms))
                .ok_or_else(|| "wake: duration is out of range".to_string())?;
            let frame_id = kernel
                .frames
                .last()
                .map(|frame| frame.id.clone())
                .ok_or_else(|| "wake: no active frame".to_string())?;
            kernel
                .schedule_wake_at(frame_id, wake_at, action)
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("scheduled"))
        });
        exact_native!(self, "inspect/find", |kernel, [query_value]| {
            let query = match query_value {
                Value::String(query) => query,
                other => {
                    return Err(NativeError::Failed(format!(
                        "inspect/find: expected string, got {}",
                        other
                    )));
                }
            };
            let results = kernel.find_bindings(query);
            // Return compact summaries: just the qualified names
            let items: Vec<Value> = results.iter().map(|r| Value::string(r)).collect();
            Ok(Value::List(items))
        });
    }
}
