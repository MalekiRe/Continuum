use crate::kernel::Kernel;
use crate::vm::value::{VARIADIC_ARITY, Value};

fn argument<'a>(args: &'a [Value], index: usize, name: &str) -> Result<&'a Value, String> {
    args.get(index)
        .ok_or_else(|| format!("{}: missing argument {}", name, index + 1))
}

fn integer_argument(args: &[Value], index: usize, name: &str) -> Result<i64, String> {
    argument(args, index, name)?
        .as_int()
        .ok_or_else(|| format!("{}: argument {} must be an integer", name, index + 1))
}

fn index_argument(args: &[Value], index: usize, name: &str) -> Result<usize, String> {
    let value = integer_argument(args, index, name)?;
    usize::try_from(value).map_err(|_| {
        format!(
            "{}: argument {} must be a non-negative index",
            name,
            index + 1
        )
    })
}

fn string_argument<'a>(args: &'a [Value], index: usize, name: &str) -> Result<&'a str, String> {
    argument(args, index, name)?
        .as_str()
        .ok_or_else(|| format!("{}: argument {} must be a string", name, index + 1))
}

fn numbers(args: &[Value], name: &str) -> Result<(f64, f64), String> {
    Ok((
        argument(args, 0, name)?
            .as_number()
            .ok_or_else(|| format!("{}: expected numbers", name))?,
        argument(args, 1, name)?
            .as_number()
            .ok_or_else(|| format!("{}: expected numbers", name))?,
    ))
}

fn arithmetic(
    args: &[Value],
    name: &str,
    ints: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<Value, String> {
    if let (Some(Value::Int(a)), Some(Value::Int(b))) = (args.first(), args.get(1)) {
        return ints(*a, *b)
            .map(Value::Int)
            .ok_or_else(|| format!("{}: integer overflow", name));
    }
    let (a, b) = numbers(args, name)?;
    Ok(Value::Float(floats(a, b)))
}

impl Kernel {
    pub fn register_tools(&mut self) {
        // Snapshots may contain natives retired after they were written. Remove
        // them explicitly before restoring the current registry.
        if let Some(namespace) =
            std::sync::Arc::make_mut(&mut self.env.namespaces).get_mut("kernel")
        {
            for name in ["read", "sleep"] {
                namespace.bindings.remove(name);
                namespace.sources.remove(name);
            }
        }

        // Arithmetic
        self.define_native("kernel/+", 2, |_kernel, args| {
            arithmetic(&args, "+", i64::checked_add, |a, b| a + b)
        });
        self.define_native("kernel/-", 2, |_kernel, args| {
            arithmetic(&args, "-", i64::checked_sub, |a, b| a - b)
        });
        self.define_native("kernel/*", 2, |_kernel, args| {
            arithmetic(&args, "*", i64::checked_mul, |a, b| a * b)
        });
        self.define_native("kernel//", 2, |_kernel, args| {
            let (a, b) = numbers(&args, "/")?;
            if b == 0.0 {
                Err("/: division by zero".into())
            } else {
                Ok(Value::Float(a / b))
            }
        });
        self.define_native("kernel/=", 2, |_kernel, args| {
            Ok(Value::Bool(args[0] == args[1]))
        });
        self.define_native("kernel/<", 2, |_kernel, args| {
            let (a, b) = numbers(&args, "<")?;
            Ok(Value::Bool(a < b))
        });
        self.define_native("kernel/>", 2, |_kernel, args| {
            let (a, b) = numbers(&args, ">")?;
            Ok(Value::Bool(a > b))
        });

        self.define_native("kernel/cons", 2, |_kernel, args| {
            let car = args[0].clone();
            let cdr = args[1].clone();
            match cdr {
                Value::List(mut items) => {
                    let mut new_list = vec![car];
                    new_list.append(&mut items);
                    Ok(Value::List(new_list))
                }
                Value::Nil => Ok(Value::List(vec![car])),
                _ => Err("cons: second argument must be a list".into()),
            }
        });

        self.define_native("kernel/car", 1, |_kernel, args| match &args[0] {
            Value::List(items) => items
                .first()
                .cloned()
                .ok_or_else(|| "car: empty list".into()),
            _ => Err("car: expected list".into()),
        });

        self.define_native("kernel/cdr", 1, |_kernel, args| match &args[0] {
            Value::List(items) if items.len() >= 2 => Ok(Value::List(items[1..].to_vec())),
            Value::List(_) => Ok(Value::Nil),
            _ => Err("cdr: expected list".into()),
        });

        self.define_native("kernel/list", VARIADIC_ARITY, |_kernel, args| {
            Ok(Value::List(args.to_vec()))
        });

        self.define_native("kernel/display", 1, |_kernel, args| {
            let msg = format!("{}", args[0]);
            if let Some(hook) = *crate::vm::eval::PRINT_HOOK.lock().unwrap() {
                hook(&msg);
            } else {
                print!("{}", msg);
            }
            Ok(args[0].clone())
        });

        self.define_native("kernel/println", 1, |_kernel, args| {
            let msg = format!("{}", args[0]);
            if let Some(hook) = *crate::vm::eval::PRINT_HOOK.lock().unwrap() {
                hook(&msg);
            } else {
                println!("{}", msg);
            }
            Ok(args[0].clone())
        });

        // Type predicates
        self.define_native("kernel/nil?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Nil)))
        });
        self.define_native("kernel/number?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(
                args[0],
                Value::Int(_) | Value::Float(_)
            )))
        });
        self.define_native("kernel/symbol?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Symbol(_))))
        });
        self.define_native("kernel/string?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::String(_))))
        });
        self.define_native("kernel/list?", 1, |_kernel, args| {
            Ok(Value::Bool(args[0].is_list()))
        });
        self.define_native("kernel/function?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Function(_))))
        });
        self.define_native("kernel/keyword?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Keyword(_))))
        });

        // Control
        // System
        self.define_native("system/version", 0, |_kernel, _args| {
            Ok(Value::string(concat!(
                "persistent-lisp-harness/",
                env!("CARGO_PKG_VERSION")
            )))
        });
        self.define_native("system/clock", 0, |_kernel, _args| {
            Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
        });
        self.define_native("agent/return", 1, |kernel, args| {
            if !kernel.current_form_is("agent/return") {
                return Err("agent/return must be a top-level form".into());
            }
            if kernel.frames.len() <= 1 {
                return Err("agent/return: root frame has no parent".into());
            }
            let value = args.into_iter().next().unwrap_or(Value::Nil);
            kernel.set_trap(crate::kernel::VmTrap::ReturnAgent { value })?;
            Ok(Value::keyword("suspended"))
        });

        self.define_native("message/reply", 2, |kernel, args| {
            if !kernel.current_form_is("message/reply") {
                return Err("message/reply must be a top-level form".into());
            }
            let message_id = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            let text = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            if !kernel.has_pending_message(&message_id) {
                return Err(format!(
                    "message/reply: unknown or completed message '{}'",
                    message_id
                ));
            }
            kernel.set_trap(crate::kernel::VmTrap::Reply { message_id, text })?;
            Ok(Value::keyword("suspended"))
        });

        // Model calls inside Lisp are explicit top-level scheduler traps. Ordinary
        // cognition is driven by the Rust scheduler, not by agent/step.
        self.define_native("model/call", 1, |kernel, args| {
            if !kernel.current_form_is("model/call") {
                return Err("model/call must be a top-level form".into());
            }
            let prompt = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            kernel.set_trap(crate::kernel::VmTrap::CallModel { prompt })?;
            Ok(Value::keyword("suspended"))
        });
        self.define_native("human/wait", 0, |kernel, _args| {
            if !kernel.current_form_is("human/wait") {
                return Err("human/wait must be a top-level form".into());
            }
            kernel.set_trap(crate::kernel::VmTrap::AwaitHuman)?;
            Ok(Value::keyword("suspended"))
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
                other => return Err(format!("source/get: expected name, got {}", other)),
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
            let ns = _kernel
                .env
                .namespaces
                .get(namespace)
                .ok_or_else(|| format!("no history for {}", name))?;
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

        self.define_native("string-append", 2, |_kernel, args| {
            let a = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            let b = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            Ok(Value::string(&format!("{}{}", a, b)))
        });
        self.define_native("nth", 2, |_kernel, args| {
            let index = index_argument(&args, 0, "nth")?;
            let items = argument(&args, 1, "nth")?
                .as_list()
                .ok_or_else(|| "nth: argument 2 must be a list".to_string())?;
            items
                .get(index)
                .cloned()
                .ok_or_else(|| format!("nth: index {} out of bounds (len {})", index, items.len()))
        });
        self.define_native("length", 1, |_kernel, args| {
            let length = match argument(&args, 0, "length")? {
                Value::List(items) => items.len(),
                Value::String(value) => value.chars().count(),
                _ => return Err("length: expected list or string".into()),
            };
            i64::try_from(length)
                .map(Value::Int)
                .map_err(|_| "length: value is too large".into())
        });
        self.define_native("bash", 1, |kernel, args| {
            if !kernel.current_form_is("bash") {
                return Err(
                    "bash must be a top-level form until VM continuations are explicit".into(),
                );
            }
            let command = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("bash: expected command string, got {}", other)),
            };
            kernel.set_trap(crate::kernel::VmTrap::RunBash { command })?;
            Ok(Value::keyword("suspended"))
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
                .map(|f| f.id.clone())
                .unwrap_or_default();
            kernel.wake_timers.push(crate::kernel::WakeEntry {
                wake_at,
                action,
                frame_id,
            });
            Ok(Value::keyword("scheduled"))
        });

        self.define_native("agent/call", 2, |kernel, args| {
            if !kernel.current_form_is("agent/call") {
                return Err(
                    "agent/call must be a top-level form until VM continuations are explicit"
                        .into(),
                );
            }
            let name = match &args[0] {
                Value::String(s) | Value::Symbol(s) => s.clone(),
                other => return Err(format!("agent/call: expected agent name, got {}", other)),
            };
            let request = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            kernel.set_trap(crate::kernel::VmTrap::CallAgent { name, request })?;
            Ok(Value::keyword("suspended"))
        });

        self.define_native("map/get", 2, |_kernel, args| {
            let map = argument(&args, 0, "map/get")?
                .as_map()
                .ok_or_else(|| "map/get: argument 1 must be a map".to_string())?;
            let key = argument(&args, 1, "map/get")?;
            Ok(map.get(key).cloned().unwrap_or(Value::Nil))
        });

        self.define_native("vector/get", 2, |_kernel, args| {
            let vector = argument(&args, 0, "vector/get")?
                .as_vector()
                .ok_or_else(|| "vector/get: argument 1 must be a vector".to_string())?;
            let index = index_argument(&args, 1, "vector/get")?;
            vector.get(index).cloned().ok_or_else(|| {
                format!(
                    "vector/get: index {} out of bounds (len {})",
                    index,
                    vector.len()
                )
            })
        });

        self.define_native("append", VARIADIC_ARITY, |_kernel, args| {
            let mut result = Vec::new();
            for arg in args {
                match arg {
                    Value::List(items) => result.extend(items),
                    other => result.push(other),
                }
            }
            Ok(Value::List(result))
        });
        self.define_native("kernel/error", 1, |_kernel, args| {
            let msg = format!("{}", args[0]);
            Err(msg)
        });

        // inspect/find — semantic search returning compact summaries first
        self.define_native("inspect/find", 1, |_kernel, args| {
            let query = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("inspect/find: expected string, got {}", other)),
            };

            let results = _kernel.find_bindings(&query);
            // Return compact summaries: just the qualified names
            let items: Vec<Value> = results.iter().map(|r| Value::string(r)).collect();
            Ok(Value::List(items))
        });

        // inspect/describe — full details for a specific binding

        // inspect/source — show source code of a definition
        self.define_native("string-search", 2, |_kernel, args| {
            let needle = string_argument(&args, 0, "string-search")?;
            let haystack = string_argument(&args, 1, "string-search")?;
            if let Some(byte_index) = haystack.find(needle) {
                let scalar_index = haystack[..byte_index].chars().count();
                i64::try_from(scalar_index)
                    .map(Value::Int)
                    .map_err(|_| "string-search: index is too large".into())
            } else {
                Ok(Value::Bool(false))
            }
        });
        self.define_native("substring", 3, |_kernel, args| {
            let value = string_argument(&args, 0, "substring")?;
            let start = index_argument(&args, 1, "substring")?;
            let end = index_argument(&args, 2, "substring")?;
            if start > end {
                return Err(format!(
                    "substring: start index {} exceeds end index {}",
                    start, end
                ));
            }

            let scalar_count = value.chars().count();
            if end > scalar_count {
                return Err(format!(
                    "substring: index {} out of bounds (len {})",
                    end, scalar_count
                ));
            }

            let start_byte = value
                .char_indices()
                .nth(start)
                .map_or(value.len(), |(index, _)| index);
            let end_byte = value
                .char_indices()
                .nth(end)
                .map_or(value.len(), |(index, _)| index);
            Ok(Value::string(&value[start_byte..end_byte]))
        });

        // history/read — read a specific event by ID

        // history/zoom — zoom into a summary range, returning raw events

        // history/find — search events by text content

        // Model invocation — calls any OpenAI-compatible API

        // Agent think: structured cognition call

        // Model chat: multi-turn conversation
    }
}
