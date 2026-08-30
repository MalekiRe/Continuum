//! Persistent Agent Lisp Harness — Continuous Agent
//!
//! "Unless it explicitly returns or waits, the kernel immediately
//!  schedules its next turn. Human messages and control work take
//!  priority. There is no idle backoff."
//!     — The Design

use persistent_lisp_harness::kernel::{self, SnapshotKind, FrameStatus};
use persistent_lisp_harness::Kernel;
use std::collections::VecDeque;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};


/// Shared log buffer — last 1000 lines, used by /thoughts endpoint.
static LOG_BUF: std::sync::LazyLock<Arc<Mutex<VecDeque<String>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(1000))));


/// Shared chat history for /chat endpoint.
static CHAT_HISTORY: std::sync::LazyLock<Arc<Mutex<Vec<ChatEntry>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

/// Push a log line to stdout and the shared buffer.
fn slog(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    println!("{msg}");
    let mut buf = LOG_BUF.lock().unwrap();
    if buf.len() >= 1000 {
        buf.pop_front();
    }
    buf.push_back(msg.to_string());
}

/// Chat message for /chat endpoint.
#[derive(Clone, serde::Serialize)]
struct ChatEntry {
    role: String,
    message: String,
    timestamp: String,
}

fn load_or_create_kernel() -> &'static mut Kernel {
    if Path::new("snapshots").exists() {
        match Kernel::recover_from_latest() {
            Ok(k) => {
                let ptr = Box::leak(Box::new(k));
                slog("[kernel] recovered from snapshot");
                return ptr;
            }
            Err(e) => {
                slog(&format!("[kernel] recovery failed: {} — starting fresh", e));
            }
        }
    }
    let k = Kernel::new();
    let _ = std::fs::create_dir_all("data");
    let ptr = Box::leak(Box::new(k));
    slog(&format!("[kernel] fresh start — version {}", ptr.version));
    ptr
}

/// Check for human messages from stdin. Returns true if exit was requested.
fn handle_human_input(kernel: &mut Kernel, rx: &mpsc::Receiver<String>) -> bool {
    let human_msg = rx.try_recv().ok();
    if let Some(msg) = human_msg {
        if msg == "!!exit" || msg == "!!quit" {
            kernel.snapshot(SnapshotKind::Full);
            slog("[kernel] goodbye!");
            return true;
        }
        kernel.human_message(&msg);
        slog("[human] delivered to agent frame");
    }
    false
}

/// Take an hourly full snapshot if due.
fn check_hourly_snapshot(kernel: &mut Kernel, timer: &mut Instant) {
    if timer.elapsed() >= Duration::from_secs(3600) {
        kernel.check_hourly_snapshot();
        *timer = Instant::now();
    }
}

/// Check if the agent's root frame has completed and needs restarting.
fn maybe_restart_agent(kernel: &mut Kernel) -> bool {
    if !kernel.frames.is_empty()
        && kernel.frames.iter().all(|f| f.status == FrameStatus::Completed)
    {
        slog("[agent] all frames completed. Starting fresh...");
        kernel.eval("(println \"Agent ready\")").ok();
        true
    } else {
        false
    }
}

/// Check for a pending subagent result and deliver it to the agent.
fn handle_subagent_result(kernel: &mut Kernel) {
    if let Some(result) = kernel.take_subagent_result() {
        slog(&format!("[agent] subagent returned: {}", result));
    }
}

/// Check if the current frame is waiting for human input, and deliver it.
fn handle_waiting_frame(kernel: &mut Kernel) -> bool {
    let is_waiting = kernel
        .frames
        .last()
        .map(|f| f.status == FrameStatus::Waiting)
        .unwrap_or(false);

    if is_waiting {
        if let Some(msg) = kernel.take_pending_message() {
            let _ = kernel.eval_repl(&format!(
                r#"(agent/cognize "Human message received: {}")"#,
                msg
            ));
        }
        true
    } else {
        false
    }
}

/// Supervision checks — all advisory, agent stays in control.
fn check_supervision(kernel: &mut Kernel) {
    let Some(started_at) = kernel.eval_started_at else {
        return;
    };
    let now = chrono::Utc::now();
    let elapsed = (now - started_at).num_seconds() as u64;
    let cfg = &kernel.supervision;

    if elapsed >= cfg.advisory_after_seconds && elapsed % 300 == 0 {
        slog(&format!("eval has been running for {}s", elapsed));
        if let Some(frame) = kernel.frames.last_mut() {
            frame.message_queue.push(
                r#"(system/SupervisorNotice "You have been working on this task for a while. Consider whether your current approach is making progress or could be optimized.")"#.into()
            );
        }
        return;
    }

    if elapsed < cfg.min_elapsed_seconds {
        return;
    }

    let cutoff = now - chrono::Duration::seconds(cfg.window_seconds as i64);
    while let Some(front) = kernel.token_reports.front() {
        if front.0 < cutoff {
            kernel.token_reports.pop_front();
        } else {
            break;
        }
    }

    let window_tokens: u64 = kernel.token_reports.iter().map(|(_, n)| n).sum();
    let expected_secs = if cfg.expected_tokens_per_sec > 0 {
        window_tokens / cfg.expected_tokens_per_sec
    } else {
        0
    };
    let actual_elapsed = elapsed - cfg.min_elapsed_seconds;

    if expected_secs > 0 && actual_elapsed > expected_secs * cfg.timeout_multiplier {
        slog(&format!("{}s elapsed, {} tokens in window (expected ~{}s at {} tok/s)",
            elapsed, window_tokens, expected_secs, cfg.expected_tokens_per_sec));
        if let Some(frame) = kernel.frames.last_mut() {
            frame.message_queue.push(
                r#"(system/SupervisorNotice "Token rate is low — consider optimizing your approach. Batch operations, reduce redundant testing, or streamline your workflow.")"#.into()
            );
        }
    }

    if window_tokens == 0 && actual_elapsed >= 300 {
        slog(&format!("{}s with no tokens reported — may be waiting on a blocking call", elapsed));
        if let Some(frame) = kernel.frames.last_mut() {
            frame.message_queue.push(
                r#"(system/SupervisorNotice "No tokens reported in 5+ minutes. If waiting on a tool call, consider whether it has hung or if you should try a different approach.")"#.into()
            );
        }
    }
}
/// Run one cognition turn for the agent.
fn run_cognition_turn(kernel: &mut Kernel) {
    let source = match kernel.take_pending_message() {
        Some(msg) => format!("(agent/cognize {:?})", msg),
        None => "(agent/step)".to_string(),
    };
    let is_chat = source.starts_with("(agent/cognize");
    match kernel.eval(&source) {
        Ok(v) => {
            if is_chat {
                let response = format!("{}", v);
                let trimmed = response.trim_matches('\"');
                if !trimmed.is_empty() && trimmed != "nil" {
                    if let Ok(mut history) = CHAT_HISTORY.lock() {
                        history.push(ChatEntry {
                            role: "agent".into(),
                            message: trimmed.to_string(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                }
            }
        }
        Err(e) => slog(&format!("[agent] error: {}", e)),
    }
}

fn main() {
    let _ = dotenvy::dotenv();

    slog("╔══════════════════════════════════════════════╗");
    slog("║  Persistent Agent Lisp Harness v0.1.0       ║");
    slog("║  Continuous autonomous agent.                ║");
    slog("║  Type a message + Enter to interact.         ║");
    slog("║  Type '!!exit' to quit.                     ║");
    slog("╚══════════════════════════════════════════════╝");

    let kernel = load_or_create_kernel();

    // Load agent core
    let agent_core = r#"
                                                                        
        
        
        (define-data result/Result
          (Ok value)
          (Err problem)
          (Cancelled reason)
          (Indeterminate problem))

        (define (agent/step)
          (let ((response (model/chat "You are Continuum, a persistent Lisp agent. Generate a single brief thought about what you could explore or learn. Be curious. Be original. One sentence.")))
            (println response)
            nil))

        (define (agent/cognize msg)
          (let ((response (model/chat msg)))
            (println response)
            msg))
    "#;

    // Route all Lisp output through the log buffer
    *persistent_lisp_harness::vm::eval::PRINT_HOOK.lock().unwrap() = Some(|msg| slog(msg));

    match kernel.eval(agent_core) {
        Ok(_) => slog("[agent] core loaded"),
        Err(e) => slog(&format!("[agent] core: {}", e)),
    }

    // Human input channel
    let (tx, rx) = mpsc::channel::<String>();
    let chat_tx = tx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(input) => {
                    let input = input.trim().to_string();
                    if !input.is_empty() {
                        let _ = tx.send(input);
                    }
                }
                Err(_) => break,
            }
        }

    });
    // ---- HTTP server — HTML pages + chat API ----
    let log_buf = LOG_BUF.clone();


    thread::spawn(move || {
        let server = tiny_http::Server::http("0.0.0.0:8080").unwrap();
        slog("[http] listening on http://0.0.0.0:8080");

        let thoughts_html = include_str!("../web/thoughts.html");
        let chat_html = include_str!("../web/chat.html");

        loop {
            let Ok(mut req) = server.recv() else { continue; };
            let url = req.url().to_string();
            let method = req.method().as_str().to_string();

            let resp: tiny_http::Response<std::io::Cursor<Vec<u8>>> =
                match (method.as_str(), url.as_str()) {
                    ("GET", "/thoughts" | "/") => {
                        tiny_http::Response::from_string(thoughts_html.to_string())
                            .with_header("Content-Type: text/html; charset=utf-8".parse::<tiny_http::Header>().unwrap())
                    }
                    ("GET", "/chat") => {
                        tiny_http::Response::from_string(chat_html.to_string())
                            .with_header("Content-Type: text/html; charset=utf-8".parse::<tiny_http::Header>().unwrap())
                    }
                    ("GET", "/thoughts.json") => {
                        let json = serde_json::to_string(&*log_buf.lock().unwrap()).unwrap();
                        tiny_http::Response::from_string(json)
                            .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap())
                    }
                    ("GET", "/chat/history") => {
                        let history = CHAT_HISTORY.lock().unwrap();
                        let mut html = String::new();
                        for entry in history.iter() {
                            let cls = if entry.role == "user" { "msg user" } else { "msg agent" };
                            html.push_str(&format!(
                                r#"<div class="{}"><div>{}</div><div class="meta">{}</div></div>"#,
                                cls, entry.message, entry.timestamp
                            ));
                        }
                        tiny_http::Response::from_string(html)
                            .with_header("Content-Type: text/html".parse::<tiny_http::Header>().unwrap())
                    }
                    ("POST", "/chat/send") => {
                        let mut body = String::new();
                        let _ = req.as_reader().read_to_string(&mut body);
                        let msg = urlencoding::decode(body.trim_start_matches("message="))
                            .unwrap_or_default().to_string();

                        if !msg.is_empty() {
                            let entry = ChatEntry {
                                role: "user".into(),
                                message: msg.clone(),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                            };
                            CHAT_HISTORY.lock().unwrap().push(entry);
                            let _ = chat_tx.send(msg);
                        }

                        // Return updated chat HTML
                        let history = CHAT_HISTORY.lock().unwrap();
                        let mut html = String::new();
                        for entry in history.iter() {
                            let cls = if entry.role == "user" { "msg user" } else { "msg agent" };
                            html.push_str(&format!(
                                r#"<div class="{}"><div>{}</div><div class="meta">{}</div></div>"#,
                                cls, entry.message, entry.timestamp
                            ));
                        }
                        tiny_http::Response::from_string(html)
                            .with_header("Content-Type: text/html".parse::<tiny_http::Header>().unwrap())
                    }
                    _ => tiny_http::Response::from_string("not found".to_string()).with_status_code(404),
                };
            let _ = req.respond(resp);
        }
    });
    // Continuous cognition loop
    let mut hourly_timer = Instant::now();

    loop {
        if handle_human_input(kernel, &rx) {
            break;
        }
        check_supervision(kernel);
        check_hourly_snapshot(kernel, &mut hourly_timer);
        kernel.check_wake_timers();


        if maybe_restart_agent(kernel) {
            continue;
        }
        handle_subagent_result(kernel);

        if handle_waiting_frame(kernel) {
            continue;
        }

        // Check for pending messages and cognize
        run_cognition_turn(kernel);
    }
}
