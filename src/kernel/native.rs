use crate::kernel::SnapshotKind;
use crate::vm::value::Value;
use crate::kernel::Kernel;
use std::collections::HashMap;

impl Kernel {
    pub fn register_tools(&mut self) {

        // Arithmetic
        self.define_native("kernel/+", 2, |_kernel, args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 + b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + b as f64)),
                _ => Err("+: expected numbers".into()),
            }
        });

        self.define_native("kernel/-", 2, |_kernel, args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 - b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - b as f64)),
                _ => Err("-: expected numbers".into()),
            }
        });

        self.define_native("kernel/*", 2, |_kernel, args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(a as f64 * b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * b as f64)),
                _ => Err("*: expected numbers".into()),
            }
        });

        self.define_native("kernel//", 2, |_kernel, args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => {
                    if b == 0 { Err("/: division by zero".into()) } else { Ok(Value::Float(a as f64 / b as f64)) }
                }
                (Value::Float(a), Value::Float(b)) => {
                    if b == 0.0 { Err("/: division by zero".into()) } else { Ok(Value::Float(a / b)) }
                }
                _ => Err("/: expected numbers".into()),
            }
        });

        self.define_native("kernel/=", 2, |_kernel, args| {
            Ok(Value::Bool(args[0] == args[1]))
        });

        self.define_native("kernel/<", 2, |_kernel, args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
                _ => Err("<: expected numbers".into()),
            }
        });

        self.define_native("kernel/>", 2, |_kernel, args| {
            let a = args[0].clone();
            let b = args[1].clone();
            match (a, b) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
                _ => Err(">: expected numbers".into()),
            }
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

        self.define_native("kernel/car", 1, |_kernel, args| {
            match &args[0] {
                Value::List(items) => items.first().cloned().ok_or_else(|| "car: empty list".into()),
                _ => Err("car: expected list".into()),
            }
        });

        self.define_native("kernel/cdr", 1, |_kernel, args| {
            match &args[0] {
                Value::List(items) if items.len() >= 2 => Ok(Value::List(items[1..].to_vec())),
                Value::List(_) => Ok(Value::Nil),
                _ => Err("cdr: expected list".into()),
            }
        });

        self.define_native("kernel/list", 0, |_kernel, args| {
            Ok(Value::List(args.to_vec()))
        });

        self.define_native("kernel/display", 1, |_kernel, args| {
            print!("{}", args[0]);
            Ok(args[0].clone())
        });

        self.define_native("kernel/println", 1, |_kernel, args| {
            println!("{}", args[0]);
            Ok(args[0].clone())
        });

        self.define_native("kernel/read", 0, |_kernel, _args| {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)
                .map_err(|e| format!("read error: {}", e))?;
            Ok(Value::string(input.trim()))
        });

        // Type predicates
        self.define_native("kernel/nil?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Nil)))
        });
        self.define_native("kernel/number?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Int(_) | Value::Float(_))))
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
        self.define_native("control/Continue", 0, |_kernel, _args| {
            Ok(Value::Keyword("Continue".to_string()))
        });
        self.define_native("control/CancelCurrent", 1, |_kernel, args| {
            Ok(Value::Tagged {
                family: "control".into(),
                variant: "CancelCurrent".into(),
                fields: args.to_vec(),
            })
        });
        self.define_native("control/Error", 1, |_kernel, args| {
            let msg = format!("{}", args[0]);
            Err(msg)
        });

        // System
        self.define_native("system/version", 0, |_kernel, _args| {
            Ok(Value::string("persistent-lisp-harness/0.1.0"))
        });
        self.define_native("system/clock", 0, |_kernel, _args| {
            Ok(Value::string(&chrono::Utc::now().to_rfc3339()))
        });
        self.define_native("system/interrupt", 0, |_kernel, _args| {
            // Set the interrupt flag — Lisp will notice at the next safepoint
            crate::vm::eval::EVAL_INTERRUPTED.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(Value::keyword("interrupted"))
        });
        self.define_native("system/clear-interrupt", 0, |_kernel, _args| {
            crate::vm::eval::EVAL_INTERRUPTED.store(false, std::sync::atomic::Ordering::Relaxed);
            crate::vm::eval::TURN_COUNTER.store(0, std::sync::atomic::Ordering::Relaxed);
            Ok(Value::keyword("cleared"))
        });

        self.define_native("system/report-tokens", 1, |_kernel, args| {
            let count = match &args[0] {
                Value::Int(n) => *n,
                Value::Float(f) => *f as i64,
                _ => return Err("system/report-tokens: expected integer".into()),
            };
            if count > 0 {
                _kernel.token_reports.push_back((chrono::Utc::now(), count as u64));
            }
            Ok(Value::keyword("ok"))
        });
        self.define_native("system/snapshot", 0, |_kernel, _args| {
                let snap = _kernel.snapshot(SnapshotKind::Incremental);
                Ok(Value::string(&format!("snapshot saved: {}", snap.id)))
        });
                self.define_native("inspect/namespaces", 0, |_kernel, _args| {
                let names: Vec<Value> = _kernel.env.namespace_names().iter().map(|n| {
                    let count = _kernel.env.namespaces.get(n)
                        .map(|ns| ns.list_bindings().len())
                        .unwrap_or(0);
                    Value::list(vec![Value::symbol(n), Value::int(count as i64)])
                }).collect();
                Ok(Value::List(names))
        });
        self.define_native("inspect/bindings", 1, |_kernel, args| {
            let ns_name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                _ => return Err("inspect/bindings: expected symbol".into()),
            };
                let bindings = _kernel.inspect_namespace(&ns_name)
                    .unwrap_or_default();
                let items: Vec<Value> = bindings.iter().map(|b| Value::symbol(b)).collect();
                Ok(Value::List(items))
        });
        self.define_native("inspect/history", 1, |_kernel, args| {
            let name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                _ => return Err("inspect/history: expected symbol".into()),
            };
                let qualified = if name.contains('/') { name.clone() } else { format!("user/{}", name) };
                let parts: Vec<&str> = qualified.splitn(2, '/').collect();
                if parts.len() == 2 {
                    if let Some(ns) = _kernel.env.namespaces.get(parts[0]) {
                        let records = ns.history(parts[1]);
                        if records.is_empty() {
                            return Ok(Value::string("no history"));
                        }
                        let entries: Vec<Value> = records.iter().map(|r| {
                            Value::list(vec![
                                Value::string(&r.timestamp),
                                Value::int(r.version as i64),
                                Value::string(&format!("{}", r.value)),
                            ])
                        }).collect();
                        return Ok(Value::List(entries));
                    }
                }
                Err(format!("no history for {}", name))
        });
    

                 
        
        self.define_native("bash", 1, |_kernel, args| {
            let cmd = match &args[0] {
                Value::String(s) => s.clone(),
                Value::List(items) => {
                    items.iter().map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => format!("{}", other),
                    }).collect::<Vec<_>>().join(" ")
                }
                other => return Err(format!("proc/run: expected string or list, got {}", other)),
            };
            match std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .output()
            {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    Ok(Value::Map(HashMap::from([
                        (Value::keyword("exit"), Value::Int(output.status.code().unwrap_or(-1) as i64)),
                        (Value::keyword("stdout"), Value::string(&stdout)),
                        (Value::keyword("stderr"), Value::string(&stderr)),
                    ])))
                }
                Err(e) => Err(format!("proc/run: {}", e)),
            }
        });

        
        self.define_native("wake", 2, |_kernel, args| {
            let duration_ms = match &args[0] {
                Value::Int(ms) => *ms,
                other => return Err(format!("wake: expected integer milliseconds, got {}", other)),
            };
            let action = format!("{}", args[1]);

                let wake_at = chrono::Utc::now() + chrono::Duration::milliseconds(duration_ms);
                let frame_id = _kernel.frames.last().map(|f| f.id.clone()).unwrap_or_default();
                _kernel.wake_timers.push(crate::kernel::WakeEntry {
                    wake_at: wake_at.to_rfc3339(),
                    action,
                    frame_id,
                });
                Ok(Value::keyword("scheduled"))
        });

                self.define_native("agent/call", 2, |_kernel, args| {
            let name = match &args[0] {
                Value::String(s) => s.clone(),
                Value::Symbol(s) => s.clone(),
                other => return Err(format!("agent/call: expected name, got {}", other)),
            };
            let request = format!("{}", args[1]);

                let child_name = name.clone();
                let request_text = request.clone();

                // Spawn the child frame
                let _child_id = _kernel.spawn_subagent(&child_name, &request_text)?;

                // Mark parent as waiting (paused)
                if let Some(frame) = _kernel.frames.last_mut() {
                    frame.status = crate::kernel::FrameStatus::Waiting;
                }

                // Evaluate in the child frame context
                let child_result = _kernel.eval(&request_text).map_err(|e| {
                    format!("agent/call: child error: {}", e)
                })?;

                // Return from subagent — pops child, delivers to parent
                _kernel.return_from_subagent(child_result.clone());

                // Check for delivered result in parent
                if let Some(parent) = _kernel.frames.last_mut() {
                    parent.status = crate::kernel::FrameStatus::Running;
                    if let Some(result) = parent.state.pending_subagent_result.take() {
                        return Ok(result);
                    }
                }

                Ok(child_result)
        });

        
        
        
        self.define_native("map/get", 2, |_kernel, args| {
            let map = match &args[0] {
                Value::Map(m) => m,
                other => return Err(format!("map/get: expected map, got {}", other)),
            };
            let key = &args[1];
            Ok(map.get(key).cloned().unwrap_or(Value::Nil))
        });

        self.define_native("vector/get", 2, |_kernel, args| {
            let vec = match &args[0] {
                Value::Vector(v) => v,
                other => return Err(format!("vector/get: expected vector, got {}", other)),
            };
            let idx = match &args[1] {
                Value::Int(i) => *i as usize,
                other => return Err(format!("vector/get: expected integer index, got {}", other)),
            };
            vec.get(idx).cloned().ok_or_else(|| format!("vector/get: index {} out of bounds (len {})", idx, vec.len()))
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
        self.define_native("inspect/source", 1, |_kernel, args| {
            let name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                other => return Err(format!("inspect/source: expected symbol, got {}", other)),
            };

                let qualified = if name.contains('/') { name.clone() } else { format!("user/{}", name) };
                match _kernel.env.lookup(&qualified) {
                    Some(val) => Ok(Value::string(&format!("{}", val))),
                    None => Err(format!("no binding for {}", name)),
                }
        });

        // history/read — read a specific event by ID

        // history/zoom — zoom into a summary range, returning raw events

        // history/find — search events by text content

        // Model invocation — calls any OpenAI-compatible API
        
        // Agent think: structured cognition call
        
        // Model chat: multi-turn conversation
            }
}
