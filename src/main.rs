//! Continuum: a model inhabiting a persistent Lisp world.

use anyhow::{Context, Result};
use persistent_lisp_harness::{
    EvalInterruptHandle, Executor, ExecutorConfig, Kernel, ModelInterruptHandle, OpenRouterModel,
    OutputSink, Scheduler, SchedulerError, TurnOutcome,
};
use std::collections::VecDeque;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
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

fn load_kernel() -> Result<Kernel> {
    if snapshot_files_exist() {
        let kernel = Kernel::recover_from_latest()
            .context("continuity violation: snapshots exist but none recover")?;
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
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    executor: Executor,
    model_interrupt: ModelInterruptHandle,
    eval_interrupt: EvalInterruptHandle,
    intervention_gate: Arc<Mutex<()>>,
) {
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            // Publish cancellation and the corresponding message atomically with
            // respect to main's acknowledgement of sticky pre-activation signals.
            let _intervention = intervention_gate.lock().unwrap();
            eval_interrupt.request_interrupt();
            model_interrupt.request_interrupt();
            if let Err(error) = executor.cancel() {
                slog(format!("[executor] cancellation failed: {error}"));
            }
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

fn start_http(
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    executor: Executor,
    model_interrupt: ModelInterruptHandle,
    eval_interrupt: EvalInterruptHandle,
    intervention_gate: Arc<Mutex<()>>,
) {
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
                    let message = url::form_urlencoded::parse(body.as_bytes())
                        .find_map(|(name, value)| (name == "message").then(|| value.into_owned()))
                        .unwrap_or_default();
                    if !message.is_empty() {
                        let _intervention = intervention_gate.lock().unwrap();
                        eval_interrupt.request_interrupt();
                        model_interrupt.request_interrupt();
                        if let Err(error) = executor.cancel() {
                            slog(format!("[executor] cancellation failed: {error}"));
                        }
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
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    slog("Continuum v0.2 — persistent context → model Lisp action → result");
    let mut kernel = load_kernel()?;
    let eval_interrupt = kernel.eval_interrupt_handle();
    kernel.set_output_sink(OutputSink::new(|message| {
        slog(message.trim_end_matches('\n'))
    }));
    kernel.set_root_instructions_if_empty(root_instructions());

    let workspace = std::env::var_os("CONTINUUM_AGENT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("data/workspace").to_path_buf());
    let executor = Executor::new(ExecutorConfig::with_working_directory(workspace))
        .context("initialize Bash executor")?;
    let scheduler = Scheduler::new(OpenRouterModel::default(), executor.clone());
    let model_interrupt = scheduler.model_interrupt_handle();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let intervention_gate = Arc::new(Mutex::new(()));
    start_input_thread(
        tx.clone(),
        executor.clone(),
        model_interrupt.clone(),
        eval_interrupt.clone(),
        intervention_gate.clone(),
    );
    start_http(
        tx,
        executor,
        model_interrupt.clone(),
        eval_interrupt.clone(),
        intervention_gate.clone(),
    );

    enum RuntimeEvent {
        Human(Option<String>),
        Turn(Result<TurnOutcome, SchedulerError>),
    }

    let mut snapshot_timer = Instant::now();
    loop {
        kernel.check_wake_timers();
        if snapshot_timer.elapsed() >= Duration::from_secs(3600) {
            if let Err(error) = kernel.snapshot() {
                slog(format!("[snapshot] {}", error));
            }
            snapshot_timer = Instant::now();
        }

        let event = tokio::select! {
            biased;
            message = rx.recv() => RuntimeEvent::Human(message),
            outcome = scheduler.run_turn(&mut kernel) => RuntimeEvent::Turn(outcome),
        };
        match event {
            RuntimeEvent::Human(Some(message)) => {
                // The selected turn future is now dropped. Clear sticky pre-activation
                // interrupts before starting a fresh turn that includes this message.
                let _intervention = intervention_gate.lock().unwrap();
                model_interrupt.clear_pending();
                eval_interrupt.clear_pending();
                if matches!(message.as_str(), "!!exit" | "!!quit") {
                    if let Err(error) = kernel.snapshot() {
                        eprintln!("[snapshot] {}", error);
                    }
                    return Ok(());
                }
                deliver_human(&mut kernel, message);
            }
            RuntimeEvent::Human(None) => return Ok(()),
            RuntimeEvent::Turn(Ok(TurnOutcome::Evaluated { source, result, .. })) => {
                slog(format!("[lisp] {} => {}", source, result));
            }
            RuntimeEvent::Turn(Ok(TurnOutcome::ToolCompleted { source, result, .. })) => {
                slog(format!("[tool] {} => {}", source, result));
            }
            RuntimeEvent::Turn(Ok(TurnOutcome::Spawned { child_id, .. })) => {
                slog(format!("[agent] spawned {}", child_id));
            }
            RuntimeEvent::Turn(Ok(TurnOutcome::Returned { result, .. })) => {
                slog(format!("[agent] child returned {}", result));
            }
            RuntimeEvent::Turn(Ok(TurnOutcome::Replied { text, .. })) => {
                add_chat("agent", text.clone());
                slog(format!("[agent] {}", text));
            }
            RuntimeEvent::Turn(Ok(TurnOutcome::Idle)) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            RuntimeEvent::Turn(Err(error)) => {
                slog(format!("[turn] {}", error));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
