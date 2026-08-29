use crate::vm::value::Value;
use crate::kernel::Kernel;
use std::collections::HashMap;

impl Kernel {
    pub fn register_tools(&mut self) {
        self.define_native("web/search", 1, |args| {
            let query = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("web/search: expected string, got {}", other)),
            };

            // Try Serper API if key is set
            if let Ok(api_key) = std::env::var("SERPER_API_KEY") {
                let client = reqwest::blocking::Client::new();
                let body = serde_json::json!({"q": query});
                match client
                    .post("https://google.serper.dev/search")
                    .header("X-API-KEY", &api_key)
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                {
                    Ok(resp) => {
                        match resp.json::<serde_json::Value>() {
                            Ok(json) => {
                                // Extract organic results
                                let results = json["organic"].as_array()
                                    .map(|arr| {
                                        arr.iter().map(|item| {
                                            let title = item["title"].as_str().unwrap_or("");
                                            let link = item["link"].as_str().unwrap_or("");
                                            let snippet = item["snippet"].as_str().unwrap_or("");
                                            Value::string(&format!("{} - {}: {}", title, link, snippet))
                                        }).collect::<Vec<Value>>()
                                    })
                                    .unwrap_or_default();
                                return Ok(Value::List(results));
                            }
                            Err(e) => return Err(format!("web/search: parse error: {}", e)),
                        }
                    }
                    Err(e) => return Err(format!("web/search: request error: {}", e)),
                }
            }

            // Fallback: return placeholder
            Ok(Value::list(vec![
                Value::string(&format!("no results for: {} (set SERPER_API_KEY)", query)),
            ]))
        });

        self.define_native("fs/read", 1, |args| {
            let path = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("fs/read: expected string, got {}", other)),
            };
            match std::fs::read_to_string(&path) {
                Ok(content) => Ok(Value::string(&content)),
                Err(e) => Err(format!("fs/read: {}", e)),
            }
        });

        self.define_native("fs/write", 2, |args| {
            let path = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("fs/write: expected string for path, got {}", other)),
            };
            let content = match &args[1] {
                Value::String(s) => s.clone(),
                other => return Err(format!("fs/write: expected string for content, got {}", other)),
            };
            match std::fs::write(&path, &content) {
                Ok(_) => Ok(Value::Nil),
                Err(e) => Err(format!("fs/write: {}", e)),
            }
        });

        self.define_native("proc/run", 1, |args| {
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

        // proc/pid — returns an opaque kernel reference to the current process
        self.define_native("proc/pid", 0, |_args| {
            Ok(Value::KernelRef(crate::vm::value::KernelRef {
                kind: "process".into(),
                id: format!("{}", std::process::id()),
                metadata: std::collections::HashMap::new(),
            }))
        });

        // fs/open — returns an opaque kernel reference to an open file
        self.define_native("fs/open", 1, |args| {
            let path = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("fs/open: expected string, got {}", other)),
            };
            match std::fs::File::open(&path) {
                Ok(_file) => {
                    // Use a UUID-based reference since we can't store the File handle
                    let id = uuid::Uuid::new_v4().to_string();
                    Ok(Value::KernelRef(crate::vm::value::KernelRef {
                        kind: "file".into(),
                        id,
                        metadata: std::collections::HashMap::from([
                            ("path".to_string(), path),
                        ]),
                    }))
                }
                Err(e) => Err(format!("fs/open: {}", e)),
            }
        });

        self.define_native("message/reply", 1, |args| {
            let text = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            println!("[message/reply] {}", text);
            Ok(Value::keyword("sent"))
        });

        self.define_native("clock/wake", 2, |args| {
            let _duration = match &args[0] {
                Value::Int(ms) => *ms,
                _ => return Err("clock/wake: expected integer milliseconds".into()),
            };
            let _action = args[1].clone();
            Ok(Value::keyword("scheduled"))
        });

                self.define_native("agent/call", 2, |args| {
            let name = match &args[0] {
                Value::String(s) => s.clone(),
                Value::Symbol(s) => s.clone(),
                other => return Err(format!("agent/call: expected name, got {}", other)),
            };
            let request = format!("{}", args[1]);

            crate::kernel::with_kernel(|k| {
                let child_name = name.clone();
                let request_text = request.clone();

                // Spawn the child frame
                let _child_id = k.spawn_subagent(&child_name, &request_text)?;

                // Mark parent as waiting (paused)
                if let Some(frame) = k.frames.last_mut() {
                    frame.status = crate::kernel::FrameStatus::Waiting;
                }

                // Evaluate in the child frame context
                let child_result = k.eval(&request_text).map_err(|e| {
                    format!("agent/call: child error: {}", e)
                })?;

                // Return from subagent — pops child, delivers to parent
                k.return_from_subagent(child_result.clone());

                // Check for delivered result in parent
                if let Some(parent) = k.frames.last_mut() {
                    parent.status = crate::kernel::FrameStatus::Running;
                    if let Some(result) = parent.state.pending_subagent_result.take() {
                        return Ok(result);
                    }
                }

                Ok(child_result)
            })
        });

        self.define_native("string/join", 2, |args| {
            let sep = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("string/join: separator must be string, got {}", other)),
            };
            let parts = match &args[1] {
                Value::List(items) => items.iter().map(|v| format!("{}", v)).collect::<Vec<_>>(),
                other => vec![format!("{}", other)],
            };
            Ok(Value::string(&parts.join(&sep)))
        });

        self.define_native("string/split", 2, |args| {
            let text = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("string/split: expected string, got {}", other)),
            };
            let sep = match &args[1] {
                Value::String(s) => s.clone(),
                other => return Err(format!("string/split: separator must be string, got {}", other)),
            };
            let parts: Vec<Value> = text.split(&sep).map(|s| Value::string(s)).collect();
            Ok(Value::List(parts))
        });

        self.define_native("string/contains?", 2, |args| {
            let text = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("string/contains?: expected string, got {}", other)),
            };
            let substr = match &args[1] {
                Value::String(s) => s.clone(),
                other => return Err(format!("string/contains?: expected string, got {}", other)),
            };
            Ok(Value::Bool(text.contains(&substr)))
        });

        self.define_native("map/get", 2, |args| {
            let map = match &args[0] {
                Value::Map(m) => m,
                other => return Err(format!("map/get: expected map, got {}", other)),
            };
            let key = &args[1];
            Ok(map.get(key).cloned().unwrap_or(Value::Nil))
        });

        self.define_native("vector/get", 2, |args| {
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

        self.define_native("kernel/error", 1, |args| {
            let msg = format!("{}", args[0]);
            Err(msg)
        });

        // inspect/find — semantic search returning compact summaries first
        self.define_native("inspect/find", 1, |args| {
            let query = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("inspect/find: expected string, got {}", other)),
            };

            crate::kernel::with_kernel(|k| {
                let results = k.find_bindings(&query);
                // Return compact summaries: just the qualified names
                let items: Vec<Value> = results.iter().map(|r| Value::string(r)).collect();
                Ok(Value::List(items))
            })
        });

        // inspect/describe — full details for a specific binding
        self.define_native("inspect/describe", 1, |args| {
            let name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                other => return Err(format!("inspect/describe: expected symbol, got {}", other)),
            };

            let name_clone = name.clone();
            crate::kernel::with_kernel(|k| {
                let qualified = if name_clone.contains('/') { name_clone.clone() } else { format!("user/{}", name_clone) };
                match k.env.lookup(&qualified) {
                    Some(val) => Ok(Value::string(&format!("{}", val))),
                    None => Err(format!("no binding for {}", name_clone)),
                }
            })
        });

        // inspect/source — show source code of a definition
        self.define_native("inspect/source", 1, |args| {
            let name = match &args[0] {
                Value::Symbol(s) => s.clone(),
                other => return Err(format!("inspect/source: expected symbol, got {}", other)),
            };

            crate::kernel::with_kernel(|k| {
                let qualified = if name.contains('/') { name.clone() } else { format!("user/{}", name) };
                match k.env.lookup(&qualified) {
                    Some(val) => Ok(Value::string(&format!("{}", val))),
                    None => Err(format!("no binding for {}", name)),
                }
            })
        });

        // history/read — read a specific event by ID
        self.define_native("history/read", 1, |args| {
            let event_id = match &args[0] {
                Value::Int(id) => *id as u64,
                other => return Err(format!("history/read: expected integer, got {}", other)),
            };

            // Read from event log file
            let log_path = "data/event.log";
            match std::fs::read_to_string(log_path) {
                Ok(content) => {
                    for line in content.lines() {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                            if event["id"].as_u64() == Some(event_id) {
                                return Ok(Value::string(&serde_json::to_string_pretty(&event).unwrap_or_default()));
                            }
                        }
                    }
                    Ok(Value::string(&format!("no event with id {}", event_id)))
                }
                Err(e) => Err(format!("history/read: {}", e)),
            }
        });

        // history/zoom — zoom into a summary range, returning raw events
        self.define_native("history/zoom", 2, |args| {
            let from = match &args[0] {
                Value::Int(id) => *id as u64,
                other => return Err(format!("history/zoom: expected integer from, got {}", other)),
            };
            let to = match &args[1] {
                Value::Int(id) => *id as u64,
                other => return Err(format!("history/zoom: expected integer to, got {}", other)),
            };

            let log_path = "data/event.log";
            match std::fs::read_to_string(log_path) {
                Ok(content) => {
                    let mut events = Vec::new();
                    for line in content.lines() {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                            if let Some(id) = event["id"].as_u64() {
                                if id >= from && id <= to {
                                    let summary = format!("event #{}: {}",
                                        id,
                                        serde_json::to_string(&event["kind"]).unwrap_or_default()
                                    );
                                    events.push(Value::string(&summary));
                                }
                                if id > to {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(Value::List(events))
                }
                Err(e) => Err(format!("history/zoom: {}", e)),
            }
        });

        // history/find — search events by text content
        self.define_native("history/find", 1, |args| {
            let query = match &args[0] {
                Value::String(s) => s.clone(),
                other => return Err(format!("history/find: expected string, got {}", other)),
            };

            let q = query.to_lowercase();
            let log_path = "data/event.log";
            match std::fs::read_to_string(log_path) {
                Ok(content) => {
                    let mut results = Vec::new();
                    for line in content.lines() {
                        if line.to_lowercase().contains(&q) {
                            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                                let id = event["id"].as_u64().unwrap_or(0);
                                results.push(Value::string(&format!("#{}", id)));
                            }
                        }
                    }
                    Ok(Value::List(results))
                }
                Err(e) => Err(format!("history/find: {}", e)),
            }
        });

        // Model invocation — calls any OpenAI-compatible API
        self.define_native("model/invoke", 1, |args| {
            let prompt = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };

            let config = crate::kernel::model::ModelConfig::default();
            let request = crate::kernel::model::ModelRequest::from_prompt(&prompt);

            match crate::kernel::model::invoke_model(&config, &request) {
                Ok(resp) => {
                    // Record cost for logging
                    if resp.cost > 0.0 {
                        let _ = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open("data/model-costs.log")
                            .map(|mut f| {
                                use std::io::Write;
                                let _ = writeln!(f, "{} | model={} tokens={}+{} cost=${:.6}",
                                    chrono::Utc::now().to_rfc3339(),
                                    resp.model_used,
                                    resp.tokens_prompt,
                                    resp.tokens_generated,
                                    resp.cost);
                            });
                    }
                    Ok(Value::Tagged {
                        family: "result".into(),
                        variant: "Ok".into(),
                        fields: vec![
                            Value::string(&resp.text),
                            Value::string(&resp.model_used),
                            Value::int(resp.tokens_generated as i64),
                            Value::Float(resp.cost),
                        ],
                    })
                }
                Err(e) => Ok(Value::Tagged {
                    family: "result".into(),
                    variant: "Err".into(),
                    fields: vec![Value::string(&e)],
                }),
            }
        });

        // Agent think: structured cognition call
        self.define_native("agent/think", 2, |args| {
            let context = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            let instruction = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };

            let prompt = format!(
                "Context:
{}

Instruction:
{}

Response:",
                context, instruction
            );

            let config = crate::kernel::model::ModelConfig::default();
            let request = crate::kernel::model::ModelRequest::from_prompt(&prompt);

            match crate::kernel::model::invoke_model(&config, &request) {
                Ok(resp) => Ok(Value::Tagged {
                    family: "result".into(),
                    variant: "Ok".into(),
                    fields: vec![Value::string(&resp.text)],
                }),
                Err(e) => Ok(Value::Tagged {
                    family: "result".into(),
                    variant: "Err".into(),
                    fields: vec![Value::string(&e)],
                }),
            }
        });

        // Model chat: multi-turn conversation
        self.define_native("model/chat", 2, |args| {
            let messages_val = &args[0];
            let config_override = &args[1];

            let messages = match messages_val {
                Value::List(items) => {
                    items.iter().filter_map(|item| {
                        match item {
                            Value::List(parts) if parts.len() == 2 => {
                                match (&parts[0], &parts[1]) {
                                    (Value::String(role), Value::String(content)) => {
                                        Some(crate::kernel::model::Message {
                                            role: role.clone(),
                                            content: content.clone(),
                                        })
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        }
                    }).collect::<Vec<_>>()
                }
                _ => return Err("model/chat: expected list of (role content) pairs".into()),
            };

            if messages.is_empty() {
                return Err("model/chat: at least one message required".into());
            }

            let mut config = crate::kernel::model::ModelConfig::default();

            // Check for model override
            if let Value::Keyword(model_name) = config_override {
                config.model = model_name.to_string();
            }

            let request = crate::kernel::model::ModelRequest::from_chat(messages);

            match crate::kernel::model::invoke_model(&config, &request) {
                Ok(resp) => Ok(Value::string(&resp.text)),
                Err(e) => Err(format!("model/chat: {}", e)),
            }
        });
    }
}
