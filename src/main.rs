//! Continuum: a model inhabiting a persistent Lisp world.

use persistent_lisp_harness::{
    Executor, ExecutorConfig, Kernel, ModelInterruptHandle, OpenRouterModel, Scheduler, TurnOutcome,
};
use std::collections::VecDeque;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

static LOG_BUF: LazyLock<Arc<Mutex<VecDeque<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(1000))));
static CHAT_HISTORY: LazyLock<Arc<Mutex<VecDeque<ChatEntry>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(VecDeque::new())));

#[derive(Clone, serde::Serialize)]
struct ChatEntry {
    role: String,
    message: String,
    timestamp: String,
}

fn slog(message: impl AsRef<str>) {
    let message = message.as_ref();
    println!("{}", message);
    let mut log = LOG_BUF.lock().unwrap();
    if log.len() == 1000 {
        log.pop_front();
    }
    log.push_back(message.to_string());
}

fn snapshot_files_exist() -> bool {
    std::fs::read_dir("snapshots")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
}

fn load_kernel() -> Result<Kernel, String> {
    if snapshot_files_exist() {
        let kernel = Kernel::recover_from_latest().map_err(|e| {
            format!(
                "continuity violation: snapshots exist but none recover: {}",
                e
            )
        })?;
        slog("[kernel] recovered latest valid snapshot");
        Ok(kernel)
    } else {
        slog("[kernel] fresh start");
        Ok(Kernel::new())
    }
}

fn root_instructions() -> String {
    r#"You are Continuum, a persistent agent inhabiting a live Lisp machine.
Choose exactly one useful Lisp action per turn. Its evaluated result returns in your next context.
Use definitions to build reusable namespaced tools. Inspect before guessing.
Use (bash "command") only as a top-level form. It runs in your fixed agent workspace.
Use (model/call "prompt") only as a top-level form for a focused model subtask.
Use (agent/call "name" "task") only as a top-level form to create a child agent.
A child finishes with top-level (agent/return value).
Respond to a pending human message with top-level (message/reply "message-id" "text").
Do not emit prose or Markdown: emit one Lisp form."#
        .into()
}

fn add_chat(role: &str, message: impl Into<String>) {
    let mut history = CHAT_HISTORY.lock().unwrap();
    if history.len() == 1_000 {
        history.pop_front();
    }
    history.push_back(ChatEntry {
        role: role.into(),
        message: message.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    });
}

fn deliver_human(kernel: &mut Kernel, text: String) {
    add_chat("user", text.clone());
    match kernel.human_message(&text) {
        Ok(id) => slog(format!("[human:{}] queued for active frames", id)),
        Err(error) => slog(format!("[human] rejected: {}", error)),
    }
}

fn start_input_thread(
    tx: mpsc::Sender<String>,
    executor: Executor,
    model_interrupt: ModelInterruptHandle,
) {
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            // Any human intervention cancels an in-flight shell process group.
            persistent_lisp_harness::vm::eval::request_interrupt();
            model_interrupt.request_interrupt();
            executor.cancel();
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn chat_html() -> String {
    CHAT_HISTORY
        .lock()
        .unwrap()
        .iter()
        .map(|entry| {
            let class = if entry.role == "user" {
                "msg user"
            } else {
                "msg agent"
            };
            format!(
                r#"<div class="{}"><div>{}</div><div class="meta">{}</div></div>"#,
                class,
                escape_html(&entry.message),
                escape_html(&entry.timestamp)
            )
        })
        .collect()
}

fn start_http(tx: mpsc::Sender<String>, executor: Executor, model_interrupt: ModelInterruptHandle) {
    let logs = LOG_BUF.clone();
    thread::spawn(move || {
        let server = tiny_http::Server::http("0.0.0.0:8080").expect("HTTP listen failed");
        slog("[http] listening on http://0.0.0.0:8080");
        let thoughts = include_str!("../web/thoughts.html");
        let chat = include_str!("../web/chat.html");
        for mut request in server.incoming_requests() {
            let method = request.method().as_str();
            let url = request.url();
            let response = match (method, url) {
                ("GET", "/" | "/thoughts") => tiny_http::Response::from_string(thoughts)
                    .with_header(
                        "Content-Type: text/html; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                ("GET", "/chat") => tiny_http::Response::from_string(chat).with_header(
                    "Content-Type: text/html; charset=utf-8"
                        .parse::<tiny_http::Header>()
                        .unwrap(),
                ),
                ("GET", "/thoughts.json") => tiny_http::Response::from_string(
                    serde_json::to_string(&*logs.lock().unwrap()).unwrap(),
                )
                .with_header(
                    "Content-Type: application/json"
                        .parse::<tiny_http::Header>()
                        .unwrap(),
                ),
                ("GET", "/chat/history") => tiny_http::Response::from_string(chat_html())
                    .with_header(
                        "Content-Type: text/html"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                ("POST", "/chat/send") => {
                    let mut body = String::new();
                    let _ = request
                        .as_reader()
                        .take(64 * 1024)
                        .read_to_string(&mut body);
                    let message = urlencoding::decode(body.trim_start_matches("message="))
                        .unwrap_or_default()
                        .into_owned();
                    if !message.is_empty() {
                        persistent_lisp_harness::vm::eval::request_interrupt();
                        model_interrupt.request_interrupt();
                        executor.cancel();
                        let _ = tx.send(message);
                    }
                    tiny_http::Response::from_string(chat_html()).with_header(
                        "Content-Type: text/html"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    )
                }
                _ => tiny_http::Response::from_string("not found").with_status_code(404),
            };
            let _ = request.respond(response);
        }
    });
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    slog("Continuum v0.2 — persistent context → model Lisp action → result");
    let mut kernel = load_kernel().unwrap_or_else(|error| {
        eprintln!("[kernel] {}", error);
        std::process::exit(2);
    });
    *persistent_lisp_harness::vm::eval::PRINT_HOOK
        .lock()
        .unwrap() = Some(|message| slog(message));
    if let Some(root) = kernel.frames.first_mut()
        && root.state.instructions.is_empty()
    {
        root.state.instructions = root_instructions();
    }

    let workspace = std::env::var_os("CONTINUUM_AGENT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("data/workspace").to_path_buf());
    let executor = Executor::new(ExecutorConfig::rooted(workspace)).unwrap_or_else(|error| {
        eprintln!("[executor] {}", error);
        std::process::exit(2);
    });
    let scheduler = Scheduler::new(OpenRouterModel::default(), executor.clone());
    let model_interrupt = scheduler.model_interrupt_handle();
    let (tx, rx) = mpsc::channel();
    start_input_thread(tx.clone(), executor.clone(), model_interrupt.clone());
    start_http(tx, executor, model_interrupt);

    let mut snapshot_timer = Instant::now();
    loop {
        while let Ok(message) = rx.try_recv() {
            if matches!(message.as_str(), "!!exit" | "!!quit") {
                if let Err(error) = kernel.snapshot() {
                    eprintln!("[snapshot] {}", error);
                }
                return;
            }
            deliver_human(&mut kernel, message);
        }
        kernel.check_wake_timers();
        if snapshot_timer.elapsed() >= Duration::from_secs(3600) {
            if let Err(error) = kernel.snapshot() {
                slog(format!("[snapshot] {}", error));
            }
            snapshot_timer = Instant::now();
        }

        match scheduler.run_turn(&mut kernel).await {
            Ok(TurnOutcome::Evaluated { source, result, .. }) => {
                slog(format!("[lisp] {} => {}", source, result));
            }
            Ok(TurnOutcome::ToolCompleted { source, result, .. }) => {
                slog(format!("[tool] {} => {}", source, result));
            }
            Ok(TurnOutcome::Spawned { child_id, .. }) => {
                slog(format!("[agent] spawned {}", child_id))
            }
            Ok(TurnOutcome::Returned { result, .. }) => {
                slog(format!("[agent] child returned {}", result))
            }
            Ok(TurnOutcome::Replied { text, .. }) => {
                add_chat("agent", text.clone());
                slog(format!("[agent] {}", text));
            }
            Ok(TurnOutcome::Idle) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(error) => {
                slog(format!("[turn] {}", error));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
